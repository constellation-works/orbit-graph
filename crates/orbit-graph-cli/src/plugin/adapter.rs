use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};

use git2::{Oid, Repository};
use serde_json::{Value, json};

use orbit_graph::current_observation_cutoff;
use orbit_graph::{
    DeliveryEvidence, DeliveryImport, GraphError, HybridTaskHit, Provenance, TaskAssociation,
    TaskTextAvailability, TemporalFact, TemporalStatus,
};

use super::{json_error, string_field};

const ORBIT_OUTPUT_LIMIT: u64 = 1_048_576;
/// Bytes of a failed child's stderr quoted in its error.
const STDERR_EXCERPT_LIMIT: usize = 4_096;
/// How long a terminated `orbit` process group has to exit after SIGTERM
/// before it is killed (STD-03 §R13).
const TERMINATION_GRACE: Duration = Duration::from_secs(5);
/// Poll interval while waiting on a child or its readers.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// MCP protocol revision announced to `orbit mcp serve`.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const MCP_INITIALIZE_ID: u64 = 1;
const MCP_CALL_ID: u64 = 2;

pub(super) struct OrbitAdapter<'a> {
    repository: &'a Path,
    workspace: Option<&'a str>,
    /// The bound on each Orbit call, resolved once from the environment.
    timeout: Duration,
}

/// What `orbit_sync` concluded about one run.
pub(super) enum RunVerdict {
    /// A verified delivery, ready to import.
    Deliverable(Box<VerifiedDelivery>),
    /// The run was examined and is not an eligible delivery; the reason says
    /// which check it failed.
    Excluded(String),
}

/// A run's verified delivery.
pub(super) struct VerifiedDelivery {
    /// The delivery the run attests.
    pub(super) delivery: DeliveryImport,
    /// The caller's snapshot of its task, if one was supplied.
    pub(super) supplied_snapshot: Option<TaskAssociation>,
}

/// Hits from `orbit.search`, and how many malformed results were dropped.
pub(super) struct HybridSearch {
    pub(super) hits: Vec<HybridTaskHit>,
    pub(super) dropped: usize,
}

impl<'a> OrbitAdapter<'a> {
    pub(super) fn new(repository: &'a Path, workspace: Option<&'a str>, timeout: Duration) -> Self {
        Self {
            repository,
            workspace,
            timeout,
        }
    }

    fn require_workspace(&self) -> Result<&str, GraphError> {
        self.workspace
            .filter(|workspace| !workspace.trim().is_empty())
            .ok_or_else(|| {
                GraphError::invalid_input(
                    "route authoritative Orbit request",
                    "workspace",
                    "workspace is required; ORBIT_TOOL_WORKSPACE_ROOT and cwd are not authority selectors",
                )
            })
    }

    /// Resolve the explicit workspace through Orbit's MCP-only discovery tool.
    ///
    /// `orbit.workspace.list` is served by `orbit mcp serve`, not the generic
    /// `orbit tool run` registry, and its public rows carry no checkout path.
    /// The requested repository is therefore bound to the selected workspace by
    /// the one repository identity discovery does publish: `git_remote`, which
    /// Orbit records from the checkout's `origin`.
    fn require_authority(&self) -> Result<AuthorityRoute, GraphError> {
        let workspace = self.require_workspace()?;
        self.authority_for_workspace(workspace, None)
    }

    fn authority_for_workspace(
        &self,
        workspace: &str,
        expected_name: Option<&str>,
    ) -> Result<AuthorityRoute, GraphError> {
        let value = self.mcp_tool_call("orbit.workspace.list", json!({}))?;
        let selected = value
            .get("workspaces")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                GraphError::invalid_data(
                    "decode Orbit workspace discovery",
                    "orbit.workspace.list response has no workspaces array",
                )
            })?
            .iter()
            .find(|item| {
                string_field(item, "id") == Some(workspace)
                    || (expected_name.is_none() && string_field(item, "name") == Some(workspace))
            })
            .ok_or_else(|| {
                GraphError::invalid_input(
                    "validate Orbit workspace authority",
                    "workspace",
                    format!("workspace {workspace:?} is not registered in the explicit authority"),
                )
            })?;
        let actual_id = required_string(selected, "id")?;
        if expected_name.is_some_and(|name| string_field(selected, "name") != Some(name)) {
            return Err(GraphError::invalid_data(
                "verify Orbit task workspace owner",
                "task owner name does not match the selected public workspace",
            ));
        }
        let status = string_field(selected, "status");
        if (expected_name.is_some() && status != Some("active"))
            || status.is_some_and(|status| status != "active")
        {
            return Err(GraphError::invalid_input(
                "validate Orbit workspace authority",
                "workspace",
                format!("workspace {actual_id:?} is not active"),
            ));
        }
        let remote = string_field(selected, "git_remote").ok_or_else(|| {
            GraphError::invalid_input(
                "validate Orbit workspace repository",
                "workspace",
                format!(
                    "workspace {actual_id:?} publishes no git_remote, so the requested repository cannot be bound to it"
                ),
            )
        })?;
        verify_repository_remote(self.repository, remote)?;
        Ok(AuthorityRoute {
            workspace_id: actual_id.to_string(),
        })
    }

    pub(super) fn task_show(&self, task_id: &str) -> Result<Value, GraphError> {
        self.task_observation(task_id).map(|(task, _)| task)
    }

    fn task_observation(&self, task_id: &str) -> Result<(Value, AuthorityRoute), GraphError> {
        let workspace = self.require_workspace()?;
        if Path::new(workspace).is_absolute() {
            self.verify_explicit_checkout(workspace)?;
            // Orbit resolves an explicit path against its own registry. Its
            // unprojected task read publishes the owning logical workspace;
            // projecting fields would omit that proof. A remote alone cannot
            // identify a workspace: multiple registered checkouts may share it.
            let task = self.tool_run(
                "orbit.task.show",
                json!({"id": task_id, "workspace": workspace, "model": "codex"}),
            )?;
            if required_string(&task, "id")? != task_id {
                return Err(GraphError::invalid_data(
                    "verify Orbit task workspace owner",
                    "selected workspace returned a different task ID",
                ));
            }
            let owner = task.get("workspace").ok_or_else(|| {
                GraphError::invalid_data(
                    "verify Orbit task workspace owner",
                    "task read omitted its public workspace owner",
                )
            })?;
            let owner_id = required_string(owner, "id")?;
            let owner_name = required_string(owner, "name")?;
            let route = self.authority_for_workspace(owner_id, Some(owner_name))?;
            if route.workspace_id != owner_id {
                return Err(GraphError::invalid_data(
                    "verify Orbit task workspace owner",
                    "task owner is not the selected public workspace ID",
                ));
            }
            return Ok((task, route));
        }

        let route = self.require_authority()?;
        let task = self.tool_run(
            "orbit.task.show",
            json!({
                "id": task_id,
                "workspace": route.workspace_id,
                "fields": ["id", "title", "description", "acceptance_criteria", "status", "created_at", "history", "job_run_id"],
                "model": "codex",
            }),
        )?;
        Ok((task, route))
    }

    fn verify_explicit_checkout(&self, workspace: &str) -> Result<(), GraphError> {
        let selector = Path::new(workspace).canonicalize().map_err(|source| {
            GraphError::io("resolve explicit Orbit workspace path", workspace, source)
        })?;
        let repository = self.repository.canonicalize().map_err(|source| {
            GraphError::io("resolve routed Git repository", self.repository, source)
        })?;
        if selector != repository {
            return Err(GraphError::invalid_input(
                "validate Orbit workspace repository",
                "workspace",
                "explicit workspace checkout path does not match the requested repository",
            ));
        }
        Ok(())
    }

    pub(super) fn task_snapshot(&self, task_id: &str) -> Result<TaskAssociation, GraphError> {
        let (task, route) = self.task_observation(task_id)?;
        task_snapshot_from_value(&task, &route.workspace_id)
    }

    pub(super) fn hybrid_search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<HybridSearch, GraphError> {
        let workspace = self.require_workspace()?;
        let value = self.tool_run(
            "orbit.search",
            json!({
                "workspace": workspace,
                "query": query,
                "kind": "task",
                "limit": limit,
                "model": "codex",
            }),
        )?;
        let results = value
            .get("results")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                GraphError::invalid_data("decode orbit.search results", "expected a results array")
            })?;
        let lexical = value.get("mode").and_then(Value::as_str) == Some("lexical");
        let hits = results
            .iter()
            .enumerate()
            .filter_map(|(position, item)| {
                let task_id = item.get("id")?.as_str()?;
                if task_id.trim().is_empty()
                    || item
                        .get("kind")
                        .is_some_and(|kind| kind.as_str() != Some("task"))
                {
                    return None;
                }
                let score = match item.get("score") {
                    Some(score) => {
                        let score = score.as_f64()?;
                        if !score.is_finite() || score < 0.0 {
                            return None;
                        }
                        score
                    }
                    None if lexical
                        && item.get("kind").and_then(Value::as_str) == Some("task")
                        && item.get("source").and_then(Value::as_str) == Some("lexical") =>
                    {
                        // Public lexical task hits omit scores. Preserve their
                        // original order as reciprocal-rank relevance weights,
                        // not semantic confidence. Dropped hits keep their ranks.
                        1.0 / (position + 1) as f64
                    }
                    None => return None,
                };
                Some(HybridTaskHit {
                    task_id: task_id.to_string(),
                    score,
                })
            })
            .collect::<Vec<_>>();
        Ok(HybridSearch {
            dropped: results.len() - hits.len(),
            hits,
        })
    }

    pub(super) fn delivery_from_run(
        &self,
        run_id: &str,
        branch: &str,
        supplied_snapshots: &BTreeMap<String, TaskAssociation>,
        repository_identity: &str,
    ) -> Result<RunVerdict, GraphError> {
        let workspace = self.require_workspace()?;
        let route = if Path::new(workspace).is_absolute() {
            self.verify_explicit_checkout(workspace)?;
            None
        } else {
            Some(self.require_authority()?)
        };
        let run = self.tool_run("orbit.workflow.run.show", json!({"id": run_id}))?;
        let state = run
            .pointer("/run/state")
            .or_else(|| run.get("state"))
            .and_then(Value::as_str);
        if state != Some("success") {
            return Ok(RunVerdict::Excluded(format!(
                "run {run_id} is not successful"
            )));
        }
        let Some(commit) = run
            .pointer("/pipeline_state/step_outputs/2")
            .or_else(|| run.pointer("/pipeline_state/pipeline/commit"))
        else {
            return Ok(RunVerdict::Excluded(format!(
                "run {run_id} has no public commit step output"
            )));
        };
        if commit.get("committed").and_then(Value::as_bool) != Some(true)
            || string_field(commit, "phase") != Some("commit")
        {
            return Ok(RunVerdict::Excluded(format!(
                "run {run_id} did not attest a committed output"
            )));
        }
        let task_id = required_string(commit, "task_id")?;
        let before = required_string(commit, "base_sha")?;
        let after = required_string(commit, "commit_sha")?;
        // A host-rewritten checkout selector needs the committed task's
        // public owner proof before any delivery can be imported.
        let observation = if route.is_none() {
            Some(self.task_observation(task_id)?)
        } else {
            None
        };
        if let Some(reason) = verify_git_delivery(self.repository, branch, before, after)? {
            return Ok(RunVerdict::Excluded(reason));
        }
        if let Some(reason) = verify_run_workspace(&run, self.repository)? {
            return Ok(RunVerdict::Excluded(reason));
        }
        let (current_task, observed_route) = match observation {
            Some(observation) => observation,
            None => self.task_observation(task_id)?,
        };
        let route = route.unwrap_or(observed_route);
        if required_string(&current_task, "id")? != task_id {
            return Ok(RunVerdict::Excluded(
                "selected workspace returned a different task ID".to_string(),
            ));
        }
        let task = task_snapshot_from_value(&current_task, route.workspace_id.as_str())?;
        let supplied_snapshot = supplied_snapshots.get(task_id).cloned();
        let finished_at = run
            .pointer("/run/finished_at")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(RunVerdict::Deliverable(Box::new(VerifiedDelivery {
            delivery: DeliveryImport {
                schema_version: orbit_graph::DELIVERY_IMPORT_SCHEMA_VERSION,
                repository: repository_identity.to_string(),
                landing_branch: branch.to_string(),
                before_revision: before.to_string(),
                after_revision: after.to_string(),
                delivery_id: format!("orbit-run:{run_id}:{task_id}"),
                evidence: DeliveryEvidence::VerifiedDelivery,
                source: Provenance {
                    system: "orbit.workflow.run.show+git".to_string(),
                    record_id: Some(run_id.to_string()),
                },
                delivered_at: TemporalFact {
                    status: TemporalStatus::Uncertain,
                    timestamp: finished_at,
                    source: Provenance {
                        system: "orbit.workflow.run.show.run.finished_at".to_string(),
                        record_id: Some(run_id.to_string()),
                    },
                },
                captured_at: current_observation_cutoff()?,
                tasks: vec![task],
            },
            supplied_snapshot,
        })))
    }

    fn tool_run(&self, name: &str, input: Value) -> Result<Value, GraphError> {
        let encoded = serde_json::to_string(&input).map_err(json_error)?;
        self.orbit_json(
            name,
            &["tool", "run", name, "--input", encoded.as_str(), "--full"],
        )
    }

    /// Configure an `orbit` child that leads its own process group, so the
    /// whole group, grandchildren included, can be terminated together
    /// (STD-03 §R11); inherits the caller's environment (including any
    /// host-issued callback credential); and dies with its parent on Linux.
    fn orbit_command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("orbit");
        command
            .current_dir(self.repository)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            command.process_group(0);
            // SAFETY: the closure runs in the forked child before exec and
            // only calls the async-signal-safe `prctl`.
            unsafe {
                command.pre_exec(|| {
                    #[cfg(target_os = "linux")]
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        command
    }

    /// Run `orbit <args>` for `tool` and decode its JSON stdout. A non-zero
    /// exit that carries Orbit's structured `{"code", "error"}` refusal is
    /// [`GraphError::OrbitRefused`]; any other is
    /// [`GraphError::Subprocess`].
    fn orbit_json(&self, tool: &str, args: &[&str]) -> Result<Value, GraphError> {
        const OPERATION: &str = "invoke public Orbit CLI";
        let deadline = Instant::now() + self.timeout;
        let mut child = GroupChild::spawn(self.orbit_command(args), OPERATION, self.repository)?;
        let (Some(stdout), Some(stderr)) = (child.child.stdout.take(), child.child.stderr.take())
        else {
            child.terminate();
            return Err(GraphError::invalid_data(
                OPERATION,
                "stdio pipes were not available",
            ));
        };
        let stdout_reader = thread::spawn(move || read_bounded(stdout));
        let stderr_reader = thread::spawn(move || read_bounded(stderr));
        loop {
            match child.has_exited() {
                Ok(true) => break,
                Ok(false) => {}
                Err(source) => {
                    child.terminate();
                    return Err(GraphError::io(
                        "wait for public Orbit CLI",
                        self.repository,
                        source,
                    ));
                }
            }
            if Instant::now() >= deadline {
                child.terminate();
                return Err(GraphError::timeout(
                    OPERATION,
                    self.timeout,
                    format!("orbit {tool} did not exit; its process group was terminated"),
                ));
            }
            thread::sleep(POLL_INTERVAL);
        }
        // A descendant that outlived the leader may still hold the pipes:
        // the sweep stops it before the readers are joined.
        let status = child.reap().map_err(|source| {
            GraphError::io("wait for public Orbit CLI", self.repository, source)
        })?;
        let stdout = join_capture(
            stdout_reader,
            "stdout",
            self.repository,
            deadline,
            self.timeout,
        )?;
        let stderr = join_capture(
            stderr_reader,
            "stderr",
            self.repository,
            deadline,
            self.timeout,
        )?;
        if stdout.len() as u64 + stderr.len() as u64 > ORBIT_OUTPUT_LIMIT {
            return Err(GraphError::invalid_data(
                OPERATION,
                format!("combined stdout/stderr exceeded {ORBIT_OUTPUT_LIMIT} bytes"),
            ));
        }
        if !status.success() {
            return Err(orbit_refusal(tool, stderr.as_slice()).unwrap_or_else(|| {
                GraphError::subprocess(OPERATION, describe_status(status), bounded_text(&stderr))
            }));
        }
        serde_json::from_slice(stdout.as_slice()).map_err(json_error)
    }

    /// Call one tool through a short-lived `orbit mcp serve` stdio session.
    ///
    /// The server is started without `--operator` or `--root`, so it serves
    /// exactly the authority Orbit gives this caller and every `tools/call`
    /// lands on Orbit's own policy and plugin-callback gates. The whole session
    /// shares one timeout and one output bound with [`Self::orbit_json`].
    fn mcp_tool_call(&self, name: &str, arguments: Value) -> Result<Value, GraphError> {
        let timeout = self.timeout;
        let deadline = Instant::now() + timeout;
        let mut command = self.orbit_command(&["mcp", "serve"]);
        command.stdin(Stdio::piped());
        let mut child = GroupChild::spawn(command, "invoke Orbit MCP server", self.repository)?;
        let (Some(stdin), Some(stdout), Some(stderr)) = (
            child.child.stdin.take(),
            child.child.stdout.take(),
            child.child.stderr.take(),
        ) else {
            child.terminate();
            return Err(GraphError::invalid_data(
                "invoke Orbit MCP server",
                "stdio pipes were not available",
            ));
        };
        let messages = spawn_line_reader(stdout);
        let stderr_reader = thread::spawn(move || read_bounded(stderr));
        let mut session = McpSession {
            child,
            stdin: Some(stdin),
            messages,
            stderr_reader: Some(stderr_reader),
            deadline,
            timeout,
        };
        let outcome = session.call(name, arguments);
        session.finish(outcome.is_ok());
        let response = outcome?;
        mcp_call_result(name, response)
    }
}

/// One bounded `orbit mcp serve` child and its stdio.
struct McpSession {
    child: GroupChild,
    stdin: Option<ChildStdin>,
    messages: Receiver<StdoutLine>,
    stderr_reader: Option<thread::JoinHandle<std::io::Result<Vec<u8>>>>,
    deadline: Instant,
    timeout: Duration,
}

enum StdoutLine {
    Line(Vec<u8>),
    Exceeded,
    Failed(std::io::Error),
}

impl McpSession {
    fn call(&mut self, name: &str, arguments: Value) -> Result<Value, GraphError> {
        let initialize = self.send(&json!({
            "jsonrpc": "2.0",
            "id": MCP_INITIALIZE_ID,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "orbit-graph", "version": env!("CARGO_PKG_VERSION")},
            },
        }));
        self.settle(initialize, MCP_INITIALIZE_ID)?;
        let call = self
            .send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .and_then(|()| {
                self.send(&json!({
                    "jsonrpc": "2.0",
                    "id": MCP_CALL_ID,
                    "method": "tools/call",
                    "params": {"name": name, "arguments": arguments},
                }))
            });
        self.settle(call, MCP_CALL_ID)
    }

    /// Await the response to a sent request. When the write itself failed, the
    /// server's own outcome (exit status, overflow, or timeout) is the better
    /// explanation, so it is reported in preference to the broken pipe.
    fn settle(&mut self, sent: Result<(), GraphError>, id: u64) -> Result<Value, GraphError> {
        let response = self.response(id);
        match sent {
            Ok(()) => response,
            Err(error) => Err(response.err().unwrap_or(error)),
        }
    }

    fn send(&mut self, message: &Value) -> Result<(), GraphError> {
        let mut line = serde_json::to_vec(message).map_err(json_error)?;
        line.push(b'\n');
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            GraphError::invalid_data("invoke Orbit MCP server", "stdin was already closed")
        })?;
        stdin
            .write_all(line.as_slice())
            .and_then(|()| stdin.flush())
            .map_err(|source| {
                GraphError::invalid_data(
                    "invoke Orbit MCP server",
                    format!("could not write request: {source}"),
                )
            })
    }

    /// Wait for the JSON-RPC response carrying `id`, skipping notifications.
    fn response(&mut self, id: u64) -> Result<Value, GraphError> {
        loop {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            let line = match self.messages.recv_timeout(remaining) {
                Ok(StdoutLine::Line(line)) => line,
                Ok(StdoutLine::Exceeded) => {
                    return Err(GraphError::invalid_data(
                        "invoke Orbit MCP server",
                        format!("stdout exceeded {ORBIT_OUTPUT_LIMIT} bytes"),
                    ));
                }
                Ok(StdoutLine::Failed(error)) => {
                    return Err(GraphError::invalid_data(
                        "read Orbit MCP server stdout",
                        error.to_string(),
                    ));
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(GraphError::timeout(
                        "invoke Orbit MCP server",
                        self.timeout,
                        format!("no response to JSON-RPC request {id}"),
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(GraphError::invalid_data(
                        "invoke Orbit MCP server",
                        format!("closed stdout before responding; {}", self.exit_detail()),
                    ));
                }
            };
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let message: Value = serde_json::from_slice(line.as_slice()).map_err(json_error)?;
            if message.get("method").is_some() || message.get("id") != Some(&json!(id)) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(GraphError::invalid_data(
                    "invoke Orbit MCP server",
                    format!(
                        "JSON-RPC error: {}",
                        string_field(error, "message").unwrap_or("unspecified")
                    ),
                ));
            }
            return message.get("result").cloned().ok_or_else(|| {
                GraphError::invalid_data(
                    "decode Orbit MCP response",
                    "JSON-RPC response has neither result nor error",
                )
            });
        }
    }

    /// Wait briefly for the child to exit so its stderr can explain a failure.
    fn exit_detail(&mut self) -> String {
        self.stdin = None;
        let exited = loop {
            match self.child.has_exited() {
                Ok(true) => break true,
                Ok(false) if Instant::now() < self.deadline => thread::sleep(POLL_INTERVAL),
                Ok(false) | Err(_) => break false,
            }
        };
        if !exited {
            return "server did not exit".to_string();
        }
        let Ok(status) = self.child.reap() else {
            return "server exit status could not be read".to_string();
        };
        let stderr = self
            .stderr_reader
            .take()
            .and_then(|reader| join_by(reader, self.deadline))
            .and_then(|joined| joined.ok())
            .and_then(Result::ok);
        match stderr {
            Some(stderr) => format!("{}: {}", describe_status(status), bounded_text(&stderr)),
            None => format!("{}; stderr was not captured", describe_status(status)),
        }
    }

    /// Close stdin and reap the server; a server still running at the
    /// deadline, or after any failure, is killed rather than waited on.
    fn finish(mut self, graceful: bool) {
        self.stdin = None;
        if graceful {
            while Instant::now() < self.deadline {
                match self.child.has_exited() {
                    Ok(true) => {
                        let _ = self.child.reap();
                        return;
                    }
                    Ok(false) => thread::sleep(POLL_INTERVAL),
                    Err(_) => break,
                }
            }
        }
        self.child.terminate();
    }
}

/// Stdout lines buffered between the reader thread and the session.
const MCP_LINE_QUEUE: usize = 64;

/// Stream newline-delimited stdout, stopping after [`ORBIT_OUTPUT_LIMIT`] bytes.
///
/// The queue holds [`MCP_LINE_QUEUE`] lines. When it is full the reader
/// waits, which backpressures the server through its stdout pipe
/// (`STD-03 §R2`); once the session drops the receiver, the reader's next
/// send fails and it stops.
fn spawn_line_reader(stdout: impl Read + Send + 'static) -> Receiver<StdoutLine> {
    let (sender, receiver) = mpsc::sync_channel(MCP_LINE_QUEUE);
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout).take(ORBIT_OUTPUT_LIMIT + 1);
        loop {
            let mut line = Vec::new();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => return,
                Ok(_) if reader.limit() == 0 => {
                    let _ = sender.send(StdoutLine::Exceeded);
                    return;
                }
                Ok(_) => {
                    if sender.send(StdoutLine::Line(line)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = sender.send(StdoutLine::Failed(error));
                    return;
                }
            }
        }
    });
    receiver
}

/// Unwrap an MCP `CallToolResult`, surfacing Orbit's structured refusals.
fn mcp_call_result(name: &str, result: Value) -> Result<Value, GraphError> {
    let payload = match result.get("structuredContent") {
        Some(structured) => structured.clone(),
        None => result
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .map(|text| serde_json::from_str(text).map_err(json_error))
            .transpose()?
            .ok_or_else(|| {
                GraphError::invalid_data(
                    "decode Orbit MCP response",
                    format!("{name} returned no structured content"),
                )
            })?,
    };
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(GraphError::orbit_refused(
            name,
            string_field(&payload, "code").unwrap_or("error"),
            string_field(&payload, "message").unwrap_or("unspecified"),
        ));
    }
    Ok(payload)
}

struct AuthorityRoute {
    workspace_id: String,
}

/// Bind the requested repository to a workspace by its published `git_remote`.
///
/// Orbit records a workspace's `git_remote` from its checkout's `origin`, so the
/// requested repository must have an `origin` naming the same remote. URLs are
/// compared after trimming a trailing `/` or `.git` only; they are never echoed,
/// because a remote URL can carry credentials.
fn verify_repository_remote(repository: &Path, workspace_remote: &str) -> Result<(), GraphError> {
    let repo = Repository::discover(repository)
        .map_err(|error| GraphError::git("open routed Git repository", error))?;
    let origin = repo.find_remote("origin").map_err(|error| {
        GraphError::invalid_input(
            "validate Orbit workspace repository",
            "repository",
            format!(
                "requested repository has no readable origin remote: {}",
                error.message()
            ),
        )
    })?;
    let matches = origin
        .url()
        .is_ok_and(|url| normalized_remote(url) == normalized_remote(workspace_remote));
    if !matches {
        return Err(GraphError::invalid_input(
            "validate Orbit workspace repository",
            "repository",
            "requested repository origin does not match the workspace's registered git_remote",
        ));
    }
    Ok(())
}

fn normalized_remote(url: &str) -> &str {
    let url = url.trim().trim_end_matches('/');
    url.strip_suffix(".git").unwrap_or(url)
}

/// Whether the run's checkout and the routed repository share Git object
/// authority: `Some(reason)` when they do not.
fn verify_same_repository(left: &Path, right: &Path) -> Result<Option<String>, GraphError> {
    let left = Repository::discover(left)
        .map_err(|error| GraphError::git("open routed Git repository", error))?;
    let right = Repository::discover(right)
        .map_err(|error| GraphError::git("open authority Git repository", error))?;
    let left_common = left.commondir().canonicalize().map_err(|source| {
        GraphError::io(
            "canonicalize routed Git common directory",
            left.commondir(),
            source,
        )
    })?;
    let right_common = right.commondir().canonicalize().map_err(|source| {
        GraphError::io(
            "canonicalize authority Git common directory",
            right.commondir(),
            source,
        )
    })?;
    if left_common != right_common {
        return Ok(Some(
            "configured workspace checkout and requested repository do not share Git object authority"
                .to_string(),
        ));
    }
    Ok(None)
}

/// Whether the run was prepared in a checkout of the routed repository:
/// `Some(reason)` when it was not, or does not say.
fn verify_run_workspace(run: &Value, repository: &Path) -> Result<Option<String>, GraphError> {
    let Some(path) = run
        .pointer("/pipeline_state/step_outputs/0/workspace_path")
        .and_then(Value::as_str)
    else {
        return Ok(Some(
            "run omitted public prepare workspace_path".to_string(),
        ));
    };
    verify_same_repository(Path::new(path), repository)
}

fn read_bounded(reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(ORBIT_OUTPUT_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Join a pipe reader, waiting no later than `deadline`: a descendant that
/// escaped the process group can hold the pipe open indefinitely.
fn join_capture(
    reader: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    stream: &str,
    repository: &Path,
    deadline: Instant,
    timeout: Duration,
) -> Result<Vec<u8>, GraphError> {
    let joined = join_by(reader, deadline).ok_or_else(|| {
        GraphError::timeout(
            "invoke public Orbit CLI",
            timeout,
            format!("its {stream} stayed open after it exited"),
        )
    })?;
    joined
        .map_err(|_| {
            GraphError::invalid_data(
                "invoke public Orbit CLI",
                format!("{stream} reader thread panicked"),
            )
        })?
        .map_err(|source| match stream {
            "stdout" => GraphError::io("read public Orbit CLI stdout", repository, source),
            _ => GraphError::io("read public Orbit CLI stderr", repository, source),
        })
}

/// Join `handle` if it finishes by `deadline`; `None` leaves it detached.
fn join_by<T>(handle: thread::JoinHandle<T>, deadline: Instant) -> Option<thread::Result<T>> {
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(POLL_INTERVAL);
    }
    Some(handle.join())
}

/// An `orbit` child that leads its own process group (STD-03 §R11).
///
/// The leader is reaped only by [`GroupChild::reap`] or
/// [`GroupChild::terminate`], after the last signal to its group: until then
/// it is at worst a zombie whose PID, and so its group ID, the kernel cannot
/// hand to another process (STD-03 §R14). Exit is observed without reaping.
struct GroupChild {
    child: Child,
    /// Set once the leader has been reaped; nothing is signalled after.
    reaped: Option<ExitStatus>,
}

impl GroupChild {
    fn spawn(
        mut command: Command,
        operation: &'static str,
        repository: &Path,
    ) -> Result<Self, GraphError> {
        command
            .spawn()
            .map(|child| Self {
                child,
                reaped: None,
            })
            .map_err(|source| GraphError::io(operation, repository, source))
    }

    /// Whether the leader has exited, observed without reaping it.
    #[cfg(unix)]
    fn has_exited(&mut self) -> std::io::Result<bool> {
        if self.reaped.is_some() {
            return Ok(true);
        }
        let pid = libc::id_t::from(self.child.id());
        // SAFETY: an all-zero `siginfo_t` is a valid value, and `waitid`
        // writes only into it. `WNOWAIT` leaves the child waitable.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // With `WNOHANG`, `si_pid` stays zero while the child runs.
        #[cfg(target_os = "linux")]
        // SAFETY: `waitid` filled `info` for a child-state change.
        let exited_pid = unsafe { info.si_pid() };
        #[cfg(not(target_os = "linux"))]
        let exited_pid = info.si_pid;
        Ok(exited_pid != 0)
    }

    #[cfg(not(unix))]
    fn has_exited(&mut self) -> std::io::Result<bool> {
        if self.reaped.is_some() {
            return Ok(true);
        }
        let status = self.child.try_wait()?;
        self.reaped = status;
        Ok(status.is_some())
    }

    /// Send `signal` to the whole group, while its leader is unreaped.
    #[cfg(unix)]
    fn signal_group(&self, signal: libc::c_int) {
        if self.reaped.is_some() {
            return;
        }
        let Ok(group) = libc::pid_t::try_from(self.child.id()) else {
            return;
        };
        // SAFETY: `killpg` only sends a signal, to the group this child
        // created at spawn and still leads: it has not been reaped, so its
        // PID is not reused. ESRCH (nothing left to signal) is not an error.
        unsafe {
            libc::killpg(group, signal);
        }
    }

    /// Reap an exited leader after sweeping its group: any member still
    /// running, such as a grandchild holding the leader's stdout, is
    /// SIGKILLed first (STD-03 §R13).
    fn reap(&mut self) -> std::io::Result<ExitStatus> {
        if let Some(status) = self.reaped {
            return Ok(status);
        }
        #[cfg(unix)]
        self.signal_group(libc::SIGKILL);
        let status = self.child.wait()?;
        self.reaped = Some(status);
        Ok(status)
    }

    /// Stop the group: SIGTERM, up to [`TERMINATION_GRACE`] for the leader
    /// to exit, then SIGKILL to the whole group whether or not anything
    /// answered, and reap (STD-03 §R12, §R13).
    fn terminate(&mut self) {
        #[cfg(unix)]
        {
            self.signal_group(libc::SIGTERM);
            let grace = Instant::now() + TERMINATION_GRACE;
            while Instant::now() < grace && matches!(self.has_exited(), Ok(false)) {
                thread::sleep(POLL_INTERVAL);
            }
        }
        #[cfg(not(unix))]
        let _ = self.child.kill();
        let _ = self.reap();
    }
}

/// How a child ended: `exit N`, or `signal N` for one killed by a signal
/// (STD-02 §R29).
fn describe_status(status: ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("exit {code}");
    }
    #[cfg(unix)]
    if let Some(signal) = status.signal() {
        return format!("signal {signal}");
    }
    "an unknown exit status".to_string()
}

/// Orbit's structured refusal on a failed `orbit tool run`: a JSON object
/// on stderr with a string `code` and an `error` (or `message`). Diagnostic
/// lines may precede the object, which must start on its own line and consume
/// the remaining stderr. Embedded JSON, multiple documents and trailing text
/// are not refusal framing; a malformed first object is never skipped.
fn orbit_refusal(tool: &str, stderr: &[u8]) -> Option<GraphError> {
    let mut document = stderr.trim_ascii();
    while !document.starts_with(b"{") {
        let newline = document.iter().position(|byte| *byte == b'\n')?;
        document = document[newline + 1..].trim_ascii_start();
    }
    let refusal: Value = serde_json::from_slice(document).ok()?;
    let code = string_field(&refusal, "code")?;
    let message = string_field(&refusal, "error")
        .or_else(|| string_field(&refusal, "message"))
        .unwrap_or("unspecified");
    Some(GraphError::orbit_refused(tool, code, message))
}

fn task_snapshot_from_value(task: &Value, workspace: &str) -> Result<TaskAssociation, GraphError> {
    let task_id = required_string(task, "id")?;
    let captured_at = current_observation_cutoff()?;
    let started = task
        .get("history")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|entry| {
            string_field(entry, "event") == Some("started")
                || matches!(
                    string_field(entry, "to_status"),
                    Some("in_progress" | "in-progress")
                )
        });
    let pending = matches!(
        string_field(task, "status"),
        Some("proposed" | "backlog" | "someday")
    );
    let availability = if !started && pending {
        TaskTextAvailability::KnownPreExecution
    } else if started {
        TaskTextAvailability::PostExecution
    } else {
        TaskTextAvailability::Uncertain
    };
    let created_at = required_string(task, "created_at")?;
    Ok(TaskAssociation {
        task_id: task_id.to_string(),
        title: required_string(task, "title")?.to_string(),
        description: string_field(task, "description")
            .unwrap_or_default()
            .to_string(),
        acceptance_criteria: task
            .get("acceptance_criteria")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        source: Provenance {
            system: "orbit.task.show".to_string(),
            record_id: Some(format!("{workspace}:{task_id}@{captured_at}")),
        },
        created_at: TemporalFact {
            status: TemporalStatus::Known,
            timestamp: Some(created_at.to_string()),
            source: Provenance {
                system: "orbit.task.show.created_at".to_string(),
                record_id: Some(format!("{workspace}:{task_id}")),
            },
        },
        snapshot_available_at: TemporalFact {
            status: TemporalStatus::Known,
            timestamp: Some(captured_at.clone()),
            source: Provenance {
                system: "orbit.task.show.observation".to_string(),
                record_id: Some(format!("{workspace}:{task_id}@{captured_at}")),
            },
        },
        text_availability: availability,
        captured_at,
    })
}

/// Whether `before..after` is a delivery on `branch`: `Some(reason)` when
/// the revisions are not an ancestry the branch reaches.
fn verify_git_delivery(
    repository: &Path,
    branch: &str,
    before: &str,
    after: &str,
) -> Result<Option<String>, GraphError> {
    let repo = Repository::open(repository)
        .map_err(|error| GraphError::git("open routed delivery repository", error))?;
    let before = Oid::from_str(before)
        .map_err(|error| GraphError::git("parse Orbit base revision", error))?;
    let after = Oid::from_str(after)
        .map_err(|error| GraphError::git("parse Orbit commit revision", error))?;
    repo.find_commit(before)
        .map_err(|error| GraphError::git("verify Orbit base commit", error))?;
    repo.find_commit(after)
        .map_err(|error| GraphError::git("verify Orbit delivery commit", error))?;
    if !repo
        .graph_descendant_of(after, before)
        .map_err(|error| GraphError::git("verify Orbit commit ancestry", error))?
    {
        return Ok(Some(
            "commit_sha is not a strict descendant of base_sha".to_string(),
        ));
    }
    let branch = branch.trim_start_matches("refs/heads/");
    let tip = repo
        .revparse_single(format!("refs/heads/{branch}").as_str())
        .and_then(|object| object.peel_to_commit())
        .map(|commit| commit.id())
        .map_err(|error| GraphError::git("resolve routed landing branch", error))?;
    if after != tip
        && !repo
            .graph_descendant_of(tip, after)
            .map_err(|error| GraphError::git("verify delivery reachability", error))?
    {
        return Ok(Some(
            "commit_sha is not reachable from the configured landing branch".to_string(),
        ));
    }
    Ok(None)
}

pub(super) fn canonical_repository(path: &Path) -> Result<PathBuf, GraphError> {
    let canonical = path
        .canonicalize()
        .map_err(|source| GraphError::io("canonicalize routed repository", path, source))?;
    Repository::open(canonical.as_path())
        .map_err(|error| GraphError::git("open explicitly routed repository", error))?;
    Ok(canonical)
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, GraphError> {
    string_field(value, field).ok_or_else(|| {
        GraphError::invalid_data(
            "decode public Orbit response",
            format!("missing string field {field:?}"),
        )
    })
}

/// The first [`STDERR_EXCERPT_LIMIT`] bytes of `bytes`, marked when cut.
fn bounded_text(bytes: &[u8]) -> String {
    let Some(cut) = bytes
        .len()
        .checked_sub(STDERR_EXCERPT_LIMIT)
        .filter(|cut| *cut > 0)
    else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    format!(
        "{}…[truncated {cut} bytes]",
        String::from_utf8_lossy(&bytes[..STDERR_EXCERPT_LIMIT])
    )
}

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
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

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
/// Orbit's public task-scoped delivery read.
pub(super) const DELIVERY_TOOL: &str = "orbit.workflow.run.delivery";
/// The `orbit.workflow.run.delivery` wire version this adapter reads.
const RUN_DELIVERY_SCHEMA_VERSION: u64 = 1;

pub(super) struct OrbitAdapter<'a> {
    repository: &'a Path,
    workspace: Option<&'a str>,
    /// The bound on each Orbit call, resolved once from the environment.
    timeout: Duration,
    /// `TMPDIR` for each nested `orbit` call, when the caller's is unusable.
    callback_tmpdir: Option<&'a Path>,
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
    /// How the host landed the delivery and which Git check verified it.
    pub(super) landing: Value,
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
            callback_tmpdir: None,
        }
    }

    /// Give each nested `orbit` call `dir` as its `TMPDIR`. Orbit's plugin
    /// sandbox lets the backend write only its own state, not the inherited
    /// `TMPDIR` or `/tmp`. Without this, Orbit 0.25.1's delivery read cannot
    /// run Git for the run's repository identity and answers `null`.
    pub(super) fn with_callback_tmpdir(mut self, dir: Option<&'a Path>) -> Self {
        self.callback_tmpdir = dir;
        self
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

    /// Judge one requested task/run pair through Orbit's public task-scoped
    /// delivery read, `orbit.workflow.run.delivery`, and local Git.
    ///
    /// The task read binds the pair to its public workspace owner first; the
    /// delivery read is routed to that same workspace and must answer for
    /// exactly the requested task and run. Task ownership comes only from the
    /// request and Orbit's answer, never from a run's other step outputs.
    pub(super) fn delivery_for_task_run(
        &self,
        task_id: &str,
        run_id: &str,
        branch: &str,
        supplied_snapshots: &BTreeMap<String, TaskAssociation>,
        repository_identity: &str,
    ) -> Result<RunVerdict, GraphError> {
        let (current_task, route) = self.task_observation(task_id)?;
        if required_string(&current_task, "id")? != task_id {
            return Ok(RunVerdict::Excluded(
                "selected workspace returned a different task ID".to_string(),
            ));
        }
        let value = self.tool_run(
            DELIVERY_TOOL,
            json!({
                "run_id": run_id,
                "task_id": task_id,
                "workspace": route.workspace_id,
                "model": "codex",
            }),
        )?;
        let observation = decode_run_delivery(value, task_id, run_id)?;
        if observation.workspace_id != route.workspace_id {
            return Ok(RunVerdict::Excluded(format!(
                "the delivery read answered for workspace {:?}, not the selected {:?}",
                observation.workspace_id, route.workspace_id
            )));
        }
        if let Some(reason) =
            verify_observed_repository(self.repository, observation.repository.as_deref())?
        {
            return Ok(RunVerdict::Excluded(reason));
        }
        let landed = match landed_evidence(&observation) {
            Ok(landed) => landed,
            Err(reason) => return Ok(RunVerdict::Excluded(reason)),
        };
        let range = match verify_landed_range(self.repository, branch, &landed)? {
            Ok(range) => range,
            Err(reason) => return Ok(RunVerdict::Excluded(reason)),
        };
        let task = task_snapshot_from_value(&current_task, route.workspace_id.as_str())?;
        let supplied_snapshot = supplied_snapshots.get(task_id).cloned();
        // The landing step's own finish time is the closest public proxy for
        // the landing instant; neither it nor the run's finish time attests
        // the exact merge moment, so the fact stays `uncertain`.
        let delivered_at = match (
            observation.landing.observed_at.clone(),
            observation.run_finished_at.clone(),
        ) {
            (Some(timestamp), _) => (
                TemporalStatus::Uncertain,
                Some(timestamp),
                "orbit.workflow.run.delivery.landing.observed_at",
            ),
            (None, Some(timestamp)) => (
                TemporalStatus::Uncertain,
                Some(timestamp),
                "orbit.workflow.run.delivery.run_finished_at",
            ),
            (None, None) => (
                TemporalStatus::Unavailable,
                None,
                "orbit.workflow.run.delivery",
            ),
        };
        Ok(RunVerdict::Deliverable(Box::new(VerifiedDelivery {
            delivery: DeliveryImport {
                schema_version: orbit_graph::DELIVERY_IMPORT_SCHEMA_VERSION,
                repository: repository_identity.to_string(),
                landing_branch: branch.to_string(),
                before_revision: range.before.to_string(),
                after_revision: range.after.to_string(),
                delivery_id: format!("orbit-run:{run_id}:{task_id}"),
                evidence: DeliveryEvidence::VerifiedDelivery,
                source: Provenance {
                    system: format!("{DELIVERY_TOOL}+git"),
                    record_id: Some(run_id.to_string()),
                },
                delivered_at: TemporalFact {
                    status: delivered_at.0,
                    timestamp: delivered_at.1,
                    source: Provenance {
                        system: delivered_at.2.to_string(),
                        record_id: Some(run_id.to_string()),
                    },
                },
                captured_at: current_observation_cutoff()?,
                tasks: vec![task],
            },
            supplied_snapshot,
            landing: json!({
                "method": landed.method.as_str(),
                "verified_by": range.verified_by,
            }),
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
        if let Some(dir) = self.callback_tmpdir {
            command.env("TMPDIR", dir);
        }
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

/// The `orbit.workflow.run.delivery` schema version 1 answer, decoded from
/// its published JSON shape (Orbit's `RunDeliveryObservation`) without linking
/// any Orbit crate. Only the fields this adapter judges are read; every enum is
/// closed, so a value this adapter does not know is malformed, never guessed.
#[derive(Debug, Deserialize)]
struct RunDeliveryObservation {
    workspace_id: String,
    repository: Option<String>,
    task_id: String,
    run_id: String,
    run_state: String,
    run_finished_at: Option<String>,
    delivery_status: DeliveryStatus,
    commit: CommitObservation,
    landing: LandingObservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DeliveryStatus {
    Landed,
    Committed,
    NoChange,
    InProgress,
    NotDelivered,
    Unavailable,
}

#[derive(Debug, Deserialize)]
struct CommitObservation {
    status: CommitStatus,
    base_sha: Option<String>,
    head_sha: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CommitStatus {
    Committed,
    AlreadyCommitted,
    VerifiedNoDiff,
    VerifiedAlreadyLanded,
    SkippedNoDiffExpected,
    Pending,
    NotReached,
    Unavailable,
}

#[derive(Debug, Deserialize)]
struct LandingObservation {
    status: LandingStatus,
    method: Option<LandingMethod>,
    landed_commit: Option<String>,
    observed_at: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LandingStatus {
    Merged,
    NotRequested,
    NotApplicable,
    Pending,
    NotReached,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LandingMethod {
    PullRequest,
    LocalFastForward,
}

impl LandingMethod {
    const fn as_str(self) -> &'static str {
        match self {
            Self::PullRequest => "pull_request",
            Self::LocalFastForward => "local_fast_forward",
        }
    }
}

/// A wire enum's own spelling, for reasons that quote Orbit's answer.
fn wire_name(value: impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Decode a delivery answer and check that it is for exactly the requested
/// task and run. An unsupported version, a malformed shape or an answer for
/// another task or run is invalid data: the pair could not be examined.
fn decode_run_delivery(
    value: Value,
    task_id: &str,
    run_id: &str,
) -> Result<RunDeliveryObservation, GraphError> {
    const OPERATION: &str = "decode orbit.workflow.run.delivery";
    match value.get("schema_version").and_then(Value::as_u64) {
        Some(RUN_DELIVERY_SCHEMA_VERSION) => {}
        version => {
            return Err(GraphError::invalid_data(
                OPERATION,
                format!(
                    "unsupported schema_version {}; this plugin reads version {RUN_DELIVERY_SCHEMA_VERSION}",
                    version.map_or_else(|| "(missing)".to_string(), |version| version.to_string())
                ),
            ));
        }
    }
    let observation: RunDeliveryObservation = serde_json::from_value(value)
        .map_err(|error| GraphError::invalid_data(OPERATION, error.to_string()))?;
    if observation.task_id != task_id || observation.run_id != run_id {
        return Err(GraphError::invalid_data(
            OPERATION,
            "the delivery read answered for a different task or run than requested",
        ));
    }
    for (field, sha) in [
        ("commit.base_sha", observation.commit.base_sha.as_deref()),
        ("commit.head_sha", observation.commit.head_sha.as_deref()),
        (
            "landing.landed_commit",
            observation.landing.landed_commit.as_deref(),
        ),
    ] {
        if sha.is_some_and(|sha| !is_full_sha(sha)) {
            return Err(GraphError::invalid_data(
                OPERATION,
                format!("{field} is not a full lowercase commit ID"),
            ));
        }
    }
    Ok(observation)
}

/// A full SHA-1 or SHA-256 object ID in lowercase hex, as Orbit records them.
fn is_full_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Whether the host's repository identity names the requested repository:
/// `Some(reason)` when it does not, or is unknown.
///
/// The expected identity is recomputed from the routed repository exactly as
/// Orbit derives it: from the configured `remote.origin.url` (as
/// `git config --get` prints it, trimmed, without `insteadOf` rewriting), a
/// GitHub origin is `owner/name` and any other origin is `git:` and the
/// lowercase hex SHA-256 of that URL. Both are compared exactly. Without an
/// origin Orbit names the repository by a digest of its Git directory path;
/// that path-dependent identity is not reproduced, so it is refused.
fn verify_observed_repository(
    repository: &Path,
    observed: Option<&str>,
) -> Result<Option<String>, GraphError> {
    let Some(observed) = observed else {
        return Ok(Some(
            "the host could not identify the run's repository".to_string(),
        ));
    };
    let repo = Repository::discover(repository)
        .map_err(|error| GraphError::git("open routed Git repository", error))?;
    let config = repo
        .config()
        .map_err(|error| GraphError::git("read routed repository configuration", error))?;
    let url = match config.get_string("remote.origin.url") {
        Ok(url) => url.trim().to_string(),
        Err(error) if error.code() == git2::ErrorCode::NotFound => String::new(),
        Err(error) => return Err(GraphError::git("read routed repository origin", error)),
    };
    let Some(expected) = repository_identity(url.as_str()) else {
        return Ok(Some(
            "the requested repository has no origin URL, and its Git-directory identity is not verified"
                .to_string(),
        ));
    };
    Ok((observed != expected).then(|| {
        "the run's repository identity does not match the requested repository's origin".to_string()
    }))
}

/// Orbit's identity for a repository whose origin is `url`, or `None` when
/// there is no origin URL.
fn repository_identity(url: &str) -> Option<String> {
    if url.is_empty() {
        return None;
    }
    let normalized = url.trim_end_matches(".git");
    Some(
        match normalized
            .strip_prefix("https://github.com/")
            .or_else(|| normalized.strip_prefix("git@github.com:"))
        {
            Some(github) => github.to_string(),
            None => format!("git:{:x}", Sha256::digest(url.as_bytes())),
        },
    )
}

/// What a `landed` answer attests, once checked for consistency.
struct LandedEvidence {
    base: String,
    head: Option<String>,
    landed_commit: Option<String>,
    method: LandingMethod,
}

/// The landed evidence in an answer, or why the pair is not an eligible
/// delivery. Only `landed` with a merged landing of a host commit qualifies;
/// a commit alone never does, however reachable it later became.
fn landed_evidence(observation: &RunDeliveryObservation) -> Result<LandedEvidence, String> {
    let RunDeliveryObservation {
        task_id,
        run_id,
        run_state,
        commit,
        landing,
        ..
    } = observation;
    match observation.delivery_status {
        DeliveryStatus::Landed => {}
        DeliveryStatus::Committed => {
            return Err(format!(
                "run {run_id} committed task {task_id}, but the host recorded no verified landing (landing.status={})",
                wire_name(landing.status)
            ));
        }
        DeliveryStatus::NoChange => {
            return Err(format!(
                "the host verified that run {run_id} needed no new commit for task {task_id} (commit.status={}); there is no delivered change",
                wire_name(commit.status)
            ));
        }
        DeliveryStatus::InProgress => {
            return Err(format!(
                "run {run_id} has not reached a terminal outcome (run_state={run_state})"
            ));
        }
        DeliveryStatus::NotDelivered => {
            return Err(format!(
                "run {run_id} ended (run_state={run_state}) without committing anything for task {task_id} (commit.status={})",
                wire_name(commit.status)
            ));
        }
        DeliveryStatus::Unavailable => {
            return Err(format!(
                "the host's delivery evidence for run {run_id} is unavailable (commit: {}, landing: {})",
                commit
                    .reason
                    .as_deref()
                    .unwrap_or(wire_name(commit.status).as_str()),
                landing
                    .reason
                    .as_deref()
                    .unwrap_or(wire_name(landing.status).as_str()),
            ));
        }
    }
    if landing.status != LandingStatus::Merged
        || !matches!(
            commit.status,
            CommitStatus::Committed | CommitStatus::AlreadyCommitted
        )
    {
        return Err(format!(
            "run {run_id} reported landed with inconsistent evidence (commit.status={}, landing.status={})",
            wire_name(commit.status),
            wire_name(landing.status)
        ));
    }
    let base = commit
        .base_sha
        .clone()
        .ok_or_else(|| format!("the host recorded no base commit for run {run_id}"))?;
    let method = landing
        .method
        .ok_or_else(|| format!("the host recorded no landing method for run {run_id}"))?;
    Ok(LandedEvidence {
        base,
        head: commit.head_sha.clone(),
        landed_commit: landing.landed_commit.clone(),
        method,
    })
}

/// The Git range a landed delivery is imported as, and the check that
/// verified it.
struct LandedRange {
    before: Oid,
    after: Oid,
    verified_by: &'static str,
}

/// Verify a landed delivery against the routed repository: `Ok(Err(reason))`
/// when Git does not support it.
///
/// The committed head must strictly descend from the base. When the landing
/// branch reaches the head (a merge commit, fast-forward or local landing),
/// the delivery is `base..head`. Otherwise only a pull-request landing that
/// recorded its `landed_commit` can stand in: that commit must be on the
/// branch, have one parent that builds on the base, and make exactly the tree
/// changes `base..head` makes (a squash or single rebased commit): the same
/// paths, each with the same object ID and file mode before and after. It is
/// then imported as `parent..landed_commit`. A squash onto a parent that
/// changed one of those paths since the base cannot be proved equivalent, so
/// it is excluded. A local landing records no SHA, so it is never inferred.
fn verify_landed_range(
    repository: &Path,
    branch: &str,
    landed: &LandedEvidence,
) -> Result<Result<LandedRange, String>, GraphError> {
    let repo = Repository::open(repository)
        .map_err(|error| GraphError::git("open routed delivery repository", error))?;
    let commit = |sha: &str, operation: &'static str| {
        Oid::from_str(sha)
            .and_then(|oid| repo.find_commit(oid).map(|commit| commit.id()))
            .map_err(|error| GraphError::git(operation, error))
    };
    let descends = |descendant: Oid, ancestor: Oid| {
        repo.graph_descendant_of(descendant, ancestor)
            .map_err(|error| GraphError::git("verify Orbit commit ancestry", error))
    };
    let base = commit(landed.base.as_str(), "verify Orbit base commit")?;
    let tip = repo
        .revparse_single(
            format!("refs/heads/{}", branch.trim_start_matches("refs/heads/")).as_str(),
        )
        .and_then(|object| object.peel_to_commit())
        .map(|commit| commit.id())
        .map_err(|error| GraphError::git("resolve routed landing branch", error))?;
    let reaches = |oid: Oid| -> Result<bool, GraphError> { Ok(oid == tip || descends(tip, oid)?) };
    let Some(head) = landed.head.as_deref() else {
        return Ok(Err(
            "the host recorded no head commit (commit.status=already_committed), so the delivered range cannot be verified"
                .to_string(),
        ));
    };
    let head = commit(head, "verify Orbit delivery commit")?;
    if !descends(head, base)? {
        return Ok(Err(
            "head_sha is not a strict descendant of base_sha".to_string()
        ));
    }
    if reaches(head)? {
        return Ok(Ok(LandedRange {
            before: base,
            after: head,
            verified_by: "head_reachable",
        }));
    }
    let landed_commit = match (landed.method, landed.landed_commit.as_deref()) {
        (LandingMethod::LocalFastForward, _) => {
            return Ok(Err(
                "the host recorded a local landing, but head_sha is not reachable from the landing branch"
                    .to_string(),
            ));
        }
        (LandingMethod::PullRequest, None) => {
            return Ok(Err(
                "head_sha is not reachable from the landing branch and the host recorded no landed_commit"
                    .to_string(),
            ));
        }
        (LandingMethod::PullRequest, Some(landed_commit)) => {
            commit(landed_commit, "verify Orbit landed commit")?
        }
    };
    if !reaches(landed_commit)? {
        return Ok(Err(
            "landed_commit is not reachable from the landing branch".to_string(),
        ));
    }
    let landed_object = repo
        .find_commit(landed_commit)
        .map_err(|error| GraphError::git("verify Orbit landed commit", error))?;
    if landed_object.parent_count() != 1 {
        return Ok(Err(
            "landed_commit is a merge that does not reach head_sha".to_string()
        ));
    }
    let parent = landed_object
        .parent_id(0)
        .map_err(|error| GraphError::git("read Orbit landed commit parent", error))?;
    if parent != base && !descends(parent, base)? {
        return Ok(Err("landed_commit does not build on base_sha".to_string()));
    }
    let delivered = tree_changes(&repo, base, head)?;
    let squashed = tree_changes(&repo, parent, landed_commit)?;
    let paths = |changes: &[TreeChange]| {
        changes
            .iter()
            .map(|change| change.path.clone())
            .collect::<std::collections::BTreeSet<_>>()
    };
    if paths(&delivered) != paths(&squashed) {
        return Ok(Err(
            "landed_commit changes different paths than base_sha..head_sha, so it is not verified as this delivery's squash"
                .to_string(),
        ));
    }
    let after = |changes: &[TreeChange]| {
        changes
            .iter()
            .map(|change| (change.path.clone(), change.new))
            .collect::<Vec<_>>()
    };
    if after(&delivered) != after(&squashed) {
        return Ok(Err(
            "landed_commit leaves different contents or modes than head_sha at the delivered paths, so it is not verified as this delivery's squash"
                .to_string(),
        ));
    }
    if delivered != squashed {
        return Ok(Err(
            "landed_commit's parent differs from base_sha at the delivered paths, so its equivalence to base_sha..head_sha cannot be verified"
                .to_string(),
        ));
    }
    Ok(Ok(LandedRange {
        before: parent,
        after: landed_commit,
        verified_by: "landed_commit_tree_entries",
    }))
}

/// One entry a tree diff changes: its path and its (object ID, file mode)
/// before and after. An absent side has the zero ID and an unreadable mode.
#[derive(Debug, PartialEq, Eq)]
struct TreeChange {
    path: PathBuf,
    old: (Oid, u32),
    new: (Oid, u32),
}

/// Every entry a tree diff from `old` to `new` changes, in path order.
/// Renames are not detected, so a rename is a deletion and an addition, and
/// each change has one path.
fn tree_changes(repo: &Repository, old: Oid, new: Oid) -> Result<Vec<TreeChange>, GraphError> {
    let tree = |oid: Oid| {
        repo.find_commit(oid)
            .and_then(|commit| commit.tree())
            .map_err(|error| GraphError::git("read delivery tree", error))
    };
    let diff = repo
        .diff_tree_to_tree(Some(&tree(old)?), Some(&tree(new)?), None)
        .map_err(|error| GraphError::git("diff delivery trees", error))?;
    diff.deltas()
        .map(|delta| {
            let (old_file, new_file) = (delta.old_file(), delta.new_file());
            let path = new_file.path().or_else(|| old_file.path()).ok_or_else(|| {
                GraphError::invalid_data("diff delivery trees", "a changed entry has no path")
            })?;
            Ok(TreeChange {
                path: path.to_path_buf(),
                old: (old_file.id(), u32::from(old_file.mode())),
                new: (new_file.id(), u32::from(new_file.mode())),
            })
        })
        .collect()
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

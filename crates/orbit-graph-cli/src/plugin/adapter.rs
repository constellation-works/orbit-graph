use std::collections::BTreeMap;
use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use git2::{Oid, Repository};
use serde_json::{Value, json};

use orbit_graph::current_observation_cutoff;
use orbit_graph::{
    DeliveryEvidence, DeliveryImport, GraphError, HybridTaskHit, Provenance, TaskAssociation,
    TaskTextAvailability, TemporalFact, TemporalStatus,
};

use super::{json_error, string_field};

const DEFAULT_ORBIT_TIMEOUT_SECONDS: u64 = 10;
const MAX_ORBIT_TIMEOUT_SECONDS: u64 = 60;
const ORBIT_OUTPUT_LIMIT: u64 = 1_048_576;
/// MCP protocol revision announced to `orbit mcp serve`.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const MCP_INITIALIZE_ID: u64 = 1;
const MCP_CALL_ID: u64 = 2;

pub(super) struct OrbitAdapter<'a> {
    repository: &'a Path,
    workspace: Option<&'a str>,
}

impl<'a> OrbitAdapter<'a> {
    pub(super) fn new(repository: &'a Path, workspace: Option<&'a str>) -> Self {
        Self {
            repository,
            workspace,
        }
    }

    fn require_workspace(&self) -> Result<&str, GraphError> {
        self.workspace
            .filter(|workspace| !workspace.trim().is_empty())
            .ok_or_else(|| {
                GraphError::invalid_data(
                    "route authoritative Orbit request",
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
                    || string_field(item, "name") == Some(workspace)
            })
            .ok_or_else(|| {
                GraphError::invalid_data(
                    "validate Orbit workspace authority",
                    format!("workspace {workspace:?} is not registered in the explicit authority"),
                )
            })?;
        let actual_id = required_string(selected, "id")?;
        if string_field(selected, "status").is_some_and(|status| status != "active") {
            return Err(GraphError::invalid_data(
                "validate Orbit workspace authority",
                format!("workspace {actual_id:?} is not active"),
            ));
        }
        let remote = string_field(selected, "git_remote").ok_or_else(|| {
            GraphError::invalid_data(
                "validate Orbit workspace repository",
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
        let route = self.require_authority()?;
        self.tool_run(
            "orbit.task.show",
            json!({
                "id": task_id,
                "workspace": route.workspace_id,
                "fields": ["id", "title", "description", "acceptance_criteria", "status", "created_at", "history", "job_run_id"],
                "model": "codex",
            }),
        )
    }

    pub(super) fn task_snapshot(&self, task_id: &str) -> Result<TaskAssociation, GraphError> {
        task_snapshot_from_value(&self.task_show(task_id)?, self.require_workspace()?)
    }

    pub(super) fn hybrid_search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<HybridTaskHit>, GraphError> {
        let workspace = self.require_workspace()?;
        let value = self.tool_run(
            "orbit.search",
            json!({
                "workspace": workspace,
                "query": query,
                "kind": "task",
                "hybrid": true,
                "limit": limit,
                "model": "codex",
            }),
        )?;
        Ok(value
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| {
                Some(HybridTaskHit {
                    task_id: item.get("id")?.as_str()?.to_string(),
                    score: item.get("score")?.as_f64()?,
                })
            })
            .collect())
    }

    pub(super) fn delivery_from_run(
        &self,
        run_id: &str,
        branch: &str,
        supplied_snapshots: &BTreeMap<String, TaskAssociation>,
        repository_identity: &str,
    ) -> Result<DeliveryImport, GraphError> {
        let route = self.require_authority()?;
        let run = self.tool_run("orbit.workflow.run.show", json!({"id": run_id}))?;
        let state = run
            .pointer("/run/state")
            .or_else(|| run.get("state"))
            .and_then(Value::as_str);
        if state != Some("success") {
            return Err(GraphError::invalid_data(
                "verify Orbit delivery run",
                format!("run {run_id} is not successful"),
            ));
        }
        let commit = run
            .pointer("/pipeline_state/step_outputs/2")
            .or_else(|| run.pointer("/pipeline_state/pipeline/commit"))
            .ok_or_else(|| {
                GraphError::invalid_data(
                    "verify Orbit delivery run",
                    format!("run {run_id} has no public commit step output"),
                )
            })?;
        if commit.get("committed").and_then(Value::as_bool) != Some(true)
            || string_field(commit, "phase") != Some("commit")
        {
            return Err(GraphError::invalid_data(
                "verify Orbit delivery commit",
                format!("run {run_id} did not attest a committed output"),
            ));
        }
        let task_id = required_string(commit, "task_id")?;
        let before = required_string(commit, "base_sha")?;
        let after = required_string(commit, "commit_sha")?;
        verify_git_delivery(self.repository, branch, before, after)?;
        verify_run_workspace(&run, self.repository)?;
        let current_task = self.task_show(task_id)?;
        if required_string(&current_task, "id")? != task_id {
            return Err(GraphError::invalid_data(
                "verify Orbit run task workspace",
                "selected workspace returned a different task ID",
            ));
        }
        let task = if let Some(snapshot) = supplied_snapshots.get(task_id) {
            if snapshot.task_id != task_id {
                return Err(GraphError::invalid_data(
                    "verify supplied task snapshot",
                    "snapshot task_id does not match the verified run task",
                ));
            }
            snapshot.clone()
        } else {
            task_snapshot_from_value(&current_task, route.workspace_id.as_str())?
        };
        let finished_at = run
            .pointer("/run/finished_at")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(DeliveryImport {
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
        })
    }

    fn tool_run(&self, name: &str, input: Value) -> Result<Value, GraphError> {
        let encoded = serde_json::to_string(&input).map_err(json_error)?;
        self.orbit_json(&["tool", "run", name, "--input", encoded.as_str(), "--full"])
    }

    /// Configure an `orbit` child that stays in the caller's process group,
    /// inherits the caller's environment (including any host-issued callback
    /// credential), and dies with its parent on Linux.
    fn orbit_command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("orbit");
        command
            .current_dir(self.repository)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        unsafe {
            command.pre_exec(|| {
                #[cfg(target_os = "linux")]
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command
    }

    fn orbit_json(&self, args: &[&str]) -> Result<Value, GraphError> {
        let timeout = orbit_timeout()?;
        let mut command = self.orbit_command(args);
        let mut child = command
            .spawn()
            .map_err(|source| GraphError::io("invoke public Orbit CLI", self.repository, source))?;
        let stdout = child.stdout.take().ok_or_else(|| {
            GraphError::invalid_data("invoke public Orbit CLI", "stdout pipe was not available")
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            GraphError::invalid_data("invoke public Orbit CLI", "stderr pipe was not available")
        })?;
        let stdout_reader = thread::spawn(move || read_bounded(stdout));
        let stderr_reader = thread::spawn(move || read_bounded(stderr));
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|source| {
                GraphError::io("wait for public Orbit CLI", self.repository, source)
            })? {
                break status;
            }
            if started.elapsed() >= timeout {
                terminate_child(&mut child);
                return Err(GraphError::invalid_data(
                    "invoke public Orbit CLI",
                    format!("timed out after {} seconds", timeout.as_secs()),
                ));
            }
            thread::sleep(Duration::from_millis(10));
        };
        let stdout = join_capture(stdout_reader, "stdout", self.repository)?;
        let stderr = join_capture(stderr_reader, "stderr", self.repository)?;
        if stdout.len() as u64 + stderr.len() as u64 > ORBIT_OUTPUT_LIMIT {
            return Err(GraphError::invalid_data(
                "invoke public Orbit CLI",
                format!("combined stdout/stderr exceeded {ORBIT_OUTPUT_LIMIT} bytes"),
            ));
        }
        if !status.success() {
            return Err(GraphError::invalid_data(
                "invoke public Orbit CLI",
                format!(
                    "exit {}: {}",
                    status.code().unwrap_or(1),
                    bounded_text(stderr.as_slice())
                ),
            ));
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
        let timeout = orbit_timeout()?;
        let deadline = Instant::now() + timeout;
        let mut command = self.orbit_command(&["mcp", "serve"]);
        command.stdin(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|source| GraphError::io("invoke Orbit MCP server", self.repository, source))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            terminate_child(&mut child);
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
    child: Child,
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
                    return Err(GraphError::invalid_data(
                        "invoke Orbit MCP server",
                        format!("timed out after {} seconds", self.timeout.as_secs()),
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
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() < self.deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                _ => break None,
            }
        };
        let Some(status) = status else {
            return "server did not exit".to_string();
        };
        let stderr = self
            .stderr_reader
            .take()
            .and_then(|reader| reader.join().ok())
            .and_then(Result::ok)
            .unwrap_or_default();
        format!(
            "exit {}: {}",
            status.code().unwrap_or(1),
            bounded_text(stderr.as_slice())
        )
    }

    /// Close stdin and reap the server; a server still running at the
    /// deadline, or after any failure, is killed rather than waited on.
    fn finish(mut self, graceful: bool) {
        self.stdin = None;
        if graceful {
            while Instant::now() < self.deadline {
                if !matches!(self.child.try_wait(), Ok(None)) {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        terminate_child(&mut self.child);
    }
}

/// Stream newline-delimited stdout, stopping after [`ORBIT_OUTPUT_LIMIT`] bytes.
fn spawn_line_reader(stdout: impl Read + Send + 'static) -> Receiver<StdoutLine> {
    let (sender, receiver) = mpsc::channel();
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
        return Err(GraphError::invalid_data(
            "invoke Orbit MCP server",
            format!(
                "{name} refused: {}: {}",
                string_field(&payload, "code").unwrap_or("error"),
                string_field(&payload, "message").unwrap_or("unspecified")
            ),
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
    let repo = Repository::discover(repository).map_err(|error| {
        GraphError::invalid_data("open routed Git repository", error.to_string())
    })?;
    let origin = repo.find_remote("origin").map_err(|error| {
        GraphError::invalid_data(
            "validate Orbit workspace repository",
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
        return Err(GraphError::invalid_data(
            "validate Orbit workspace repository",
            "requested repository origin does not match the workspace's registered git_remote",
        ));
    }
    Ok(())
}

fn normalized_remote(url: &str) -> &str {
    let url = url.trim().trim_end_matches('/');
    url.strip_suffix(".git").unwrap_or(url)
}

fn verify_same_repository(left: &Path, right: &Path) -> Result<(), GraphError> {
    let left = Repository::discover(left).map_err(|error| {
        GraphError::invalid_data("open routed Git repository", error.to_string())
    })?;
    let right = Repository::discover(right).map_err(|error| {
        GraphError::invalid_data("open authority Git repository", error.to_string())
    })?;
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
        return Err(GraphError::invalid_data(
            "validate Orbit workspace repository",
            "configured workspace checkout and requested repository do not share Git object authority",
        ));
    }
    Ok(())
}

fn verify_run_workspace(run: &Value, repository: &Path) -> Result<(), GraphError> {
    let path = run
        .pointer("/pipeline_state/step_outputs/0/workspace_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            GraphError::invalid_data(
                "verify Orbit run workspace",
                "run omitted public prepare workspace_path",
            )
        })?;
    verify_same_repository(Path::new(path), repository)
}

fn orbit_timeout() -> Result<Duration, GraphError> {
    let seconds = match env::var("GRAPH_ORBIT_TIMEOUT_SECONDS") {
        Ok(value) => value.parse::<u64>().map_err(|error| {
            GraphError::invalid_data("parse Orbit subprocess timeout", error.to_string())
        })?,
        Err(env::VarError::NotPresent) => DEFAULT_ORBIT_TIMEOUT_SECONDS,
        Err(error) => {
            return Err(GraphError::invalid_data(
                "read Orbit subprocess timeout",
                error.to_string(),
            ));
        }
    };
    if seconds == 0 || seconds > MAX_ORBIT_TIMEOUT_SECONDS {
        return Err(GraphError::invalid_data(
            "validate Orbit subprocess timeout",
            format!("timeout must be between 1 and {MAX_ORBIT_TIMEOUT_SECONDS} seconds"),
        ));
    }
    Ok(Duration::from_secs(seconds))
}

fn read_bounded(reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(ORBIT_OUTPUT_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn join_capture(
    reader: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    stream: &str,
    repository: &Path,
) -> Result<Vec<u8>, GraphError> {
    reader
        .join()
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

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
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

fn verify_git_delivery(
    repository: &Path,
    branch: &str,
    before: &str,
    after: &str,
) -> Result<(), GraphError> {
    let repo = Repository::open(repository).map_err(|error| {
        GraphError::invalid_data("open routed delivery repository", error.to_string())
    })?;
    let before = Oid::from_str(before).map_err(|error| {
        GraphError::invalid_data("parse Orbit base revision", error.to_string())
    })?;
    let after = Oid::from_str(after).map_err(|error| {
        GraphError::invalid_data("parse Orbit commit revision", error.to_string())
    })?;
    repo.find_commit(before)
        .map_err(|error| GraphError::invalid_data("verify Orbit base commit", error.to_string()))?;
    repo.find_commit(after).map_err(|error| {
        GraphError::invalid_data("verify Orbit delivery commit", error.to_string())
    })?;
    if !repo.graph_descendant_of(after, before).map_err(|error| {
        GraphError::invalid_data("verify Orbit commit ancestry", error.to_string())
    })? {
        return Err(GraphError::invalid_data(
            "verify Orbit commit ancestry",
            "commit_sha is not a strict descendant of base_sha",
        ));
    }
    let branch = branch.trim_start_matches("refs/heads/");
    let tip = repo
        .revparse_single(format!("refs/heads/{branch}").as_str())
        .and_then(|object| object.peel_to_commit())
        .map(|commit| commit.id())
        .map_err(|error| {
            GraphError::invalid_data("resolve routed landing branch", error.to_string())
        })?;
    if after != tip
        && !repo.graph_descendant_of(tip, after).map_err(|error| {
            GraphError::invalid_data("verify delivery reachability", error.to_string())
        })?
    {
        return Err(GraphError::invalid_data(
            "verify delivery reachability",
            "commit_sha is not reachable from the configured landing branch",
        ));
    }
    Ok(())
}

pub(super) fn canonical_repository(path: &Path) -> Result<PathBuf, GraphError> {
    let canonical = path
        .canonicalize()
        .map_err(|source| GraphError::io("canonicalize routed repository", path, source))?;
    Repository::open(canonical.as_path()).map_err(|error| {
        GraphError::invalid_data("open explicitly routed repository", error.to_string())
    })?;
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

fn bounded_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(4_096)]).into_owned()
}

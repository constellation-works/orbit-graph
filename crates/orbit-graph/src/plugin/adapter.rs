use std::collections::BTreeMap;
use std::env;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use git2::{Oid, Repository};
use serde_json::{Value, json};

use crate::recommend::current_observation_cutoff;
use crate::{
    DeliveryEvidence, DeliveryImport, GraphError, HybridTaskHit, Provenance, TaskAssociation,
    TaskTextAvailability, TemporalFact, TemporalStatus,
};

use super::{json_error, string_field};

const DEFAULT_ORBIT_TIMEOUT_SECONDS: u64 = 10;
const MAX_ORBIT_TIMEOUT_SECONDS: u64 = 60;
const ORBIT_OUTPUT_LIMIT: u64 = 1_048_576;

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

    fn require_authority(&self) -> Result<AuthorityRoute, GraphError> {
        let workspace = self.require_workspace()?;
        let value = self.tool_run("orbit.workspace.list", json!({}))?;
        let workspaces = value
            .as_array()
            .or_else(|| value.get("items").and_then(Value::as_array))
            .or_else(|| value.get("workspaces").and_then(Value::as_array));
        let selected = workspaces
            .into_iter()
            .flatten()
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
        let checkout = selected
            .get("repo_root")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                GraphError::invalid_data(
                    "validate Orbit workspace repository",
                    "workspace list omitted repo_root",
                )
            })?;
        let checkout = PathBuf::from(checkout).canonicalize().map_err(|source| {
            GraphError::io(
                "canonicalize Orbit workspace repository",
                Path::new(checkout),
                source,
            )
        })?;
        verify_same_repository(self.repository, checkout.as_path())?;
        Ok(AuthorityRoute {
            workspace_id: actual_id.to_string(),
            repository_root: checkout,
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
        verify_run_workspace(&run, &route)?;
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
            schema_version: crate::DELIVERY_IMPORT_SCHEMA_VERSION,
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

    fn orbit_json(&self, args: &[&str]) -> Result<Value, GraphError> {
        let mut command = Command::new("orbit");
        command
            .current_dir(self.repository)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let timeout = orbit_timeout()?;
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
}

struct AuthorityRoute {
    workspace_id: String,
    repository_root: PathBuf,
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

fn verify_run_workspace(run: &Value, route: &AuthorityRoute) -> Result<(), GraphError> {
    let path = run
        .pointer("/pipeline_state/step_outputs/0/workspace_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            GraphError::invalid_data(
                "verify Orbit run workspace",
                "run omitted public prepare workspace_path",
            )
        })?;
    verify_same_repository(Path::new(path), route.repository_root.as_path())
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

fn terminate_child(child: &mut std::process::Child) {
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

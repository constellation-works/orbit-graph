use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use git2::{Oid, Repository};
use serde_json::{Value, json};

use crate::recommend::current_observation_cutoff;
use crate::{
    DeliveryEvidence, DeliveryImport, GraphError, HybridTaskHit, Provenance, TaskAssociation,
    TaskTextAvailability, TemporalFact, TemporalStatus,
};

use super::{json_error, string_field};

pub(super) struct OrbitAdapter<'a> {
    repository: &'a Path,
    workspace: Option<&'a str>,
    orbit_root: Option<&'a Path>,
}

impl<'a> OrbitAdapter<'a> {
    pub(super) fn new(
        repository: &'a Path,
        workspace: Option<&'a str>,
        orbit_root: Option<&'a Path>,
    ) -> Self {
        Self {
            repository,
            workspace,
            orbit_root,
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

    pub(super) fn task_show(&self, task_id: &str) -> Result<Value, GraphError> {
        let workspace = self.require_workspace()?;
        self.tool_run(
            "orbit.task.show",
            json!({
                "id": task_id,
                "workspace": workspace,
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
        self.require_workspace()?;
        let run = self.orbit_json(&["run", "show", run_id, "--format", "json"])?;
        if run.pointer("/run/state").and_then(Value::as_str) != Some("success") {
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
        let task = if let Some(snapshot) = supplied_snapshots.get(task_id) {
            snapshot.clone()
        } else {
            task_snapshot_from_value(&self.task_show(task_id)?, self.require_workspace()?)?
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
        let program = env::var("ORBIT_GRAPH_ORBIT_BIN").unwrap_or_else(|_| "orbit".to_string());
        let mut command = Command::new(program);
        command.current_dir(self.repository).args(args);
        if let Some(root) = self.orbit_root {
            command.arg("--root").arg(root);
        }
        let output = command
            .output()
            .map_err(|source| GraphError::io("invoke public Orbit CLI", self.repository, source))?;
        if !output.status.success() {
            return Err(GraphError::invalid_data(
                "invoke public Orbit CLI",
                format!(
                    "exit {}: {}",
                    output.status.code().unwrap_or(1),
                    bounded_text(output.stderr.as_slice())
                ),
            ));
        }
        serde_json::from_slice(output.stdout.as_slice()).map_err(json_error)
    }
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

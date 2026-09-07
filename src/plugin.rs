//! Orbit external-tool protocol and public-authority adapter.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    DeliveryImport, GraphError, HistoryIndex, HybridTaskHit, RecommendationEngine,
    RecommendationInput, RecommendationLevel, RecommendationRequest, RecommendationVariant,
    TaskAssociation,
};

mod adapter;

use adapter::{OrbitAdapter, canonical_repository};

/// External tool name for recommendations and authoritative task lookup.
pub const RECOMMEND_TOOL_NAME: &str = "orbit.graph.recommend";
/// External tool name for index freshness/status.
pub const STATUS_TOOL_NAME: &str = "orbit.graph.status";
/// External tool name for bounded import and synchronization.
pub const MAINTAIN_TOOL_NAME: &str = "orbit.graph.maintain";
/// Version of every external-tool request and response envelope.
pub const PLUGIN_SCHEMA_VERSION: u32 = 1;

/// Whether `name` selects one of this package's no-argv external tools.
pub fn recognizes_tool(name: &str) -> bool {
    matches!(
        name,
        RECOMMEND_TOOL_NAME | STATUS_TOOL_NAME | MAINTAIN_TOOL_NAME
    )
}

/// Execute one no-argv Orbit external-tool request from JSON stdin bytes.
pub fn execute_external_tool(name: &str, input: &[u8]) -> Result<Value, GraphError> {
    match name {
        RECOMMEND_TOOL_NAME => recommend(serde_json::from_slice(input).map_err(json_error)?),
        STATUS_TOOL_NAME => status(serde_json::from_slice(input).map_err(json_error)?),
        MAINTAIN_TOOL_NAME => maintain(serde_json::from_slice(input).map_err(json_error)?),
        _ => Err(GraphError::invalid_data(
            "dispatch Orbit external tool",
            format!("unsupported ORBIT_TOOL_NAME {name:?}"),
        )),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecommendToolInput {
    schema_version: u32,
    repository: PathBuf,
    #[serde(default = "default_branch")]
    branch: String,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    orbit_root: Option<PathBuf>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    task_snapshot: Option<TaskAssociation>,
    #[serde(default)]
    level: Level,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    cutoff: Option<String>,
    #[serde(default)]
    hybrid: bool,
    #[serde(default = "default_hybrid_limit")]
    hybrid_limit: usize,
    #[serde(default)]
    hybrid_hits: Vec<HybridTaskHit>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Level {
    #[default]
    File,
    Symbol,
}

impl From<Level> for RecommendationLevel {
    fn from(value: Level) -> Self {
        match value {
            Level::File => Self::File,
            Level::Symbol => Self::Symbol,
        }
    }
}

#[derive(Debug, Serialize)]
struct AdapterEvidence {
    workspace: Option<String>,
    task_text: String,
    hybrid_search: String,
    warnings: Vec<String>,
}

fn recommend(input: RecommendToolInput) -> Result<Value, GraphError> {
    validate_schema(input.schema_version)?;
    let repository = canonical_repository(input.repository.as_path())?;
    let (intent, mut snapshot, mut task_text_source) = match (input.query, input.task_id) {
        (Some(query), None) if !query.trim().is_empty() => (
            RecommendationInput::Query(query),
            input.task_snapshot,
            "request_query".to_string(),
        ),
        (None, Some(task_id)) if !task_id.trim().is_empty() => (
            RecommendationInput::TaskId(task_id),
            input.task_snapshot,
            "supplied_snapshot".to_string(),
        ),
        _ => {
            return Err(GraphError::invalid_data(
                "validate plugin recommendation intent",
                "exactly one non-empty query or task_id is required",
            ));
        }
    };
    let adapter = OrbitAdapter::new(
        repository.as_path(),
        input.workspace.as_deref(),
        input.orbit_root.as_deref(),
    );
    if snapshot.is_none()
        && let RecommendationInput::TaskId(task_id) = &intent
    {
        snapshot = Some(adapter.task_snapshot(task_id)?);
        task_text_source = "orbit.task.show_public_observation".to_string();
    }
    let query_text = match (&intent, snapshot.as_ref()) {
        (RecommendationInput::Query(query), _) => query.clone(),
        (RecommendationInput::TaskId(_), Some(task)) => task_text(task),
        (RecommendationInput::TaskId(_), None) => String::new(),
    };
    let mut warnings = Vec::new();
    let mut hybrid_hits = input.hybrid_hits;
    let hybrid_source = if input.hybrid {
        if input.hybrid_limit == 0 || input.hybrid_limit > 100 {
            return Err(GraphError::invalid_data(
                "validate hybrid search bound",
                "hybrid_limit must be between 1 and 100",
            ));
        }
        match adapter.hybrid_search(query_text.as_str(), input.hybrid_limit) {
            Ok(mut hits) => {
                hits.append(&mut hybrid_hits);
                hybrid_hits = hits;
                "orbit.search_hybrid".to_string()
            }
            Err(error) => {
                warnings.push(format!(
                    "authoritative hybrid search unavailable; deterministic local lexical fallback used: {error}"
                ));
                "local_lexical_fallback".to_string()
            }
        }
    } else if hybrid_hits.is_empty() {
        "local_lexical".to_string()
    } else {
        "supplied_ranked_hits".to_string()
    };
    let engine = RecommendationEngine::open(repository.as_path(), input.branch.as_str())?;
    let result = engine.recommend(&RecommendationRequest {
        input: intent,
        level: input.level.into(),
        variant: RecommendationVariant::Combined,
        limit: input.limit,
        target_revision: input.revision,
        cutoff: input.cutoff,
        task_snapshot: snapshot,
        hybrid_hits,
    })?;
    serde_json::to_value(json!({
        "schema_version": PLUGIN_SCHEMA_VERSION,
        "operation": "recommend",
        "repository": repository,
        "adapter": AdapterEvidence {
            workspace: input.workspace,
            task_text: task_text_source,
            hybrid_search: hybrid_source,
            warnings,
        },
        "result": result,
    }))
    .map_err(json_error)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusToolInput {
    schema_version: u32,
    repository: PathBuf,
    #[serde(default = "default_branch")]
    branch: String,
}

fn status(input: StatusToolInput) -> Result<Value, GraphError> {
    validate_schema(input.schema_version)?;
    let repository = canonical_repository(input.repository.as_path())?;
    let index = HistoryIndex::open(repository.as_path(), input.branch.as_str())?;
    Ok(json!({
        "schema_version": PLUGIN_SCHEMA_VERSION,
        "operation": "status",
        "repository": repository,
        "status": index.status()?,
    }))
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MaintenanceOperation {
    HistorySync,
    Import,
    OrbitSync,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MaintainToolInput {
    schema_version: u32,
    operation: MaintenanceOperation,
    repository: PathBuf,
    #[serde(default = "default_branch")]
    branch: String,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    delivery: Option<DeliveryImport>,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    orbit_root: Option<PathBuf>,
    #[serde(default)]
    task_ids: Vec<String>,
    #[serde(default)]
    run_ids: Vec<String>,
    #[serde(default)]
    task_snapshots: Vec<TaskAssociation>,
}

fn maintain(input: MaintainToolInput) -> Result<Value, GraphError> {
    validate_schema(input.schema_version)?;
    let repository = canonical_repository(input.repository.as_path())?;
    let index = HistoryIndex::open(repository.as_path(), input.branch.as_str())?;
    match input.operation {
        MaintenanceOperation::HistorySync => {
            let limit = input.limit.unwrap_or(100);
            if limit == 0 || limit > 1_000 {
                return Err(GraphError::invalid_data(
                    "validate plugin history sync bound",
                    "limit must be between 1 and 1000",
                ));
            }
            Ok(json!({
                "schema_version": PLUGIN_SCHEMA_VERSION,
                "operation": "history_sync",
                "repository": repository,
                "coverage": {"complete": true, "kind": "bounded_first_parent_git"},
                "result": index.sync(Some(limit))?,
            }))
        }
        MaintenanceOperation::Import => {
            let delivery = input.delivery.ok_or_else(|| {
                GraphError::invalid_data(
                    "validate plugin import",
                    "delivery is required for operation=import",
                )
            })?;
            Ok(json!({
                "schema_version": PLUGIN_SCHEMA_VERSION,
                "operation": "import",
                "repository": repository,
                "result": index.import(delivery)?,
            }))
        }
        MaintenanceOperation::OrbitSync => sync_orbit(repository, index, input),
    }
}

fn sync_orbit(
    repository: PathBuf,
    index: HistoryIndex,
    input: MaintainToolInput,
) -> Result<Value, GraphError> {
    let bound = input.limit.unwrap_or(25);
    if bound == 0 || bound > 100 {
        return Err(GraphError::invalid_data(
            "validate Orbit adapter sync bound",
            "limit must be between 1 and 100",
        ));
    }
    let adapter = OrbitAdapter::new(
        repository.as_path(),
        input.workspace.as_deref(),
        input.orbit_root.as_deref(),
    );
    let explicit_run_count = input.run_ids.len();
    let task_count = input.task_ids.len();
    let mut run_ids = input.run_ids;
    let mut task_ids_examined = 0;
    let mut truncated = false;
    for task_id in &input.task_ids {
        if run_ids.len() >= bound {
            truncated = true;
            break;
        }
        task_ids_examined += 1;
        let task = adapter.task_show(task_id)?;
        if let Some(run_id) = string_field(&task, "job_run_id") {
            run_ids.push(run_id.to_string());
        }
    }
    let mut seen = BTreeSet::new();
    run_ids.retain(|id| seen.insert(id.clone()));
    truncated |= run_ids.len() > bound || task_ids_examined < task_count;
    let discovered_run_count = run_ids.len();
    run_ids.truncate(bound);
    let snapshots = input
        .task_snapshots
        .into_iter()
        .map(|snapshot| (snapshot.task_id.clone(), snapshot))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut outcomes = Vec::new();
    for run_id in &run_ids {
        match adapter.delivery_from_run(
            run_id,
            input.branch.as_str(),
            &snapshots,
            index.repository(),
        ) {
            Ok(delivery) => {
                let delivery_id = delivery.delivery_id.clone();
                let task_ids = delivery
                    .tasks
                    .iter()
                    .map(|task| task.task_id.clone())
                    .collect::<Vec<_>>();
                match index.import(delivery) {
                    Ok(report) => outcomes.push(json!({
                        "run_id": run_id,
                        "delivery_id": delivery_id,
                        "task_ids": task_ids,
                        "status": if report.inserted {"inserted"} else {"already_indexed"},
                    })),
                    Err(error) => outcomes.push(json!({
                        "run_id": run_id,
                        "status": "excluded",
                        "reason": error.to_string(),
                    })),
                }
            }
            Err(error) => outcomes.push(json!({
                "run_id": run_id,
                "status": "excluded",
                "reason": error.to_string(),
            })),
        }
    }
    Ok(json!({
        "schema_version": PLUGIN_SCHEMA_VERSION,
        "operation": "orbit_sync",
        "repository": repository,
        "authority": {
            "workspace": input.workspace,
            "orbit_root": input.orbit_root,
            "interfaces": ["orbit.task.show", "orbit run show", "git"],
        },
        "coverage": {
            "complete": false,
            "kind": "explicit_bounded_run_set",
            "explicit_run_ids": explicit_run_count,
            "task_ids": task_count,
            "task_ids_examined": task_ids_examined,
            "discovered_unique_runs": discovered_run_count,
            "processed": run_ids.len(),
            "truncated": truncated,
            "note": "Only explicit run_ids and each requested task's current job_run_id are examined. Orbit exposes no cursor-paginated detailed delivery feed; older retries and unlisted tasks are not claimed as covered.",
            "resume": "resubmit omitted run_ids or task_ids; delivery IDs are idempotent",
        },
        "outcomes": outcomes,
        "status": index.status()?,
    }))
}

fn validate_schema(version: u32) -> Result<(), GraphError> {
    if version == PLUGIN_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(GraphError::invalid_data(
            "validate Orbit plugin schema",
            format!("expected schema_version {PLUGIN_SCHEMA_VERSION}, got {version}"),
        ))
    }
}

fn task_text(task: &TaskAssociation) -> String {
    let mut text = format!("{} {}", task.title, task.description);
    for criterion in &task.acceptance_criteria {
        text.push(' ');
        text.push_str(criterion);
    }
    text
}

fn string_field<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(Value::as_str)
}

fn json_error(error: serde_json::Error) -> GraphError {
    GraphError::invalid_data("decode or encode JSON", error.to_string())
}

fn default_branch() -> String {
    "main".to_string()
}

fn default_hybrid_limit() -> usize {
    20
}

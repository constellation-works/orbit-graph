//! Orbit external-tool protocol and public-authority adapter.

use std::collections::BTreeSet;
use std::env;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    DeliveryImport, EXTRACTOR_VERSION, GraphError, HISTORY_INDEX_SCHEMA_VERSION, HistoryIndex,
    HybridTaskHit, RecommendationEngine, RecommendationInput, RecommendationLevel,
    RecommendationRequest, RecommendationVariant, STORE_SCHEMA_VERSION, TaskAssociation,
};

mod adapter;
mod code_index;
mod error;

use adapter::{OrbitAdapter, canonical_repository};
use code_index::{Incomplete, IndexState};
pub use error::{ToolError, ToolErrorCode};

/// External tool name for recommendations and authoritative task lookup.
pub const RECOMMEND_TOOL_NAME: &str = "orbit.graph.recommend";
/// External tool name for index freshness/status.
pub const STATUS_TOOL_NAME: &str = "orbit.graph.status";
/// External tool name for bounded import and synchronization.
pub const MAINTAIN_TOOL_NAME: &str = "orbit.graph.maintain";
/// External tool name for deterministic build and schema version reporting.
pub const VERSION_TOOL_NAME: &str = "orbit.graph.version";
/// Version of every external-tool request and response envelope.
pub const PLUGIN_SCHEMA_VERSION: u32 = 1;

const V2_RECOMMEND_TOOL_NAME: &str = "graph.recommend";
const V2_STATUS_TOOL_NAME: &str = "graph.status";
const V2_MAINTAIN_TOOL_NAME: &str = "graph.maintain";
const V2_VERSION_TOOL_NAME: &str = "graph.version";

/// Whether `name` selects one of this package's no-argv external tools.
pub fn recognizes_tool(name: &str) -> bool {
    matches!(
        name,
        RECOMMEND_TOOL_NAME
            | STATUS_TOOL_NAME
            | MAINTAIN_TOOL_NAME
            | VERSION_TOOL_NAME
            | V2_RECOMMEND_TOOL_NAME
            | V2_STATUS_TOOL_NAME
            | V2_MAINTAIN_TOOL_NAME
            | V2_VERSION_TOOL_NAME
    )
}

/// Execute one no-argv Orbit external-tool request from JSON stdin bytes.
///
/// Failures carry a stable [`ToolErrorCode`]: a request the tool refuses
/// before touching any repository is [`ToolErrorCode::InvalidRequest`], a
/// routed repository that cannot be opened is
/// [`ToolErrorCode::RepositoryUnavailable`], and every other failure is
/// [`ToolErrorCode::GraphError`].
pub fn execute_external_tool(name: &str, input: &[u8]) -> Result<Value, ToolError> {
    match name {
        RECOMMEND_TOOL_NAME | V2_RECOMMEND_TOOL_NAME => recommend(decode_input(input)?),
        STATUS_TOOL_NAME | V2_STATUS_TOOL_NAME => status(decode_input(input)?),
        MAINTAIN_TOOL_NAME | V2_MAINTAIN_TOOL_NAME => maintain(decode_input(input)?),
        VERSION_TOOL_NAME | V2_VERSION_TOOL_NAME => version(decode_input(input)?),
        _ => Err(ToolError::invalid_request(
            "dispatch Orbit external tool",
            format!("unsupported ORBIT_TOOL_NAME {name:?}"),
        )),
    }
}

fn decode_input<T: serde::de::DeserializeOwned>(input: &[u8]) -> Result<T, ToolError> {
    serde_json::from_slice(input)
        .map_err(|error| ToolError::invalid_request("decode plugin tool input", error.to_string()))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionToolInput {}

fn version(_input: VersionToolInput) -> Result<Value, ToolError> {
    Ok(json!({
        "crate_version": env!("CARGO_PKG_VERSION"),
        "extractor_version": EXTRACTOR_VERSION,
        "store_schema_version": STORE_SCHEMA_VERSION,
        "history_schema_version": HISTORY_INDEX_SCHEMA_VERSION,
        "plugin_schema_version": PLUGIN_SCHEMA_VERSION,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecommendToolInput {
    #[serde(default = "default_schema_version")]
    schema_version: u32,
    repository: PathBuf,
    #[serde(default = "default_branch")]
    branch: String,
    #[serde(default)]
    workspace: Option<String>,
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

fn recommend(input: RecommendToolInput) -> Result<Value, ToolError> {
    validate_schema(input.schema_version)?;
    if input.hybrid && (input.hybrid_limit == 0 || input.hybrid_limit > 100) {
        return Err(ToolError::invalid_request(
            "validate hybrid search bound",
            "hybrid_limit must be between 1 and 100",
        ));
    }
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
            return Err(ToolError::invalid_request(
                "validate plugin recommendation intent",
                "exactly one non-empty query or task_id is required",
            ));
        }
    };
    let repository = routed_repository(input.repository.as_path())?;
    let adapter = OrbitAdapter::new(repository.as_path(), input.workspace.as_deref());
    if let RecommendationInput::TaskId(task_id) = &intent {
        if input.cutoff.is_none() {
            let observed = adapter.task_snapshot(task_id)?;
            if snapshot
                .as_ref()
                .is_some_and(|value| value.task_id != observed.task_id)
            {
                return Err(ToolError::graph(GraphError::invalid_data(
                    "verify supplied task snapshot",
                    "supplied task ID does not match the selected public workspace task",
                )));
            }
            if snapshot.is_none() {
                snapshot = Some(observed);
                task_text_source = "orbit.task.show_public_observation".to_string();
            } else {
                task_text_source = "supplied_snapshot+verified_live_workspace".to_string();
            }
        } else if snapshot.is_none() {
            snapshot = Some(adapter.task_snapshot(task_id)?);
            task_text_source = "orbit.task.show_public_observation".to_string();
        }
    }
    let query_text = match (&intent, snapshot.as_ref()) {
        (RecommendationInput::Query(query), _) => query.clone(),
        (RecommendationInput::TaskId(_), Some(task)) => task_text(task),
        (RecommendationInput::TaskId(_), None) => String::new(),
    };
    let mut warnings = Vec::new();
    let mut hybrid_hits = input.hybrid_hits;
    let hybrid_source = if input.hybrid {
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
    let index_dir = plugin_index_dir(repository.as_path())?;
    let engine = match index_dir.as_deref() {
        Some(index_dir) => RecommendationEngine::open_with_index_dir(
            repository.as_path(),
            input.branch.as_str(),
            index_dir,
            IndexState::read(index_dir)?.structure_index(index_dir),
        )?,
        None => RecommendationEngine::open(repository.as_path(), input.branch.as_str())?,
    };
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
    .map_err(|error| ToolError::graph(json_error(error)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusToolInput {
    #[serde(default = "default_schema_version")]
    schema_version: u32,
    repository: PathBuf,
    #[serde(default = "default_branch")]
    branch: String,
}

fn status(input: StatusToolInput) -> Result<Value, ToolError> {
    validate_schema(input.schema_version)?;
    let repository = routed_repository(input.repository.as_path())?;
    let index = open_history_index(repository.as_path(), input.branch.as_str())?;
    let mut response = json!({
        "schema_version": PLUGIN_SCHEMA_VERSION,
        "operation": "status",
        "repository": repository,
        "status": index.status()?,
    });
    if let Some(index_dir) = plugin_index_dir(repository.as_path())? {
        response["code_index"] = code_index_status(repository.as_path(), index_dir.as_path())?;
    }
    Ok(response)
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MaintenanceOperation {
    HistorySync,
    Import,
    OrbitSync,
    GraphSync,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MaintainToolInput {
    #[serde(default = "default_schema_version")]
    schema_version: u32,
    operation: MaintenanceOperation,
    repository: PathBuf,
    /// Landing branch for history operations; `None` means `main`.
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    delivery: Option<DeliveryImport>,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    task_ids: Vec<String>,
    #[serde(default)]
    run_ids: Vec<String>,
    #[serde(default)]
    task_snapshots: Vec<TaskAssociation>,
    #[serde(default)]
    full: bool,
    #[serde(default)]
    budget_ms: Option<u64>,
}

fn maintain(input: MaintainToolInput) -> Result<Value, ToolError> {
    validate_schema(input.schema_version)?;
    let mut budget_ms = code_index::DEFAULT_BUDGET_MS;
    match input.operation {
        MaintenanceOperation::HistorySync => {
            let limit = input.limit.unwrap_or(100);
            if limit == 0 || limit > 1_000 {
                return Err(ToolError::invalid_request(
                    "validate plugin history sync bound",
                    "limit must be between 1 and 1000",
                ));
            }
        }
        MaintenanceOperation::Import => {
            if input.delivery.is_none() {
                return Err(ToolError::invalid_request(
                    "validate plugin import",
                    "delivery is required for operation=import",
                ));
            }
        }
        MaintenanceOperation::OrbitSync => {
            let bound = input.limit.unwrap_or(25);
            if bound == 0 || bound > 100 {
                return Err(ToolError::invalid_request(
                    "validate Orbit adapter sync bound",
                    "limit must be between 1 and 100",
                ));
            }
        }
        MaintenanceOperation::GraphSync => {
            budget_ms = input.budget_ms.unwrap_or(budget_ms);
            if !(code_index::MIN_BUDGET_MS..=code_index::MAX_BUDGET_MS).contains(&budget_ms) {
                return Err(ToolError::invalid_request(
                    "validate graph_sync budget",
                    format!(
                        "budget_ms must be between {} and {}",
                        code_index::MIN_BUDGET_MS,
                        code_index::MAX_BUDGET_MS
                    ),
                ));
            }
            let inapplicable = [
                ("branch", input.branch.is_some()),
                ("limit", input.limit.is_some()),
                ("delivery", input.delivery.is_some()),
                ("workspace", input.workspace.is_some()),
                ("task_ids", !input.task_ids.is_empty()),
                ("run_ids", !input.run_ids.is_empty()),
                ("task_snapshots", !input.task_snapshots.is_empty()),
            ]
            .into_iter()
            .filter_map(|(field, supplied)| supplied.then_some(field))
            .collect::<Vec<_>>();
            if !inapplicable.is_empty() {
                // STD-01 R29: never accept a field and silently ignore it.
                return Err(ToolError::invalid_request(
                    "validate graph_sync input",
                    format!(
                        "operation=graph_sync does not use {}; it indexes the checkout as it is",
                        inapplicable.join(", ")
                    ),
                ));
            }
            if env::var_os("ORBIT_PLUGIN_STATE").is_none_or(|state| state.is_empty()) {
                return Err(ToolError::invalid_request(
                    "validate graph_sync environment",
                    "graph_sync maintains the plugin's code-graph index and needs \
                     ORBIT_PLUGIN_STATE, which Orbit sets for plugin tools; outside Orbit, run \
                     `orbit-graph sync` in the repository instead",
                ));
            }
        }
    }
    if !matches!(input.operation, MaintenanceOperation::GraphSync)
        && (input.full || input.budget_ms.is_some())
    {
        return Err(ToolError::invalid_request(
            "validate plugin maintenance input",
            "full and budget_ms apply only to operation=graph_sync",
        ));
    }
    let repository = routed_repository(input.repository.as_path())?;
    let branch = input.branch.clone().unwrap_or_else(default_branch);
    let history_index = || open_history_index(repository.as_path(), branch.as_str());
    match input.operation {
        MaintenanceOperation::HistorySync => {
            let limit = input.limit.unwrap_or(100);
            let result = history_index()?.sync(Some(limit))?;
            Ok(json!({
                "schema_version": PLUGIN_SCHEMA_VERSION,
                "operation": "history_sync",
                "repository": repository,
                "coverage": {
                    "complete": result.complete,
                    "kind": "bounded_first_parent_git",
                    "note": if result.complete {
                        "caught up to the frozen snapshot tip"
                    } else {
                        "partial newest-first suffix; resubmit to continue from resume_from"
                    },
                },
                "result": result,
            }))
        }
        MaintenanceOperation::Import => {
            let delivery = input.delivery.ok_or_else(|| {
                ToolError::invalid_request(
                    "validate plugin import",
                    "delivery is required for operation=import",
                )
            })?;
            Ok(json!({
                "schema_version": PLUGIN_SCHEMA_VERSION,
                "operation": "import",
                "repository": repository,
                "result": history_index()?.import(delivery)?,
            }))
        }
        MaintenanceOperation::OrbitSync => {
            let index = history_index()?;
            sync_orbit(repository, index, input).map_err(ToolError::graph)
        }
        MaintenanceOperation::GraphSync => graph_sync(repository, input.full, budget_ms),
    }
}

/// Build and publish the plugin's code-graph index within `budget_ms`.
fn graph_sync(repository: PathBuf, full: bool, budget_ms: u64) -> Result<Value, ToolError> {
    let index_dir = plugin_index_dir(repository.as_path())?.ok_or_else(|| {
        ToolError::invalid_request(
            "resolve code-graph index directory",
            "graph_sync needs ORBIT_PLUGIN_STATE",
        )
    })?;
    let result = code_index::sync(
        repository.as_path(),
        index_dir.as_path(),
        code_index::SyncRequest {
            full,
            budget: std::time::Duration::from_millis(budget_ms),
        },
    )?;
    let coverage = match result.incomplete {
        None => json!({
            "complete": true,
            "kind": "code_graph",
            "state": "published",
            "note": "the new index is published; recommendations use it while the checkout stays at its revision",
        }),
        Some(incomplete) => json!({
            "complete": false,
            "kind": "code_graph",
            "state": "budget_exhausted",
            "phase": match incomplete {
                Incomplete::ExtractionBudget => "extracting",
                Incomplete::ResolutionBudget => "resolving",
            },
            "note": "the build did not finish inside budget_ms and was discarded; the previously published index, if any, is unchanged and no partial index is ever published",
            "resume": if result.seeded {
                "retry with a larger budget_ms (at most 110000); an incremental build only re-extracts files changed since the published index"
            } else {
                "retry with a larger budget_ms (at most 110000); a first or full build of a repository this size may not fit the plugin timeout, so keep using recommendations without structural evidence meanwhile"
            },
        }),
    };
    Ok(json!({
        "schema_version": PLUGIN_SCHEMA_VERSION,
        "operation": "graph_sync",
        "repository": repository,
        "coverage": coverage,
        "result": {
            "requested_full": full,
            "seeded_from_published": result.seeded,
            "budget_ms": budget_ms,
            "files_indexed": result.files_indexed,
            "files_changed": result.files_changed,
            "files_removed": result.files_removed,
            "timings": result.timings,
            "unowned_files": result.unowned,
        },
        "code_index": code_index_status(repository.as_path(), index_dir.as_path())?,
    }))
}

/// The published code-graph index and whether it matches the checkout.
fn code_index_status(
    repository: &std::path::Path,
    index_dir: &std::path::Path,
) -> Result<Value, GraphError> {
    let state = IndexState::read(index_dir)?;
    let head = code_index::checkout_revision(repository);
    let mut value = state.to_json();
    value["directory"] = json!(index_dir);
    value["checkout_revision"] = json!(head);
    value["fresh"] = json!(match &state {
        IndexState::Ready(published) => published.revision.is_some() && published.revision == head,
        IndexState::Missing | IndexState::Incompatible(_) => false,
    });
    Ok(value)
}

fn sync_orbit(
    repository: PathBuf,
    index: HistoryIndex,
    input: MaintainToolInput,
) -> Result<Value, GraphError> {
    let bound = input.limit.unwrap_or(25);
    let adapter = OrbitAdapter::new(repository.as_path(), input.workspace.as_deref());
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
            input.branch.as_deref().unwrap_or("main"),
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
                let existing = index.delivery(delivery_id.as_str())?;
                if existing.as_ref().is_some_and(|existing| {
                    existing.delivery.before_revision == delivery.before_revision
                        && existing.delivery.after_revision == delivery.after_revision
                }) {
                    outcomes.push(json!({
                        "run_id": run_id,
                        "delivery_id": delivery_id,
                        "task_ids": task_ids,
                        "status": "already_indexed",
                        "reason": "preserved immutable first-observed delivery envelope",
                    }));
                    continue;
                }
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
            "interfaces": ["orbit.workspace.list", "orbit.task.show", "orbit.workflow.run.show", "git"],
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

fn validate_schema(version: u32) -> Result<(), ToolError> {
    if version == PLUGIN_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(ToolError::invalid_request(
            "validate Orbit plugin schema",
            format!("expected schema_version {PLUGIN_SCHEMA_VERSION}, got {version}"),
        ))
    }
}

/// Canonicalize and open the explicitly routed repository, reporting a
/// missing or non-Git path as [`ToolErrorCode::RepositoryUnavailable`].
fn routed_repository(path: &std::path::Path) -> Result<PathBuf, ToolError> {
    canonical_repository(path).map_err(ToolError::repository_unavailable)
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

fn open_history_index(
    repository: &std::path::Path,
    branch: &str,
) -> Result<HistoryIndex, GraphError> {
    match plugin_index_dir(repository)?.as_deref() {
        Some(index_dir) => HistoryIndex::open_with_index_dir(repository, branch, index_dir),
        None => HistoryIndex::open(repository, branch),
    }
}

fn plugin_index_dir(repository: &std::path::Path) -> Result<Option<PathBuf>, GraphError> {
    let Some(state_root) = env::var_os("ORBIT_PLUGIN_STATE") else {
        return Ok(None);
    };
    if state_root.is_empty() {
        return Err(GraphError::invalid_data(
            "resolve plugin index directory",
            "ORBIT_PLUGIN_STATE must not be empty when set",
        ));
    }
    let repository_hash = blake3::hash(repository.as_os_str().as_encoded_bytes());
    Ok(Some(
        PathBuf::from(state_root).join(repository_hash.to_hex().as_str()),
    ))
}

fn default_branch() -> String {
    "main".to_string()
}

const fn default_schema_version() -> u32 {
    PLUGIN_SCHEMA_VERSION
}

fn default_hybrid_limit() -> usize {
    20
}

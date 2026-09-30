//! Orbit external-tool protocol and public-authority adapter.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use orbit_graph::{
    DeliveryImport, EXTRACTOR_VERSION, GraphError, GraphErrorClass, HISTORY_INDEX_SCHEMA_VERSION,
    HistoryIndex, HybridTaskHit, RecommendationEngine, RecommendationInput, RecommendationLevel,
    RecommendationRequest, RecommendationVariant, STORE_SCHEMA_VERSION, TaskAssociation,
};

mod adapter;
mod changes;
mod code_index;
mod error;
mod query;

use adapter::{OrbitAdapter, RunVerdict, canonical_repository};
use code_index::{Incomplete, IndexState};
pub(crate) use error::orbit_detail;
pub use error::{ToolError, ToolErrorCode};
use query::QueryTool;

/// Version of every external-tool request and response envelope.
pub const PLUGIN_SCHEMA_VERSION: u32 = 1;

/// One external tool: its first-party name, the v2 alias Orbit uses for an
/// unverified install, whether its input is routed to a `repository`, and
/// the function that runs it.
#[derive(Clone, Copy)]
pub(crate) struct ToolSpec {
    /// `orbit.graph.<verb>`, from a verified first-party install.
    pub(crate) name: &'static str,
    /// `graph.<verb>`, otherwise.
    pub(crate) v2_alias: &'static str,
    /// Whether a v2 envelope without `input.repository` is routed to
    /// `context.workspace_root`.
    pub(crate) needs_repository: bool,
    handler: fn(&ToolCall<'_>) -> Result<Value, ToolError>,
}

impl ToolSpec {
    /// Whether `name` selects this tool, in either spelling.
    fn answers_to(&self, name: &str) -> bool {
        self.name == name || self.v2_alias == name
    }
}

const VERSION_TOOL: ToolSpec = ToolSpec {
    name: "orbit.graph.version",
    v2_alias: "graph.version",
    needs_repository: false,
    handler: |call| version(decode_input(call.input)?),
};
const STATUS_TOOL: ToolSpec = ToolSpec {
    name: "orbit.graph.status",
    v2_alias: "graph.status",
    needs_repository: true,
    handler: |call| status(call, decode_input(call.input)?),
};
const RECOMMEND_TOOL: ToolSpec = ToolSpec {
    name: "orbit.graph.recommend",
    v2_alias: "graph.recommend",
    needs_repository: true,
    handler: |call| recommend(call, decode_input(call.input)?),
};
const MAINTAIN_TOOL: ToolSpec = ToolSpec {
    name: "orbit.graph.maintain",
    v2_alias: "graph.maintain",
    needs_repository: true,
    handler: |call| maintain(call, decode_input(call.input)?),
};
const CHANGES_TOOL: ToolSpec = ToolSpec {
    name: "orbit.graph.changes",
    v2_alias: "graph.changes",
    needs_repository: true,
    handler: changes::execute,
};

/// Every external tool this backend serves, in manifest order: the one
/// place a tool name is spelled (STD-02 §R24).
pub(crate) const TOOLS: [ToolSpec; 13] = [
    VERSION_TOOL,
    STATUS_TOOL,
    RECOMMEND_TOOL,
    MAINTAIN_TOOL,
    ToolSpec {
        name: "orbit.graph.search",
        v2_alias: "graph.search",
        needs_repository: true,
        handler: |call| query::execute(QueryTool::Search, call),
    },
    ToolSpec {
        name: "orbit.graph.show",
        v2_alias: "graph.show",
        needs_repository: true,
        handler: |call| query::execute(QueryTool::Show, call),
    },
    ToolSpec {
        name: "orbit.graph.refs",
        v2_alias: "graph.refs",
        needs_repository: true,
        handler: |call| query::execute(QueryTool::Refs, call),
    },
    ToolSpec {
        name: "orbit.graph.callees",
        v2_alias: "graph.callees",
        needs_repository: true,
        handler: |call| query::execute(QueryTool::Callees, call),
    },
    ToolSpec {
        name: "orbit.graph.impact",
        v2_alias: "graph.impact",
        needs_repository: true,
        handler: |call| query::execute(QueryTool::Impact, call),
    },
    ToolSpec {
        name: "orbit.graph.trace",
        v2_alias: "graph.trace",
        needs_repository: true,
        handler: |call| query::execute(QueryTool::Trace, call),
    },
    ToolSpec {
        name: "orbit.graph.deps",
        v2_alias: "graph.deps",
        needs_repository: true,
        handler: |call| query::execute(QueryTool::Deps, call),
    },
    ToolSpec {
        name: "orbit.graph.overview",
        v2_alias: "graph.overview",
        needs_repository: true,
        handler: |call| query::execute(QueryTool::Overview, call),
    },
    CHANGES_TOOL,
];

/// The tool `name` selects, in either spelling.
pub(crate) fn find_tool(name: &str) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|tool| tool.answers_to(name))
}

/// One decoded tool request and everything resolved for it.
pub(crate) struct ToolCall<'a> {
    /// Whether the request named the tool by its v2 alias.
    v2: bool,
    input: &'a [u8],
    config: &'a PluginConfig,
    environment: &'a PluginEnvironment,
}

impl ToolCall<'_> {
    /// How `tool` is spelled for this caller: the alias when the caller used
    /// aliases, so a remedy names a tool the caller can call (STD-01 §R21).
    pub(crate) fn spelling(&self, tool: &ToolSpec) -> &'static str {
        if self.v2 { tool.v2_alias } else { tool.name }
    }
}

/// `ORBIT_PLUGIN_STATE`: the plugin state root Orbit grants a plugin tool.
pub const PLUGIN_STATE_ENV: &str = "ORBIT_PLUGIN_STATE";
/// `GRAPH_ORBIT_TIMEOUT_SECONDS`: the bound on each Orbit callback.
pub const ORBIT_TIMEOUT_ENV: &str = "GRAPH_ORBIT_TIMEOUT_SECONDS";
const DEFAULT_ORBIT_TIMEOUT_SECONDS: u64 = 10;
const MAX_ORBIT_TIMEOUT_SECONDS: u64 = 60;

/// The process settings a plugin call reads, resolved and validated once
/// from the environment before any tool runs (STD-02 §R3, §R28).
#[derive(Debug, Clone)]
pub struct PluginEnvironment {
    /// The plugin state root, when Orbit set one.
    state_root: Option<PathBuf>,
    /// How long each Orbit callback may take.
    orbit_timeout: Duration,
}

impl PluginEnvironment {
    /// Validate the raw values of [`PLUGIN_STATE_ENV`] and
    /// [`ORBIT_TIMEOUT_ENV`]; a bad value is refused naming its variable.
    pub fn resolve(
        state_root: Option<OsString>,
        orbit_timeout: Option<OsString>,
    ) -> Result<Self, ToolError> {
        let state_root = match state_root {
            None => None,
            Some(state_root) if state_root.is_empty() => {
                return Err(ToolError::invalid_request(
                    "read plugin environment",
                    format!(
                        "{PLUGIN_STATE_ENV} is set but empty; unset it or name the plugin \
                         state directory"
                    ),
                ));
            }
            Some(state_root) => Some(PathBuf::from(state_root)),
        };
        let seconds = match orbit_timeout {
            None => DEFAULT_ORBIT_TIMEOUT_SECONDS,
            Some(value) => value
                .to_str()
                .and_then(|value| value.trim().parse::<u64>().ok())
                .filter(|seconds| (1..=MAX_ORBIT_TIMEOUT_SECONDS).contains(seconds))
                .ok_or_else(|| {
                    ToolError::invalid_request(
                        "read plugin environment",
                        format!(
                            "{ORBIT_TIMEOUT_ENV}={value:?} must be whole seconds between 1 and \
                             {MAX_ORBIT_TIMEOUT_SECONDS}"
                        ),
                    )
                })?,
        };
        Ok(Self {
            state_root,
            orbit_timeout: Duration::from_secs(seconds),
        })
    }

    /// The plugin state root, when Orbit set one.
    pub(crate) fn state_root(&self) -> Option<&std::path::Path> {
        self.state_root.as_deref()
    }
}

/// The plugin's effective `[plugins.graph]` configuration, which Orbit sends
/// as the envelope's `context.config` (manifest defaults under the operator's
/// values). A bare v1 request carries none, and then every key is unset.
#[derive(Debug, Clone, Default)]
pub struct PluginConfig {
    /// `branch`: the landing branch a call that names none works on.
    branch: Option<String>,
}

impl PluginConfig {
    /// Decode `context.config`. A key this backend does not read is returned
    /// for the caller to warn about and is otherwise ignored, so a config
    /// written for another plugin version keeps loading (STD-02 §R16).
    pub fn from_context(config: Option<&Value>) -> Result<(Self, Vec<String>), ToolError> {
        let Some(config) = config.filter(|config| !config.is_null()) else {
            return Ok((Self::default(), Vec::new()));
        };
        let Some(config) = config.as_object() else {
            return Err(ToolError::invalid_request(
                "read plugin configuration",
                "context.config must be an object",
            ));
        };
        let mut plugin_config = Self::default();
        let mut ignored = Vec::new();
        for (key, value) in config {
            match key.as_str() {
                "branch" => {
                    plugin_config.branch = match value {
                        Value::Null => None,
                        Value::String(branch) if !branch.trim().is_empty() => Some(branch.clone()),
                        _ => {
                            return Err(ToolError::invalid_request(
                                "read plugin configuration",
                                "context.config.branch must be a non-empty string",
                            ));
                        }
                    };
                }
                _ => ignored.push(key.clone()),
            }
        }
        Ok((plugin_config, ignored))
    }

    /// The branch a call works on: its own `branch`, else the configured one,
    /// else `main`.
    fn branch(&self, requested: Option<String>) -> String {
        requested
            .or_else(|| self.branch.clone())
            .unwrap_or_else(|| DEFAULT_BRANCH.to_string())
    }
}

/// The landing branch when neither the call nor the configuration names one.
const DEFAULT_BRANCH: &str = "main";

/// Execute one no-argv Orbit external-tool request from JSON stdin bytes.
///
/// `name` selects a [`TOOLS`] entry in either spelling. Failures carry a
/// stable [`ToolErrorCode`]: a request refused before it is acted on is
/// [`ToolErrorCode::InvalidRequest`], a routed repository that cannot be
/// opened is [`ToolErrorCode::RepositoryUnavailable`], and a graph failure
/// is classified by [`crate::command::report_graph_error`].
pub fn execute_external_tool(
    name: &str,
    input: &[u8],
    config: &PluginConfig,
    environment: &PluginEnvironment,
) -> Result<Value, ToolError> {
    let tool = find_tool(name).ok_or_else(|| {
        ToolError::invalid_request(
            "dispatch Orbit external tool",
            format!("unsupported ORBIT_TOOL_NAME {name:?}"),
        )
    })?;
    (tool.handler)(&ToolCall {
        v2: tool.v2_alias == name,
        input,
        config,
        environment,
    })
}

/// Decode a tool's input. Every string the request schemas declare has
/// `minLength: 1`, so an empty string, top-level or in a top-level array, is
/// refused here, named, before any field is interpreted. Every top-level
/// property has a non-null type: omission selects defaults, but explicit
/// null must not silently discard a supplied field (STD-01 §R29).
fn decode_input<T: serde::de::DeserializeOwned>(input: &[u8]) -> Result<T, ToolError> {
    let value: Value = serde_json::from_slice(input).map_err(|error| {
        ToolError::invalid_request("decode plugin tool input", error.to_string())
    })?;
    if let Some(fields) = value.as_object() {
        let nulls = fields
            .iter()
            .filter(|(_, value)| value.is_null())
            .map(|(field, _)| field.as_str())
            .collect::<Vec<_>>();
        if !nulls.is_empty() {
            return Err(ToolError::invalid_request(
                "decode plugin tool input",
                format!("null in {}; omit a field instead", nulls.join(", ")),
            ));
        }
        let empty = fields
            .iter()
            .filter(|(_, value)| match value {
                Value::String(text) => text.is_empty(),
                Value::Array(items) => items
                    .iter()
                    .any(|item| item.as_str().is_some_and(str::is_empty)),
                _ => false,
            })
            .map(|(field, _)| field.as_str())
            .collect::<Vec<_>>();
        if !empty.is_empty() {
            return Err(ToolError::invalid_request(
                "decode plugin tool input",
                format!("empty string in {}; omit a field instead", empty.join(", ")),
            ));
        }
    }
    serde_json::from_value(value)
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
    #[serde(default)]
    schema_version: Option<u32>,
    repository: PathBuf,
    #[serde(default)]
    branch: Option<String>,
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
    #[serde(default)]
    hybrid_limit: Option<usize>,
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
    /// Results of the authoritative task search dropped as malformed;
    /// `None` when no hybrid search ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    hybrid_hits_dropped: Option<usize>,
    warnings: Vec<String>,
}

/// Upper bound of `recommend`'s `limit`, as `schemas/recommend.request.json`
/// declares it.
const MAX_RECOMMEND_LIMIT: usize = 100;
/// Bounds and default of `recommend`'s `hybrid_limit`.
const MAX_HYBRID_LIMIT: usize = 100;
const DEFAULT_HYBRID_LIMIT: usize = 20;

fn recommend(call: &ToolCall<'_>, input: RecommendToolInput) -> Result<Value, ToolError> {
    validate_schema(input.schema_version)?;
    if !input.hybrid && input.hybrid_limit.is_some() {
        // STD-01 R29: never accept a field and silently ignore it.
        return Err(ToolError::invalid_request(
            "validate recommend input",
            "hybrid_limit applies only with hybrid: true; remove hybrid_limit or set hybrid",
        ));
    }
    let hybrid_limit = input.hybrid_limit.unwrap_or(DEFAULT_HYBRID_LIMIT);
    if !(1..=MAX_HYBRID_LIMIT).contains(&hybrid_limit) {
        return Err(ToolError::invalid_request(
            "validate hybrid search bound",
            format!("hybrid_limit must be between 1 and {MAX_HYBRID_LIMIT}"),
        ));
    }
    if input
        .limit
        .is_some_and(|limit| !(1..=MAX_RECOMMEND_LIMIT).contains(&limit))
    {
        return Err(ToolError::invalid_request(
            "validate recommendation bound",
            format!("limit must be between 1 and {MAX_RECOMMEND_LIMIT}"),
        ));
    }
    let branch = call.config.branch(input.branch);
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
    let adapter = OrbitAdapter::new(
        repository.as_path(),
        input.workspace.as_deref(),
        call.environment.orbit_timeout,
    );
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
                task_text_source = "supplied_snapshot+verified_task_id".to_string();
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
    let mut hybrid_hits_dropped = None;
    let hybrid_source = if input.hybrid {
        match adapter.hybrid_search(query_text.as_str(), hybrid_limit) {
            Ok(search) => {
                if search.dropped > 0 {
                    warnings.push(format!(
                        "dropped {} malformed orbit.search result(s) with invalid task IDs, \
                         scores, or lexical task metadata",
                        search.dropped
                    ));
                }
                hybrid_hits_dropped = Some(search.dropped);
                let mut hits = search.hits;
                hits.append(&mut hybrid_hits);
                hybrid_hits = hits;
                "orbit.search_lexical_rank".to_string()
            }
            Err(error) => {
                warnings.push(format!(
                    "authoritative Orbit task search unavailable; deterministic local lexical fallback used: {error}"
                ));
                "local_lexical_fallback".to_string()
            }
        }
    } else if hybrid_hits.is_empty() {
        "local_lexical".to_string()
    } else {
        "supplied_ranked_hits".to_string()
    };
    let index_dir = plugin_index_dir(call, repository.as_path())?;
    let engine = match index_dir.as_deref() {
        Some(index_dir) => RecommendationEngine::open_with_index_dir(
            repository.as_path(),
            branch.as_str(),
            index_dir,
            IndexState::read(index_dir)?.structure_index(index_dir),
        ),
        None => RecommendationEngine::open(repository.as_path(), branch.as_str()),
    }
    .map_err(|error| history_read_error(call, error, branch.as_str()))?;
    let result = engine.recommend(&RecommendationRequest {
        input: intent,
        level: input.level.into(),
        variant: RecommendationVariant::Combined,
        limit: input.limit,
        target_revision: input.revision,
        cutoff: input.cutoff,
        task_snapshot: snapshot,
        hybrid_hits,
        commit_text_weight: None,
        commit_text_exponent: None,
    })?;
    serde_json::to_value(json!({
        "schema_version": PLUGIN_SCHEMA_VERSION,
        "operation": "recommend",
        "repository": repository,
        "adapter": AdapterEvidence {
            workspace: input.workspace,
            task_text: task_text_source,
            hybrid_search: hybrid_source,
            hybrid_hits_dropped,
            warnings,
        },
        "result": result,
    }))
    .map_err(|error| ToolError::graph(json_error(error)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusToolInput {
    #[serde(default)]
    schema_version: Option<u32>,
    repository: PathBuf,
    #[serde(default)]
    branch: Option<String>,
}

fn status(call: &ToolCall<'_>, input: StatusToolInput) -> Result<Value, ToolError> {
    validate_schema(input.schema_version)?;
    let branch = call.config.branch(input.branch);
    let repository = routed_repository(input.repository.as_path())?;
    let index_dir = plugin_index_dir(call, repository.as_path())?;
    let index = match index_dir.as_deref() {
        Some(index_dir) => HistoryIndex::open_read_only_with_index_dir(
            repository.as_path(),
            branch.as_str(),
            index_dir,
        ),
        None => HistoryIndex::open_read_only(repository.as_path(), branch.as_str()),
    }
    .map_err(|error| history_read_error(call, error, branch.as_str()))?;
    let mut response = json!({
        "schema_version": PLUGIN_SCHEMA_VERSION,
        "operation": "status",
        "repository": repository,
        "status": index.status()?,
    });
    if let Some(index_dir) = index_dir {
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

impl MaintenanceOperation {
    const fn name(self) -> &'static str {
        match self {
            Self::HistorySync => "history_sync",
            Self::Import => "import",
            Self::OrbitSync => "orbit_sync",
            Self::GraphSync => "graph_sync",
        }
    }

    /// The optional fields this operation reads. Any other supplied one is
    /// refused, never dropped (STD-01 §R29).
    const fn reads(self) -> &'static [&'static str] {
        match self {
            Self::HistorySync => &["branch", "limit"],
            Self::Import => &["branch", "delivery"],
            Self::OrbitSync => &[
                "branch",
                "limit",
                "workspace",
                "task_ids",
                "run_ids",
                "task_snapshots",
            ],
            Self::GraphSync => &["full", "budget_ms"],
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MaintainToolInput {
    #[serde(default)]
    schema_version: Option<u32>,
    operation: MaintenanceOperation,
    repository: PathBuf,
    /// Landing branch for history operations; `None` means the configured
    /// branch, else `main`.
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    /// A `DeliveryImport`, decoded only for `import` so that another
    /// operation refuses it by name rather than by its shape.
    #[serde(default)]
    delivery: Option<Value>,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    task_ids: Option<Vec<String>>,
    #[serde(default)]
    run_ids: Option<Vec<String>>,
    /// `TaskAssociation`s, decoded only for `orbit_sync`, like `delivery`.
    #[serde(default)]
    task_snapshots: Option<Vec<Value>>,
    #[serde(default)]
    full: Option<bool>,
    #[serde(default)]
    budget_ms: Option<u64>,
}

impl MaintainToolInput {
    /// The optional fields the caller supplied, in schema order.
    fn supplied(&self) -> Vec<&'static str> {
        [
            ("branch", self.branch.is_some()),
            ("limit", self.limit.is_some()),
            ("delivery", self.delivery.is_some()),
            ("workspace", self.workspace.is_some()),
            ("task_ids", self.task_ids.is_some()),
            ("run_ids", self.run_ids.is_some()),
            ("task_snapshots", self.task_snapshots.is_some()),
            ("full", self.full.is_some()),
            ("budget_ms", self.budget_ms.is_some()),
        ]
        .into_iter()
        .filter_map(|(field, supplied)| supplied.then_some(field))
        .collect()
    }
}

/// `history_sync`'s commit bound: default and maximum.
const DEFAULT_HISTORY_LIMIT: usize = 100;
const MAX_HISTORY_LIMIT: usize = 1_000;
/// `orbit_sync`'s run bound: default and maximum.
const DEFAULT_ORBIT_SYNC_LIMIT: usize = 25;
const MAX_ORBIT_SYNC_LIMIT: usize = 100;

fn maintain(call: &ToolCall<'_>, mut input: MaintainToolInput) -> Result<Value, ToolError> {
    validate_schema(input.schema_version)?;
    let operation = input.operation;
    let inapplicable = input
        .supplied()
        .into_iter()
        .filter(|field| !operation.reads().contains(field))
        .collect::<Vec<_>>();
    if !inapplicable.is_empty() {
        // STD-01 R29: never accept a field and silently ignore it.
        return Err(ToolError::invalid_request(
            "validate plugin maintenance input",
            format!(
                "operation={} does not use {}; it reads only {}",
                operation.name(),
                inapplicable.join(", "),
                operation.reads().join(", ")
            ),
        ));
    }
    let mut budget_ms = code_index::DEFAULT_BUDGET_MS;
    match operation {
        MaintenanceOperation::HistorySync => {
            let limit = input.limit.unwrap_or(DEFAULT_HISTORY_LIMIT);
            if !(1..=MAX_HISTORY_LIMIT).contains(&limit) {
                return Err(ToolError::invalid_request(
                    "validate plugin history sync bound",
                    format!("limit must be between 1 and {MAX_HISTORY_LIMIT}"),
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
            let bound = input.limit.unwrap_or(DEFAULT_ORBIT_SYNC_LIMIT);
            if !(1..=MAX_ORBIT_SYNC_LIMIT).contains(&bound) {
                return Err(ToolError::invalid_request(
                    "validate Orbit adapter sync bound",
                    format!("limit must be between 1 and {MAX_ORBIT_SYNC_LIMIT}"),
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
            if call.environment.state_root().is_none() {
                return Err(ToolError::invalid_request(
                    "validate graph_sync environment",
                    "graph_sync maintains the plugin's code-graph index and needs \
                     ORBIT_PLUGIN_STATE, which Orbit sets for plugin tools; outside Orbit, run \
                     `orbit-graph sync` in the repository instead",
                ));
            }
        }
    }
    let delivery = input
        .delivery
        .take()
        .map(serde_json::from_value::<DeliveryImport>)
        .transpose()
        .map_err(|error| ToolError::invalid_request("decode delivery", error.to_string()))?;
    let task_snapshots = input
        .task_snapshots
        .take()
        .unwrap_or_default()
        .into_iter()
        .map(serde_json::from_value::<TaskAssociation>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ToolError::invalid_request("decode task_snapshots", error.to_string()))?;
    let repository = routed_repository(input.repository.as_path())?;
    let branch = call.config.branch(input.branch.clone());
    let history_index = || open_history_index(call, repository.as_path(), branch.as_str());
    match operation {
        MaintenanceOperation::HistorySync => {
            let limit = input.limit.unwrap_or(DEFAULT_HISTORY_LIMIT);
            let result = history_index()?.sync(Some(limit))?;
            Ok(json!({
                "schema_version": PLUGIN_SCHEMA_VERSION,
                "operation": "history_sync",
                "repository": repository,
                "branch": branch,
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
            let mut delivery = delivery.ok_or_else(|| {
                ToolError::invalid_request(
                    "validate plugin import",
                    "delivery is required for operation=import",
                )
            })?;
            let requested_evidence = delivery.evidence;
            if delivery.evidence == orbit_graph::DeliveryEvidence::VerifiedDelivery {
                delivery.evidence = orbit_graph::DeliveryEvidence::CallerAttested;
            }
            Ok(json!({
                "schema_version": PLUGIN_SCHEMA_VERSION,
                "operation": "import",
                "repository": repository,
                "branch": branch,
                "evidence": {
                    "requested": requested_evidence.as_str(),
                    "stored": delivery.evidence.as_str(),
                    "downgraded": requested_evidence != delivery.evidence,
                },
                "result": history_index()?.import(delivery)?,
            }))
        }
        MaintenanceOperation::OrbitSync => {
            let index = history_index()?;
            sync_orbit(
                call,
                repository,
                branch.as_str(),
                index,
                input,
                task_snapshots,
            )
            .map_err(ToolError::graph)
        }
        MaintenanceOperation::GraphSync => {
            graph_sync(call, repository, input.full.unwrap_or(false), budget_ms)
        }
    }
}

/// Build and publish the plugin's code-graph index within `budget_ms`.
fn graph_sync(
    call: &ToolCall<'_>,
    repository: PathBuf,
    full: bool,
    budget_ms: u64,
) -> Result<Value, ToolError> {
    let index_dir = plugin_index_dir(call, repository.as_path())?.ok_or_else(|| {
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
            "failed": result.failed.as_deref().map(failed_json),
            "skipped": result.skipped.as_deref().map(skipped_json),
            "timings": result.timings,
            "unowned_files": result.unowned,
        },
        "code_index": code_index_status(repository.as_path(), index_dir.as_path())?,
    }))
}

/// `graph_sync`'s `failed` field: the CLI `sync` shape, a count and its
/// entries.
fn failed_json(failed: &[orbit_graph::SyncFailure]) -> Value {
    json!({
        "count": failed.len(),
        "entries": failed
            .iter()
            .map(|failure| json!({
                "path": failure.path,
                "operation": failure.operation,
                "error_kind": failure.error_kind,
                "message": failure.message,
            }))
            .collect::<Vec<_>>(),
    })
}

/// `graph_sync`'s `skipped` field: the CLI `sync` shape, a count and its
/// entries.
fn skipped_json(skipped: &[orbit_graph::SyncSkip]) -> Value {
    json!({
        "count": skipped.len(),
        "entries": skipped
            .iter()
            .map(|skip| json!({"path": skip.path, "reason": skip.reason}))
            .collect::<Vec<_>>(),
    })
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
    call: &ToolCall<'_>,
    repository: PathBuf,
    branch: &str,
    index: HistoryIndex,
    input: MaintainToolInput,
    task_snapshots: Vec<TaskAssociation>,
) -> Result<Value, GraphError> {
    let bound = input.limit.unwrap_or(DEFAULT_ORBIT_SYNC_LIMIT);
    let adapter = OrbitAdapter::new(
        repository.as_path(),
        input.workspace.as_deref(),
        call.environment.orbit_timeout,
    );
    let requested_task_ids = input.task_ids.unwrap_or_default();
    let mut run_ids = input.run_ids.unwrap_or_default();
    let explicit_run_count = run_ids.len();
    let task_count = requested_task_ids.len();
    let mut task_ids_examined = 0;
    let mut truncated = false;
    let mut outcomes = SyncOutcomes::default();
    for task_id in &requested_task_ids {
        if run_ids.len() >= bound {
            truncated = true;
            break;
        }
        task_ids_examined += 1;
        // One unreadable task is reported and counted; the batch goes on
        // (STD-02 §R32).
        match adapter.task_show(task_id) {
            Ok(task) => {
                if let Some(run_id) = string_field(&task, "job_run_id") {
                    run_ids.push(run_id.to_string());
                }
            }
            Err(error) => outcomes.unverified(json!({"task_id": task_id}), &error),
        }
    }
    let mut seen = BTreeSet::new();
    run_ids.retain(|id| seen.insert(id.clone()));
    truncated |= run_ids.len() > bound || task_ids_examined < task_count;
    let discovered_run_count = run_ids.len();
    run_ids.truncate(bound);
    let snapshots = task_snapshots
        .into_iter()
        .map(|snapshot| (snapshot.task_id.clone(), snapshot))
        .collect::<std::collections::BTreeMap<_, _>>();
    for run_id in &run_ids {
        let identity = json!({"run_id": run_id});
        let (delivery, supplied_snapshot) =
            match adapter.delivery_from_run(run_id, branch, &snapshots, index.repository()) {
                Ok(RunVerdict::Deliverable(verified)) => {
                    (verified.delivery, verified.supplied_snapshot)
                }
                Ok(RunVerdict::Excluded(reason)) => {
                    outcomes.record_excluded(identity, reason);
                    continue;
                }
                Err(error) => {
                    outcomes.unverified(identity, &error);
                    continue;
                }
            };
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
            match supplied_snapshot {
                Some(snapshot) => match index.add_supplied_snapshot(&delivery_id, snapshot) {
                    Ok(inserted) => outcomes.push(json!({
                        "run_id": run_id,
                        "delivery_id": delivery_id,
                        "task_ids": task_ids,
                        "status": "already_indexed",
                        "supplied_snapshot": if inserted { "inserted" } else { "already_indexed" },
                        "reason": "preserved first-observed delivery task text and provenance",
                    })),
                    Err(error) => outcomes.refused(identity, &error),
                },
                None => outcomes.push(json!({
                    "run_id": run_id,
                    "delivery_id": delivery_id,
                    "task_ids": task_ids,
                    "status": "already_indexed",
                    "reason": "preserved immutable first-observed delivery envelope",
                })),
            }
            continue;
        }
        match index
            .import_with_supplied_snapshots(delivery, supplied_snapshot.into_iter().collect())
        {
            Ok(report) => outcomes.push(json!({
                "run_id": run_id,
                "delivery_id": delivery_id,
                "task_ids": task_ids,
                "status": if report.inserted {"inserted"} else {"already_indexed"},
            })),
            Err(error) => outcomes.refused(identity, &error),
        }
    }
    Ok(json!({
        "schema_version": PLUGIN_SCHEMA_VERSION,
        "operation": "orbit_sync",
        "repository": repository,
        "branch": branch,
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
            "excluded": outcomes.excluded,
            "failed": outcomes.failed,
            "truncated": truncated,
            "note": "Only explicit run_ids and each requested task's current job_run_id are examined. Orbit exposes no cursor-paginated detailed delivery feed; older retries and unlisted tasks are not claimed as covered. excluded counts runs examined and judged ineligible; failed counts tasks and runs that could not be examined.",
            "resume": "resubmit omitted or failed run_ids or task_ids; delivery IDs are idempotent",
        },
        "outcomes": outcomes.entries,
        "status": index.status()?,
    }))
}

/// `orbit_sync`'s per-item outcomes and their counts. `excluded` is a
/// verdict: the item was examined and is not an eligible delivery. `failed`
/// is infrastructure: the item could not be examined (STD-02 §R30).
#[derive(Default)]
struct SyncOutcomes {
    entries: Vec<Value>,
    excluded: usize,
    failed: usize,
}

impl SyncOutcomes {
    fn push(&mut self, outcome: Value) {
        self.entries.push(outcome);
    }

    fn record_excluded(&mut self, mut identity: Value, reason: String) {
        identity["status"] = json!("excluded");
        identity["reason"] = json!(reason);
        self.excluded += 1;
        self.entries.push(identity);
    }

    fn record_failed(&mut self, mut identity: Value, error: &GraphError) {
        identity["status"] = json!("failed");
        identity["reason"] = json!(error.to_string());
        identity["error"] = error::failure_json(&ToolError::graph(error.clone()));
        self.failed += 1;
        self.entries.push(identity);
    }

    /// An item Orbit's authority could not vouch for: a verdict when the
    /// request's workspace or repository does not hold that authority, a
    /// failure when Orbit or Git could not be read.
    fn unverified(&mut self, identity: Value, error: &GraphError) {
        match error.class() {
            GraphErrorClass::InvalidInput => self.record_excluded(identity, error.to_string()),
            GraphErrorClass::InvalidData
            | GraphErrorClass::NotFound
            | GraphErrorClass::Git
            | GraphErrorClass::Io { .. }
            | GraphErrorClass::Sqlite
            | GraphErrorClass::Timeout
            | GraphErrorClass::OrbitRefused
            | GraphErrorClass::Subprocess
            | GraphErrorClass::IndexMissing
            | GraphErrorClass::IndexIncompatible
            | GraphErrorClass::UnsafeStatePath
            | GraphErrorClass::VersionMismatch => self.record_failed(identity, error),
        }
    }

    /// A delivery the history index refused to store: a verdict when the
    /// index judged its content, a failure when it could not write it.
    fn refused(&mut self, identity: Value, error: &GraphError) {
        match error.class() {
            GraphErrorClass::InvalidInput | GraphErrorClass::InvalidData => {
                self.record_excluded(identity, error.to_string());
            }
            GraphErrorClass::NotFound
            | GraphErrorClass::Git
            | GraphErrorClass::Io { .. }
            | GraphErrorClass::Sqlite
            | GraphErrorClass::Timeout
            | GraphErrorClass::OrbitRefused
            | GraphErrorClass::Subprocess
            | GraphErrorClass::IndexMissing
            | GraphErrorClass::IndexIncompatible
            | GraphErrorClass::UnsafeStatePath
            | GraphErrorClass::VersionMismatch => self.record_failed(identity, error),
        }
    }
}

/// Check a request's `schema_version`. It stays `None` when the caller sent
/// none: nothing fabricates the current version at decode time (STD-02
/// §R16). An unversioned request is read under version 1, the only request
/// contract there has been; a future version 2 must decide what an
/// unversioned request means before it ships.
fn validate_schema(version: Option<u32>) -> Result<(), ToolError> {
    match version {
        None | Some(PLUGIN_SCHEMA_VERSION) => Ok(()),
        Some(version) => Err(ToolError::invalid_request(
            "validate Orbit plugin schema",
            format!("expected schema_version {PLUGIN_SCHEMA_VERSION}, got {version}"),
        )),
    }
}

/// Canonicalize and open the explicitly routed repository, reporting a
/// missing or non-Git path as [`error::ToolErrorCode::RepositoryUnavailable`].
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

/// A read-only tool's history-open failure; a missing index names the
/// `history_sync` maintenance call that builds it, in the caller's own tool
/// spelling (STD-01 §R21).
fn history_read_error(call: &ToolCall<'_>, error: GraphError, branch: &str) -> ToolError {
    if let GraphError::IndexMissing { path, .. } = &error {
        let maintain = call.spelling(&MAINTAIN_TOOL);
        let request = json!({"operation": "history_sync", "branch": branch});
        return ToolError::history_index_missing(format!(
            "no history index has been built for branch {branch} (none at {}); run \
             {maintain} with {request} and retry",
            path.display()
        ));
    }
    ToolError::graph(error)
}

/// The writable history index `maintain` imports and synchronizes into.
fn open_history_index(
    call: &ToolCall<'_>,
    repository: &std::path::Path,
    branch: &str,
) -> Result<HistoryIndex, GraphError> {
    match plugin_index_dir(call, repository)?.as_deref() {
        Some(index_dir) => HistoryIndex::open_with_index_dir(repository, branch, index_dir),
        None => HistoryIndex::open(repository, branch),
    }
}

/// The index directory for `repository` under the plugin state root.
fn index_dir_in(state_root: &std::path::Path, repository: &std::path::Path) -> PathBuf {
    let repository_hash = blake3::hash(repository.as_os_str().as_encoded_bytes());
    state_root.join(repository_hash.to_hex().as_str())
}

fn plugin_index_dir(
    call: &ToolCall<'_>,
    repository: &std::path::Path,
) -> Result<Option<PathBuf>, GraphError> {
    let Some(state_root) = call.environment.state_root() else {
        return Ok(None);
    };
    let index_dir = index_dir_in(state_root, repository);
    // An existing per-repository directory is checked before any tool reads
    // or writes it: a symlink or another user's directory is refused
    // (STD-05 §R6, §R9). Writers create it owner-only when missing.
    if state_root.exists()
        && let Some(name) = index_dir.file_name()
    {
        orbit_graph::private_state_dir(
            state_root,
            std::path::Path::new(name),
            orbit_graph::StateAccess::Read,
        )?;
    }
    Ok(Some(index_dir))
}

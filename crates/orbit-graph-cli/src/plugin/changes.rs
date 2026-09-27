//! The `changes` plugin tool: bounded change analysis for agents.
//!
//! Unlike the query tools it reads no published code-graph index: it indexes
//! the two sides of the comparison itself, through `orbit-graph-changes`.
//! Committed snapshots are cached under the plugin state directory, never in
//! the repository, so a warm call skips indexing. Every call finishes inside
//! its `budget_ms`, which is below the Orbit tool timeout: when the budget
//! runs out the result is returned with `complete: false` and the reason,
//! never left to hang.

use std::path::PathBuf;

use orbit_graph::GraphError;
use orbit_graph_changes::analysis::{
    self, AnalysisError, ChangesBounds, ChangesRange, ChangesRequest, DEFAULT_NODE_CAP,
};
use orbit_graph_changes::evidence::{DEFAULT_TIME_BUDGET_MS, EVIDENCE_DEPTH};
use orbit_graph_changes::filters::FilterSet;
use orbit_graph_changes::snapshot::{ComparisonOptions, SnapshotError};
use serde::Deserialize;
use serde_json::{Value, json};

use super::code_index;
use super::query::ConfidenceInput;
use super::{
    PLUGIN_SCHEMA_VERSION, ToolCall, ToolError, decode_input, routed_repository, validate_schema,
};

/// Plugin default: changed symbols analysed per call.
const DEFAULT_MAX_SYMBOLS: usize = 25;
/// Plugin default: callers kept per changed symbol.
const DEFAULT_MAX_CALLERS: usize = 5;
/// Plugin default: entry points kept per changed symbol.
const DEFAULT_MAX_ENTRY_POINTS: usize = 3;
/// Plugin default: candidate tests kept per changed symbol.
const DEFAULT_MAX_TESTS: usize = 5;
/// Serialized `result` size above which the document is shrunk, with every
/// cut recorded.
const MAX_RESULT_BYTES: usize = 512 * 1024;
/// Snapshot cache directory under the repository's plugin state directory.
const CACHE_DIR_NAME: &str = "changes-snapshots";
/// Parent of task-owned temporary trees under the same directory; each is
/// removed when its call ends.
const SCRATCH_DIR_NAME: &str = "changes-scratch";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangesInput {
    #[serde(default)]
    schema_version: Option<u32>,
    repository: PathBuf,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    head: Option<String>,
    #[serde(default)]
    symbols: Vec<String>,
    #[serde(default)]
    confidence: Option<ConfidenceInput>,
    #[serde(default)]
    depth: Option<u8>,
    #[serde(default)]
    node_cap: Option<usize>,
    #[serde(default)]
    query_budget_ms: Option<u64>,
    #[serde(default)]
    max_symbols: Option<usize>,
    #[serde(default)]
    max_callers: Option<usize>,
    #[serde(default)]
    max_entry_points: Option<usize>,
    #[serde(default)]
    max_tests: Option<usize>,
    #[serde(default)]
    budget_ms: Option<u64>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

impl ChangesInput {
    /// Validate every field without touching the repository.
    fn validate(self) -> Result<ChangesRequest, ToolError> {
        validate_schema(self.schema_version)?;
        let budget_ms = self.budget_ms.unwrap_or(code_index::DEFAULT_BUDGET_MS);
        if !(code_index::MIN_BUDGET_MS..=code_index::MAX_BUDGET_MS).contains(&budget_ms) {
            // The Orbit tool timeout is 120 s: a longer budget could be killed
            // mid-call instead of returning an incomplete result.
            return Err(ToolError::invalid_request(
                "validate changes budget",
                format!(
                    "budget_ms must be between {} and {}",
                    code_index::MIN_BUDGET_MS,
                    code_index::MAX_BUDGET_MS
                ),
            ));
        }
        for (field, value) in [
            ("base", self.base.as_deref()),
            ("head", self.head.as_deref()),
        ] {
            if value.is_some_and(|value| value.trim().is_empty()) {
                return Err(ToolError::invalid_request(
                    "validate changes range",
                    format!("{field} must not be empty"),
                ));
            }
        }
        let range = match (self.base, self.head) {
            (base, None) => ChangesRange::WorkingTree { base },
            (Some(base), Some(head)) => ChangesRange::Revisions { base, head },
            (None, Some(_)) => {
                return Err(ToolError::invalid_request(
                    "validate changes range",
                    "head requires base; omit both to compare the working tree against the \
                     default base",
                ));
            }
        };
        let request = ChangesRequest {
            repository: self.repository,
            range,
            selection: self.symbols,
            filters: FilterSet {
                language: self.language,
                scope: self.scope,
                ..FilterSet::default()
            },
            min_confidence: super::query::confidence(self.confidence),
            bounds: ChangesBounds {
                depth: self.depth.unwrap_or(EVIDENCE_DEPTH),
                node_cap: self.node_cap.unwrap_or(DEFAULT_NODE_CAP),
                query_budget_ms: self.query_budget_ms.unwrap_or(DEFAULT_TIME_BUDGET_MS),
                max_symbols: self.max_symbols.unwrap_or(DEFAULT_MAX_SYMBOLS),
                max_callers: self.max_callers.unwrap_or(DEFAULT_MAX_CALLERS),
                max_entry_points: self.max_entry_points.unwrap_or(DEFAULT_MAX_ENTRY_POINTS),
                max_tests: self.max_tests.unwrap_or(DEFAULT_MAX_TESTS),
                budget_ms: Some(budget_ms),
            },
            cache: ComparisonOptions::default(),
        };
        analysis::validate(&request).map_err(invalid_input)?;
        Ok(request)
    }
}

/// Run the `changes` tool.
pub(crate) fn execute(call: &ToolCall<'_>) -> Result<Value, ToolError> {
    let mut request = decode_input::<ChangesInput>(call.input)?.validate()?;
    let repository = routed_repository(request.repository.as_path())?;
    request.repository = repository.clone();
    let mut notices = Vec::new();
    request.cache = match call.environment.state_root() {
        Some(state_root) => {
            // The manifest grants writes to plugin state, not to the system
            // temporary directory, so the working-tree head is built there too.
            let state = super::index_dir_in(state_root, repository.as_path());
            ComparisonOptions {
                cache_dir: Some(state.join(CACHE_DIR_NAME)),
                no_cache: false,
                scratch_dir: Some(state.join(SCRATCH_DIR_NAME)),
            }
        }
        None => {
            // Never fall back to the repository: without plugin state the
            // call caches nothing.
            notices.push(
                "ORBIT_PLUGIN_STATE is not set, so no snapshot was cached; every call indexes \
                 both sides"
                    .to_string(),
            );
            ComparisonOptions {
                cache_dir: None,
                no_cache: true,
                ..ComparisonOptions::default()
            }
        }
    };

    let mut document = analysis::analyse(&request).map_err(analysis_error)?;
    document.notices.extend(notices);
    if !document.fit_to_bytes(MAX_RESULT_BYTES) {
        return Err(ToolError::graph(GraphError::invalid_data(
            "bound change analysis result",
            format!(
                "the result exceeds the {MAX_RESULT_BYTES}-byte response ceiling even with every \
                 list emptied; select fewer symbols or narrow the scope"
            ),
        )));
    }
    let complete = document.complete;
    let truncated = document.truncated;
    Ok(json!({
        "schema_version": PLUGIN_SCHEMA_VERSION,
        "operation": "changes",
        "repository": repository,
        "complete": complete,
        "truncated": truncated,
        "result": document,
    }))
}

fn invalid_input(error: AnalysisError) -> ToolError {
    ToolError::invalid_request("validate changes input", error.to_string())
}

/// Map a change-analysis failure onto the plugin's stable codes: a ref that
/// does not resolve, or no default base, is the caller's input and is refused
/// before anything is indexed.
fn analysis_error(error: AnalysisError) -> ToolError {
    match error {
        AnalysisError::InvalidRange { .. }
        | AnalysisError::InvalidBound { .. }
        | AnalysisError::InvalidSelection { .. }
        | AnalysisError::Snapshot(SnapshotError::Revision { .. })
        | AnalysisError::Snapshot(SnapshotError::NoDefaultBase { .. }) => invalid_input(error),
        AnalysisError::Snapshot(SnapshotError::Repository { .. }) => {
            ToolError::repository_unavailable(GraphError::invalid_data(
                "open repository for change analysis",
                error.to_string(),
            ))
        }
        error => ToolError::graph(GraphError::invalid_data(
            "analyse changes",
            error.to_string(),
        )),
    }
}

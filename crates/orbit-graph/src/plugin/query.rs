//! Read-only code-graph query tools over the published plugin index.
//!
//! Every tool reads the generation that `graph_sync` published for the
//! routed repository, opened strictly read-only: a query creates, migrates,
//! locks, or synchronizes nothing (STD-01 R31). Library query results are
//! passed through unshaped under `result`; the plugin only caps top-level
//! arrays and reports what it cut, so fields the library adds reach callers
//! without a plugin change.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::code_index::{self, IndexState, PublishedIndex};
use super::{PLUGIN_SCHEMA_VERSION, ToolError, decode_input, routed_repository, validate_schema};
use crate::{
    DEFAULT_IMPACT_DEPTH, DEFAULT_SEARCH_LIMIT, DEFAULT_TRACE_DEPTH, Graph, ImpactDirection,
    OverviewFormat, RefConfidence, RefKind, RefOpts, SearchKind, SearchQuery, Selector,
};

/// Default cap on each top-level result array.
const DEFAULT_LIMIT: usize = 50;
/// Largest accepted `limit`.
const MAX_LIMIT: usize = 500;
/// Default source bytes returned by `show`.
const DEFAULT_SHOW_BYTES: usize = 16 * 1024;
/// Largest accepted `max_bytes` for `show`.
const MAX_SHOW_BYTES: usize = 64 * 1024;
/// Largest accepted traversal depth for `impact` and `trace`.
const MAX_DEPTH: u8 = 10;
/// Serialized size above which array caps are tightened further.
const MAX_RESULT_BYTES: usize = 256 * 1024;

/// The read-only query tools, by verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueryTool {
    Search,
    Show,
    Refs,
    Callees,
    Impact,
    Trace,
    Deps,
    Overview,
}

impl QueryTool {
    /// Every query tool, in manifest order.
    pub(crate) const ALL: [Self; 8] = [
        Self::Search,
        Self::Show,
        Self::Refs,
        Self::Callees,
        Self::Impact,
        Self::Trace,
        Self::Deps,
        Self::Overview,
    ];

    /// The manifest verb.
    pub(crate) const fn verb(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Show => "show",
            Self::Refs => "refs",
            Self::Callees => "callees",
            Self::Impact => "impact",
            Self::Trace => "trace",
            Self::Deps => "deps",
            Self::Overview => "overview",
        }
    }

    /// The query tool `name` selects: `orbit.graph.<verb>` from a verified
    /// first-party install, or `graph.<verb>` otherwise.
    pub(crate) fn from_tool_name(name: &str) -> Option<Self> {
        let verb = name
            .strip_prefix("orbit.graph.")
            .or_else(|| name.strip_prefix("graph."))?;
        Self::ALL.into_iter().find(|tool| tool.verb() == verb)
    }
}

/// Decode and validate a query tool's input without touching any repository.
fn validate(tool: QueryTool, input: &[u8]) -> Result<QueryRequest, ToolError> {
    match tool {
        QueryTool::Search => decode_input::<SearchInput>(input)?.validate(),
        QueryTool::Show => decode_input::<ShowInput>(input)?.validate(),
        QueryTool::Refs => decode_input::<RefsInput>(input)?.validate(),
        QueryTool::Callees => decode_input::<CalleesInput>(input)?.validate(),
        QueryTool::Impact => decode_input::<ImpactInput>(input)?.validate(),
        QueryTool::Trace => decode_input::<TraceInput>(input)?.validate(),
        QueryTool::Deps => decode_input::<DepsInput>(input)?.validate(),
        QueryTool::Overview => decode_input::<OverviewInput>(input)?.validate(),
    }
}

/// Run query tool `tool`, invoked under `name`.
pub(crate) fn execute(tool: QueryTool, name: &str, input: &[u8]) -> Result<Value, ToolError> {
    let maintain = if name.starts_with("orbit.") {
        "orbit.graph.maintain"
    } else {
        "graph.maintain"
    };
    let request = validate(tool, input)?;
    // Checked before routing: without plugin state there is no index to read.
    let state_root = std::env::var_os("ORBIT_PLUGIN_STATE")
        .filter(|state| !state.is_empty())
        .ok_or_else(|| {
            ToolError::invalid_request(
                "locate code-graph index",
                "query tools read the plugin's code-graph index and need ORBIT_PLUGIN_STATE, \
                 which Orbit sets for plugin tools; outside Orbit, run the orbit-graph CLI in \
                 the repository instead",
            )
        })?;
    let repository = routed_repository(request.repository.as_path())?;
    let index_dir = super::index_dir_in(Path::new(&state_root), repository.as_path());
    let published = match IndexState::read(index_dir.as_path())? {
        IndexState::Ready(published) => published,
        IndexState::Missing => {
            return Err(ToolError::index_missing(format!(
                "no code-graph index has been built for {}; run {maintain} with \
                 {{\"operation\":\"graph_sync\"}} and retry",
                repository.display()
            )));
        }
        IndexState::Incompatible(published) => {
            return Err(ToolError::index_incompatible(format!(
                "the code-graph index for {} was built by extractor {} and this plugin reads \
                 extractor {}; run {maintain} with {{\"operation\":\"graph_sync\",\"full\":true}} \
                 and retry",
                repository.display(),
                published.extractor_version,
                crate::EXTRACTOR_VERSION
            )));
        }
    };
    let graph = Graph::open_read_only(
        repository.as_path(),
        index_dir.join(&published.database).as_path(),
    )?;
    let raw = request.query.run(&graph)?;
    let (result, truncation) = bound(raw, request.limit);
    let mut response = json!({
        "schema_version": PLUGIN_SCHEMA_VERSION,
        "operation": tool.verb(),
        "repository": repository,
        "index": index_report(repository.as_path(), &published, maintain),
        "result": result,
        "truncated": !truncation.is_empty(),
    });
    if !truncation.is_empty() {
        response["truncation"] = Value::Object(truncation);
    }
    Ok(response)
}

/// Which revision answered, and how to refresh it when it is not the
/// checkout's.
fn index_report(repository: &Path, published: &PublishedIndex, maintain: &str) -> Value {
    let checkout = code_index::checkout_revision(repository);
    let fresh = published.revision.is_some() && published.revision == checkout;
    let mut report = json!({
        "revision": published.revision,
        "checkout_revision": checkout,
        "fresh": fresh,
        "worktree_dirty": published.worktree_dirty,
        "synced_at": published.synced_at,
        "files": published.files,
    });
    if !fresh {
        report["stale"] = json!({
            "reason": format!(
                "the code-graph index was built at {} but the checkout is at {}; results \
                 describe the older code",
                published.revision.as_deref().unwrap_or("an unborn branch"),
                checkout.as_deref().unwrap_or("an unborn branch"),
            ),
            "fix": {"tool": maintain, "input": {"operation": "graph_sync"}},
        });
    }
    report
}

/// Cap each top-level array of `result` at `limit`, tightening further while
/// the serialized result exceeds [`MAX_RESULT_BYTES`]. Returns the bounded
/// result and, per cut field, how many entries were returned of how many.
fn bound(mut result: Value, limit: usize) -> (Value, Map<String, Value>) {
    let Some(object) = result.as_object_mut() else {
        return (result, Map::new());
    };
    let totals = object
        .iter()
        .filter_map(|(field, value)| Some((field.clone(), value.as_array()?.len())))
        .collect::<Vec<_>>();
    let mut cap = limit;
    loop {
        for (field, _) in &totals {
            if let Some(Value::Array(items)) = object.get_mut(field) {
                items.truncate(cap);
            }
        }
        let size = serde_json::to_vec(&*object).map_or(0, |bytes| bytes.len());
        if size <= MAX_RESULT_BYTES || cap <= 1 {
            break;
        }
        cap /= 2;
    }
    let mut truncation = Map::new();
    for (field, total) in totals {
        let returned = object
            .get(&field)
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        if returned < total {
            truncation.insert(field, json!({"returned": returned, "total": total}));
        }
    }
    (result, truncation)
}

/// A validated query, ready to run once the repository is routed.
#[derive(Debug, PartialEq)]
struct QueryRequest {
    repository: PathBuf,
    limit: usize,
    query: Query,
}

#[derive(Debug, PartialEq)]
enum Query {
    Search(SearchQuery),
    Show {
        selector: Selector,
        max_bytes: usize,
    },
    Refs {
        selector: Selector,
        opts: RefOpts,
    },
    Callees(Selector),
    Impact {
        selector: Selector,
        depth: u8,
        confidence: RefConfidence,
        direction: ImpactDirection,
    },
    Trace {
        command: String,
        depth: u8,
        confidence: RefConfidence,
    },
    Deps(Selector),
    Overview {
        scope: Option<Selector>,
        format: OverviewFormat,
    },
}

impl Query {
    fn run(&self, graph: &Graph) -> Result<Value, ToolError> {
        let value = match self {
            Self::Search(query) => to_value(graph.search(query)?)?,
            Self::Show {
                selector,
                max_bytes,
            } => to_value(graph.show(selector, *max_bytes)?)?,
            Self::Refs { selector, opts } => to_value(graph.refs(selector, opts)?)?,
            Self::Callees(selector) => json!({"callees": graph.callees(selector)?}),
            Self::Impact {
                selector,
                depth,
                confidence,
                direction,
            } => {
                to_value(graph.impact_with_direction(selector, *depth, *confidence, *direction)?)?
            }
            Self::Trace {
                command,
                depth,
                confidence,
            } => to_value(graph.trace(command, *depth, *confidence)?)?,
            Self::Deps(selector) => to_value(graph.deps(selector)?)?,
            Self::Overview { scope, format } => to_value(graph.overview(scope.as_ref(), *format)?)?,
        };
        Ok(value)
    }
}

fn to_value(value: impl serde::Serialize) -> Result<Value, ToolError> {
    serde_json::to_value(value).map_err(|error| {
        ToolError::graph(crate::GraphError::invalid_data(
            "encode code-graph query result",
            error.to_string(),
        ))
    })
}

fn selector(field: &'static str, text: &str) -> Result<Selector, ToolError> {
    text.parse::<Selector>().map_err(|error| {
        ToolError::invalid_request("validate code-graph selector", format!("{field}: {error}"))
    })
}

fn limit(value: Option<usize>, default: usize) -> Result<usize, ToolError> {
    let limit = value.unwrap_or(default);
    if (1..=MAX_LIMIT).contains(&limit) {
        Ok(limit)
    } else {
        Err(ToolError::invalid_request(
            "validate code-graph query limit",
            format!("limit must be between 1 and {MAX_LIMIT}"),
        ))
    }
}

fn depth(value: Option<u8>, default: u8) -> Result<u8, ToolError> {
    let depth = value.unwrap_or(default);
    if (1..=MAX_DEPTH).contains(&depth) {
        Ok(depth)
    } else {
        Err(ToolError::invalid_request(
            "validate code-graph traversal depth",
            format!("depth must be between 1 and {MAX_DEPTH}"),
        ))
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConfidenceInput {
    Exact,
    ImportResolved,
    SameModule,
    FuzzyName,
}

impl ConfidenceInput {
    fn into_graph(self) -> RefConfidence {
        match self {
            Self::Exact => RefConfidence::Exact,
            Self::ImportResolved => RefConfidence::ImportResolved,
            Self::SameModule => RefConfidence::SameModule,
            Self::FuzzyName => RefConfidence::FuzzyName,
        }
    }
}

fn confidence(value: Option<ConfidenceInput>) -> RefConfidence {
    value.map_or(RefConfidence::SameModule, ConfidenceInput::into_graph)
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SearchKindInput {
    Symbol,
    String,
    Config,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RefKindInput {
    Call,
    Type,
    Use,
    TraitBound,
    Impl,
    Extends,
    Implements,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DirectionInput {
    Inbound,
    Outbound,
    Both,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FormatInput {
    Summary,
    Full,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchInput {
    #[serde(default = "super::default_schema_version")]
    schema_version: u32,
    repository: PathBuf,
    query: String,
    #[serde(default)]
    kind: Option<SearchKindInput>,
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

impl SearchInput {
    fn validate(self) -> Result<QueryRequest, ToolError> {
        validate_schema(self.schema_version)?;
        if self.query.trim().is_empty() {
            return Err(ToolError::invalid_request(
                "validate code-graph search",
                "query must not be empty",
            ));
        }
        let limit = limit(self.limit, DEFAULT_SEARCH_LIMIT)?;
        Ok(QueryRequest {
            repository: self.repository,
            limit,
            query: Query::Search(SearchQuery {
                query: self.query,
                kind: self.kind.map(|kind| match kind {
                    SearchKindInput::Symbol => SearchKind::Symbol,
                    SearchKindInput::String => SearchKind::String,
                    SearchKindInput::Config => SearchKind::Config,
                }),
                lang: self.lang,
                limit: Some(limit),
            }),
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShowInput {
    #[serde(default = "super::default_schema_version")]
    schema_version: u32,
    repository: PathBuf,
    selector: String,
    #[serde(default)]
    max_bytes: Option<usize>,
}

impl ShowInput {
    fn validate(self) -> Result<QueryRequest, ToolError> {
        validate_schema(self.schema_version)?;
        let max_bytes = self.max_bytes.unwrap_or(DEFAULT_SHOW_BYTES);
        if !(1..=MAX_SHOW_BYTES).contains(&max_bytes) {
            return Err(ToolError::invalid_request(
                "validate code-graph show",
                format!("max_bytes must be between 1 and {MAX_SHOW_BYTES}"),
            ));
        }
        Ok(QueryRequest {
            repository: self.repository,
            limit: DEFAULT_LIMIT,
            query: Query::Show {
                selector: selector("selector", &self.selector)?,
                max_bytes,
            },
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RefsInput {
    #[serde(default = "super::default_schema_version")]
    schema_version: u32,
    repository: PathBuf,
    selector: String,
    #[serde(default)]
    confidence: Option<ConfidenceInput>,
    #[serde(default)]
    kind: Option<RefKindInput>,
    #[serde(default)]
    limit: Option<usize>,
}

impl RefsInput {
    fn validate(self) -> Result<QueryRequest, ToolError> {
        validate_schema(self.schema_version)?;
        Ok(QueryRequest {
            repository: self.repository,
            limit: limit(self.limit, DEFAULT_LIMIT)?,
            query: Query::Refs {
                selector: selector("selector", &self.selector)?,
                opts: RefOpts {
                    confidence: confidence(self.confidence),
                    kind: self.kind.map(|kind| match kind {
                        RefKindInput::Call => RefKind::Call,
                        RefKindInput::Type => RefKind::Type,
                        RefKindInput::Use => RefKind::Use,
                        RefKindInput::TraitBound => RefKind::TraitBound,
                        RefKindInput::Impl => RefKind::Impl,
                        RefKindInput::Extends => RefKind::Extends,
                        RefKindInput::Implements => RefKind::Implements,
                    }),
                },
            },
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CalleesInput {
    #[serde(default = "super::default_schema_version")]
    schema_version: u32,
    repository: PathBuf,
    selector: String,
    #[serde(default)]
    limit: Option<usize>,
}

impl CalleesInput {
    fn validate(self) -> Result<QueryRequest, ToolError> {
        validate_schema(self.schema_version)?;
        Ok(QueryRequest {
            repository: self.repository,
            limit: limit(self.limit, DEFAULT_LIMIT)?,
            query: Query::Callees(selector("selector", &self.selector)?),
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImpactInput {
    #[serde(default = "super::default_schema_version")]
    schema_version: u32,
    repository: PathBuf,
    selector: String,
    #[serde(default)]
    depth: Option<u8>,
    #[serde(default)]
    direction: Option<DirectionInput>,
    #[serde(default)]
    confidence: Option<ConfidenceInput>,
    #[serde(default)]
    limit: Option<usize>,
}

impl ImpactInput {
    fn validate(self) -> Result<QueryRequest, ToolError> {
        validate_schema(self.schema_version)?;
        Ok(QueryRequest {
            repository: self.repository,
            limit: limit(self.limit, DEFAULT_LIMIT)?,
            query: Query::Impact {
                selector: selector("selector", &self.selector)?,
                depth: depth(self.depth, DEFAULT_IMPACT_DEPTH)?,
                confidence: confidence(self.confidence),
                direction: match self.direction {
                    None | Some(DirectionInput::Both) => ImpactDirection::Both,
                    Some(DirectionInput::Inbound) => ImpactDirection::Inbound,
                    Some(DirectionInput::Outbound) => ImpactDirection::Outbound,
                },
            },
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TraceInput {
    #[serde(default = "super::default_schema_version")]
    schema_version: u32,
    repository: PathBuf,
    command: String,
    #[serde(default)]
    depth: Option<u8>,
    #[serde(default)]
    confidence: Option<ConfidenceInput>,
}

impl TraceInput {
    fn validate(self) -> Result<QueryRequest, ToolError> {
        validate_schema(self.schema_version)?;
        let command = self.command.trim();
        let command = command
            .strip_prefix("command:")
            .map_or(command, str::trim)
            .to_string();
        if command.is_empty() {
            return Err(ToolError::invalid_request(
                "validate code-graph trace",
                "command must not be empty",
            ));
        }
        Ok(QueryRequest {
            repository: self.repository,
            limit: DEFAULT_LIMIT,
            query: Query::Trace {
                command,
                depth: depth(self.depth, DEFAULT_TRACE_DEPTH)?,
                confidence: confidence(self.confidence),
            },
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DepsInput {
    #[serde(default = "super::default_schema_version")]
    schema_version: u32,
    repository: PathBuf,
    selector: String,
    #[serde(default)]
    limit: Option<usize>,
}

impl DepsInput {
    fn validate(self) -> Result<QueryRequest, ToolError> {
        validate_schema(self.schema_version)?;
        Ok(QueryRequest {
            repository: self.repository,
            limit: limit(self.limit, DEFAULT_LIMIT)?,
            query: Query::Deps(selector("selector", &self.selector)?),
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OverviewInput {
    #[serde(default = "super::default_schema_version")]
    schema_version: u32,
    repository: PathBuf,
    #[serde(default)]
    selector: Option<String>,
    #[serde(default)]
    format: Option<FormatInput>,
    #[serde(default)]
    limit: Option<usize>,
}

impl OverviewInput {
    fn validate(self) -> Result<QueryRequest, ToolError> {
        validate_schema(self.schema_version)?;
        Ok(QueryRequest {
            repository: self.repository,
            limit: limit(self.limit, DEFAULT_LIMIT)?,
            query: Query::Overview {
                scope: self
                    .selector
                    .as_deref()
                    .map(|text| selector("selector", text))
                    .transpose()?,
                format: match self.format {
                    None | Some(FormatInput::Summary) => OverviewFormat::Summary,
                    Some(FormatInput::Full) => OverviewFormat::Full,
                },
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use std::path::PathBuf;

    use super::{MAX_RESULT_BYTES, Query, QueryRequest, QueryTool, bound, validate};
    use crate::{
        ImpactDirection, OverviewFormat, RefConfidence, RefKind, RefOpts, SearchKind, SearchQuery,
        Selector,
    };

    fn selector(text: &str) -> Selector {
        text.parse().expect("selector")
    }

    /// STD-01 R36: every property a request schema advertises reaches the
    /// library call. Each input sets every advertised property to a
    /// non-default value, and the test fails if a schema gains a property
    /// this table does not exercise.
    #[test]
    fn every_advertised_parameter_reaches_the_query() {
        let symbol = "symbol:src/lib.rs#entry:function";
        let cases = [
            (
                QueryTool::Search,
                json!({"query": "parse", "kind": "config", "lang": "rust", "limit": 7}),
                7,
                Query::Search(SearchQuery {
                    query: "parse".to_string(),
                    kind: Some(SearchKind::Config),
                    lang: Some("rust".to_string()),
                    limit: Some(7),
                }),
            ),
            (
                QueryTool::Show,
                json!({"selector": symbol, "max_bytes": 99}),
                super::DEFAULT_LIMIT,
                Query::Show {
                    selector: selector(symbol),
                    max_bytes: 99,
                },
            ),
            (
                QueryTool::Refs,
                json!({"selector": symbol, "confidence": "fuzzy_name", "kind": "trait_bound", "limit": 3}),
                3,
                Query::Refs {
                    selector: selector(symbol),
                    opts: RefOpts {
                        confidence: RefConfidence::FuzzyName,
                        kind: Some(RefKind::TraitBound),
                    },
                },
            ),
            (
                QueryTool::Callees,
                json!({"selector": symbol, "limit": 4}),
                4,
                Query::Callees(selector(symbol)),
            ),
            (
                QueryTool::Impact,
                json!({"selector": symbol, "depth": 9, "direction": "outbound", "confidence": "exact", "limit": 5}),
                5,
                Query::Impact {
                    selector: selector(symbol),
                    depth: 9,
                    confidence: RefConfidence::Exact,
                    direction: ImpactDirection::Outbound,
                },
            ),
            (
                QueryTool::Trace,
                json!({"command": "command: sync ", "depth": 8, "confidence": "import_resolved"}),
                super::DEFAULT_LIMIT,
                Query::Trace {
                    command: "sync".to_string(),
                    depth: 8,
                    confidence: RefConfidence::ImportResolved,
                },
            ),
            (
                QueryTool::Deps,
                json!({"selector": "dir:src", "limit": 6}),
                6,
                Query::Deps(selector("dir:src")),
            ),
            (
                QueryTool::Overview,
                json!({"selector": "dir:src", "format": "full", "limit": 2}),
                2,
                Query::Overview {
                    scope: Some(selector("dir:src")),
                    format: OverviewFormat::Full,
                },
            ),
        ];
        let schemas = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas");
        for (tool, mut input, limit, query) in cases {
            let schema: serde_json::Value = serde_json::from_slice(
                &std::fs::read(schemas.join(format!("{}.request.json", tool.verb())))
                    .expect("read request schema"),
            )
            .expect("parse request schema");
            let advertised = schema["properties"]
                .as_object()
                .expect("properties")
                .keys()
                .filter(|name| !["schema_version", "repository"].contains(&name.as_str()))
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            let exercised = input
                .as_object()
                .expect("input")
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(advertised, exercised, "{}", tool.verb());
            input["schema_version"] = json!(1);
            input["repository"] = json!("/work/repo");
            let request = validate(tool, input.to_string().as_bytes()).expect("valid input");
            assert_eq!(
                request,
                QueryRequest {
                    repository: PathBuf::from("/work/repo"),
                    limit,
                    query,
                },
                "{}",
                tool.verb()
            );
        }
    }

    #[test]
    fn tool_names_select_verbs_in_both_spellings() {
        for tool in QueryTool::ALL {
            let verb = tool.verb();
            assert_eq!(
                QueryTool::from_tool_name(&format!("orbit.graph.{verb}")),
                Some(tool)
            );
            assert_eq!(
                QueryTool::from_tool_name(&format!("graph.{verb}")),
                Some(tool)
            );
        }
        assert_eq!(QueryTool::from_tool_name("graph.recommend"), None);
        assert_eq!(QueryTool::from_tool_name("search"), None);
    }

    #[test]
    fn bound_caps_top_level_arrays_and_reports_the_cut() {
        let (result, truncation) = bound(
            json!({"refs": [1, 2, 3, 4], "relations": [1], "target": {"x": [1, 2, 3]}}),
            2,
        );
        assert_eq!(result["refs"], json!([1, 2]));
        assert_eq!(result["relations"], json!([1]));
        assert_eq!(result["target"]["x"], json!([1, 2, 3]));
        assert_eq!(
            serde_json::Value::Object(truncation),
            json!({"refs": {"returned": 2, "total": 4}})
        );
    }

    #[test]
    fn bound_tightens_caps_until_the_result_fits() {
        let big = "x".repeat(4096);
        let items = (0..200).map(|_| json!(big)).collect::<Vec<_>>();
        let (result, truncation) = bound(json!({"matches": items}), 200);
        let size = serde_json::to_vec(&result).expect("encode").len();
        assert!(size <= MAX_RESULT_BYTES, "{size}");
        assert_eq!(truncation["matches"]["total"], 200);
        assert!(
            truncation["matches"]["returned"]
                .as_u64()
                .expect("returned")
                < 200
        );
    }

    #[test]
    fn non_object_results_pass_through() {
        let (result, truncation) = bound(serde_json::Value::Null, 5);
        assert!(result.is_null());
        assert!(truncation.is_empty());
    }
}

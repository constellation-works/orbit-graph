use std::path::PathBuf;

use clap::{Args, ValueEnum};
use orbit_graph::RefConfidence;
use orbit_graph_changes::analysis::{
    AnalysisError, ChangesBounds, ChangesRange, ChangesRequest, DEFAULT_MAX_CALLERS,
    DEFAULT_MAX_ENTRY_POINTS, DEFAULT_MAX_SYMBOLS, DEFAULT_MAX_TESTS, DEFAULT_NODE_CAP, analyse,
};
use orbit_graph_changes::evidence::{DEFAULT_TIME_BUDGET_MS, EVIDENCE_DEPTH};
use orbit_graph_changes::filters::FilterSet;
use orbit_graph_changes::snapshot::{ComparisonOptions, SnapshotError};
use serde_json::{Value, json};

use super::{CliError, CommandContext, display_value, json_value};
use crate::output::{Column, CommandOutput, TableView, View, ViewBlock};

const CHANGES_AFTER_HELP: &str = "\
RANGE is <base>..<head> to compare two revisions, or <base> alone to compare
the working tree (staged, unstaged and untracked files; not ignored ones)
against <base>. Without RANGE the working tree is compared against the merge
base of HEAD and the branch it will land on: its upstream, else origin/HEAD,
else main, else master. Nothing is fetched.

Every caller, entry point and candidate test is labelled with its source
(call_path, import_relationship, reference_path, changed_symbol,
naming_heuristic, runtime_invocation), its evidence category and its
confidence. Candidate tests are what the evidence points at, not proof of
coverage.

Committed snapshots are cached under .orbit-graph/explorer/snapshots in the
repository (graph scratch state), so a second run over the same commits skips
indexing; pass --no-cache to write nothing there. The working tree is never
cached.

Examples:
  orbit-graph changes --json
  orbit-graph changes main..HEAD --json
  orbit-graph changes origin/main --max-tests 20
  orbit-graph changes v1.2.0..v1.3.0 --symbol 'symbol:src/lib.rs#run:function'";

#[derive(Debug, Args)]
#[command(after_help = CHANGES_AFTER_HELP)]
pub struct ChangesCommand {
    /// Revisions to compare: <base>..<head>, or <base> to compare the working
    /// tree against it (default: the working tree against the merge base with
    /// the upstream, origin/HEAD, main or master).
    #[arg(value_name = "RANGE")]
    range: Option<String>,
    /// Analyse only this changed symbol (symbol:<path>#<name>:<kind>);
    /// repeatable. A selector that matches no changed symbol is listed under
    /// unmatched_selection.
    #[arg(long = "symbol", value_name = "SELECTOR")]
    symbols: Vec<String>,
    /// Minimum resolution confidence of every hop (default: same_module).
    /// Pass fuzzy to include name-only matches.
    #[arg(long, value_enum, default_value_t = ConfidenceArg::SameModule)]
    confidence: ConfidenceArg,
    /// Maximum hops from a changed symbol to a caller or entry point (1..=10).
    #[arg(long, default_value_t = EVIDENCE_DEPTH)]
    depth: u8,
    /// Maximum nodes, and paths, one traversal may return (1..=2000).
    #[arg(long, default_value_t = DEFAULT_NODE_CAP)]
    node_cap: usize,
    /// Wall-clock budget of one traversal, in milliseconds (100..=60000).
    #[arg(long, default_value_t = DEFAULT_TIME_BUDGET_MS)]
    query_budget_ms: u64,
    /// Maximum changed symbols analysed, in changed-symbol order (1..=1000);
    /// the rest are listed under not_analysed.
    #[arg(long, default_value_t = DEFAULT_MAX_SYMBOLS)]
    max_symbols: usize,
    /// Maximum callers kept per changed symbol, strongest first (1..=500).
    #[arg(long, default_value_t = DEFAULT_MAX_CALLERS)]
    max_callers: usize,
    /// Maximum entry points kept per changed symbol, nearest first (1..=100).
    #[arg(long, default_value_t = DEFAULT_MAX_ENTRY_POINTS)]
    max_entry_points: usize,
    /// Maximum candidate tests kept per changed symbol, strongest first
    /// (1..=500).
    #[arg(long, default_value_t = DEFAULT_MAX_TESTS)]
    max_tests: usize,
    /// Wall-clock budget for the whole run, indexing included, in
    /// milliseconds (1000..=3600000; default: none). When it runs out the
    /// result is returned with complete: false.
    #[arg(long, value_name = "MS")]
    budget_ms: Option<u64>,
    /// Keep only changed symbols whose file is in this language.
    #[arg(long, value_name = "LANGUAGE")]
    language: Option<String>,
    /// Keep only changed symbols whose path starts with this prefix.
    #[arg(long, value_name = "PREFIX")]
    scope: Option<String>,
    /// Snapshot cache directory (default:
    /// <repository>/.orbit-graph/explorer/snapshots).
    #[arg(long, value_name = "DIR", conflicts_with = "no_cache")]
    cache_dir: Option<PathBuf>,
    /// Build snapshots in temporary directories and cache nothing.
    #[arg(long)]
    no_cache: bool,
}

impl ChangesCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<Value, CliError> {
        let range = match &self.range {
            Some(range) => ChangesRange::parse(range)?,
            None => ChangesRange::WorkingTree { base: None },
        };
        let request = ChangesRequest {
            repository: context.worktree_root().to_path_buf(),
            range,
            selection: self.symbols.clone(),
            filters: FilterSet {
                language: self.language.clone(),
                scope: self.scope.clone(),
                ..FilterSet::default()
            },
            min_confidence: self.confidence.into_graph(),
            bounds: ChangesBounds {
                depth: self.depth,
                node_cap: self.node_cap,
                query_budget_ms: self.query_budget_ms,
                max_symbols: self.max_symbols,
                max_callers: self.max_callers,
                max_entry_points: self.max_entry_points,
                max_tests: self.max_tests,
                budget_ms: self.budget_ms,
            },
            cache: ComparisonOptions {
                cache_dir: self.cache_dir.clone(),
                no_cache: self.no_cache,
                ..ComparisonOptions::default()
            },
        };
        json_value(analyse(&request)?)
    }
}

/// Stable error code of a change-analysis failure.
pub(crate) fn error_code(error: &AnalysisError) -> &'static str {
    match error {
        AnalysisError::InvalidRange { .. }
        | AnalysisError::InvalidBound { .. }
        | AnalysisError::InvalidSelection { .. } => "argument_error",
        AnalysisError::Snapshot(SnapshotError::Revision { .. }) => "revision_not_found",
        AnalysisError::Snapshot(SnapshotError::NoDefaultBase { .. }) => "no_default_base",
        AnalysisError::Snapshot(SnapshotError::Repository { .. }) => "repository_unavailable",
        // `AnalysisError` and `SnapshotError` are `#[non_exhaustive]`: every
        // other failure is a change-analysis failure.
        _ => "changes_error",
    }
}

/// Whether a change-analysis failure is a usage error (exit code 2).
pub(crate) fn is_usage_error(error: &AnalysisError) -> bool {
    error_code(error) == "argument_error"
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let symbols = document["symbols"].as_array().cloned().unwrap_or_default();
    let tests = document["tests"].as_array().cloned().unwrap_or_default();
    let comparison = format!(
        "{}..{}",
        display_value(&document["comparison"]["base"]["requested_ref"]),
        display_value(&document["comparison"]["head"]["requested_ref"])
    );

    let mut symbol_table = TableView::new(vec![
        Column::fixed("status"),
        Column::number("callers"),
        Column::number("entry_points"),
        Column::number("tests"),
        Column::text("symbol"),
    ]);
    for symbol in &symbols {
        symbol_table.push_row([
            display_value(&symbol["status"]),
            found_of(symbol, "callers"),
            found_of(symbol, "entry_points"),
            found_of(symbol, "candidate_tests"),
            display_value(&symbol["selector"]),
        ]);
    }
    let mut test_table = TableView::new(vec![
        Column::fixed("source"),
        Column::fixed("confidence"),
        Column::number("symbols"),
        Column::path("test"),
    ]);
    for test in &tests {
        test_table.push_row([
            display_value(&test["source"]),
            display_value(&test["confidence"]),
            test["changed_symbols"]
                .as_array()
                .map_or(0, Vec::len)
                .to_string(),
            display_value(&test["test"]["label"]),
        ]);
    }

    let mut notices = Vec::new();
    if let Some(message) = document["incomplete"]["message"].as_str() {
        notices.push(format!("incomplete: {message}"));
    }
    if document["truncated"].as_bool() == Some(true) {
        let flags = document["truncation"].as_array().map_or(0, Vec::len);
        notices.push(format!(
            "{flags} bound(s) cut this result; see truncation in --json output, or raise the \
             named --max-* bound"
        ));
    }
    if let Some(unmatched) = document["unmatched_selection"].as_array()
        && !unmatched.is_empty()
    {
        notices.push(format!(
            "{} --symbol selector(s) matched no changed symbol",
            unmatched.len()
        ));
    }
    for notice in document["notices"].as_array().into_iter().flatten() {
        if let Some(notice) = notice.as_str() {
            notices.push(notice.to_owned());
        }
    }

    let mut context = document.clone();
    if let Some(object) = context.as_object_mut() {
        object.remove("symbols");
        object.remove("tests");
    }
    let mut records = vec![json!({"record_type": "changes_context", "context": context})];
    records.extend(
        symbols
            .into_iter()
            .map(|symbol| json!({"record_type": "changed_symbol", "symbol": symbol})),
    );
    records.extend(
        tests
            .into_iter()
            .map(|test| json!({"record_type": "candidate_test", "test": test})),
    );

    let mut output = CommandOutput::with_view(
        document,
        View::Blocks(vec![
            ViewBlock::table(
                symbol_table
                    .with_plain_record_type("changed_symbol")
                    .with_empty_message(format!(
                        "no changed symbols were analysed for {comparison:?}"
                    )),
            ),
            ViewBlock::text(""),
            ViewBlock::table(
                test_table
                    .with_plain_record_type("candidate_test")
                    .with_empty_message(format!(
                        "no candidate tests were found for {comparison:?}"
                    )),
            ),
        ]),
    )
    .with_ndjson_records(records);
    for notice in notices {
        output = output.with_notice(notice);
    }
    output
}

/// `kept` or `kept/found` for one per-symbol list.
fn found_of(symbol: &Value, list: &str) -> String {
    let kept = symbol[list].as_array().map_or(0, Vec::len);
    let found = symbol[format!("{list}_found")].as_u64().unwrap_or(0);
    if found as usize > kept {
        format!("{kept}/{found}")
    } else {
        kept.to_string()
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
#[clap(rename_all = "snake_case")]
enum ConfidenceArg {
    Exact,
    #[value(alias = "import_resolved")]
    Import,
    SameModule,
    #[value(alias = "fuzzy_name")]
    Fuzzy,
}

impl ConfidenceArg {
    fn into_graph(self) -> RefConfidence {
        match self {
            Self::Exact => RefConfidence::Exact,
            Self::Import => RefConfidence::ImportResolved,
            Self::SameModule => RefConfidence::SameModule,
            Self::Fuzzy => RefConfidence::FuzzyName,
        }
    }
}

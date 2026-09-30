use clap::{Args, ValueEnum};
use orbit_graph::{SearchKind, SearchQuery};
use serde_json::Value;

use super::{CliError, CommandContext, json_value, truncation_notice};
use crate::output::{Column, CommandOutput, TableView, View, ViewBlock};

#[derive(Debug, Args)]
pub struct SearchCommand {
    /// Text to search for; must not be empty.
    #[arg(value_parser = parse_query)]
    query: String,
    /// Filter by result kind (default: all kinds).
    #[arg(long, value_enum)]
    kind: Option<SearchKindArg>,
    /// Filter by indexed language token, such as rust or python (default: all languages).
    #[arg(long)]
    lang: Option<String>,
    /// Maximum matches, at least 1 (default: 20).
    #[arg(long, value_parser = parse_limit)]
    limit: Option<usize>,
}

/// Reject an empty or whitespace-only query before any work (STD-01 §R29).
fn parse_query(raw: &str) -> Result<String, String> {
    if raw.trim().is_empty() {
        return Err("the search query must not be empty".to_owned());
    }
    Ok(raw.to_owned())
}

/// Reject a zero limit, which could only ever return nothing (STD-01 §R29).
fn parse_limit(raw: &str) -> Result<usize, String> {
    match raw.parse::<usize>() {
        Ok(0) => Err("--limit must be at least 1".to_owned()),
        Ok(limit) => Ok(limit),
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let matches = document["matches"].as_array().cloned().unwrap_or_default();
    let mut table = TableView::new(vec![
        Column::fixed("kind"),
        Column::text("match"),
        Column::path("path"),
        Column::number("line"),
    ]);
    for item in &matches {
        table.push_row([
            super::display_value(&item["kind"]),
            item.get("name")
                .or_else(|| item.get("value"))
                .map_or_else(|| "-".to_owned(), super::display_value),
            super::display_value(&item["path"]),
            super::display_value(&item["line"]),
        ]);
    }
    let notice = truncation_notice(
        &document,
        matches.len(),
        "search matches",
        "raise --limit to see more",
    );
    let mut output = CommandOutput::with_view(
        document,
        View::Blocks(vec![ViewBlock::table(
            table.with_empty_message("no search matches"),
        )]),
    )
    .with_ndjson_records(matches);
    if let Some(notice) = notice {
        output = output.with_notice(notice);
    }
    output
}

impl SearchCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let graph = context.open_graph()?;
        let query = SearchQuery {
            query: self.query.clone(),
            kind: self.kind.map(SearchKindArg::into_graph),
            lang: self.lang.clone(),
            limit: self.limit,
        };
        json_value(graph.search(&query)?)
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SearchKindArg {
    Symbol,
    String,
    Config,
}

impl SearchKindArg {
    fn into_graph(self) -> SearchKind {
        match self {
            Self::Symbol => SearchKind::Symbol,
            Self::String => SearchKind::String,
            Self::Config => SearchKind::Config,
        }
    }
}

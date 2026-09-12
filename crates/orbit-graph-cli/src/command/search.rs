use clap::{Args, ValueEnum};
use orbit_graph::{SearchKind, SearchQuery};
use serde_json::Value;

use super::{CliError, CommandContext, json_value};
use crate::output::{Column, CommandOutput, TableView, View, ViewBlock};

#[derive(Debug, Args)]
pub struct SearchCommand {
    query: String,
    #[arg(long, value_enum)]
    kind: Option<SearchKindArg>,
    #[arg(long)]
    lang: Option<String>,
    #[arg(long)]
    limit: Option<usize>,
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
    CommandOutput::with_view(
        document,
        View::Blocks(vec![ViewBlock::table(
            table.with_empty_message("no search matches"),
        )]),
    )
    .with_ndjson_records(matches)
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

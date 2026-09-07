use crate::OverviewFormat;
use crate::Selector;
use clap::{Args, ValueEnum};
use serde_json::{Value, json};

use super::output::{Column, CommandOutput, TableView, View, ViewBlock};
use super::{CliError, CommandContext, json_value};

#[derive(Debug, Args)]
pub struct OverviewCommand {
    /// Optional `dir:…` or `file:…` selector scoping the summary
    /// (default: whole worktree).
    scope: Option<String>,
    /// Output detail. `summary` returns counts plus the highest-symbol files;
    /// `full` lists every in-scope file with its symbols.
    #[arg(long, value_enum, default_value_t = FormatArg::Summary)]
    format: FormatArg,
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let mut summary = TableView::new(vec![
        Column::number("files"),
        Column::number("symbols"),
        Column::fixed("detail"),
        Column::path("scope"),
    ]);
    summary.push_row([
        super::display_value(&document["total_files"]),
        super::display_value(&document["total_symbols"]),
        super::display_value(&document["format"]),
        super::display_value(&document["scope"]),
    ]);

    let mut languages = TableView::new(vec![Column::fixed("language"), Column::number("files")]);
    if let Some(entries) = document["languages"].as_object() {
        for (language, count) in entries {
            languages.push_row([language.clone(), super::display_value(count)]);
        }
    }
    let mut kinds = TableView::new(vec![Column::fixed("symbol kind"), Column::number("count")]);
    if let Some(entries) = document["symbol_kinds"].as_object() {
        for (kind, count) in entries {
            kinds.push_row([kind.clone(), super::display_value(count)]);
        }
    }

    let files = document["files"].as_array().cloned().unwrap_or_default();
    let mut file_table = TableView::new(vec![
        Column::path("file"),
        Column::fixed("language"),
        Column::number("symbols"),
    ]);
    let mut symbol_table = TableView::new(vec![
        Column::path("file"),
        Column::fixed("kind"),
        Column::text("name"),
        Column::text("qualified"),
    ]);
    for file in &files {
        file_table.push_row([
            super::display_value(&file["path"]),
            super::display_value(&file["lang"]),
            super::display_value(&file["symbol_count"]),
        ]);
        if let Some(symbols) = file["symbols"].as_array() {
            for symbol in symbols {
                symbol_table.push_row([
                    super::display_value(&file["path"]),
                    super::display_value(&symbol["kind"]),
                    super::display_value(&symbol["name"]),
                    super::display_value(&symbol["qualified"]),
                ]);
            }
        }
    }
    let mut blocks = vec![
        ViewBlock::table(summary),
        ViewBlock::table(languages.with_empty_message("no indexed languages in scope")),
        ViewBlock::table(kinds.with_empty_message("no indexed symbol kinds in scope")),
        ViewBlock::table(file_table.with_empty_message("no indexed files in scope")),
    ];
    if document["format"] == "full" {
        blocks.push(ViewBlock::table(
            symbol_table.with_empty_message("no indexed symbols in scope"),
        ));
    }

    let mut context = document.clone();
    if let Some(object) = context.as_object_mut() {
        object.remove("files");
    }
    let records = std::iter::once(json!({
        "record_type": "overview_context",
        "context": context
    }))
    .chain(
        files
            .into_iter()
            .map(|file| json!({"record_type": "overview_file", "file": file})),
    )
    .collect();
    CommandOutput::with_view(document, View::Blocks(blocks)).with_ndjson_records(records)
}

impl OverviewCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let graph = context.open_graph()?;
        let scope = self
            .scope
            .as_deref()
            .map(str::parse::<Selector>)
            .transpose()?;
        json_value(graph.overview(scope.as_ref(), self.format.into_graph())?)
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
#[clap(rename_all = "snake_case")]
enum FormatArg {
    Summary,
    Full,
}

impl FormatArg {
    fn into_graph(self) -> OverviewFormat {
        match self {
            Self::Summary => OverviewFormat::Summary,
            Self::Full => OverviewFormat::Full,
        }
    }
}

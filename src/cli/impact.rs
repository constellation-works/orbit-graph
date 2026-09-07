use crate::Selector;
use crate::{DEFAULT_IMPACT_DEPTH, RefConfidence};
use clap::{Args, ValueEnum};
use serde_json::{Value, json};

use super::output::{Column, CommandOutput, TableView, View, ViewBlock};
use super::{CliError, CommandContext, json_value};

#[derive(Debug, Args)]
pub struct ImpactCommand {
    selector: String,
    #[arg(long, default_value_t = DEFAULT_IMPACT_DEPTH)]
    depth: u8,
    /// Minimum resolution confidence floor (default: same_module).
    ///
    /// The default traverses precise edges only. Cross-crate edges routed
    /// through `pub use` re-exports resolve at `fuzzy_name`; pass
    /// `--confidence fuzzy` to include them in the blast radius.
    #[arg(long, value_enum, default_value_t = ConfidenceArg::SameModule)]
    confidence: ConfidenceArg,
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let touched = document["touched"].as_array().cloned().unwrap_or_default();
    let fallback_touched = document["fallback"]["touched"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut table = TableView::new(vec![
        Column::fixed("set"),
        Column::number("distance"),
        Column::fixed("edge"),
        Column::text("symbol"),
    ]);
    for entry in &touched {
        push_impact_row(&mut table, "primary", entry);
    }
    for entry in &fallback_touched {
        push_impact_row(&mut table, "fallback", entry);
    }

    let mut context = document.clone();
    let mut fallback_context = None;
    if let Some(object) = context.as_object_mut() {
        object.remove("touched");
        fallback_context = object.remove("fallback").and_then(|mut fallback| {
            fallback.as_object_mut()?.remove("touched");
            Some(fallback)
        });
    }
    let mut records = vec![json!({"record_type": "impact_context", "context": context})];
    records.extend(
        touched
            .into_iter()
            .map(|entry| json!({"record_type": "impact", "impact": entry})),
    );
    if let Some(context) = fallback_context {
        records.push(json!({"record_type": "impact_fallback_context", "context": context}));
    }
    records.extend(
        fallback_touched
            .into_iter()
            .map(|entry| json!({"record_type": "fallback_impact", "impact": entry})),
    );
    CommandOutput::with_view(
        document,
        View::Blocks(vec![ViewBlock::table(table.with_empty_message(
            "selector has no downstream impact at this confidence",
        ))]),
    )
    .with_ndjson_records(records)
}

fn push_impact_row(table: &mut TableView, set: &str, entry: &Value) {
    table.push_row([
        set.to_owned(),
        super::display_value(&entry["distance"]),
        super::display_value(&entry["edge_kind"]),
        super::display_value(&entry["qualified_name"]),
    ]);
}

impl ImpactCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let graph = context.open_graph()?;
        let selector = self.selector.parse::<Selector>()?;
        json_value(graph.impact(&selector, self.depth, self.confidence.into_graph())?)
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

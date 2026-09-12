use clap::{Args, ValueEnum};
use orbit_graph::Selector;
use orbit_graph::{RefConfidence, RefKind, RefOpts};
use serde_json::{Value, json};

use super::{CliError, CommandContext, json_value};
use crate::output::{Column, CommandOutput, TableView, View, ViewBlock};

#[derive(Debug, Args)]
pub struct RefsCommand {
    symbol: String,
    /// Minimum resolution confidence floor (default: same_module).
    ///
    /// The default keeps precise results. If the precise floor finds no
    /// references, the result auto-includes lower-confidence `fuzzy_name`
    /// (name-only) matches under a `fallback` field — so cross-crate callers
    /// routed through `pub use` re-exports are not silently hidden. Pass
    /// `--confidence fuzzy` to query those matches directly in `refs`.
    #[arg(long, value_enum, default_value_t = ConfidenceArg::SameModule)]
    confidence: ConfidenceArg,
    #[arg(long, value_enum)]
    kind: Option<RefKindArg>,
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let target = document["target"]
        .get("qualified")
        .filter(|value| !value.is_null())
        .map_or_else(
            || super::display_value(&document["target"]["name"]),
            super::display_value,
        );
    let refs = document["refs"].as_array().cloned().unwrap_or_default();
    let relations = document["relations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let fallback_refs = document["fallback"]["refs"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut table = TableView::new(vec![
        Column::fixed("record"),
        Column::text("from"),
        Column::path("file"),
        Column::number("line"),
        Column::fixed("kind"),
        Column::fixed("confidence"),
        Column::text("target"),
    ]);
    for entry in &refs {
        push_ref_row(&mut table, "reference", entry, "-", target.as_str());
    }
    for entry in &relations {
        push_ref_row(
            &mut table,
            "relation",
            entry,
            super::display_value(&entry["from"]).as_str(),
            target.as_str(),
        );
    }
    for entry in &fallback_refs {
        push_ref_row(&mut table, "fallback", entry, "-", target.as_str());
    }

    let mut context = document.clone();
    let mut fallback_context = None;
    if let Some(object) = context.as_object_mut() {
        object.remove("refs");
        object.remove("relations");
        fallback_context = object.remove("fallback").and_then(|mut fallback| {
            fallback.as_object_mut()?.remove("refs");
            Some(fallback)
        });
    }
    let mut records = vec![json!({"record_type": "refs_context", "context": context})];
    records.extend(
        refs.into_iter()
            .map(|entry| json!({"record_type": "reference", "reference": entry})),
    );
    records.extend(
        relations
            .into_iter()
            .map(|entry| json!({"record_type": "relation", "relation": entry})),
    );
    if let Some(context) = fallback_context {
        records.push(json!({"record_type": "refs_fallback_context", "context": context}));
    }
    records.extend(
        fallback_refs
            .into_iter()
            .map(|entry| json!({"record_type": "fallback_reference", "reference": entry})),
    );
    CommandOutput::with_view(
        document,
        View::Blocks(vec![ViewBlock::table(table.with_empty_message(format!(
            "no references or relations found for {target}"
        )))]),
    )
    .with_ndjson_records(records)
}

fn push_ref_row(table: &mut TableView, record: &str, entry: &Value, from: &str, target: &str) {
    table.push_row([
        record.to_owned(),
        from.to_owned(),
        super::display_value(&entry["file"]),
        super::display_value(&entry["line"]),
        super::display_value(&entry["kind"]),
        super::display_value(&entry["confidence"]),
        target.to_owned(),
    ]);
}

impl RefsCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let graph = context.open_graph()?;
        let selector = self.symbol.parse::<Selector>()?;
        let opts = RefOpts {
            confidence: self.confidence.into_graph(),
            kind: self.kind.map(RefKindArg::into_graph),
        };
        json_value(graph.refs(&selector, &opts)?)
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

#[derive(Debug, Clone, Copy, ValueEnum)]
#[clap(rename_all = "snake_case")]
enum RefKindArg {
    Call,
    Type,
    Use,
    TraitBound,
    Impl,
    Extends,
    Implements,
}

impl RefKindArg {
    fn into_graph(self) -> RefKind {
        match self {
            Self::Call => RefKind::Call,
            Self::Type => RefKind::Type,
            Self::Use => RefKind::Use,
            Self::TraitBound => RefKind::TraitBound,
            Self::Impl => RefKind::Impl,
            Self::Extends => RefKind::Extends,
            Self::Implements => RefKind::Implements,
        }
    }
}

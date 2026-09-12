use std::fs;
use std::path::PathBuf;

use clap::Args;
use serde_json::{Value, json};

use orbit_graph::{EvaluationCorpus, GraphError, evaluate_corpus};

use super::{CliError, CommandContext, display_value, json_value};
use crate::output::{Column, CommandOutput, TableView, View, ViewBlock};

#[derive(Debug, Args)]
pub struct EvaluateCommand {
    /// Versioned chronological evaluation corpus JSON.
    #[arg(long)]
    input: PathBuf,
}

impl EvaluateCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let bytes = fs::read(self.input.as_path()).map_err(|source| {
            CliError::Graph(GraphError::io(
                "read evaluation corpus",
                self.input.as_path(),
                source,
            ))
        })?;
        let corpus: EvaluationCorpus =
            serde_json::from_slice(bytes.as_slice()).map_err(|error| {
                CliError::Graph(GraphError::invalid_data(
                    "decode evaluation corpus",
                    error.to_string(),
                ))
            })?;
        json_value(evaluate_corpus(context.worktree_root(), &corpus)?)
    }
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let coverage = &document["coverage"];
    let mut coverage_table = TableView::new(vec![
        Column::number("cases"),
        Column::number("evaluated"),
        Column::number("excluded"),
        Column::number("training inserted"),
        Column::number("training supplied"),
        Column::text("isolated indexes"),
        Column::text("source complete"),
        Column::text("coverage note"),
    ]);
    coverage_table.push_row([
        display_value(&coverage["cases_total"]),
        display_value(&coverage["cases_evaluated"]),
        display_value(&coverage["cases_excluded"]),
        display_value(&coverage["training_deliveries_inserted"]),
        display_value(&coverage["training_deliveries_supplied"]),
        display_value(&coverage["isolated_indexes"]),
        display_value(&coverage["source_complete"]),
        display_value(&coverage["note"]),
    ]);

    let mut metrics_table = TableView::new(vec![
        Column::text("variant"),
        Column::text("level"),
        Column::number("k"),
        Column::number("cases"),
        Column::number("recall"),
        Column::number("precision"),
        Column::number("stale rate"),
        Column::number("mean (ms)"),
    ]);
    if let Some(metrics) = document["metrics"].as_array() {
        for metric in metrics {
            metrics_table.push_row([
                display_value(&metric["variant"]),
                display_value(&metric["level"]),
                display_value(&metric["k"]),
                display_value(&metric["cases"]),
                decimal(&metric["recall_at_k"]),
                decimal(&metric["precision_at_k"]),
                decimal(&metric["stale_result_rate"]),
                decimal(&metric["mean_latency_ms"]),
            ]);
        }
    }

    let mut cases_table = TableView::new(vec![
        Column::text("case"),
        Column::text("evaluated"),
        Column::text("exclusions"),
    ]);
    if let Some(cases) = document["cases"].as_array() {
        for case in cases {
            let exclusions = case["exclusions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(display_value)
                .collect::<Vec<_>>();
            cases_table.push_row([
                display_value(&case["id"]),
                display_value(&case["evaluated"]),
                if exclusions.is_empty() {
                    "-".to_owned()
                } else {
                    exclusions.join(", ")
                },
            ]);
        }
    }

    let view = View::Blocks(vec![
        ViewBlock::table(coverage_table),
        ViewBlock::table(metrics_table.with_empty_message("no evaluation metrics")),
        ViewBlock::table(cases_table.with_empty_message("no evaluation cases")),
    ]);
    let mut context = document.clone();
    let (metrics, cases) = context.as_object_mut().map_or_else(
        || (Vec::new(), Vec::new()),
        |object| {
            let metrics = object
                .remove("metrics")
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default();
            let cases = object
                .remove("cases")
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default();
            (metrics, cases)
        },
    );
    let mut records = vec![json!({"record_type": "evaluation_context", "context": context})];
    records.extend(
        metrics
            .into_iter()
            .map(|metric| json!({"record_type": "evaluation_metric", "metric": metric})),
    );
    records.extend(
        cases
            .into_iter()
            .map(|case| json!({"record_type": "evaluation_case", "case": case})),
    );
    CommandOutput::with_view(document, view).with_ndjson_records(records)
}

fn decimal(value: &Value) -> String {
    value
        .as_f64()
        .map_or_else(|| "-".to_owned(), |value| format!("{value:.3}"))
}

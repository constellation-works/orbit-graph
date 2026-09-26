use std::fs;
use std::path::PathBuf;

use clap::Args;
use serde_json::{Value, json};

use orbit_graph::{
    EvaluationCorpus, GraphError, LiveGitEvaluation, evaluate_corpus, evaluate_live_git,
};

use super::{CliError, CommandContext, display_value, json_value};
use crate::output::{Column, CommandOutput, TableView, View, ViewBlock};

#[derive(Debug, Args)]
pub struct EvaluateCommand {
    /// Versioned chronological evaluation corpus JSON.
    ///
    /// Required unless `--live` is set.
    #[arg(long, required_unless_present = "live", conflicts_with = "live")]
    input: Option<PathBuf>,
    /// Hold out first-parent commits and score Git-only commit-text relevance.
    ///
    /// Queries each commit's subject at that commit's first parent, with no
    /// cutoff, and checks that the commit and later indexed deliveries
    /// contribute no evidence. Requires a history index from `history sync`.
    #[arg(long)]
    live: bool,
    /// Landing branch whose history index `--live` reads. Defaults to `main`.
    #[arg(long, requires = "live")]
    branch: Option<String>,
    /// Maximum held-out commits, newest first. Defaults to 300.
    #[arg(long, requires = "live")]
    limit: Option<usize>,
    /// Revision that starts the first-parent walk. Defaults to the branch tip.
    #[arg(long, requires = "live")]
    revision: Option<String>,
    /// Precision, recall, and MRR cutoff. Defaults to 10.
    #[arg(long, requires = "live")]
    k: Option<usize>,
}

impl EvaluateCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        if self.live {
            let report = evaluate_live_git(
                context.worktree_root(),
                &LiveGitEvaluation {
                    branch: self.branch.clone().unwrap_or_else(|| "main".to_string()),
                    revision: self.revision.clone(),
                    limit: self.limit.unwrap_or(300),
                    k: self.k.unwrap_or(10),
                },
            )?;
            return json_value(report);
        }
        let Some(input) = self.input.as_ref() else {
            return Err(CliError::Graph(GraphError::invalid_data(
                "validate evaluate arguments",
                "--input is required unless --live is set",
            )));
        };
        let bytes = fs::read(input.as_path()).map_err(|source| {
            CliError::Graph(GraphError::io(
                "read evaluation corpus",
                input.as_path(),
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
    if document["mode"] == "live_git" {
        return live_output(document);
    }
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

fn live_output(document: Value) -> CommandOutput {
    let mut context_table = TableView::new(vec![Column::text("field"), Column::text("value")]);
    for (field, value) in [
        ("branch", &document["landing_branch"]),
        ("tip", &document["resolved_tip"]),
        ("oldest", &document["oldest_held_out"]),
        ("newest", &document["newest_held_out"]),
        ("stop", &document["stop_reason"]),
        ("cases", &document["cases_evaluated"]),
        ("leakage", &document["leakage_violations"]),
        ("load start", &document["loadavg_start"]),
        ("load end", &document["loadavg_end"]),
        ("deliveries", &document["history_deliveries"]),
        ("k", &document["k"]),
        ("limit", &document["limit_requested"]),
        ("weight", &document["production_weight"]),
        ("exponent", &document["production_exponent"]),
    ] {
        context_table.push_row([field.to_owned(), display_value(value)]);
    }

    let mut metrics_table = TableView::new(vec![
        Column::text("cohort"),
        Column::text("variant"),
        Column::number("cases"),
        Column::number("recall"),
        Column::number("precision"),
        Column::number("mrr"),
        Column::number("mean (ms)"),
    ]);
    if let Some(cohorts) = document["cohorts"].as_array() {
        for cohort in cohorts {
            if let Some(metrics) = cohort["metrics"].as_array() {
                for metric in metrics {
                    metrics_table.push_row([
                        display_value(&metric["cohort"]),
                        display_value(&metric["name"]),
                        display_value(&metric["cases"]),
                        decimal(&metric["recall_at_k"]),
                        decimal(&metric["precision_at_k"]),
                        decimal(&metric["mrr_at_k"]),
                        decimal(&metric["mean_latency_ms"]),
                    ]);
                }
            }
        }
    }

    let mut cases_table = TableView::new(vec![
        Column::text("commit"),
        Column::text("parent"),
        Column::text("title restating"),
        Column::number("truth"),
        Column::number("leakage"),
        Column::text("subject"),
    ]);
    if let Some(cases) = document["cases"].as_array() {
        for case in cases {
            let truth = case["truth"]
                .as_array()
                .map(|items| items.len())
                .unwrap_or(0);
            let leakage = case["leakage"]
                .as_array()
                .map(|items| items.len())
                .unwrap_or(0);
            cases_table.push_row([
                display_value(&case["commit"]),
                display_value(&case["parent"]),
                display_value(&case["title_restating"]),
                truth.to_string(),
                leakage.to_string(),
                display_value(&case["subject"]),
            ]);
        }
    }

    let view = View::Blocks(vec![
        ViewBlock::table(context_table),
        ViewBlock::table(metrics_table.with_empty_message("no live evaluation metrics")),
        ViewBlock::table(cases_table.with_empty_message("no held-out commits")),
    ]);
    let mut context = document.clone();
    let (cohorts, cases) = context.as_object_mut().map_or_else(
        || (Vec::new(), Vec::new()),
        |object| {
            let cohorts = object
                .remove("cohorts")
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default();
            let cases = object
                .remove("cases")
                .and_then(|value| value.as_array().cloned())
                .unwrap_or_default();
            (cohorts, cases)
        },
    );
    let mut records = vec![json!({"record_type": "live_git_context", "context": context})];
    for cohort in cohorts {
        if let Some(metrics) = cohort["metrics"].as_array() {
            records.extend(
                metrics
                    .iter()
                    .map(|metric| json!({"record_type": "live_git_metric", "metric": metric})),
            );
        }
    }
    records.extend(
        cases
            .into_iter()
            .map(|case| json!({"record_type": "live_git_case", "case": case})),
    );
    CommandOutput::with_view(document, view).with_ndjson_records(records)
}

/// A metric cell: three decimals, or `n/a` when the report says there was no
/// data (`null`), which is not the same as a measured 0 (STD-02 §R29).
fn decimal(value: &Value) -> String {
    match value {
        Value::Null => "n/a".to_owned(),
        value => value
            .as_f64()
            .map_or_else(|| "-".to_owned(), |value| format!("{value:.3}")),
    }
}

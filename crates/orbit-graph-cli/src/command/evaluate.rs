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
            return Err(CliError::Usage(
                "--input is required unless --live is set".to_string(),
            ));
        };
        let bytes = fs::read(input.as_path()).map_err(|source| {
            CliError::Graph(GraphError::io(
                "read evaluation corpus",
                input.as_path(),
                source,
            ))
        })?;
        let corpus: EvaluationCorpus =
            serde_json::from_slice(bytes.as_slice()).map_err(|error| CliError::Input {
                operation: "decode evaluation corpus",
                reason: error.to_string(),
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

    let metrics = metrics_table(&document["metrics"]);

    let mut folds_table = TableView::new(vec![
        Column::text("fold"),
        Column::text("role"),
        Column::text("cutoff"),
        Column::text("revision"),
        Column::number("cases"),
        Column::number("bm25 files"),
        Column::number("bm25 symbols"),
    ]);
    for fold in document["folds"].as_array().into_iter().flatten() {
        folds_table.push_row([
            display_value(&fold["id"]),
            display_value(&fold["role"]),
            display_value(&fold["cutoff"]),
            display_value(&fold["revision"]),
            display_value(&fold["cases_evaluated"]),
            display_value(&fold["bm25_file_documents"]),
            display_value(&fold["bm25_symbol_documents"]),
        ]);
    }

    let mut paired_table = TableView::new(vec![
        Column::text("word_index vs"),
        Column::text("level"),
        Column::text("metric"),
        Column::number("cases"),
        Column::number("difference"),
        Column::number("low"),
        Column::number("high"),
        Column::text("verdict"),
    ]);
    for row in document["paired_differences"]
        .as_array()
        .into_iter()
        .flatten()
    {
        paired_table.push_row([
            display_value(&row["baseline"]),
            display_value(&row["level"]),
            display_value(&row["metric"]),
            display_value(&row["cases"]),
            decimal(&row["mean_difference"]),
            decimal(&row["interval_low"]),
            decimal(&row["interval_high"]),
            display_value(&row["verdict"]),
        ]);
    }

    let mut gates_table = TableView::new(vec![
        Column::text("gate"),
        Column::text("status"),
        Column::text("scope"),
        Column::number("cases"),
        Column::text("keep"),
        Column::text("alternative"),
        Column::text("reason"),
    ]);
    let gates = &document["gates"];
    for gate in gates["results"].as_array().into_iter().flatten() {
        gates_table.push_row([
            display_value(&gate["gate"]),
            display_value(&gate["status"]),
            display_value(&gates["scope"]),
            display_value(&gates["cases"]),
            display_value(&gate["keep"]),
            display_value(&gate["alternative"]),
            display_value(&gate["reason"]),
        ]);
    }

    let mut sweep_table = TableView::new(vec![
        Column::number("cell"),
        Column::text("selected"),
        Column::text("options"),
        Column::number("selection file recall"),
        Column::number("selection symbol recall"),
        Column::number("headline file recall"),
        Column::number("headline symbol recall"),
    ]);
    let sweep = &document["sweep"];
    for cell in sweep["cells"].as_array().into_iter().flatten() {
        sweep_table.push_row([
            display_value(&cell["cell"]),
            if sweep["selected_cell"] == cell["cell"] {
                "yes".to_owned()
            } else {
                "-".to_owned()
            },
            display_value(&cell["options"]),
            decimal(&cell["selection"][0]["mean_recall_at_k"]),
            decimal(&cell["selection"][1]["mean_recall_at_k"]),
            decimal(&cell["headline"][0]["mean_recall_at_k"]),
            decimal(&cell["headline"][1]["mean_recall_at_k"]),
        ]);
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

    let mut blocks = vec![
        ViewBlock::table(coverage_table.with_plain_record_type("evaluation_coverage")),
        ViewBlock::table(folds_table.with_plain_record_type("evaluation_fold")),
        ViewBlock::table(
            metrics
                .with_plain_record_type("evaluation_metric")
                .with_empty_message("no evaluation metrics"),
        ),
        ViewBlock::table(
            paired_table
                .with_plain_record_type("evaluation_paired_difference")
                .with_empty_message("no paired differences"),
        ),
        ViewBlock::table(gates_table.with_plain_record_type("evaluation_gate")),
    ];
    if sweep.is_object() {
        blocks.push(ViewBlock::table(
            sweep_table
                .with_plain_record_type("evaluation_sweep_cell")
                .with_empty_message("no sweep cells"),
        ));
        if !sweep["headline"].is_null() {
            blocks.push(ViewBlock::table(
                metrics_table(&sweep["headline"]["metrics"])
                    .with_plain_record_type("evaluation_headline_metric")
                    .with_empty_message("no headline metrics"),
            ));
        }
    }
    blocks.push(ViewBlock::table(
        cases_table
            .with_plain_record_type("evaluation_case")
            .with_empty_message("no evaluation cases"),
    ));
    let view = View::Blocks(blocks);
    let mut context = document.clone();
    let take = |key: &str, context: &mut Value| {
        context
            .as_object_mut()
            .and_then(|object| object.remove(key))
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default()
    };
    let metric_rows = take("metrics", &mut context);
    let paired = take("paired_differences", &mut context);
    let cases = take("cases", &mut context);
    let mut records = vec![json!({"record_type": "evaluation_context", "context": context})];
    records.extend(
        metric_rows
            .into_iter()
            .map(|metric| json!({"record_type": "evaluation_metric", "metric": metric})),
    );
    records.extend(paired.into_iter().map(
        |difference| json!({"record_type": "evaluation_paired_difference", "difference": difference}),
    ));
    records.extend(
        cases
            .into_iter()
            .map(|case| json!({"record_type": "evaluation_case", "case": case})),
    );
    CommandOutput::with_view(document, view).with_ndjson_records(records)
}

/// One row per variant and level: pooled recall and precision, per-case mean
/// recall, coverage, stale rate and latency.
fn metrics_table(metrics: &Value) -> TableView {
    let mut table = TableView::new(vec![
        Column::text("variant"),
        Column::text("level"),
        Column::number("k"),
        Column::number("cases"),
        Column::number("recall"),
        Column::number("mean recall"),
        Column::number("precision"),
        Column::number("coverage"),
        Column::number("stale rate"),
        Column::number("mean (ms)"),
    ]);
    for metric in metrics.as_array().into_iter().flatten() {
        table.push_row([
            display_value(&metric["variant"]),
            display_value(&metric["level"]),
            display_value(&metric["k"]),
            display_value(&metric["cases"]),
            decimal(&metric["recall_at_k"]),
            decimal(&metric["mean_recall_at_k"]),
            decimal(&metric["precision_at_k"]),
            decimal(&metric["coverage"]),
            decimal(&metric["stale_result_rate"]),
            decimal(&metric["mean_latency_ms"]),
        ]);
    }
    table
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
        ViewBlock::table(context_table.with_plain_record_type("live_git_context")),
        ViewBlock::table(
            metrics_table
                .with_plain_record_type("live_git_metric")
                .with_empty_message("no live evaluation metrics"),
        ),
        ViewBlock::table(
            cases_table
                .with_plain_record_type("live_git_case")
                .with_empty_message("no held-out commits"),
        ),
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

use std::fs;
use std::path::PathBuf;

use clap::{Args, ValueEnum};

use orbit_graph::{
    GraphError, HybridTaskHit, RecommendationEngine, RecommendationInput, RecommendationLevel,
    RecommendationRequest, RecommendationVariant, TaskAssociation,
};

use serde_json::{Value, json};

use super::{CliError, CommandContext, display_value, json_value};
use crate::output::{Column, CommandOutput, TableView, View, ViewBlock};

#[derive(Debug, Args)]
#[command(group(
    clap::ArgGroup::new("intent")
        .required(true)
        .multiple(false)
        .args(["query", "task_id"])
))]
pub struct RecommendCommand {
    /// Free-text task/change query (mutually exclusive with --task-id).
    #[arg(long)]
    query: Option<String>,
    /// Target task ID (mutually exclusive with --query); its own delivery is excluded.
    #[arg(long)]
    task_id: Option<String>,
    /// Destination granularity.
    #[arg(long, value_enum, default_value_t = LevelArg::File)]
    level: LevelArg,
    /// Ranking strategy (primarily useful for evaluation and diagnosis).
    #[arg(long, value_enum, default_value_t = VariantArg::Combined)]
    variant: VariantArg,
    /// Maximum recommendations (1..=100).
    #[arg(long)]
    limit: Option<usize>,
    /// Landing branch whose history index should be used.
    #[arg(long, default_value = "main")]
    branch: String,
    /// Git revision expression; defaults to the current checkout commit.
    #[arg(long)]
    revision: Option<String>,
    /// Chronological evidence cutoff (RFC 3339 or unix:seconds).
    #[arg(long)]
    cutoff: Option<String>,
    /// JSON file containing the target task's authoritative pre-execution snapshot.
    #[arg(long, requires = "task_id")]
    task_snapshot: Option<PathBuf>,
    /// JSON file containing an array of externally ranked hybrid task hits.
    #[arg(long)]
    hybrid_hits: Option<PathBuf>,
}

impl RecommendCommand {
    pub(crate) fn run(&self, context: &CommandContext) -> Result<serde_json::Value, CliError> {
        let input = match (&self.query, &self.task_id) {
            (Some(query), None) => RecommendationInput::Query(query.clone()),
            (None, Some(task_id)) => RecommendationInput::TaskId(task_id.clone()),
            _ => {
                return Err(CliError::Graph(GraphError::invalid_data(
                    "validate recommend arguments",
                    "exactly one of --query or --task-id is required",
                )));
            }
        };
        let hybrid_hits = match &self.hybrid_hits {
            Some(path) => {
                let bytes = fs::read(path).map_err(|source| {
                    CliError::Graph(GraphError::io(
                        "read recommendation hybrid hits",
                        path,
                        source,
                    ))
                })?;
                serde_json::from_slice::<Vec<HybridTaskHit>>(bytes.as_slice()).map_err(|error| {
                    CliError::Graph(GraphError::invalid_data(
                        "decode recommendation hybrid hits JSON",
                        error.to_string(),
                    ))
                })?
            }
            None => Vec::new(),
        };
        let request = RecommendationRequest {
            input,
            level: self.level.into_graph(),
            variant: self.variant.into_graph(),
            limit: self.limit,
            target_revision: self.revision.clone(),
            cutoff: self.cutoff.clone(),
            task_snapshot: self
                .task_snapshot
                .as_ref()
                .map(|path| read_json::<TaskAssociation>(path, "task snapshot"))
                .transpose()?,
            hybrid_hits,
        };
        let engine = RecommendationEngine::open(context.worktree_root(), self.branch.as_str())?;
        json_value(engine.recommend(&request)?)
    }
}

pub(crate) fn output(document: Value) -> CommandOutput {
    let freshness = format!(
        "{}: {}",
        display_value(&document["source_freshness"]["status"]),
        display_value(&document["source_freshness"]["reason"])
    );
    let fallbacks = document["fallbacks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|fallback| display_value(&fallback["kind"]))
        .collect::<Vec<_>>()
        .join(", ");
    let mut table = TableView::new(vec![
        Column::number("rank"),
        Column::text("selector"),
        Column::number("score"),
        Column::text("evidence"),
        Column::text("freshness"),
    ]);
    if let Some(recommendations) = document["recommendations"].as_array() {
        for recommendation in recommendations {
            let mut evidence = recommendation["reasons"]
                .as_array()
                .into_iter()
                .flatten()
                .take(2)
                .map(|reason| {
                    format!(
                        "{} ({:+.3})",
                        display_value(&reason["explanation"]),
                        reason["contribution"].as_f64().unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>();
            if recommendation["file_fallback"].as_bool() == Some(true) {
                evidence.push(format!(
                    "fallback: {}",
                    display_value(&recommendation["fallback_reason"])
                ));
            }
            if !fallbacks.is_empty() {
                evidence.push(format!("limits: {fallbacks}"));
            }
            table.push_row([
                display_value(&recommendation["rank"]),
                display_value(&recommendation["selector"]),
                recommendation["score"]
                    .as_f64()
                    .map_or_else(|| "-".to_owned(), |score| format!("{score:.4}")),
                if evidence.is_empty() {
                    "no scored evidence".to_owned()
                } else {
                    evidence.join("; ")
                },
                freshness.clone(),
            ]);
        }
    }
    let empty = if fallbacks.is_empty() {
        format!("no recommendations; source freshness is {freshness}")
    } else {
        format!("no recommendations; {freshness}; fallbacks: {fallbacks}")
    };
    let view = View::Blocks(vec![ViewBlock::table(table.with_empty_message(empty))]);

    let mut context = document.clone();
    let recommendations = context
        .as_object_mut()
        .and_then(|object| object.remove("recommendations"))
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    let mut records = vec![json!({"record_type": "recommendation_context", "context": context})];
    records.extend(recommendations.into_iter().map(
        |recommendation| json!({"record_type": "recommendation", "recommendation": recommendation}),
    ));
    CommandOutput::with_view(document, view).with_ndjson_records(records)
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum VariantArg {
    Combined,
    TaskSearchOnly,
    GraphOnly,
    Frequency,
}

impl VariantArg {
    fn into_graph(self) -> RecommendationVariant {
        match self {
            Self::Combined => RecommendationVariant::Combined,
            Self::TaskSearchOnly => RecommendationVariant::TaskSearchOnly,
            Self::GraphOnly => RecommendationVariant::GraphOnly,
            Self::Frequency => RecommendationVariant::Frequency,
        }
    }
}

fn read_json<T: serde::de::DeserializeOwned>(
    path: &std::path::Path,
    description: &str,
) -> Result<T, CliError> {
    let bytes = fs::read(path).map_err(|source| {
        CliError::Graph(GraphError::io(
            "read recommendation JSON input",
            path,
            source,
        ))
    })?;
    serde_json::from_slice(bytes.as_slice()).map_err(|error| {
        CliError::Graph(GraphError::invalid_data(
            "decode recommendation JSON input",
            format!("invalid {description}: {error}"),
        ))
    })
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum LevelArg {
    File,
    Symbol,
}

impl LevelArg {
    fn into_graph(self) -> RecommendationLevel {
        match self {
            Self::File => RecommendationLevel::File,
            Self::Symbol => RecommendationLevel::Symbol,
        }
    }
}

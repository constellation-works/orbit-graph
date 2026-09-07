use std::fs;
use std::path::PathBuf;

use clap::{Args, ValueEnum};

use crate::{
    GraphError, HybridTaskHit, RecommendationEngine, RecommendationInput, RecommendationLevel,
    RecommendationRequest, TaskAssociation,
};

use super::{CliError, CommandContext, json_value};

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

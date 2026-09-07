use std::fs;
use std::path::PathBuf;

use clap::Args;

use crate::{EvaluationCorpus, GraphError, evaluate_corpus};

use super::{CliError, CommandContext, json_value};

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

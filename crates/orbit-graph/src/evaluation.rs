//! Evaluates recommendation variants against dated delivery corpora using extraction, graph,
//! store, sync, recommendation, runtime and error contracts.
//!
//! Each case is ranked by every `recommend` variant and by two history-free
//! baselines ([`baselines`]): BM25 over the tree at the case's fold cutoff
//! commit, and the paths and symbols the task text names. Two combinations of
//! the word index with them are derived from the same rankings, and
//! [`readout`] turns the per-case rankings into metrics, paired differences
//! and the context-index design's gates G1 and G2. An optional grid re-ranks
//! every case under each word-index configuration, chooses one on the earlier
//! folds and reads the headline numbers on the later ones.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use git2::{Oid, Repository};
use orbit_graph_extract::history::{
    parse_timestamp, repository_identity, validate_task_association,
};
use serde::{Deserialize, Serialize};

use crate::recommend::MAX_RECOMMENDATION_LIMIT;
use crate::{
    DeliveryEvidence, DeliveryImport, Graph, GraphError, HistoryIndex, HybridTaskHit, Provenance,
    RecommendationEngine, RecommendationInput, RecommendationLevel, RecommendationRequest,
    RecommendationVariant, SyncMode, SyncPolicy, TaskAssociation, TemporalFact, TemporalStatus,
    WordIndexOptions, WordIndexPostingWeight, WordIndexSmoothingUnit, WordIndexTextFields,
};

mod baselines;
mod readout;

use baselines::{Bm25Index, CodeTree, explicit_mentions};
use readout::{
    CaseRun, CaseScores, LEVELS, OptionComparison, WordIndexOption, WordIndexSource, level_index,
};
pub use readout::{
    EvaluationMetrics, EvaluationVariant, GateReadout, GateResult, GateStatus, PairedDifference,
    PairedMetric, Readout, SplitMetrics,
};

/// Version of the chronological evaluation report JSON contract.
///
/// Version 2 reports `null` rather than `0.0` for a metric with no data: an
/// empty denominator or no latency samples (STD-02 §R29).
///
/// Version 3 adds the `bm25_code`, `explicit_mentions`,
/// `word_index_plus_mentions` and `word_index_bm25_rrf` rows, per-row
/// coverage and mean recall, the added-files split, paired differences,
/// gates G1 and G2, folds and the optional word-index sweep.
///
/// Version 4 adds the symbol-first, posting-weight, smoothing-unit and
/// added-weight options to every echoed `word_index` options object, gate G4
/// per option (with the `option`, `selected_setting` and `kept_setting`
/// fields on every gate result), `gates.selected_word_index_options` and
/// `sweep.kept_cell`; G1, G2 and the sweep's headline readout are read on the
/// cell G4 keeps.
pub const EVALUATION_SCHEMA_VERSION: u32 = 4;

/// Version of the evaluation corpus (input) JSON contract. It moves
/// independently of [`EVALUATION_SCHEMA_VERSION`]: the input shape did not
/// change when the report did, and the optional `folds`, `fold`,
/// `baseline_text_fields` and `word_index_grid` fields leave every earlier
/// corpus valid with the same digest.
pub const EVALUATION_CORPUS_SCHEMA_VERSION: u32 = 1;

/// The deepest ranking a combination reads: the word index and BM25 are
/// ranked this deep, and their own rows cut at K.
const COMBINATION_DEPTH: usize = 50;

/// Most word-index configurations one grid may hold.
pub const MAX_WORD_INDEX_GRID_CELLS: usize = 256;

/// Reproducible evaluation input composed only of public envelopes and observations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationCorpus {
    /// Must equal [`EVALUATION_CORPUS_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Stable repository identity expected by every delivery envelope.
    pub repository: String,
    /// Landing branch holding the verified delivery revisions.
    pub landing_branch: String,
    /// Provenance for this bounded corpus/export.
    pub source: Provenance,
    /// Whether the producer claims this is the complete eligible corpus.
    pub complete: bool,
    /// Explanation of limits, pagination, or exclusions at collection time.
    pub coverage_note: String,
    /// Top-K used for every ranking variant.
    pub k: usize,
    /// Delivered changes available for training; future entries remain cutoff-filtered.
    #[serde(default)]
    pub training_deliveries: Vec<DeliveryImport>,
    /// Chronologically held-out prospective cases.
    pub cases: Vec<EvaluationCase>,
    /// Parameters of the `word_index` variant for every case. Omitted, the
    /// design's defaults apply (title text, window 4,000, boost 1.5,
    /// min_support 2, smoothing 1, window-stopword share 0.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_index: Option<WordIndexOptions>,
    /// Rolling-origin folds, each with the cutoff commit BM25 indexes. Every
    /// case then names its fold. Omitted, each case is its own fold, cut at
    /// its own cutoff and target revision.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folds: Vec<EvaluationFold>,
    /// Task text BM25 and explicit mentions read. Omitted, the title,
    /// description and acceptance criteria: the task text as filed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_text_fields: Option<WordIndexTextFields>,
    /// A parameter grid for the word index, chosen on the earlier folds and
    /// read on the later ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_index_grid: Option<WordIndexGrid>,
}

/// One rolling-origin fold: the cases first dispatched after `cutoff`,
/// ranked against the tree of the `revision` that was current then.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationFold {
    /// Fold identifier the cases name.
    pub id: String,
    /// The fold's cutoff; no case of the fold may cut earlier.
    pub cutoff: String,
    /// The full commit that was the landing branch's tip at `cutoff`. BM25
    /// indexes its tree; it must be the target revision of each of the
    /// fold's cases or an ancestor of it.
    pub revision: String,
}

/// Word-index parameters to sweep. An empty or omitted axis holds the
/// corpus's `word_index` value; the cells are every combination of the
/// axes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WordIndexGrid {
    /// The first this many folds, in cutoff order, choose the cell; the rest
    /// report the headline numbers. At least 1 and fewer than the folds.
    pub selection_folds: usize,
    /// `text_fields` values.
    pub text_fields: Vec<WordIndexTextFields>,
    /// `window_size` values.
    pub window_size: Vec<usize>,
    /// `symbol_boost` values.
    pub symbol_boost: Vec<f64>,
    /// `min_support` values.
    pub min_support: Vec<usize>,
    /// `smoothing` values.
    pub smoothing: Vec<f64>,
    /// `max_term_share` values.
    pub max_term_share: Vec<f64>,
    /// `symbol_first` values. This and the three option axes below are
    /// omitted when empty, so a grid written before they existed digests
    /// unchanged.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub symbol_first: Vec<bool>,
    /// `posting_weight` values (design §5.4).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub posting_weight: Vec<WordIndexPostingWeight>,
    /// `smoothing_unit` values, crossed with the weighted `posting_weight`
    /// settings; identical under `presence`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub smoothing_unit: Vec<WordIndexSmoothingUnit>,
    /// `added_weight` values from 0 to 1 (design §5.5).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub added_weight: Vec<f64>,
}

/// One prospective task observation paired with later verified Git truth.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationCase {
    /// Stable case identifier.
    pub id: String,
    /// Immutable repository revision observed when the task text was captured.
    pub target_revision: String,
    /// Observation-time cutoff; all ranking evidence must be strictly earlier.
    pub cutoff: String,
    /// Authoritative pre-execution task-text observation.
    pub task_snapshot: TaskAssociation,
    /// Later delivery used only to derive truth; never imported into training.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_out_delivery: Option<DeliveryImport>,
    /// Trustworthy lower bound proving the delivery happened after the query cutoff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prospective_delivery_lower_bound: Option<TemporalFact>,
    /// Optional ranked task-search hits captured before the same cutoff.
    #[serde(default)]
    pub hybrid_hits: Vec<HybridTaskHit>,
    /// Public observation time shared by supplied hybrid hits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hybrid_hits_observed_at: Option<TemporalFact>,
    /// Public retriever/export provenance for supplied hybrid hits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hybrid_hits_source: Option<Provenance>,
    /// The corpus fold this case belongs to; required when the corpus
    /// declares folds and refused when it does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fold: Option<String>,
    /// Provenance for the task observation and delivery association.
    pub source: Provenance,
}

/// Complete chronological evaluation result.
#[derive(Debug, Clone, Serialize)]
pub struct EvaluationReport {
    /// Evaluation schema version.
    pub schema_version: u32,
    /// Repository identity verified by the history adapter.
    pub repository: String,
    /// Fully resolved revision at which the evaluation was run.
    pub evaluated_at_revision: String,
    /// Corpus/export provenance.
    pub source: Provenance,
    /// Stable digest of the normalized input corpus.
    pub corpus_digest: String,
    /// Declared collection completeness and limitations.
    pub coverage: EvaluationCoverage,
    /// Parameters the `word_index` rows were scored with.
    pub word_index_options: WordIndexOptions,
    /// Task text BM25 and explicit mentions read.
    pub baseline_text_fields: WordIndexTextFields,
    /// Folds in cutoff order, with the BM25 index built for each.
    pub folds: Vec<EvaluationFoldResult>,
    /// One row for every ranking and destination level, over every
    /// evaluated case, with the corpus's word-index options.
    pub metrics: Vec<EvaluationMetrics>,
    /// The same rows over each side of the added-files split.
    pub split_metrics: Vec<SplitMetrics>,
    /// `word_index` minus every other ranking, per case, with a 95%
    /// bootstrap interval.
    pub paired_differences: Vec<PairedDifference>,
    /// Gates G1 and G2 of the context-index design (§10.5): over every case
    /// without a grid, over the headline folds on the selected cell with one.
    pub gates: GateReadout,
    /// The word-index grid, when the corpus asks for one.
    pub sweep: Option<SweepReport>,
    /// Per-case admission or exclusion evidence.
    pub cases: Vec<EvaluationCaseResult>,
}

/// One fold as evaluated.
#[derive(Debug, Clone, Serialize)]
pub struct EvaluationFoldResult {
    /// Fold identifier (the case ID for an implicit per-case fold).
    pub id: String,
    /// Fold cutoff.
    pub cutoff: String,
    /// Cutoff commit whose tree BM25 indexed.
    pub revision: String,
    /// `selection` or `headline` under a grid, otherwise `all`.
    pub role: String,
    /// Evaluated cases in the fold.
    pub cases_evaluated: usize,
    /// File documents BM25 indexed, or `null` when no case reached ranking.
    pub bm25_file_documents: Option<usize>,
    /// Symbol documents BM25 indexed, or `null` when no case reached ranking.
    pub bm25_symbol_documents: Option<usize>,
    /// Blobs left out of the BM25 file documents, by reason.
    pub bm25_skipped: BTreeMap<String, usize>,
}

/// The word-index grid and the readout on its chosen cell.
#[derive(Debug, Clone, Serialize)]
pub struct SweepReport {
    /// Folds that chose the cell.
    pub selection_folds: Vec<String>,
    /// Folds the headline numbers are read on.
    pub headline_folds: Vec<String>,
    /// Evaluated cases in the selection folds.
    pub selection_cases: usize,
    /// Evaluated cases in the headline folds.
    pub headline_cases: usize,
    /// How the cell was chosen.
    pub selection_rule: String,
    /// Every cell with its word-index rows on each side.
    pub cells: Vec<SweepCell>,
    /// Index of the chosen cell, or `null` when no selection case had file
    /// truth to choose by.
    pub selected_cell: Option<usize>,
    /// Index of the cell gate G4 leaves standing: the chosen cell with every
    /// option that did not pass G4 set back to its baseline. `null` without a
    /// chosen cell or when that configuration is not in the grid.
    pub kept_cell: Option<usize>,
    /// Every ranking over the headline folds, with the word index and its
    /// combinations on the kept cell; `null` without one.
    pub headline: Option<Readout>,
}

/// One grid cell.
#[derive(Debug, Clone, Serialize)]
pub struct SweepCell {
    /// Position in the grid.
    pub cell: usize,
    /// The cell's word-index options.
    pub options: WordIndexOptions,
    /// `word_index` file and symbol rows over the selection folds.
    pub selection: Vec<EvaluationMetrics>,
    /// `word_index` file and symbol rows over the headline folds. Reported
    /// for every cell; only the chosen cell's are headline numbers.
    pub headline: Vec<EvaluationMetrics>,
}

/// Corpus and case coverage accounting.
#[derive(Debug, Clone, Serialize)]
pub struct EvaluationCoverage {
    /// Whether the source claimed complete coverage.
    pub source_complete: bool,
    /// Producer's bounded-collection note.
    pub note: String,
    /// Number of supplied cases.
    pub cases_total: usize,
    /// Number with verified held-out truth and valid chronology.
    pub cases_evaluated: usize,
    /// Number excluded fail-closed.
    pub cases_excluded: usize,
    /// Number of supplied training envelopes.
    pub training_deliveries_supplied: usize,
    /// Number loaded into each evaluated case's fresh derived history index.
    pub training_deliveries_inserted: usize,
    /// Evaluation never opened or mutated the caller's operational indexes.
    pub isolated_indexes: bool,
}

/// Admission result for one prospective case.
#[derive(Debug, Clone, Serialize)]
pub struct EvaluationCaseResult {
    /// Case identifier.
    pub id: String,
    /// Whether metrics included this case.
    pub evaluated: bool,
    /// Fail-closed exclusion reasons.
    pub exclusions: Vec<String>,
    /// Immutable target revision used for recommendation.
    pub target_revision: String,
    /// Fold the case was read in, or `null` when it names no valid fold.
    pub fold: Option<String>,
    /// Held-out delivery identifier when available.
    pub held_out_delivery_id: Option<String>,
    /// Whether more than half of the delivery's changed files were added;
    /// `null` when no truth was derived.
    pub mostly_added_files: Option<bool>,
    /// Frozen target-tree graph provenance used by graph-bearing variants,
    /// or `null` when none was used.
    pub graph_snapshot: Option<Provenance>,
    /// Whether each graph-bearing query level actually applied frozen structure.
    pub graph_structure_applied: BTreeMap<String, bool>,
    /// Honest denominator and omission accounting for held-out truth.
    pub truth_coverage: TruthCoverage,
    /// Case provenance.
    pub source: Provenance,
}

/// Held-out truth eligibility and explicit omission reasons by destination level.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TruthCoverage {
    /// Changed files represented by the held-out delivery.
    pub file_changes_total: usize,
    /// Existing target-tree files used in the file recall denominator.
    pub file_truth_eligible: usize,
    /// Changed files omitted from the denominator, by reason.
    pub file_truth_omitted: BTreeMap<String, usize>,
    /// Symbol changes or file-level symbol-attribution gaps represented in truth.
    pub symbol_changes_total: usize,
    /// Existing target-tree symbols used in the symbol recall denominator.
    pub symbol_truth_eligible: usize,
    /// Symbol truth omitted from the denominator, by reason.
    pub symbol_truth_omitted: BTreeMap<String, usize>,
}

/// One fold as resolved from the corpus.
struct Fold {
    id: String,
    cutoff: String,
    cutoff_time: orbit_graph_extract::history::Timestamp,
    revision: Oid,
    /// Explicitly declared (`false` for an implicit per-case fold).
    declared: bool,
}

/// BM25 index of the fold whose cases are being ranked.
struct FoldIndex {
    fold: usize,
    tree: Rc<CodeTree>,
    index: Bm25Index,
}

/// Evaluate every ranking without importing held-out deliveries.
///
/// Every `recommend` variant, `word_index` included, ranks from the case's
/// isolated history index under the case's strict cutoff, so only deliveries
/// landed before the cutoff contribute evidence. BM25 reads the tree of the
/// case's fold cutoff commit and explicit mentions the case's target tree;
/// neither reads history.
pub fn evaluate_corpus(
    repo_root: &Path,
    corpus: &EvaluationCorpus,
) -> Result<EvaluationReport, GraphError> {
    validate_corpus(corpus)?;
    let source_repo = Repository::open(repo_root)
        .map_err(|error| GraphError::git("open evaluation repository", error))?;
    let routed_identity = repository_identity(&source_repo)?;
    if routed_identity != corpus.repository {
        return Err(GraphError::invalid_input(
            "validate evaluation repository routing",
            "repository",
            "corpus repository does not match the routed credential-free repository identity",
        ));
    }
    let digest = blake3::hash(
        serde_json::to_vec(corpus)
            .map_err(|error| {
                GraphError::invalid_data("encode evaluation corpus", error.to_string())
            })?
            .as_slice(),
    )
    .to_hex()
    .to_string();
    let base_options = corpus.word_index.unwrap_or_default();
    let baseline_text_fields = corpus
        .baseline_text_fields
        .unwrap_or(WordIndexTextFields::TitleDescriptionCriteria);
    let cells = grid_cells(&base_options, corpus.word_index_grid.as_ref())?;
    let (folds, case_folds) = resolve_folds(&source_repo, corpus)?;
    if let Some(grid) = corpus.word_index_grid.as_ref()
        && (grid.selection_folds == 0 || grid.selection_folds >= folds.len())
    {
        return Err(GraphError::invalid_input(
            "validate word index grid",
            "word_index_grid.selection_folds",
            format!(
                "selection_folds must be at least 1 and fewer than the {} fold(s), leaving headline folds",
                folds.len()
            ),
        ));
    }
    // `k` is at most 100 (`validate_corpus`), the recommendation limit.
    let depth = corpus.k.clamp(COMBINATION_DEPTH, MAX_RECOMMENDATION_LIMIT);

    // Cases run fold by fold so one BM25 index is held at a time; results
    // keep the corpus order.
    let mut order = (0..corpus.cases.len()).collect::<Vec<_>>();
    order.sort_by_key(|&index| (case_folds[index].unwrap_or(usize::MAX), index));
    let mut case_results: Vec<Option<EvaluationCaseResult>> = vec![None; corpus.cases.len()];
    let mut scores = Vec::new();
    let mut target_cache: Option<Rc<CodeTree>> = None;
    let mut fold_cache: Option<FoldIndex> = None;
    let mut fold_indexes: BTreeMap<usize, (usize, usize, BTreeMap<String, usize>)> =
        BTreeMap::new();
    for index in order {
        let case = &corpus.cases[index];
        let fold = case_folds[index];
        let mut exclusions = validate_case(repo_root, corpus, case)?;
        match fold {
            None => exclusions.push("fold_missing_or_undeclared".to_string()),
            Some(fold) => exclusions.extend(validate_case_fold(&source_repo, &folds[fold], case)?),
        }
        let mut truth_coverage = TruthCoverage::default();
        let mut graph_structure_applied = BTreeMap::new();
        let mut graph_snapshot = None;
        let mut mostly_added_files = None;
        if let (true, Some(fold)) = (exclusions.is_empty(), fold) {
            let workspace = EvaluationWorkspace::create(
                repo_root,
                corpus.repository.as_str(),
                corpus.landing_branch.as_str(),
                case.target_revision.as_str(),
            )?;
            let index = HistoryIndex::open(workspace.path(), corpus.landing_branch.as_str())?;
            for delivery in &corpus.training_deliveries {
                index.import(delivery.clone())?;
            }
            Graph::open(workspace.path(), SyncPolicy::Manual)?.sync(SyncMode::Full)?;
            graph_snapshot = Some(Provenance {
                system: "git.detached_target_tree+isolated_orbit_graph".to_string(),
                record_id: Some(case.target_revision.clone()),
            });
            let target = Oid::from_str(case.target_revision.as_str())
                .map_err(|error| GraphError::git("parse evaluation target revision", error))?;
            let target_tree = cached_tree(
                &source_repo,
                target,
                &mut target_cache,
                fold_cache.as_ref().map(|cached| &cached.tree),
            )?;
            let mut truth = [BTreeSet::new(), BTreeSet::new()];
            if let Some(held_out) = case.held_out_delivery.clone() {
                let extracted = index.preview(held_out)?;
                let outcome = held_out_truth(&target_tree, &extracted);
                truth = outcome.truth;
                truth_coverage = outcome.coverage;
                let added = truth_coverage
                    .file_truth_omitted
                    .get("added_file_absent_at_target")
                    .copied()
                    .unwrap_or_default();
                mostly_added_files = Some(added * 2 > truth_coverage.file_changes_total);
            }
            if truth.iter().all(BTreeSet::is_empty) {
                exclusions.push("held_out_delivery_has_no_target-resolvable_truth".to_string());
            } else {
                if fold_cache.as_ref().is_none_or(|cached| cached.fold != fold) {
                    // Release the previous fold's index before building the next.
                    drop(fold_cache.take());
                    let tree = cached_tree(
                        &source_repo,
                        folds[fold].revision,
                        &mut None,
                        Some(&target_tree),
                    )?;
                    let index = Bm25Index::build(&source_repo, &tree)?;
                    fold_indexes.insert(
                        fold,
                        (
                            index.file_documents,
                            index.symbol_documents,
                            index.skipped.clone(),
                        ),
                    );
                    fold_cache = Some(FoldIndex { fold, tree, index });
                }
                let bm25 = fold_cache
                    .as_ref()
                    .map(|cached| &cached.index)
                    .ok_or_else(|| {
                        GraphError::invalid_data("rank evaluation BM25", "fold index was not built")
                    })?;
                let engine =
                    RecommendationEngine::open(workspace.path(), corpus.landing_branch.as_str())?;
                let mut case_scores = CaseScores {
                    fold,
                    mostly_added: mostly_added_files.unwrap_or(false),
                    truth,
                    ..CaseScores::default()
                };
                let rank = |variant: RecommendationVariant,
                            level: RecommendationLevel,
                            limit: usize,
                            options: Option<WordIndexOptions>|
                 -> Result<(CaseRun, bool), GraphError> {
                    let started = Instant::now();
                    let result = engine.recommend(&RecommendationRequest {
                        input: RecommendationInput::TaskId(case.task_snapshot.task_id.clone()),
                        level,
                        variant,
                        limit: Some(limit),
                        target_revision: Some(case.target_revision.clone()),
                        cutoff: Some(case.cutoff.clone()),
                        task_snapshot: Some(case.task_snapshot.clone()),
                        hybrid_hits: case.hybrid_hits.clone(),
                        commit_text_weight: None,
                        commit_text_exponent: None,
                        word_index: options,
                    })?;
                    let latency_micros = started.elapsed().as_micros();
                    Ok((
                        CaseRun {
                            selectors: result
                                .recommendations
                                .into_iter()
                                .map(|recommendation| recommendation.selector)
                                .collect(),
                            latency_micros,
                        },
                        result.structure_applied,
                    ))
                };
                for variant in EvaluationVariant::ALL {
                    let Some(recommendation) = variant.recommendation() else {
                        continue;
                    };
                    for level in LEVELS {
                        let word_index = recommendation == RecommendationVariant::WordIndex;
                        let (run, structure_applied) = rank(
                            recommendation,
                            level,
                            if word_index { depth } else { corpus.k },
                            word_index.then_some(base_options),
                        )?;
                        if matches!(
                            recommendation,
                            RecommendationVariant::Combined | RecommendationVariant::GraphOnly
                        ) {
                            graph_structure_applied.insert(
                                format!("{}:{}", variant.name(), level_name(level)),
                                structure_applied,
                            );
                        }
                        case_scores.runs.insert((variant, level_index(level)), run);
                    }
                }
                let text = baseline_text_fields.text(&case.task_snapshot);
                for level in LEVELS {
                    let started = Instant::now();
                    let selectors = bm25.rank(level, text.as_str(), depth)?;
                    case_scores.runs.insert(
                        (EvaluationVariant::Bm25Code, level_index(level)),
                        CaseRun {
                            selectors,
                            latency_micros: started.elapsed().as_micros(),
                        },
                    );
                }
                let started = Instant::now();
                let mentions = explicit_mentions(text.as_str(), &target_tree);
                let latency_micros = started.elapsed().as_micros();
                for level in LEVELS {
                    case_scores.runs.insert(
                        (EvaluationVariant::ExplicitMentions, level_index(level)),
                        CaseRun {
                            selectors: mentions.level(level).to_vec(),
                            latency_micros,
                        },
                    );
                }
                for options in &cells {
                    let mut cell = [CaseRun::default(), CaseRun::default()];
                    for level in LEVELS {
                        cell[level_index(level)] = rank(
                            RecommendationVariant::WordIndex,
                            level,
                            depth,
                            Some(*options),
                        )?
                        .0;
                    }
                    case_scores.grid.push(cell);
                }
                case_scores.not_live = case_scores
                    .runs
                    .values()
                    .chain(case_scores.grid.iter().flatten())
                    .flat_map(|run| run.selectors.iter())
                    .filter(|selector| !target_tree.is_live(selector))
                    .cloned()
                    .collect();
                scores.push(case_scores);
            }
        }
        case_results[index] = Some(EvaluationCaseResult {
            id: case.id.clone(),
            evaluated: exclusions.is_empty(),
            exclusions,
            target_revision: case.target_revision.clone(),
            fold: fold.map(|fold| folds[fold].id.clone()),
            held_out_delivery_id: case
                .held_out_delivery
                .as_ref()
                .map(|delivery| delivery.delivery_id.clone()),
            mostly_added_files,
            graph_snapshot,
            graph_structure_applied,
            truth_coverage,
            source: case.source.clone(),
        });
    }
    drop(fold_cache);
    let case_results = case_results.into_iter().flatten().collect::<Vec<_>>();

    let all = scores.iter().collect::<Vec<_>>();
    let base = readout::readout(all.as_slice(), WordIndexSource::Base, corpus.k);
    let (sweep, gates) = match corpus.word_index_grid.as_ref() {
        Some(grid) => {
            let (sweep, gates) = sweep(all.as_slice(), &folds, &cells, grid, corpus.k);
            (Some(sweep), gates)
        }
        None => (
            None,
            GateReadout {
                scope: "all_cases".to_string(),
                word_index_options: Some(base_options),
                selected_word_index_options: None,
                cases: all.len(),
                results: readout::gates(all.as_slice(), WordIndexSource::Base, corpus.k, None)
                    .into_iter()
                    .chain(WordIndexOption::ALL.map(|option| {
                        readout::gate_four(
                            all.as_slice(),
                            option,
                            None,
                            OptionComparison::Unavailable(
                                "G4 compares configurations of a word_index_grid read on its headline folds; this corpus has no grid".to_string(),
                            ),
                            corpus.k,
                        )
                    }))
                    .collect(),
            },
        ),
    };
    let folds = folds
        .iter()
        .enumerate()
        .map(|(position, fold)| {
            let indexed = fold_indexes.remove(&position);
            EvaluationFoldResult {
                id: fold.id.clone(),
                cutoff: fold.cutoff.clone(),
                revision: fold.revision.to_string(),
                role: match corpus.word_index_grid.as_ref() {
                    Some(grid) if position < grid.selection_folds => "selection",
                    Some(_) => "headline",
                    None => "all",
                }
                .to_string(),
                cases_evaluated: scores.iter().filter(|case| case.fold == position).count(),
                bm25_file_documents: indexed.as_ref().map(|indexed| indexed.0),
                bm25_symbol_documents: indexed.as_ref().map(|indexed| indexed.1),
                bm25_skipped: indexed.map(|indexed| indexed.2).unwrap_or_default(),
            }
        })
        .collect();

    let evaluated = case_results.iter().filter(|case| case.evaluated).count();
    let revision = source_repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map(|commit| commit.id().to_string())
        .map_err(|error| GraphError::git("resolve evaluation HEAD", error))?;
    Ok(EvaluationReport {
        schema_version: EVALUATION_SCHEMA_VERSION,
        repository: corpus.repository.clone(),
        evaluated_at_revision: revision,
        source: corpus.source.clone(),
        corpus_digest: digest,
        word_index_options: base_options,
        baseline_text_fields,
        coverage: EvaluationCoverage {
            source_complete: corpus.complete,
            note: corpus.coverage_note.clone(),
            cases_total: corpus.cases.len(),
            cases_evaluated: evaluated,
            cases_excluded: corpus.cases.len() - evaluated,
            training_deliveries_supplied: corpus.training_deliveries.len(),
            training_deliveries_inserted: if evaluated == 0 {
                0
            } else {
                corpus.training_deliveries.len()
            },
            isolated_indexes: true,
        },
        folds,
        metrics: base.metrics,
        split_metrics: base.split_metrics,
        paired_differences: base.paired_differences,
        gates,
        sweep,
        cases: case_results,
    })
}

/// The code tree at `revision`: the cached one when it matches, else the
/// `shared` tree when that matches, else a fresh load (which replaces the
/// cache).
fn cached_tree(
    repo: &Repository,
    revision: Oid,
    cache: &mut Option<Rc<CodeTree>>,
    shared: Option<&Rc<CodeTree>>,
) -> Result<Rc<CodeTree>, GraphError> {
    if let Some(tree) = cache.as_ref().filter(|tree| tree.revision() == revision) {
        return Ok(Rc::clone(tree));
    }
    let tree = match shared.filter(|tree| tree.revision() == revision) {
        Some(tree) => Rc::clone(tree),
        None => Rc::new(CodeTree::load(repo, revision)?),
    };
    *cache = Some(Rc::clone(&tree));
    Ok(tree)
}

/// Every cell of `grid` over `base`, or none without a grid.
///
/// The cells are every combination of the axes, earlier axes varying
/// slowest. Under `presence` the two smoothing units are the same
/// configuration, so such cells are kept once with `tasks`; a cell that sets
/// `posting_weight` or `added_weight` away from its baseline without
/// `symbol_first` is left out, as those options need symbol-first labels.
fn grid_cells(
    base: &WordIndexOptions,
    grid: Option<&WordIndexGrid>,
) -> Result<Vec<WordIndexOptions>, GraphError> {
    let Some(grid) = grid else {
        return Ok(Vec::new());
    };
    fn axis<T: Copy>(
        cells: Vec<WordIndexOptions>,
        values: &[T],
        set: impl Fn(&mut WordIndexOptions, T),
    ) -> Vec<WordIndexOptions> {
        if values.is_empty() {
            return cells;
        }
        let mut out = Vec::with_capacity(cells.len() * values.len());
        for cell in cells {
            for value in values {
                let mut cell = cell;
                set(&mut cell, *value);
                out.push(cell);
            }
        }
        out
    }
    let count = [
        grid.text_fields.len(),
        grid.window_size.len(),
        grid.symbol_boost.len(),
        grid.min_support.len(),
        grid.smoothing.len(),
        grid.max_term_share.len(),
        grid.symbol_first.len(),
        grid.posting_weight.len(),
        grid.smoothing_unit.len(),
        grid.added_weight.len(),
    ]
    .iter()
    .try_fold(1usize, |total, len| total.checked_mul((*len).max(1)));
    if count.is_none_or(|count| count > MAX_WORD_INDEX_GRID_CELLS) {
        return Err(GraphError::invalid_input(
            "validate word index grid",
            "word_index_grid",
            format!("a grid may hold at most {MAX_WORD_INDEX_GRID_CELLS} cells"),
        ));
    }
    let mut cells = vec![*base];
    cells = axis(cells, &grid.text_fields, |cell, value| {
        cell.text_fields = value
    });
    cells = axis(cells, &grid.window_size, |cell, value| {
        cell.window_size = value
    });
    cells = axis(cells, &grid.symbol_boost, |cell, value| {
        cell.symbol_boost = value
    });
    cells = axis(cells, &grid.min_support, |cell, value| {
        cell.min_support = value
    });
    cells = axis(cells, &grid.smoothing, |cell, value| cell.smoothing = value);
    cells = axis(cells, &grid.max_term_share, |cell, value| {
        cell.max_term_share = value;
    });
    cells = axis(cells, &grid.symbol_first, |cell, value| {
        cell.symbol_first = value
    });
    cells = axis(cells, &grid.posting_weight, |cell, value| {
        cell.posting_weight = value;
    });
    cells = axis(cells, &grid.smoothing_unit, |cell, value| {
        cell.smoothing_unit = value;
    });
    cells = axis(cells, &grid.added_weight, |cell, value| {
        cell.added_weight = value
    });
    let mut kept: Vec<WordIndexOptions> = Vec::new();
    for cell in cells {
        let cell = cell.normalized();
        if !cell.symbol_first
            && (cell.posting_weight != WordIndexPostingWeight::Presence || cell.added_weight != 1.0)
        {
            continue;
        }
        cell.validate()?;
        if !kept.contains(&cell) {
            kept.push(cell);
        }
    }
    if kept.is_empty() {
        return Err(GraphError::invalid_input(
            "validate word index grid",
            "word_index_grid",
            "no cell remains: posting_weight and added_weight need symbol_first",
        ));
    }
    Ok(kept)
}

/// The corpus's folds in cutoff order, and each case's fold position.
fn resolve_folds(
    repo: &Repository,
    corpus: &EvaluationCorpus,
) -> Result<(Vec<Fold>, Vec<Option<usize>>), GraphError> {
    let mut folds = Vec::new();
    if corpus.folds.is_empty() {
        for case in &corpus.cases {
            folds.push(Fold {
                id: case.id.clone(),
                cutoff: case.cutoff.clone(),
                cutoff_time: parse_timestamp("evaluation cutoff", case.cutoff.as_str())?,
                revision: Oid::from_str(case.target_revision.as_str())
                    .map_err(|error| GraphError::git("parse evaluation target revision", error))?,
                declared: false,
            });
        }
    } else {
        let mut ids = BTreeSet::new();
        for fold in &corpus.folds {
            if fold.id.trim().is_empty() || !ids.insert(fold.id.as_str()) {
                return Err(GraphError::invalid_input(
                    "validate evaluation folds",
                    "folds",
                    "every fold needs a non-empty, unique id",
                ));
            }
            let revision = Oid::from_str(fold.revision.as_str())
                .ok()
                .filter(|oid| oid.to_string() == fold.revision)
                .filter(|oid| repo.find_commit(*oid).is_ok())
                .ok_or_else(|| {
                    GraphError::invalid_input(
                        "validate evaluation folds",
                        "folds.revision",
                        format!(
                            "fold {} revision must be a full commit ID in the repository",
                            fold.id
                        ),
                    )
                })?;
            folds.push(Fold {
                id: fold.id.clone(),
                cutoff: fold.cutoff.clone(),
                cutoff_time: parse_timestamp("evaluation fold cutoff", fold.cutoff.as_str())?,
                revision,
                declared: true,
            });
        }
    }
    let mut positions = (0..folds.len()).collect::<Vec<_>>();
    positions.sort_by(|&a, &b| {
        folds[a]
            .cutoff_time
            .cmp(&folds[b].cutoff_time)
            .then_with(|| folds[a].id.cmp(&folds[b].id))
            .then_with(|| a.cmp(&b))
    });
    let mut rank = vec![0; folds.len()];
    for (sorted, &original) in positions.iter().enumerate() {
        rank[original] = sorted;
    }
    let case_folds = corpus
        .cases
        .iter()
        .enumerate()
        .map(|(index, case)| {
            if corpus.folds.is_empty() {
                case.fold.is_none().then_some(rank[index])
            } else {
                case.fold.as_deref().and_then(|id| {
                    folds
                        .iter()
                        .position(|fold| fold.id == id)
                        .map(|original| rank[original])
                })
            }
        })
        .collect();
    let mut slots = folds.into_iter().map(Some).collect::<Vec<_>>();
    let ordered = positions
        .into_iter()
        .filter_map(|original| slots[original].take())
        .collect();
    Ok((ordered, case_folds))
}

/// Exclusions from a case's place in its fold: BM25 must not index a tree
/// later than the case's own, nor a fold cut after the case.
fn validate_case_fold(
    repo: &Repository,
    fold: &Fold,
    case: &EvaluationCase,
) -> Result<Vec<String>, GraphError> {
    let mut exclusions = Vec::new();
    if !fold.declared {
        return Ok(exclusions);
    }
    let cutoff = parse_timestamp("evaluation cutoff", case.cutoff.as_str())?;
    if fold.cutoff_time > cutoff {
        exclusions.push("fold_cutoff_after_case_cutoff".to_string());
    }
    let target = Oid::from_str(case.target_revision.as_str())
        .map_err(|error| GraphError::git("parse evaluation target revision", error))?;
    if target != fold.revision
        && !repo
            .graph_descendant_of(target, fold.revision)
            .map_err(|error| GraphError::git("check fold revision ancestry", error))?
    {
        exclusions.push("fold_revision_not_ancestor_of_target".to_string());
    }
    Ok(exclusions)
}

/// Choose a grid cell on the selection folds and read every ranking, and the
/// gates, on the headline folds with it.
fn sweep(
    cases: &[&CaseScores],
    folds: &[Fold],
    cells: &[WordIndexOptions],
    grid: &WordIndexGrid,
    k: usize,
) -> (SweepReport, GateReadout) {
    let selection = cases
        .iter()
        .copied()
        .filter(|case| case.fold < grid.selection_folds)
        .collect::<Vec<_>>();
    let headline = cases
        .iter()
        .copied()
        .filter(|case| case.fold >= grid.selection_folds)
        .collect::<Vec<_>>();
    let rows = |side: &[&CaseScores], cell: usize| {
        LEVELS
            .map(|level| {
                readout::metrics(
                    side,
                    EvaluationVariant::WordIndex,
                    level,
                    WordIndexSource::Cell(cell),
                    k,
                )
            })
            .to_vec()
    };
    let cells = cells
        .iter()
        .enumerate()
        .map(|(cell, options)| SweepCell {
            cell,
            options: *options,
            selection: rows(selection.as_slice(), cell),
            headline: rows(headline.as_slice(), cell),
        })
        .collect::<Vec<_>>();
    let mut selected: Option<(usize, f64, f64)> = None;
    for cell in &cells {
        let Some(file) = cell.selection[0].mean_recall_at_k else {
            continue;
        };
        let symbol = cell.selection[1]
            .mean_recall_at_k
            .unwrap_or(f64::NEG_INFINITY);
        if selected.is_none_or(|(_, best_file, best_symbol)| {
            file > best_file || (file == best_file && symbol > best_symbol)
        }) {
            selected = Some((cell.cell, file, symbol));
        }
    }
    let selected_cell = selected.map(|(cell, _, _)| cell);
    let cell_of = |options: &WordIndexOptions| {
        cells
            .iter()
            .position(|cell| cell.options == options.normalized())
    };
    // G4: each option of the chosen cell against the same cell with that
    // option at its baseline, on the headline folds.
    let selected_options = selected_cell.map(|cell| cells[cell].options);
    let mut kept_options = selected_options;
    let mut option_gates = Vec::new();
    for option in WordIndexOption::ALL {
        let swept = cells.iter().any(|cell| !option.is_baseline(&cell.options));
        let comparison = match selected_options {
            _ if !swept => OptionComparison::Unavailable(format!(
                "the grid does not sweep {}: every cell holds its baseline",
                option.name()
            )),
            None => OptionComparison::Unavailable(
                "no selection-fold case has file truth to choose a grid cell by".to_string(),
            ),
            Some(_) if headline.is_empty() => {
                OptionComparison::Unavailable("no headline-fold case was evaluated".to_string())
            }
            Some(options) if option.is_baseline(&options) => OptionComparison::AtBaseline,
            Some(options) => match cell_of(&option.reset(options)) {
                Some(baseline) => OptionComparison::Cells {
                    selected: selected_cell.unwrap_or_default(),
                    baseline,
                },
                None => OptionComparison::Unavailable(format!(
                    "the grid has no cell equal to the chosen one with {} at its baseline",
                    option.name()
                )),
            },
        };
        let gate = readout::gate_four(
            headline.as_slice(),
            option,
            selected_options.as_ref(),
            comparison,
            k,
        );
        if gate.status != GateStatus::Passed {
            kept_options = kept_options.map(|options| option.reset(options));
        }
        option_gates.push(gate);
    }
    let kept_cell = kept_options.as_ref().and_then(cell_of);
    let unavailable = match (selected_cell, kept_cell) {
        (None, _) => Some("no selection-fold case has file truth to choose a grid cell by"),
        (Some(_), _) if headline.is_empty() => Some("no headline-fold case was evaluated"),
        (Some(_), None) => Some("the configuration G4 keeps is not a cell of the grid"),
        (Some(_), Some(_)) => None,
    };
    let source = WordIndexSource::Cell(kept_cell.unwrap_or_default());
    let gates = GateReadout {
        scope: "headline_folds".to_string(),
        word_index_options: kept_cell.map(|cell| cells[cell].options),
        selected_word_index_options: selected_options,
        cases: headline.len(),
        results: readout::gates(headline.as_slice(), source, k, unavailable)
            .into_iter()
            .chain(option_gates)
            .collect(),
    };
    let fold_ids = |range: std::ops::Range<usize>| {
        folds[range]
            .iter()
            .map(|fold| fold.id.clone())
            .collect::<Vec<_>>()
    };
    let report = SweepReport {
        selection_folds: fold_ids(0..grid.selection_folds),
        headline_folds: fold_ids(grid.selection_folds..folds.len()),
        selection_cases: selection.len(),
        headline_cases: headline.len(),
        selection_rule: "highest word_index mean file recall@k on the selection folds, then mean symbol recall@k, then the earlier cell".to_string(),
        cells,
        selected_cell,
        kept_cell,
        headline: kept_cell.map(|_| readout::readout(headline.as_slice(), source, k)),
    };
    (report, gates)
}

fn validate_corpus(corpus: &EvaluationCorpus) -> Result<(), GraphError> {
    if corpus.schema_version != EVALUATION_CORPUS_SCHEMA_VERSION {
        return Err(GraphError::invalid_input(
            "validate evaluation schema",
            "schema_version",
            format!(
                "expected schema_version {EVALUATION_CORPUS_SCHEMA_VERSION}, got {}",
                corpus.schema_version
            ),
        ));
    }
    if corpus.repository.trim().is_empty()
        || corpus.landing_branch.trim().is_empty()
        || corpus.source.system.trim().is_empty()
        || corpus.k == 0
        || corpus.k > 100
    {
        return Err(GraphError::invalid_input(
            "validate evaluation corpus",
            "corpus",
            "repository, landing_branch, source.system, and 1..=100 k are required",
        ));
    }
    if let Some(options) = corpus.word_index.as_ref() {
        options.validate()?;
    }
    Ok(())
}

fn validate_case(
    repo_root: &Path,
    corpus: &EvaluationCorpus,
    case: &EvaluationCase,
) -> Result<Vec<String>, GraphError> {
    let mut exclusions = Vec::new();
    if case.id.trim().is_empty() || case.source.system.trim().is_empty() {
        exclusions.push("missing_case_id_or_provenance".to_string());
    }
    if validate_task_association(&case.task_snapshot).is_err() {
        exclusions.push("invalid_task_snapshot".to_string());
    }
    let cutoff = parse_timestamp("evaluation cutoff", case.cutoff.as_str())?;
    let snapshot_time = case
        .task_snapshot
        .snapshot_available_at
        .timestamp
        .as_deref()
        .map(|value| parse_timestamp("evaluation snapshot availability", value))
        .transpose()?;
    if case.task_snapshot.snapshot_available_at.status != TemporalStatus::Known
        || snapshot_time.is_none_or(|time| time >= cutoff)
        || case.task_snapshot.text_availability != crate::TaskTextAvailability::KnownPreExecution
    {
        exclusions.push("task_text_not_attested_strictly_before_cutoff".to_string());
    }
    if !case.hybrid_hits.is_empty() {
        let observed = case
            .hybrid_hits_observed_at
            .as_ref()
            .and_then(|fact| {
                (fact.status == TemporalStatus::Known)
                    .then_some(fact.timestamp.as_deref())
                    .flatten()
            })
            .map(|value| parse_timestamp("hybrid hit observation", value))
            .transpose()?;
        if observed.is_none_or(|time| time >= cutoff)
            || case
                .hybrid_hits_source
                .as_ref()
                .is_none_or(|source| source.system.trim().is_empty())
        {
            exclusions.push("hybrid_hits_not_attested_strictly_before_cutoff".to_string());
        }
    }
    let Some(held_out) = case.held_out_delivery.as_ref() else {
        exclusions.push("held_out_delivery_unavailable".to_string());
        return Ok(exclusions);
    };
    if held_out.evidence != DeliveryEvidence::VerifiedDelivery {
        exclusions.push("held_out_delivery_not_verified".to_string());
    }
    let delivered_after_cutoff = if held_out.delivered_at.status == TemporalStatus::Known {
        held_out
            .delivered_at
            .timestamp
            .as_deref()
            .map(|value| parse_timestamp("held-out delivery timestamp", value))
            .transpose()?
            .is_some_and(|time| time > cutoff)
    } else {
        false
    };
    let known_delivery_not_after_cutoff =
        held_out.delivered_at.status == TemporalStatus::Known && !delivered_after_cutoff;
    let prospective_after_cutoff = case
        .prospective_delivery_lower_bound
        .as_ref()
        .filter(|fact| fact.status == TemporalStatus::Known)
        .and_then(|fact| fact.timestamp.as_deref())
        .map(|value| parse_timestamp("prospective delivery lower bound", value))
        .transpose()?
        .is_some_and(|time| time > cutoff);
    // A known delivery time is authoritative. If it predates (or equals) the
    // cutoff, reject even when a prospective lower bound claims a later start —
    // that combination is contradictory chronology, not proof of post-cutoff
    // delivery. Prospective lower bounds only admit cases whose landing time is
    // uncertain/unknown.
    if known_delivery_not_after_cutoff && prospective_after_cutoff {
        exclusions.push("contradictory_delivery_chronology".to_string());
    } else if !delivered_after_cutoff && !prospective_after_cutoff {
        exclusions.push("held_out_delivery_not_proven_after_cutoff".to_string());
    }
    if !held_out
        .tasks
        .iter()
        .any(|task| task.task_id == case.task_snapshot.task_id)
    {
        exclusions.push("held_out_delivery_task_mismatch".to_string());
    }
    if corpus.training_deliveries.iter().any(|delivery| {
        delivery.before_revision == held_out.before_revision
            && delivery.after_revision == held_out.after_revision
    }) {
        exclusions.push("target_delivery_already_present_in_training_index".to_string());
    }
    if corpus.training_deliveries.iter().any(|delivery| {
        delivery
            .tasks
            .iter()
            .any(|task| task.task_id == case.task_snapshot.task_id)
    }) {
        exclusions.push("target_task_delivery_present_in_training_index".to_string());
    }
    let repo = Repository::open(repo_root)
        .map_err(|error| GraphError::git("open evaluation repository", error))?;
    let target = Oid::from_str(case.target_revision.as_str())
        .map_err(|error| GraphError::git("parse evaluation target revision", error))?;
    let before = Oid::from_str(held_out.before_revision.as_str())
        .map_err(|error| GraphError::git("parse held-out before revision", error))?;
    if target != before
        && !repo
            .graph_descendant_of(before, target)
            .map_err(|error| GraphError::git("check prospective revision ancestry", error))?
    {
        exclusions.push("target_revision_not_ancestor_of_held_out_base".to_string());
    }
    if held_out.repository != corpus.repository
        || held_out.landing_branch.trim_start_matches("refs/heads/")
            != corpus.landing_branch.trim_start_matches("refs/heads/")
    {
        exclusions.push("held_out_delivery_scope_mismatch".to_string());
    }
    Ok(exclusions)
}

fn held_out_truth(tree: &CodeTree, delivery: &crate::DeliveredChange) -> TruthOutcome {
    let mut files = BTreeSet::new();
    let mut symbols = BTreeSet::new();
    let mut coverage = TruthCoverage::default();
    for file in &delivery.files {
        coverage.file_changes_total += 1;
        if let Some(path) = file.old_path.as_deref() {
            let selector = format!("file:{path}");
            if tree.is_live(selector.as_str()) {
                files.insert(selector);
                coverage.file_truth_eligible += 1;
            } else {
                increment(
                    &mut coverage.file_truth_omitted,
                    "old_path_unresolvable_at_target",
                );
            }
        } else {
            increment(
                &mut coverage.file_truth_omitted,
                "added_file_absent_at_target",
            );
        }
        if file.symbols.is_empty() {
            coverage.symbol_changes_total += 1;
            let reason = file
                .before_fallback
                .map(|reason| format!("symbol_truth_unavailable_{}", reason.as_str()))
                .unwrap_or_else(|| "symbol_truth_unavailable_no_attribution".to_string());
            increment(&mut coverage.symbol_truth_omitted, reason.as_str());
        }
        for change in &file.symbols {
            coverage.symbol_changes_total += 1;
            if let Some(before) = change.before.as_ref() {
                let symbol = &before.symbol;
                let selector = format!(
                    "symbol:{}#{}:{}",
                    symbol.file_path, symbol.qualified, symbol.kind
                );
                if tree.is_live(selector.as_str()) {
                    symbols.insert(selector);
                    coverage.symbol_truth_eligible += 1;
                } else {
                    increment(
                        &mut coverage.symbol_truth_omitted,
                        "before_symbol_unresolvable_at_target",
                    );
                }
            } else {
                increment(
                    &mut coverage.symbol_truth_omitted,
                    "added_symbol_absent_at_target",
                );
            }
        }
    }
    TruthOutcome {
        truth: [files, symbols],
        coverage,
    }
}

struct TruthOutcome {
    /// Truth by level ([`level_index`]).
    truth: [BTreeSet<String>; 2],
    coverage: TruthCoverage,
}

fn increment(counts: &mut BTreeMap<String, usize>, reason: &str) {
    *counts.entry(reason.to_string()).or_default() += 1;
}

fn level_name(level: RecommendationLevel) -> &'static str {
    match level {
        RecommendationLevel::File => "file",
        RecommendationLevel::Symbol => "symbol",
    }
}

/// An isolated clone for one evaluation case.
///
/// The clone lives in a fresh directory that `tempfile` creates under the
/// system temporary directory with a random name and mode `0700`
/// (`STD-05 §R8`), so no other user can pre-create, read or redirect it, and
/// dropping the workspace removes only that directory.
struct EvaluationWorkspace {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl EvaluationWorkspace {
    fn create(
        source: &Path,
        repository_identity: &str,
        landing_branch: &str,
        target_revision: &str,
    ) -> Result<Self, GraphError> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("orbit-graph-evaluation-");
        let temp_dir = crate::runtime::runtime().temp_dir.as_deref();
        let dir = match temp_dir {
            Some(temp_dir) => builder.tempdir_in(temp_dir),
            None => builder.tempdir(),
        }
        .map_err(|source| {
            GraphError::io(
                "create evaluation workspace",
                temp_dir.unwrap_or(Path::new("the system temporary directory")),
                source,
            )
        })?;
        let path = dir.path().join("repository");
        let workspace = Self { _dir: dir, path };
        let source_text = source.to_str().ok_or_else(|| {
            GraphError::invalid_data(
                "clone evaluation repository",
                "repository path is not UTF-8",
            )
        })?;
        let repo = Repository::clone(source_text, workspace.path())
            .map_err(|error| GraphError::git("clone isolated evaluation repository", error))?;
        Repository::remote_set_url(&repo, "origin", repository_identity)
            .map_err(|error| GraphError::git("set isolated evaluation identity", error))?;
        let source_repo = Repository::open(source)
            .map_err(|error| GraphError::git("open evaluation source repository", error))?;
        let branch = landing_branch.trim_start_matches("refs/heads/");
        let branch_tip = source_repo
            .revparse_single(format!("refs/heads/{branch}").as_str())
            .and_then(|object| object.peel_to_commit())
            .map(|commit| commit.id())
            .map_err(|error| GraphError::git("resolve evaluation landing branch", error))?;
        repo.reference(
            format!("refs/heads/{branch}").as_str(),
            branch_tip,
            true,
            "frozen evaluation landing branch",
        )
        .map_err(|error| GraphError::git("create isolated evaluation branch", error))?;
        let target = Oid::from_str(target_revision)
            .map_err(|error| GraphError::git("parse evaluation target revision", error))?;
        let target_object = repo.find_object(target, None).map_err(|error| {
            GraphError::invalid_data("load isolated evaluation target", error.to_string())
        })?;
        repo.set_head_detached(target)
            .map_err(|error| GraphError::git("detach isolated evaluation HEAD", error))?;
        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.force().remove_untracked(true);
        repo.checkout_tree(&target_object, Some(&mut checkout))
            .map_err(|error| GraphError::git("materialize isolated target tree", error))?;
        drop(target_object);
        drop(repo);
        Ok(workspace)
    }

    fn path(&self) -> &Path {
        self.path.as_path()
    }
}

#[cfg(test)]
mod tests;

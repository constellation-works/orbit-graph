//! Leakage-safe chronological evaluation over versioned public delivery data.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use git2::{Oid, Repository};
use serde::{Deserialize, Serialize};

use crate::extract::history::{parse_timestamp, repository_identity, validate_task_association};
use crate::recommend::selector_is_live_at;
use crate::{
    DeliveryEvidence, DeliveryImport, Graph, GraphError, HistoryIndex, HybridTaskHit, Provenance,
    RecommendationEngine, RecommendationInput, RecommendationLevel, RecommendationRequest,
    RecommendationVariant, SyncMode, SyncPolicy, TaskAssociation, TemporalFact, TemporalStatus,
};

static EVALUATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Version of the chronological evaluation JSON contract.
pub const EVALUATION_SCHEMA_VERSION: u32 = 1;

/// Reproducible evaluation input composed only of public envelopes and observations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationCorpus {
    /// Must equal [`EVALUATION_SCHEMA_VERSION`].
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
    /// One row for every ranking variant and destination level.
    pub metrics: Vec<EvaluationMetrics>,
    /// Per-case admission or exclusion evidence.
    pub cases: Vec<EvaluationCaseResult>,
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

/// Aggregate metrics for one ranking variant and destination level.
#[derive(Debug, Clone, Serialize)]
pub struct EvaluationMetrics {
    /// Ranking strategy.
    pub variant: RecommendationVariant,
    /// File or symbol destination level.
    pub level: RecommendationLevel,
    /// Requested K.
    pub k: usize,
    /// Cases contributing truth.
    pub cases: usize,
    /// Relevant destinations in held-out truth.
    pub relevant: usize,
    /// Correct recommendations within K.
    pub true_positives: usize,
    /// Micro-averaged recall at K.
    pub recall_at_k: f64,
    /// Precision at K, with K slots per evaluated case as denominator.
    pub precision_at_k: f64,
    /// Recommendations absent from the immutable target tree divided by returned results.
    pub stale_result_rate: f64,
    /// Mean query latency in milliseconds.
    pub mean_latency_ms: f64,
    /// Maximum query latency in milliseconds.
    pub max_latency_ms: f64,
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
    /// Held-out delivery identifier when available.
    pub held_out_delivery_id: Option<String>,
    /// Frozen target-tree graph provenance used by graph-bearing variants.
    #[serde(skip_serializing_if = "Option::is_none")]
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

#[derive(Debug, Default)]
struct MetricAccumulator {
    cases: usize,
    relevant: usize,
    true_positives: usize,
    predictions: usize,
    stale: usize,
    latency_micros: Vec<u128>,
}

/// Evaluate all four ranking variants without importing held-out deliveries.
pub fn evaluate_corpus(
    repo_root: &Path,
    corpus: &EvaluationCorpus,
) -> Result<EvaluationReport, GraphError> {
    validate_corpus(corpus)?;
    let source_repo = Repository::open(repo_root).map_err(|error| {
        GraphError::invalid_data("open evaluation repository", error.to_string())
    })?;
    let routed_identity = repository_identity(&source_repo)?;
    if routed_identity != corpus.repository {
        return Err(GraphError::invalid_data(
            "validate evaluation repository routing",
            format!(
                "corpus repository {:?} does not match routed repository {:?}",
                corpus.repository, routed_identity
            ),
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
    let mut aggregates: BTreeMap<(u8, u8), MetricAccumulator> = BTreeMap::new();
    let mut case_results = Vec::new();
    for case in &corpus.cases {
        let mut exclusions = validate_case(repo_root, corpus, case)?;
        let mut truth = BTreeMap::new();
        let mut truth_coverage = TruthCoverage::default();
        let mut graph_structure_applied = BTreeMap::new();
        let mut graph_snapshot = None;
        if exclusions.is_empty() {
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
            if let Some(held_out) = case.held_out_delivery.clone() {
                let extracted = index.preview(held_out)?;
                let outcome =
                    held_out_truth(workspace.path(), case.target_revision.as_str(), &extracted)?;
                truth = outcome.truth;
                truth_coverage = outcome.coverage;
            }
            if truth.values().all(std::collections::BTreeSet::is_empty) {
                exclusions.push("held_out_delivery_has_no_target-resolvable_truth".to_string());
            } else {
                let engine =
                    RecommendationEngine::open(workspace.path(), corpus.landing_branch.as_str())?;
                for variant in variants() {
                    for level in levels() {
                        let started = Instant::now();
                        let result = engine.recommend(&RecommendationRequest {
                            input: RecommendationInput::TaskId(case.task_snapshot.task_id.clone()),
                            level,
                            variant,
                            limit: Some(corpus.k),
                            target_revision: Some(case.target_revision.clone()),
                            cutoff: Some(case.cutoff.clone()),
                            task_snapshot: Some(case.task_snapshot.clone()),
                            hybrid_hits: case.hybrid_hits.clone(),
                        })?;
                        if matches!(
                            variant,
                            RecommendationVariant::Combined | RecommendationVariant::GraphOnly
                        ) {
                            graph_structure_applied.insert(
                                format!("{}:{}", variant_name(variant), level_name(level)),
                                result.structure_applied,
                            );
                        }
                        let elapsed = started.elapsed().as_micros();
                        let expected = truth.get(&level_key(level)).ok_or_else(|| {
                            GraphError::invalid_data(
                                "evaluate held-out truth",
                                "missing initialized destination level",
                            )
                        })?;
                        let accumulator = aggregates
                            .entry((variant_key(variant), level_key(level)))
                            .or_default();
                        accumulator.cases += 1;
                        accumulator.relevant += expected.len();
                        accumulator.latency_micros.push(elapsed);
                        for recommendation in &result.recommendations {
                            accumulator.predictions += 1;
                            if expected.contains(recommendation.selector.as_str()) {
                                accumulator.true_positives += 1;
                            }
                            if !selector_is_live_at(
                                workspace.path(),
                                case.target_revision.as_str(),
                                recommendation.selector.as_str(),
                            )? {
                                accumulator.stale += 1;
                            }
                        }
                    }
                }
            }
        }
        let evaluated = exclusions.is_empty();
        case_results.push(EvaluationCaseResult {
            id: case.id.clone(),
            evaluated,
            exclusions,
            target_revision: case.target_revision.clone(),
            held_out_delivery_id: case
                .held_out_delivery
                .as_ref()
                .map(|delivery| delivery.delivery_id.clone()),
            graph_snapshot,
            graph_structure_applied,
            truth_coverage,
            source: case.source.clone(),
        });
    }

    let evaluated = case_results.iter().filter(|case| case.evaluated).count();
    let mut metrics = Vec::new();
    for variant in variants() {
        for level in levels() {
            let value = aggregates
                .remove(&(variant_key(variant), level_key(level)))
                .unwrap_or_default();
            let latency_total: u128 = value.latency_micros.iter().sum();
            metrics.push(EvaluationMetrics {
                variant,
                level,
                k: corpus.k,
                cases: value.cases,
                relevant: value.relevant,
                true_positives: value.true_positives,
                recall_at_k: ratio(value.true_positives, value.relevant),
                precision_at_k: ratio(value.true_positives, value.cases * corpus.k),
                stale_result_rate: ratio(value.stale, value.predictions),
                mean_latency_ms: if value.latency_micros.is_empty() {
                    0.0
                } else {
                    latency_total as f64 / value.latency_micros.len() as f64 / 1_000.0
                },
                max_latency_ms: value.latency_micros.iter().max().copied().unwrap_or(0) as f64
                    / 1_000.0,
            });
        }
    }
    let revision = source_repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map(|commit| commit.id().to_string())
        .map_err(|error| GraphError::invalid_data("resolve evaluation HEAD", error.to_string()))?;
    Ok(EvaluationReport {
        schema_version: EVALUATION_SCHEMA_VERSION,
        repository: corpus.repository.clone(),
        evaluated_at_revision: revision,
        source: corpus.source.clone(),
        corpus_digest: digest,
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
        metrics,
        cases: case_results,
    })
}

fn validate_corpus(corpus: &EvaluationCorpus) -> Result<(), GraphError> {
    if corpus.schema_version != EVALUATION_SCHEMA_VERSION {
        return Err(GraphError::invalid_data(
            "validate evaluation schema",
            format!(
                "expected schema_version {EVALUATION_SCHEMA_VERSION}, got {}",
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
        return Err(GraphError::invalid_data(
            "validate evaluation corpus",
            "repository, landing_branch, source.system, and 1..=100 k are required",
        ));
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
    let prospective_after_cutoff = case
        .prospective_delivery_lower_bound
        .as_ref()
        .filter(|fact| fact.status == TemporalStatus::Known)
        .and_then(|fact| fact.timestamp.as_deref())
        .map(|value| parse_timestamp("prospective delivery lower bound", value))
        .transpose()?
        .is_some_and(|time| time > cutoff);
    if !delivered_after_cutoff && !prospective_after_cutoff {
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
    let repo = Repository::open(repo_root).map_err(|error| {
        GraphError::invalid_data("open evaluation repository", error.to_string())
    })?;
    let target = Oid::from_str(case.target_revision.as_str()).map_err(|error| {
        GraphError::invalid_data("parse evaluation target revision", error.to_string())
    })?;
    let before = Oid::from_str(held_out.before_revision.as_str()).map_err(|error| {
        GraphError::invalid_data("parse held-out before revision", error.to_string())
    })?;
    if target != before
        && !repo.graph_descendant_of(before, target).map_err(|error| {
            GraphError::invalid_data("check prospective revision ancestry", error.to_string())
        })?
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

fn held_out_truth(
    repo_root: &Path,
    target_revision: &str,
    delivery: &crate::DeliveredChange,
) -> Result<TruthOutcome, GraphError> {
    let mut files = BTreeSet::new();
    let mut symbols = BTreeSet::new();
    let mut coverage = TruthCoverage::default();
    for file in &delivery.files {
        coverage.file_changes_total += 1;
        if let Some(path) = file.old_path.as_deref() {
            let selector = format!("file:{path}");
            if selector_is_live_at(repo_root, target_revision, selector.as_str())? {
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
                if selector_is_live_at(repo_root, target_revision, selector.as_str())? {
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
    Ok(TruthOutcome {
        truth: BTreeMap::from([
            (level_key(RecommendationLevel::File), files),
            (level_key(RecommendationLevel::Symbol), symbols),
        ]),
        coverage,
    })
}

struct TruthOutcome {
    truth: BTreeMap<u8, BTreeSet<String>>,
    coverage: TruthCoverage,
}

fn increment(counts: &mut BTreeMap<String, usize>, reason: &str) {
    *counts.entry(reason.to_string()).or_default() += 1;
}

fn variants() -> [RecommendationVariant; 4] {
    [
        RecommendationVariant::Combined,
        RecommendationVariant::TaskSearchOnly,
        RecommendationVariant::GraphOnly,
        RecommendationVariant::Frequency,
    ]
}

fn levels() -> [RecommendationLevel; 2] {
    [RecommendationLevel::File, RecommendationLevel::Symbol]
}

fn variant_key(variant: RecommendationVariant) -> u8 {
    match variant {
        RecommendationVariant::Combined => 0,
        RecommendationVariant::TaskSearchOnly => 1,
        RecommendationVariant::GraphOnly => 2,
        RecommendationVariant::Frequency => 3,
    }
}

fn level_key(level: RecommendationLevel) -> u8 {
    match level {
        RecommendationLevel::File => 0,
        RecommendationLevel::Symbol => 1,
    }
}

fn variant_name(variant: RecommendationVariant) -> &'static str {
    match variant {
        RecommendationVariant::Combined => "combined",
        RecommendationVariant::TaskSearchOnly => "task_search_only",
        RecommendationVariant::GraphOnly => "graph_only",
        RecommendationVariant::Frequency => "frequency",
    }
}

fn level_name(level: RecommendationLevel) -> &'static str {
    match level {
        RecommendationLevel::File => "file",
        RecommendationLevel::Symbol => "symbol",
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

struct EvaluationWorkspace {
    path: PathBuf,
}

impl EvaluationWorkspace {
    fn create(
        source: &Path,
        repository_identity: &str,
        landing_branch: &str,
        target_revision: &str,
    ) -> Result<Self, GraphError> {
        let sequence = EVALUATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "orbit-graph-evaluation-{}-{sequence}",
            std::process::id()
        ));
        let workspace = Self { path };
        let source_text = source.to_str().ok_or_else(|| {
            GraphError::invalid_data(
                "clone evaluation repository",
                "repository path is not UTF-8",
            )
        })?;
        let repo = Repository::clone(source_text, workspace.path()).map_err(|error| {
            GraphError::invalid_data("clone isolated evaluation repository", error.to_string())
        })?;
        Repository::remote_set_url(&repo, "origin", repository_identity).map_err(|error| {
            GraphError::invalid_data("set isolated evaluation identity", error.to_string())
        })?;
        let source_repo = Repository::open(source).map_err(|error| {
            GraphError::invalid_data("open evaluation source repository", error.to_string())
        })?;
        let branch = landing_branch.trim_start_matches("refs/heads/");
        let branch_tip = source_repo
            .revparse_single(format!("refs/heads/{branch}").as_str())
            .and_then(|object| object.peel_to_commit())
            .map(|commit| commit.id())
            .map_err(|error| {
                GraphError::invalid_data("resolve evaluation landing branch", error.to_string())
            })?;
        repo.reference(
            format!("refs/heads/{branch}").as_str(),
            branch_tip,
            true,
            "frozen evaluation landing branch",
        )
        .map_err(|error| {
            GraphError::invalid_data("create isolated evaluation branch", error.to_string())
        })?;
        let target = Oid::from_str(target_revision).map_err(|error| {
            GraphError::invalid_data("parse evaluation target revision", error.to_string())
        })?;
        let target_object = repo.find_object(target, None).map_err(|error| {
            GraphError::invalid_data("load isolated evaluation target", error.to_string())
        })?;
        repo.set_head_detached(target).map_err(|error| {
            GraphError::invalid_data("detach isolated evaluation HEAD", error.to_string())
        })?;
        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.force().remove_untracked(true);
        repo.checkout_tree(&target_object, Some(&mut checkout))
            .map_err(|error| {
                GraphError::invalid_data("materialize isolated target tree", error.to_string())
            })?;
        drop(target_object);
        drop(repo);
        Ok(workspace)
    }

    fn path(&self) -> &Path {
        self.path.as_path()
    }
}

impl Drop for EvaluationWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.path.as_path());
    }
}

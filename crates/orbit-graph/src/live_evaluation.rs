//! Leakage-safe live evaluation of Git-only commit-text relevance.
//!
//! Each held-out commit `C` on a landing branch's first-parent chain is scored
//! by querying `C`'s subject at target `C^` with no chronological cutoff, which
//! is the only mode that reads commit text. The ancestry filter must exclude
//! `C` and every later delivery; this module checks that exclusion against the
//! indexed delivery ids instead of assuming it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;
use std::time::Instant;

use git2::{ObjectType, Oid, Repository};
use orbit_graph_extract::history::branch_tip;

use crate::recommend::{
    COMMIT_TEXT_EXPONENT, COMMIT_TEXT_WEIGHT, MAX_RECOMMENDATION_LIMIT, subject_cites_task_id,
};
use crate::{
    DeliveredChange, FileChangeKind, GraphError, HistoryIndex, RecommendationEngine,
    RecommendationInput, RecommendationLevel, RecommendationRequest, RecommendationResult,
    RecommendationVariant,
};

/// Version of the live Git evaluation report.
pub const LIVE_GIT_EVALUATION_SCHEMA_VERSION: u32 = 1;

/// Parameters for [`evaluate_live_git`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveGitEvaluation {
    /// Landing branch whose history index is read.
    pub branch: String,
    /// Walk start. `None` is the branch tip.
    pub revision: Option<String>,
    /// Maximum held-out commits, newest first.
    pub limit: usize,
    /// Precision, recall, and MRR cutoff. Must be `1..=100`.
    pub k: usize,
}

/// One commit-text scoring point. Weight zero is the no-text variant.
struct VariantSpec {
    name: &'static str,
    weight: Option<f64>,
    exponent: Option<f64>,
}

const VARIANTS: &[VariantSpec] = &[
    VariantSpec {
        name: "no_commit_text",
        weight: Some(0.0),
        exponent: Some(1.0),
    },
    VariantSpec {
        name: "linear_0.5",
        weight: Some(0.5),
        exponent: Some(1.0),
    },
    VariantSpec {
        name: "squared_0.5",
        weight: Some(0.5),
        exponent: Some(2.0),
    },
    VariantSpec {
        name: "squared_0.25",
        weight: Some(0.25),
        exponent: Some(2.0),
    },
    VariantSpec {
        name: "linear_1.0",
        weight: Some(1.0),
        exponent: Some(1.0),
    },
];

const COHORTS: &[&str] = &["all", "title_restating", "without_title_restating"];

/// Live Git-only commit-text evaluation.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LiveGitReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Discriminator for this report shape (`live_git`).
    pub mode: &'static str,
    /// Repository identity recorded in the history index.
    pub repository: String,
    /// History scope that was read.
    pub landing_branch: String,
    /// Resolved walk start.
    pub resolved_tip: String,
    /// Oldest held-out commit, when any case was evaluated.
    pub oldest_held_out: Option<String>,
    /// Newest held-out commit, when any case was evaluated.
    pub newest_held_out: Option<String>,
    /// Why the walk stopped: `limit_reached`, `history_exhausted`, or `index_exhausted`.
    pub stop_reason: String,
    /// Requested maximum number of held-out commits.
    pub limit_requested: usize,
    /// Precision/recall/MRR cutoff.
    pub k: usize,
    /// Production commit-text weight the binary uses when a request does not override it.
    pub production_weight: f64,
    /// Production commit-text exponent.
    pub production_exponent: f64,
    /// Deliveries stored for this repository and branch.
    pub history_deliveries: usize,
    /// Deliveries stored as Git-only evidence.
    pub history_git_only: usize,
    /// Deliveries stored as verified evidence.
    pub history_verified: usize,
    /// Whether the history cursor is the branch tip with no partial bootstrap.
    pub history_complete: bool,
    /// One-minute load average at the start of scoring, when `/proc/loadavg` is readable.
    pub loadavg_start: Option<f64>,
    /// One-minute load average after scoring.
    pub loadavg_end: Option<f64>,
    /// `std::thread::available_parallelism`, when the host reports it.
    pub available_parallelism: Option<usize>,
    /// Held-out commits whose own delivery was indexed.
    pub held_out_indexed_cases: usize,
    /// Cases included in the leakage check.
    pub cases_evaluated: usize,
    /// Indexed commits skipped because the subject was empty.
    pub skipped_empty_subject: usize,
    /// Supporting ids that belonged to a held-out or later delivery.
    pub leakage_violations: usize,
    /// Scoring points applied to every case.
    pub variants: Vec<LiveGitVariantSpec>,
    /// Aggregate metrics for each cohort and variant.
    pub cohorts: Vec<LiveGitCohort>,
    /// Per-commit queries, truth, and leakage evidence.
    pub cases: Vec<LiveGitCase>,
}

/// A scoring point named in the report.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LiveGitVariantSpec {
    /// Stable variant name.
    pub name: String,
    /// Whether commit text was read.
    pub commit_text: bool,
    /// Weight when commit text is enabled.
    pub weight: Option<f64>,
    /// Exponent when commit text is enabled.
    pub exponent: Option<f64>,
}

/// Metrics for one cohort.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LiveGitCohort {
    /// `all`, `title_restating`, or `without_title_restating`.
    pub name: String,
    /// Evaluated cases in the cohort, including those with no target-live truth.
    pub cases: usize,
    /// Cases that contributed to precision, recall, and MRR.
    pub cases_with_truth: usize,
    /// One row per scoring point.
    pub metrics: Vec<LiveGitMetric>,
}

/// Precision@k, recall@k, and MRR@k for one cohort and variant.
///
/// Precision reserves `k` slots per case. A metric with no cases or no relevant
/// truth is `null`, not zero.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LiveGitMetric {
    /// Cohort name.
    pub cohort: String,
    /// Variant name.
    pub name: String,
    /// Whether this row read commit text.
    pub commit_text: bool,
    /// Weight, or `null` when commit text is disabled.
    pub weight: Option<f64>,
    /// Exponent, or `null` when commit text is disabled.
    pub exponent: Option<f64>,
    /// Cutoff K.
    pub k: usize,
    /// Cases with at least one target-live truth file.
    pub cases: usize,
    /// Target-live truth files across those cases.
    pub relevant: usize,
    /// Truth files that appeared in the top K.
    pub true_positives: usize,
    /// Micro-averaged recall@k. `null` when `relevant` is zero.
    pub recall_at_k: Option<f64>,
    /// Micro-averaged precision@k with K reserved slots. `null` when `cases` is zero.
    pub precision_at_k: Option<f64>,
    /// Mean reciprocal rank of the first hit within K. Misses contribute zero.
    /// `null` when `cases` is zero.
    pub mrr_at_k: Option<f64>,
    /// Mean recommendation latency in milliseconds. `null` when `cases` is zero.
    pub mean_latency_ms: Option<f64>,
}

/// One held-out commit.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LiveGitCase {
    /// Held-out commit.
    pub commit: String,
    /// Target revision (`commit^`, the first parent).
    pub parent: String,
    /// Query text: the commit subject.
    pub subject: String,
    /// The subject cites a bracketed task id (`[ORB-123]`), the squash-merge marker.
    pub title_restating: bool,
    /// Indexed delivery ids whose after-revision is this commit.
    pub held_out_delivery_ids: Vec<String>,
    /// True when at least one such delivery was in the index.
    pub held_out_indexed: bool,
    /// Indexed delivery ids that are not ancestors of `parent` and are not `parent`.
    pub prohibited_delivery_ids: Vec<String>,
    /// Changed files that exist at `parent`.
    pub truth: Vec<String>,
    /// Changed files omitted from `truth`, by reason.
    pub truth_omitted: BTreeMap<String, usize>,
    /// Prohibited ids that still appeared in a recommendation. Empty when the filter held.
    pub leakage: Vec<String>,
    /// Rankings for each variant.
    pub variants: Vec<LiveGitCaseVariant>,
}

/// One variant's ranking of a held-out commit.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LiveGitCaseVariant {
    /// Variant name.
    pub name: String,
    /// Ranked selectors, best first, at most K.
    pub ranked: Vec<String>,
    /// Delivery ids cited by supporting ids or reason text.
    pub evidence_delivery_ids: Vec<String>,
    /// Truth files among `ranked`.
    pub hits_at_k: usize,
    /// Reciprocal rank of the first hit, or zero when nothing hit. `null` without truth.
    pub reciprocal_rank: Option<f64>,
    /// Wall time of this recommendation.
    pub latency_ms: f64,
}

#[derive(Default)]
struct MetricAccumulator {
    cases: usize,
    relevant: usize,
    true_positives: usize,
    reciprocal_rank: f64,
    latency_ms: f64,
}

impl MetricAccumulator {
    fn add(&mut self, relevant: usize, hits: usize, reciprocal: f64, latency_ms: f64) {
        self.cases += 1;
        self.relevant += relevant;
        self.true_positives += hits;
        self.reciprocal_rank += reciprocal;
        self.latency_ms += latency_ms;
    }

    fn finish(&self, cohort: &str, spec: &VariantSpec, k: usize) -> LiveGitMetric {
        let enabled = spec.weight.is_some_and(|weight| weight > 0.0);
        LiveGitMetric {
            cohort: cohort.to_string(),
            name: spec.name.to_string(),
            commit_text: enabled,
            weight: spec.weight.filter(|weight| *weight > 0.0),
            exponent: enabled.then_some(spec.exponent).flatten(),
            k,
            cases: self.cases,
            relevant: self.relevant,
            true_positives: self.true_positives,
            recall_at_k: (self.relevant > 0)
                .then(|| self.true_positives as f64 / self.relevant as f64),
            precision_at_k: (self.cases > 0)
                .then(|| self.true_positives as f64 / (self.cases * k) as f64),
            mrr_at_k: (self.cases > 0).then(|| self.reciprocal_rank / self.cases as f64),
            mean_latency_ms: (self.cases > 0).then(|| self.latency_ms / self.cases as f64),
        }
    }
}

/// Hold out first-parent commits and compare commit-text scoring points.
///
/// Reads an existing history index and does not import deliveries. Fails when
/// no held-out commit is indexed, or when any recommendation cites a delivery
/// that is not an ancestor of the target.
pub fn evaluate_live_git(
    repo_root: &Path,
    request: &LiveGitEvaluation,
) -> Result<LiveGitReport, GraphError> {
    validate_request(request)?;
    let repo = Repository::open(repo_root).map_err(|error| {
        GraphError::invalid_data("open repository for live evaluation", error.to_string())
    })?;
    let start = match request.revision.as_deref() {
        Some(revision) => resolve_revision(&repo, revision)?,
        None => branch_tip(&repo, request.branch.trim())?,
    };
    let index = HistoryIndex::open_read_only(repo_root, request.branch.trim())?;
    let status = index.status()?;
    let deliveries = index.deliveries()?;
    drop(index);
    let by_after = index_by_after(deliveries.as_slice());
    let known_delivery_ids = deliveries
        .iter()
        .map(|delivery| delivery.delivery.delivery_id.clone())
        .collect::<BTreeSet<_>>();
    let order = first_parent_order(&repo, start)?;
    let position = order
        .iter()
        .enumerate()
        .map(|(index, oid)| (*oid, index))
        .collect::<HashMap<_, _>>();
    let loadavg_start = read_loadavg();
    let engine = RecommendationEngine::open(repo_root, request.branch.trim())?;

    let mut cases = Vec::new();
    let mut skipped_empty_subject = 0_usize;
    let mut stop_reason = "history_exhausted".to_string();
    for oid in &order {
        if cases.len() == request.limit {
            stop_reason = "limit_reached".to_string();
            break;
        }
        let commit = repo
            .find_commit(*oid)
            .map_err(git_error("load held-out commit"))?;
        if commit.parent_count() == 0 {
            stop_reason = "history_exhausted".to_string();
            break;
        }
        let parent = commit
            .parent_id(0)
            .map_err(git_error("load held-out parent"))?;
        let Some(delivery_indexes) = by_after.get(&oid.to_string()) else {
            stop_reason = "index_exhausted".to_string();
            break;
        };
        let subject = commit_subject(&commit);
        if subject.is_empty() {
            skipped_empty_subject += 1;
            continue;
        }
        cases.push(evaluate_case(
            &CaseScope {
                repo: &repo,
                engine: &engine,
                deliveries: deliveries.as_slice(),
                position: &position,
                known_delivery_ids: &known_delivery_ids,
                k: request.k,
            },
            *oid,
            parent,
            subject,
            delivery_indexes,
        )?);
    }

    if cases.is_empty() {
        return Err(GraphError::invalid_data(
            "evaluate live git history",
            format!(
                "no held-out commit was indexed on {} at {start}; run `orbit-graph history sync --branch {}` through that revision before evaluate --live (stop: {stop_reason})",
                request.branch.trim(),
                request.branch.trim(),
            ),
        ));
    }

    let leakage_violations = cases.iter().map(|case| case.leakage.len()).sum::<usize>();
    let held_out_indexed_cases = cases.iter().filter(|case| case.held_out_indexed).count();
    let cohorts = COHORTS
        .iter()
        .map(|name| cohort_report(name, request.k, cases.as_slice()))
        .collect::<Vec<_>>();
    let report = LiveGitReport {
        schema_version: LIVE_GIT_EVALUATION_SCHEMA_VERSION,
        mode: "live_git",
        repository: status.repository,
        landing_branch: status.landing_branch,
        resolved_tip: start.to_string(),
        oldest_held_out: cases.last().map(|case| case.commit.clone()),
        newest_held_out: cases.first().map(|case| case.commit.clone()),
        stop_reason,
        limit_requested: request.limit,
        k: request.k,
        production_weight: COMMIT_TEXT_WEIGHT,
        production_exponent: COMMIT_TEXT_EXPONENT,
        history_deliveries: status.deliveries,
        history_git_only: status.git_only_deliveries,
        history_verified: status.verified_deliveries,
        history_complete: status.complete,
        loadavg_start,
        loadavg_end: read_loadavg(),
        available_parallelism: std::thread::available_parallelism().ok().map(|n| n.get()),
        held_out_indexed_cases,
        cases_evaluated: cases.len(),
        skipped_empty_subject,
        leakage_violations,
        variants: VARIANTS
            .iter()
            .map(|spec| {
                let enabled = spec.weight.is_some_and(|weight| weight > 0.0);
                LiveGitVariantSpec {
                    name: spec.name.to_string(),
                    commit_text: enabled,
                    weight: spec.weight.filter(|weight| *weight > 0.0),
                    exponent: enabled.then_some(spec.exponent).flatten(),
                }
            })
            .collect(),
        cohorts,
        cases,
    };
    if leakage_violations > 0 {
        let sample = report
            .cases
            .iter()
            .find_map(|case| case.leakage.first())
            .map(|item| format!("; first: {item}"))
            .unwrap_or_default();
        return Err(GraphError::invalid_data(
            "evaluate live git history",
            format!(
                "{leakage_violations} held-out or later delivery id(s) contributed recommendation evidence{sample}"
            ),
        ));
    }
    Ok(report)
}

fn validate_request(request: &LiveGitEvaluation) -> Result<(), GraphError> {
    if request.branch.trim().is_empty() {
        return Err(GraphError::invalid_data(
            "validate live evaluation",
            "branch must be non-empty",
        ));
    }
    if request.limit == 0 || request.limit > 5_000 {
        return Err(GraphError::invalid_data(
            "validate live evaluation",
            "limit must be between 1 and 5000",
        ));
    }
    if !(1..=MAX_RECOMMENDATION_LIMIT).contains(&request.k) {
        return Err(GraphError::invalid_data(
            "validate live evaluation",
            format!("k must be between 1 and {MAX_RECOMMENDATION_LIMIT}"),
        ));
    }
    Ok(())
}

fn resolve_revision(repo: &Repository, revision: &str) -> Result<Oid, GraphError> {
    repo.revparse_single(revision)
        .and_then(|object| object.peel_to_commit())
        .map(|commit| commit.id())
        .map_err(|error| {
            GraphError::invalid_data(
                "resolve live evaluation revision",
                format!("{revision}: {error}"),
            )
        })
}

fn first_parent_order(repo: &Repository, start: Oid) -> Result<Vec<Oid>, GraphError> {
    let mut order = Vec::new();
    let mut current = start;
    loop {
        order.push(current);
        let commit = repo
            .find_commit(current)
            .map_err(git_error("walk first-parent history"))?;
        if commit.parent_count() == 0 || order.len() == 100_000 {
            break;
        }
        current = commit
            .parent_id(0)
            .map_err(git_error("walk first-parent history"))?;
    }
    Ok(order)
}

fn index_by_after(deliveries: &[DeliveredChange]) -> HashMap<String, Vec<usize>> {
    let mut by_after: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, delivery) in deliveries.iter().enumerate() {
        by_after
            .entry(delivery.delivery.after_revision.clone())
            .or_default()
            .push(index);
    }
    by_after
}

fn commit_subject(commit: &git2::Commit<'_>) -> String {
    let message = String::from_utf8_lossy(commit.message_bytes()).into_owned();
    message
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

struct CaseScope<'a> {
    repo: &'a Repository,
    engine: &'a RecommendationEngine,
    deliveries: &'a [DeliveredChange],
    position: &'a HashMap<Oid, usize>,
    known_delivery_ids: &'a BTreeSet<String>,
    k: usize,
}

fn evaluate_case(
    scope: &CaseScope<'_>,
    commit: Oid,
    parent: Oid,
    subject: String,
    delivery_indexes: &[usize],
) -> Result<LiveGitCase, GraphError> {
    let held_out_delivery_ids = delivery_indexes
        .iter()
        .map(|index| scope.deliveries[*index].delivery.delivery_id.clone())
        .collect::<Vec<_>>();
    let truth_source = delivery_indexes
        .iter()
        .copied()
        .find(|index| scope.deliveries[*index].delivery.delivery_id == format!("git:{commit}"))
        .or_else(|| delivery_indexes.first().copied());
    let (truth, truth_omitted) = match truth_source {
        Some(index) => truth_at_parent(scope.repo, parent, &scope.deliveries[index])?,
        None => (BTreeSet::new(), BTreeMap::new()),
    };
    let prohibited = prohibited_ids(scope.repo, scope.deliveries, scope.position, parent)?;
    let title_restating = subject_cites_task_id(subject.as_str());
    let mut leakage = Vec::new();
    let mut variants = Vec::with_capacity(VARIANTS.len());
    for spec in VARIANTS {
        let started = Instant::now();
        let result = scope.engine.recommend(&RecommendationRequest {
            input: RecommendationInput::Query(subject.clone()),
            level: RecommendationLevel::File,
            variant: RecommendationVariant::Combined,
            limit: Some(scope.k),
            target_revision: Some(parent.to_string()),
            cutoff: None,
            task_snapshot: None,
            hybrid_hits: Vec::new(),
            commit_text_weight: spec.weight,
            commit_text_exponent: spec.exponent,
        })?;
        let latency_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let cited = cited_ids(&result, scope.known_delivery_ids);
        let leaked = cited
            .iter()
            .filter(|id| prohibited.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in &leaked {
            leakage.push(format!("{} cited {id}", spec.name));
        }
        let ranked = result
            .recommendations
            .iter()
            .map(|item| item.selector.clone())
            .collect::<Vec<_>>();
        let hits_at_k = ranked
            .iter()
            .filter(|selector| truth.contains(*selector))
            .count();
        let reciprocal_rank = if truth.is_empty() {
            None
        } else {
            Some(
                ranked
                    .iter()
                    .position(|selector| truth.contains(selector))
                    .map(|index| 1.0 / (index as f64 + 1.0))
                    .unwrap_or(0.0),
            )
        };
        variants.push(LiveGitCaseVariant {
            name: spec.name.to_string(),
            ranked,
            evidence_delivery_ids: cited.into_iter().collect(),
            hits_at_k,
            reciprocal_rank,
            latency_ms,
        });
    }
    Ok(LiveGitCase {
        commit: commit.to_string(),
        parent: parent.to_string(),
        subject,
        title_restating,
        held_out_delivery_ids,
        held_out_indexed: true,
        prohibited_delivery_ids: prohibited.into_iter().collect(),
        truth: truth.into_iter().collect(),
        truth_omitted,
        leakage,
        variants,
    })
}

fn truth_at_parent(
    repo: &Repository,
    parent: Oid,
    change: &DeliveredChange,
) -> Result<(BTreeSet<String>, BTreeMap<String, usize>), GraphError> {
    let tree = repo
        .find_commit(parent)
        .and_then(|commit| commit.tree())
        .map_err(git_error("load live-evaluation target tree"))?;
    let mut truth = BTreeSet::new();
    let mut omitted = BTreeMap::new();
    for file in &change.files {
        let (path, skip) = match file.kind {
            FileChangeKind::Added => (None, Some("added")),
            FileChangeKind::Copied => (None, Some("copied")),
            FileChangeKind::Unreadable => (None, Some("unreadable")),
            FileChangeKind::Modified
            | FileChangeKind::Deleted
            | FileChangeKind::Renamed
            | FileChangeKind::TypeChanged => {
                (file.old_path.as_deref().or(file.new_path.as_deref()), None)
            }
        };
        let Some(path) = path else {
            *omitted
                .entry(skip.unwrap_or("unreadable").to_string())
                .or_default() += 1;
            continue;
        };
        if !tree_has_blob(&tree, path) {
            *omitted.entry("not_live_at_target".to_string()).or_default() += 1;
            continue;
        }
        truth.insert(format!("file:{path}"));
    }
    Ok((truth, omitted))
}

fn tree_has_blob(tree: &git2::Tree<'_>, path: &str) -> bool {
    tree.get_path(Path::new(path))
        .ok()
        .is_some_and(|entry| entry.kind() == Some(ObjectType::Blob))
}

fn prohibited_ids(
    repo: &Repository,
    deliveries: &[DeliveredChange],
    position: &HashMap<Oid, usize>,
    target: Oid,
) -> Result<BTreeSet<String>, GraphError> {
    let target_pos = position.get(&target).copied();
    let mut ids = BTreeSet::new();
    for delivery in deliveries {
        let after = Oid::from_str(delivery.delivery.after_revision.as_str()).map_err(|error| {
            GraphError::invalid_data("parse indexed delivery revision", error.to_string())
        })?;
        let prohibited = match (target_pos, position.get(&after).copied()) {
            (Some(target_pos), Some(after_pos)) => after_pos < target_pos,
            _ => {
                after != target
                    && !repo
                        .graph_descendant_of(target, after)
                        .map_err(git_error("check delivery ancestry"))?
            }
        };
        if prohibited {
            ids.insert(delivery.delivery.delivery_id.clone());
        }
    }
    Ok(ids)
}

fn cited_ids(result: &RecommendationResult, known: &BTreeSet<String>) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for recommendation in &result.recommendations {
        ids.extend(recommendation.supporting_delivery_ids.iter().cloned());
        for reason in &recommendation.reasons {
            for token in reason
                .explanation
                .split(|ch: char| ch.is_whitespace() || ch == ',' || ch == ';')
            {
                if known.contains(token) {
                    ids.insert(token.to_string());
                }
            }
        }
    }
    ids
}

fn cohort_report(name: &str, k: usize, cases: &[LiveGitCase]) -> LiveGitCohort {
    let selected = cases
        .iter()
        .filter(|case| match name {
            "title_restating" => case.title_restating,
            "without_title_restating" => !case.title_restating,
            _ => true,
        })
        .collect::<Vec<_>>();
    let mut accumulators = VARIANTS
        .iter()
        .map(|spec| (spec.name, MetricAccumulator::default()))
        .collect::<BTreeMap<_, _>>();
    let mut cases_with_truth = 0_usize;
    for case in &selected {
        if case.truth.is_empty() {
            continue;
        }
        cases_with_truth += 1;
        for variant in &case.variants {
            let Some(accumulator) = accumulators.get_mut(variant.name.as_str()) else {
                continue;
            };
            accumulator.add(
                case.truth.len(),
                variant.hits_at_k,
                variant.reciprocal_rank.unwrap_or(0.0),
                variant.latency_ms,
            );
        }
    }
    LiveGitCohort {
        name: name.to_string(),
        cases: selected.len(),
        cases_with_truth,
        metrics: VARIANTS
            .iter()
            .filter_map(|spec| {
                accumulators
                    .get(spec.name)
                    .map(|accumulator| accumulator.finish(name, spec, k))
            })
            .collect(),
    }
}

fn read_loadavg() -> Option<f64> {
    let text = fs::read_to_string("/proc/loadavg").ok()?;
    text.split_whitespace().next()?.parse().ok()
}

fn git_error(operation: &'static str) -> impl FnOnce(git2::Error) -> GraphError {
    move |error| GraphError::invalid_data(operation, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precision_reserves_k_and_an_empty_cohort_is_null() {
        let spec = &VARIANTS[2];
        let mut accumulator = MetricAccumulator::default();
        accumulator.add(2, 1, 0.5, 10.0);
        accumulator.add(2, 0, 0.0, 30.0);
        let metric = accumulator.finish("all", spec, 10);
        assert_eq!(metric.cases, 2);
        assert_eq!(metric.relevant, 4);
        assert_eq!(metric.true_positives, 1);
        assert!((metric.recall_at_k.expect("recall") - 0.25).abs() < 1e-12);
        assert!((metric.precision_at_k.expect("precision") - 0.05).abs() < 1e-12);
        assert!((metric.mrr_at_k.expect("mrr") - 0.25).abs() < 1e-12);
        let empty = MetricAccumulator::default().finish("all", spec, 10);
        assert!(empty.recall_at_k.is_none());
        assert!(empty.precision_at_k.is_none());
        assert!(empty.mrr_at_k.is_none());
        assert!(subject_cites_task_id("fix: title [ORB-12] (#1)"));
        assert!(!subject_cites_task_id("fix: title without an id"));
    }
}

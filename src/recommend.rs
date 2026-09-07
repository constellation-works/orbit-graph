//! Explainable destination recommendations learned from verified change history.
//!
//! The engine consumes only delivered changes from [`HistoryIndex`](crate::HistoryIndex).
//! Planned task context is deliberately absent from the request contract. Callers may
//! inject ranked task hits from an external hybrid retriever; standalone callers get a
//! deterministic lexical baseline over the same historical task snapshots.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use git2::{
    DiffFindOptions, DiffOptions, ObjectType, Oid, Repository, TreeWalkMode, TreeWalkResult,
};
use serde::{Deserialize, Serialize};

use crate::extract::RawSymbol;
use crate::extract::history::{
    DeliveredChange, DeliveryEvidence, FileChange, SymbolIdentity, TaskAssociation,
    TaskTextAvailability, TemporalStatus,
};
use crate::extract::languages;
use crate::{Graph, GraphError, HistoryIndex, SyncPolicy};

/// Default number of ranked destinations returned by recommendation queries.
pub const DEFAULT_RECOMMENDATION_LIMIT: usize = 10;
const MAX_RECOMMENDATION_LIMIT: usize = 100;

/// Granularity of requested recommendation destinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationLevel {
    /// Rank repository-relative files, normalizing all symbols in one delivery to one vote.
    File,
    /// Rank live symbols and surface explicit file fallbacks for file-only evidence.
    Symbol,
}

/// Exactly one source of recommendation intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationInput {
    /// Free-text standalone query.
    Query(String),
    /// Target task identifier. Its own delivery evidence is always excluded.
    TaskId(String),
}

/// One ranked task result supplied by an external hybrid search adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HybridTaskHit {
    /// Historical task identifier.
    pub task_id: String,
    /// Non-negative finite relevance score. Scores are normalized within the request.
    pub score: f64,
}

/// Public recommendation request, reusable by standalone and chronological evaluators.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecommendationRequest {
    /// Query text or target task ID; the enum makes the exactly-one invariant structural.
    pub input: RecommendationInput,
    /// File or symbol destination granularity.
    pub level: RecommendationLevel,
    /// Top-K bound. Defaults to 10 and must be between 1 and 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Git revision expression to resolve. Defaults to the current checkout commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_revision: Option<String>,
    /// Optional chronological cutoff (RFC 3339 or `unix:<seconds>`). Unknown delivery
    /// times fail closed when this is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cutoff: Option<String>,
    /// Optional externally ranked historical tasks. Local lexical retrieval remains active
    /// for tasks not present in this list.
    #[serde(default)]
    pub hybrid_hits: Vec<HybridTaskHit>,
}

/// History-index freshness relative to the resolved target revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecommendationFreshness {
    /// Current, stale, or unavailable.
    pub status: RecommendationFreshnessStatus,
    /// Last indexed branch revision, when known.
    pub index_revision: Option<String>,
    /// Human-readable evidence for the status.
    pub reason: String,
}

/// Explicit freshness state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationFreshnessStatus {
    /// The history cursor is the requested revision.
    Current,
    /// The cursor exists but differs from the requested revision.
    Stale,
    /// No cursor is available.
    Unavailable,
}

/// Why evidence was unavailable or reduced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecommendationFallback {
    /// Stable machine-readable category.
    pub kind: String,
    /// Concrete limitation or fallback explanation.
    pub reason: String,
}

/// Directional co-change evidence for a recommendation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecommendationAssociation {
    /// Seed selector whose occurrence predicts the destination.
    pub from_selector: String,
    /// Number of eligible deliveries containing both locations.
    pub support: usize,
    /// Eligible deliveries containing the source selector.
    pub source_count: usize,
    /// Eligible deliveries containing the destination selector.
    pub destination_count: usize,
    /// `support / source_count`; direction is significant.
    pub confidence: f64,
    /// Confidence divided by destination prevalence.
    pub lift: f64,
}

/// Counts supporting one ranked recommendation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecommendationCounts {
    /// Unique deliveries contributing actual-change evidence.
    pub deliveries: usize,
    /// Unique historical tasks contributing relevance evidence.
    pub tasks: usize,
    /// Eligible history deliveries used for prevalence and association denominators.
    pub eligible_history_deliveries: usize,
}

/// One explainable score contribution.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecommendationReason {
    /// Stable reason category.
    pub kind: String,
    /// Additive score contribution after discounts.
    pub contribution: f64,
    /// Human-readable evidence summary.
    pub explanation: String,
}

/// One ranked current destination.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Recommendation {
    /// One-based rank after deterministic score and selector ordering.
    pub rank: usize,
    /// Explainable relevance score, not a probability.
    pub score: f64,
    /// Live `file:` or `symbol:` selector at the resolved target revision.
    pub selector: String,
    /// True for a file-level destination emitted because symbol evidence was unavailable.
    pub file_fallback: bool,
    /// Explicit file-fallback reason in symbol mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    /// Stable task IDs contributing evidence.
    pub supporting_task_ids: Vec<String>,
    /// Stable delivery IDs contributing evidence.
    pub supporting_delivery_ids: Vec<String>,
    /// Evidence cardinalities.
    pub counts: RecommendationCounts,
    /// Strongest directional association expansion, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub association: Option<RecommendationAssociation>,
    /// Ordered additive score explanations.
    pub reasons: Vec<RecommendationReason>,
}

/// Complete recommendation response and freshness/cutoff metadata.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecommendationResult {
    /// Echoed intent source.
    pub input: RecommendationInput,
    /// Requested result granularity.
    pub level: RecommendationLevel,
    /// Fully resolved immutable target commit.
    pub resolved_target_revision: String,
    /// Explicit cutoff, or the target revision's Git timestamp when omitted.
    pub effective_cutoff: String,
    /// History source freshness.
    pub source_freshness: RecommendationFreshness,
    /// Whether HEAD-only structural evidence was applied.
    pub structure_applied: bool,
    /// Explicit evidence limitations and fallbacks.
    pub fallbacks: Vec<RecommendationFallback>,
    /// Ranked top-K destinations.
    pub recommendations: Vec<Recommendation>,
}

/// Repository-scoped recommendation engine.
#[derive(Debug, Clone)]
pub struct RecommendationEngine {
    repo_root: PathBuf,
    landing_branch: String,
}

impl RecommendationEngine {
    /// Open a standalone engine over one landing-branch history scope.
    pub fn open(repo_root: &Path, landing_branch: &str) -> Result<Self, GraphError> {
        let index = HistoryIndex::open(repo_root, landing_branch)?;
        Ok(Self {
            repo_root: index.repo_root().to_path_buf(),
            landing_branch: index.landing_branch().to_string(),
        })
    }

    /// Rank live destinations at the requested revision.
    pub fn recommend(
        &self,
        request: &RecommendationRequest,
    ) -> Result<RecommendationResult, GraphError> {
        validate_request(request)?;
        let repo = Repository::open(self.repo_root.as_path()).map_err(|error| {
            GraphError::invalid_data("open repository for recommendations", error.to_string())
        })?;
        let target = resolve_target(&repo, request.target_revision.as_deref())?;
        let target_commit = repo
            .find_commit(target)
            .map_err(git_error("load recommendation target"))?;
        let effective_cutoff = request
            .cutoff
            .clone()
            .unwrap_or_else(|| format!("unix:{}", target_commit.time().seconds()));
        let cutoff_seconds = parse_timestamp(effective_cutoff.as_str())?;
        let index = HistoryIndex::open(self.repo_root.as_path(), self.landing_branch.as_str())?;
        let status = index.status()?;
        let freshness = freshness(&repo, status.cursor, target);
        let deliveries = index.deliveries()?;
        let mut resolver = TargetTree::load(&repo, target)?;
        let query = resolve_query(&deliveries, request, cutoff_seconds, &repo, target)?;
        let hybrid = normalized_hybrid_hits(request.hybrid_hits.as_slice())?;
        let target_task = match &request.input {
            RecommendationInput::TaskId(id) => Some(id.as_str()),
            RecommendationInput::Query(_) => None,
        };
        let mut fallbacks = Vec::new();
        if query.is_empty() && matches!(&request.input, RecommendationInput::TaskId(_)) {
            fallbacks.push(RecommendationFallback {
                kind: "target_task_text_external".to_string(),
                reason: "the target task has no eligible local snapshot; supplied hybrid task hits provide relevance without reading target delivery output".to_string(),
            });
        }
        let eligible = eligible_deliveries(
            &repo,
            deliveries,
            target,
            cutoff_seconds,
            request.cutoff.is_some(),
            target_task,
            &mut fallbacks,
        )?;
        let mut scored = score_history(
            &repo,
            &mut resolver,
            eligible.as_slice(),
            query.as_str(),
            &hybrid,
            request.level,
            target,
        )?;

        add_lexical_baseline(&resolver, query.as_str(), request.level, &mut scored);
        let checkout_head = repo
            .head()
            .ok()
            .and_then(|head| head.peel_to_commit().ok())
            .map(|c| c.id());
        let structure_applied = if checkout_head == Some(target) {
            add_current_structure(self.repo_root.as_path(), request.level, &mut scored)?
        } else {
            false
        };
        if checkout_head != Some(target) {
            fallbacks.push(RecommendationFallback {
                kind: "structure_unavailable_for_revision".to_string(),
                reason: "bounded structural expansion was skipped because the target is not the current checkout; HEAD structure was not reused".to_string(),
            });
        } else if !structure_applied {
            fallbacks.push(RecommendationFallback {
                kind: "structure_unavailable".to_string(),
                reason: "the current graph index contained no usable structural neighbors"
                    .to_string(),
            });
        }
        if eligible.is_empty() {
            fallbacks.push(RecommendationFallback {
                kind: "cold_start".to_string(),
                reason: "no leakage-safe historical deliveries were eligible; results use current-tree lexical evidence only".to_string(),
            });
        }

        let total = eligible.len();
        let limit = request.limit.unwrap_or(DEFAULT_RECOMMENDATION_LIMIT);
        let mut recommendations = finalize(scored, total);
        recommendations.truncate(limit);
        for (index, recommendation) in recommendations.iter_mut().enumerate() {
            recommendation.rank = index + 1;
        }
        Ok(RecommendationResult {
            input: request.input.clone(),
            level: request.level,
            resolved_target_revision: target.to_string(),
            effective_cutoff,
            source_freshness: freshness,
            structure_applied,
            fallbacks,
            recommendations,
        })
    }
}

fn validate_request(request: &RecommendationRequest) -> Result<(), GraphError> {
    let input = match &request.input {
        RecommendationInput::Query(value) | RecommendationInput::TaskId(value) => value,
    };
    if input.trim().is_empty() {
        return Err(GraphError::invalid_data(
            "validate recommendation request",
            "query or task ID must be non-empty",
        ));
    }
    let limit = request.limit.unwrap_or(DEFAULT_RECOMMENDATION_LIMIT);
    if !(1..=MAX_RECOMMENDATION_LIMIT).contains(&limit) {
        return Err(GraphError::invalid_data(
            "validate recommendation request",
            format!("limit must be between 1 and {MAX_RECOMMENDATION_LIMIT}"),
        ));
    }
    if let Some(cutoff) = request.cutoff.as_deref() {
        let _ = parse_timestamp(cutoff)?;
    }
    Ok(())
}

fn resolve_target(repo: &Repository, revision: Option<&str>) -> Result<Oid, GraphError> {
    let object = match revision {
        Some(revision) => repo
            .revparse_single(revision)
            .map_err(git_error("resolve requested recommendation revision"))?,
        None => repo
            .head()
            .and_then(|head| head.peel(ObjectType::Commit))
            .map_err(git_error("resolve current checkout revision"))?,
    };
    object
        .peel_to_commit()
        .map(|commit| commit.id())
        .map_err(git_error("peel recommendation revision to commit"))
}

fn freshness(repo: &Repository, cursor: Option<String>, target: Oid) -> RecommendationFreshness {
    let Some(cursor) = cursor else {
        return RecommendationFreshness {
            status: RecommendationFreshnessStatus::Unavailable,
            index_revision: None,
            reason: "history scope has no indexed cursor".to_string(),
        };
    };
    let parsed = Oid::from_str(cursor.as_str()).ok();
    let status = if parsed == Some(target) {
        RecommendationFreshnessStatus::Current
    } else {
        RecommendationFreshnessStatus::Stale
    };
    let relation = parsed.and_then(|oid| repo.graph_descendant_of(target, oid).ok());
    let reason = match (status, relation) {
        (RecommendationFreshnessStatus::Current, _) => {
            "history cursor equals the resolved target revision".to_string()
        }
        (_, Some(true)) => "history cursor is behind the resolved target revision".to_string(),
        _ => "history cursor differs from the resolved target revision or is off its ancestry"
            .to_string(),
    };
    RecommendationFreshness {
        status,
        index_revision: Some(cursor),
        reason,
    }
}

fn resolve_query(
    deliveries: &[DeliveredChange],
    request: &RecommendationRequest,
    cutoff: i64,
    repo: &Repository,
    target: Oid,
) -> Result<String, GraphError> {
    if let RecommendationInput::Query(query) = &request.input {
        return Ok(query.trim().to_string());
    }
    let RecommendationInput::TaskId(task_id) = &request.input else {
        unreachable!()
    };
    let mut snapshots = deliveries
        .iter()
        .filter_map(|delivery| {
            let after = Oid::from_str(delivery.delivery.after_revision.as_str()).ok()?;
            if after != target && !repo.graph_descendant_of(target, after).ok()? {
                return None;
            }
            delivery
                .delivery
                .tasks
                .iter()
                .find(|task| task.task_id == *task_id && task_is_eligible(task, cutoff))
        })
        .collect::<Vec<_>>();
    snapshots.sort_by(|left, right| left.captured_at.cmp(&right.captured_at));
    if let Some(task) = snapshots.first() {
        Ok(task_text(task))
    } else if !request.hybrid_hits.is_empty() {
        Ok(String::new())
    } else {
        Err(GraphError::invalid_data(
            "resolve recommendation task query",
            format!(
                "task {task_id} has no known pre-execution text at or before the cutoff and no hybrid hits were supplied"
            ),
        ))
    }
}

fn normalized_hybrid_hits(hits: &[HybridTaskHit]) -> Result<BTreeMap<String, f64>, GraphError> {
    if hits
        .iter()
        .any(|hit| hit.task_id.trim().is_empty() || !hit.score.is_finite() || hit.score < 0.0)
    {
        return Err(GraphError::invalid_data(
            "validate hybrid task hits",
            "task IDs must be non-empty and scores finite and non-negative",
        ));
    }
    let max = hits.iter().map(|hit| hit.score).fold(0.0_f64, f64::max);
    let mut normalized = BTreeMap::new();
    for hit in hits {
        let score = if max > 0.0 { hit.score / max } else { 0.0 };
        normalized
            .entry(hit.task_id.clone())
            .and_modify(|current| *current = f64::max(*current, score))
            .or_insert(score);
    }
    Ok(normalized)
}

fn eligible_deliveries(
    repo: &Repository,
    deliveries: Vec<DeliveredChange>,
    target: Oid,
    cutoff: i64,
    explicit_cutoff: bool,
    target_task: Option<&str>,
    fallbacks: &mut Vec<RecommendationFallback>,
) -> Result<Vec<DeliveredChange>, GraphError> {
    let mut unique = BTreeMap::new();
    let mut excluded_unknown_time = 0;
    for delivery in deliveries {
        if target_task.is_some_and(|task_id| {
            delivery
                .delivery
                .tasks
                .iter()
                .any(|task| task.task_id == task_id)
        }) {
            continue;
        }
        let after = Oid::from_str(delivery.delivery.after_revision.as_str())
            .map_err(git_error("parse indexed delivery revision"))?;
        if after != target
            && !repo
                .graph_descendant_of(target, after)
                .map_err(git_error("check delivery ancestry"))?
        {
            continue;
        }
        if explicit_cutoff {
            let fact = &delivery.delivery.delivered_at;
            if fact.status != TemporalStatus::Known {
                excluded_unknown_time += 1;
                continue;
            }
            let Some(timestamp) = fact.timestamp.as_deref() else {
                excluded_unknown_time += 1;
                continue;
            };
            if parse_timestamp(timestamp)? > cutoff {
                continue;
            }
        }
        let key = (
            delivery.delivery.delivery_id.clone(),
            delivery.delivery.after_revision.clone(),
        );
        unique.entry(key).or_insert(delivery);
    }
    if excluded_unknown_time > 0 {
        fallbacks.push(RecommendationFallback {
            kind: "unknown_time_excluded".to_string(),
            reason: format!("{excluded_unknown_time} delivery record(s) with uncertain or unavailable landing time were excluded at the explicit cutoff"),
        });
    }
    Ok(unique.into_values().collect())
}

#[derive(Debug, Clone)]
struct DeliveryLocations {
    delivery_id: String,
    task_ids: BTreeSet<String>,
    similarity: f64,
    evidence_weight: f64,
    recency: f64,
    ambiguity: f64,
    breadth: f64,
    locations: BTreeMap<String, LocationMeta>,
}

#[derive(Debug, Clone, Default)]
struct LocationMeta {
    file_fallback: bool,
    fallback_reason: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct Accumulator {
    score: f64,
    tasks: BTreeSet<String>,
    deliveries: BTreeSet<String>,
    reasons: Vec<RecommendationReason>,
    association: Option<RecommendationAssociation>,
    file_fallback: bool,
    fallback_reason: Option<String>,
}

fn score_history(
    repo: &Repository,
    resolver: &mut TargetTree,
    deliveries: &[DeliveredChange],
    query: &str,
    hybrid: &BTreeMap<String, f64>,
    level: RecommendationLevel,
    target: Oid,
) -> Result<BTreeMap<String, Accumulator>, GraphError> {
    let mut rows = Vec::new();
    for delivery in deliveries {
        let mut similarity = 0.0_f64;
        let mut task_ids = BTreeSet::new();
        for task in delivery
            .delivery
            .tasks
            .iter()
            .filter(|task| task_is_eligible(task, i64::MAX))
        {
            let local = lexical_similarity(query, task_text(task).as_str());
            let relevance = f64::max(
                local,
                hybrid.get(task.task_id.as_str()).copied().unwrap_or(0.0),
            );
            if relevance > 0.0 {
                task_ids.insert(task.task_id.clone());
            }
            similarity = similarity.max(relevance);
        }
        let locations = resolver.locations_for_delivery(repo, delivery, level)?;
        if locations.is_empty() {
            continue;
        }
        let distance = commit_distance(
            repo,
            Oid::from_str(delivery.delivery.after_revision.as_str())
                .map_err(git_error("parse delivery revision for recency"))?,
            target,
        );
        rows.push(DeliveryLocations {
            delivery_id: delivery.delivery.delivery_id.clone(),
            task_ids,
            similarity,
            evidence_weight: if delivery.delivery.evidence == DeliveryEvidence::VerifiedDelivery {
                1.0
            } else {
                0.55
            },
            recency: 0.5_f64.powf(distance as f64 / 50.0),
            ambiguity: 1.0 / (delivery.delivery.tasks.len().max(1) as f64).sqrt(),
            breadth: 1.0 / (delivery.files.len().max(1) as f64).sqrt(),
            locations,
        });
    }
    let mut prevalence: BTreeMap<String, usize> = BTreeMap::new();
    for row in &rows {
        for selector in row.locations.keys() {
            *prevalence.entry(selector.clone()).or_default() += 1;
        }
    }
    let history_total = deliveries.len().max(1);
    let mut scored = BTreeMap::new();
    let mut direct_strength = BTreeMap::new();
    for row in &rows {
        if row.similarity <= 0.0 {
            continue;
        }
        for (selector, meta) in &row.locations {
            let ubiquity = 1.0
                / (1.0
                    + 2.0
                        * (*prevalence.get(selector).unwrap_or(&0) as f64 / history_total as f64));
            let artifact = artifact_discount(selector);
            let contribution = weighted_change_score(
                row.similarity,
                row.evidence_weight,
                row.recency,
                row.ambiguity,
                row.breadth,
                ubiquity,
                artifact,
            );
            let entry = scored
                .entry(selector.clone())
                .or_insert_with(Accumulator::default);
            entry.score += contribution;
            entry.deliveries.insert(row.delivery_id.clone());
            entry.tasks.extend(row.task_ids.iter().cloned());
            entry.file_fallback |= meta.file_fallback;
            if entry.fallback_reason.is_none() {
                entry.fallback_reason.clone_from(&meta.fallback_reason);
            }
            entry.reasons.push(RecommendationReason {
                kind: "historical_change".to_string(),
                contribution,
                explanation: format!("task similarity {:.3}, actual-change evidence {:.2}, recency {:.3}, broad/ambiguous/ubiquity/artifact discounts applied", row.similarity, row.evidence_weight, row.recency),
            });
            direct_strength
                .entry(selector.clone())
                .and_modify(|value| *value = f64::max(*value, contribution))
                .or_insert(contribution);
        }
    }
    add_associations(
        &rows,
        &prevalence,
        history_total,
        &direct_strength,
        &mut scored,
    );
    Ok(scored)
}

fn add_associations(
    rows: &[DeliveryLocations],
    prevalence: &BTreeMap<String, usize>,
    history_total: usize,
    direct: &BTreeMap<String, f64>,
    scored: &mut BTreeMap<String, Accumulator>,
) {
    let seeds = direct
        .iter()
        .filter(|(_, score)| **score > 0.0)
        .collect::<Vec<_>>();
    for (destination, destination_count) in prevalence {
        let mut best: Option<(f64, RecommendationAssociation)> = None;
        for (source, source_score) in &seeds {
            if *source == destination {
                continue;
            }
            let source_count = *prevalence.get(*source).unwrap_or(&0);
            if source_count == 0 {
                continue;
            }
            let support = rows
                .iter()
                .filter(|row| {
                    row.locations.contains_key(*source) && row.locations.contains_key(destination)
                })
                .count();
            if support == 0 {
                continue;
            }
            let confidence = support as f64 / source_count as f64;
            let lift = confidence / (*destination_count as f64 / history_total as f64);
            let contribution = **source_score * confidence * lift.ln_1p() * 0.25;
            let association = RecommendationAssociation {
                from_selector: (*source).clone(),
                support,
                source_count,
                destination_count: *destination_count,
                confidence,
                lift,
            };
            if best.as_ref().is_none_or(|(score, current)| {
                contribution > *score
                    || (contribution == *score && association.from_selector < current.from_selector)
            }) {
                best = Some((contribution, association));
            }
        }
        if let Some((contribution, association)) = best {
            let entry = scored.entry(destination.clone()).or_default();
            entry.score += contribution;
            entry.reasons.push(RecommendationReason {
                kind: "directional_cochange".to_string(),
                contribution,
                explanation: format!(
                    "{} predicts this destination with support {}, confidence {:.3}, lift {:.3}",
                    association.from_selector,
                    association.support,
                    association.confidence,
                    association.lift
                ),
            });
            entry.association = Some(association);
        }
    }
}

fn add_lexical_baseline(
    resolver: &TargetTree,
    query: &str,
    level: RecommendationLevel,
    scored: &mut BTreeMap<String, Accumulator>,
) {
    for (selector, text) in resolver.baseline_candidates(level) {
        let similarity = lexical_similarity(query, text.as_str());
        if similarity <= 0.0 {
            continue;
        }
        let contribution = similarity * 0.08 * artifact_discount(selector.as_str());
        let entry = scored.entry(selector).or_default();
        entry.score += contribution;
        entry.reasons.push(RecommendationReason {
            kind: "current_tree_lexical".to_string(),
            contribution,
            explanation: format!("current destination text overlaps the query ({similarity:.3})"),
        });
    }
}

fn add_current_structure(
    repo_root: &Path,
    level: RecommendationLevel,
    scored: &mut BTreeMap<String, Accumulator>,
) -> Result<bool, GraphError> {
    let graph = Graph::open(repo_root, SyncPolicy::Manual)?;
    let seeds = scored
        .iter()
        .filter(|(_, score)| score.score > 0.0)
        .take(20)
        .map(|(selector, score)| (selector.clone(), score.score))
        .collect::<Vec<_>>();
    if seeds.is_empty() {
        return Ok(false);
    }
    let mut additions = Vec::new();
    graph.with_read_connection(|conn| {
        for (selector, seed_score) in &seeds {
            let (path, qualified) = if let Some((path, qualified, _kind)) = split_symbol_selector(selector) {
                (path, Some(qualified))
            } else if let Some(path) = selector.strip_prefix("file:") {
                (path, None)
            } else {
                continue;
            };
            let sql = if qualified.is_some() {
                "SELECT DISTINCT s.file_path, s.qualified, s.kind FROM symbols s
                 WHERE (s.qualified IN (SELECT target_qualified FROM refs r JOIN symbols origin ON origin.file_path=r.from_file AND r.from_span_start>=origin.span_start AND r.from_span_end<=origin.span_end WHERE origin.file_path=?1 AND origin.qualified=?2 AND target_qualified IS NOT NULL)
                    OR (s.file_path IN (SELECT from_file FROM refs WHERE target_qualified=?2))
                    OR s.qualified IN (SELECT from_qualified FROM relations WHERE to_qualified=?2)
                    OR s.qualified IN (SELECT to_qualified FROM relations WHERE from_qualified=?2))
                 ORDER BY s.file_path, s.qualified LIMIT 6"
            } else {
                "SELECT DISTINCT s.file_path, s.qualified, s.kind FROM symbols s
                 WHERE s.qualified IN (SELECT target_qualified FROM refs WHERE from_file=?1 AND target_qualified IS NOT NULL)
                    OR s.file_path IN (SELECT r.from_file FROM refs r JOIN symbols target ON target.qualified=r.target_qualified WHERE target.file_path=?1)
                    OR s.qualified IN (SELECT r.from_qualified FROM relations r JOIN symbols target ON target.qualified=r.to_qualified WHERE target.file_path=?1)
                    OR s.qualified IN (SELECT r.to_qualified FROM relations r WHERE r.def_file=?1)
                    OR (?2 IS NOT NULL AND 0)
                 ORDER BY s.file_path, s.qualified LIMIT 6"
            };
            let mut stmt = conn.prepare(sql)
                .map_err(|source| GraphError::sqlite("prepare recommendation structure query", source))?;
            let rows = stmt.query_map(rusqlite::params![path, qualified], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))
                .map_err(|source| GraphError::sqlite("query recommendation structure", source))?
                .collect::<Result<Vec<_>, _>>().map_err(|source| GraphError::sqlite("collect recommendation structure", source))?;
            for (neighbor_path, neighbor_qualified, neighbor_kind) in rows {
                let destination = match level {
                    RecommendationLevel::File => format!("file:{neighbor_path}"),
                    RecommendationLevel::Symbol => format!("symbol:{neighbor_path}#{neighbor_qualified}:{neighbor_kind}"),
                };
                if destination != *selector { additions.push((destination, seed_score * 0.08, selector.clone())); }
            }
        }
        Ok(())
    })?;
    let applied = !additions.is_empty();
    for (destination, contribution, source) in additions {
        let entry = scored.entry(destination).or_default();
        entry.score += contribution;
        entry.reasons.push(RecommendationReason {
            kind: "current_structure".to_string(),
            contribution,
            explanation: format!("bounded caller/callee neighbor of {source}"),
        });
    }
    Ok(applied)
}

fn finalize(scored: BTreeMap<String, Accumulator>, total: usize) -> Vec<Recommendation> {
    let mut values = scored
        .into_iter()
        .filter(|(_, value)| value.score > 0.0)
        .map(|(selector, mut value)| {
            let delivery_count = value.deliveries.len();
            let task_count = value.tasks.len();
            value.reasons.sort_by(|left, right| {
                right
                    .contribution
                    .total_cmp(&left.contribution)
                    .then_with(|| left.kind.cmp(&right.kind))
            });
            Recommendation {
                rank: 0,
                score: round_score(value.score),
                selector,
                file_fallback: value.file_fallback,
                fallback_reason: value.fallback_reason,
                supporting_task_ids: value.tasks.into_iter().collect(),
                supporting_delivery_ids: value.deliveries.into_iter().collect(),
                counts: RecommendationCounts {
                    deliveries: delivery_count,
                    tasks: task_count,
                    eligible_history_deliveries: total,
                },
                association: value.association,
                reasons: value.reasons,
            }
        })
        .collect::<Vec<_>>();
    values.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.selector.cmp(&right.selector))
    });
    values
}

fn round_score(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

fn weighted_change_score(
    similarity: f64,
    evidence: f64,
    recency: f64,
    ambiguity: f64,
    breadth: f64,
    ubiquity: f64,
    artifact: f64,
) -> f64 {
    similarity * evidence * recency * ambiguity * breadth * ubiquity * artifact
}

fn task_is_eligible(task: &TaskAssociation, cutoff: i64) -> bool {
    task.text_availability == TaskTextAvailability::KnownPreExecution
        && task.snapshot_available_at.status == TemporalStatus::Known
        && task
            .snapshot_available_at
            .timestamp
            .as_deref()
            .and_then(|value| parse_timestamp(value).ok())
            .is_some_and(|value| value <= cutoff)
}

fn task_text(task: &TaskAssociation) -> String {
    let mut text = format!("{} {}", task.title, task.description);
    for criterion in &task.acceptance_criteria {
        text.push(' ');
        text.push_str(criterion);
    }
    text
}

fn lexical_similarity(left: &str, right: &str) -> f64 {
    let query = tokens(left);
    if query.is_empty() {
        return 0.0;
    }
    let document = tokens(right);
    let overlap = query.intersection(&document).count();
    overlap as f64 / query.len() as f64
}

fn tokens(value: &str) -> BTreeSet<String> {
    value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter_map(|token| {
            let mut token = token.to_ascii_lowercase();
            if token.len() > 4 && token.ends_with('s') {
                token.pop();
            }
            (!token.is_empty()).then_some(token)
        })
        .collect()
}

fn artifact_discount(selector: &str) -> f64 {
    let lower = selector.to_ascii_lowercase();
    if lower.ends_with("cargo.lock")
        || lower.ends_with("package-lock.json")
        || lower.ends_with("yarn.lock")
        || lower.contains("/generated/")
        || lower.contains(".generated.")
    {
        0.15
    } else if lower.ends_with("changelog.md")
        || lower.ends_with("readme.md")
        || lower.ends_with("cargo.toml")
    {
        0.55
    } else {
        1.0
    }
}

fn commit_distance(repo: &Repository, from: Oid, to: Oid) -> usize {
    if from == to {
        return 0;
    }
    let Ok(mut walk) = repo.revwalk() else {
        return 100;
    };
    if walk.push(to).is_err() || walk.hide(from).is_err() {
        return 100;
    }
    walk.filter_map(Result::ok).count()
}

struct TargetTree {
    revision: Oid,
    files: BTreeMap<String, Oid>,
    symbols: BTreeMap<String, Vec<RawSymbol>>,
}

impl TargetTree {
    fn load(repo: &Repository, revision: Oid) -> Result<Self, GraphError> {
        let tree = repo
            .find_commit(revision)
            .and_then(|commit| commit.tree())
            .map_err(git_error("load recommendation target tree"))?;
        let mut files = BTreeMap::new();
        tree.walk(TreeWalkMode::PreOrder, |root, entry| {
            if entry.kind() == Some(ObjectType::Blob)
                && let Some(name) = entry.name()
            {
                files.insert(format!("{root}{name}"), entry.id());
            }
            TreeWalkResult::Ok
        })
        .map_err(git_error("walk recommendation target tree"))?;
        let extractors = languages::extractors();
        let mut symbols = BTreeMap::new();
        for (path, oid) in &files {
            let path_ref = Path::new(path);
            let Some(extractor) = extractors
                .iter()
                .find(|extractor| extractor.supports(path_ref))
            else {
                continue;
            };
            let blob = repo
                .find_blob(*oid)
                .map_err(git_error("read recommendation target blob"))?;
            if blob.content().contains(&0) {
                continue;
            }
            let extracted = extractor.extract(path_ref, blob.content());
            if !extracted.symbols.is_empty() {
                symbols.insert(path.clone(), extracted.symbols);
            }
        }
        Ok(Self {
            revision,
            files,
            symbols,
        })
    }

    fn baseline_candidates(&self, level: RecommendationLevel) -> Vec<(String, String)> {
        match level {
            RecommendationLevel::File => self
                .files
                .keys()
                .map(|path| {
                    let mut text = path.clone();
                    if let Some(symbols) = self.symbols.get(path) {
                        for symbol in symbols {
                            text.push(' ');
                            text.push_str(symbol.name.as_str());
                            text.push(' ');
                            text.push_str(symbol.qualified.as_str());
                        }
                    }
                    (format!("file:{path}"), text)
                })
                .collect(),
            RecommendationLevel::Symbol => self
                .symbols
                .iter()
                .flat_map(|(path, symbols)| {
                    symbols.iter().map(move |symbol| {
                        (
                            format!("symbol:{path}#{}:{}", symbol.qualified, symbol.kind),
                            format!("{path} {} {}", symbol.name, symbol.qualified),
                        )
                    })
                })
                .collect(),
        }
    }

    fn locations_for_delivery(
        &mut self,
        repo: &Repository,
        delivery: &DeliveredChange,
        level: RecommendationLevel,
    ) -> Result<BTreeMap<String, LocationMeta>, GraphError> {
        let mut locations = BTreeMap::new();
        for file in &delivery.files {
            let Some(historical_path) = file.new_path.as_deref() else {
                continue;
            };
            let mapped_path = self.resolve_file(
                repo,
                delivery.delivery.after_revision.as_str(),
                historical_path,
            )?;
            let Some(path) = mapped_path else {
                continue;
            };
            match level {
                RecommendationLevel::File => {
                    locations.insert(format!("file:{path}"), LocationMeta::default());
                }
                RecommendationLevel::Symbol => {
                    let mut found_symbol = false;
                    for change in &file.symbols {
                        if !change.live_after {
                            continue;
                        }
                        let Some(after) = change.after.as_ref() else {
                            continue;
                        };
                        if let Some(symbol) = self.resolve_symbol(&after.symbol, path.as_str()) {
                            locations.insert(
                                format!(
                                    "symbol:{}#{}:{}",
                                    symbol.file_path, symbol.qualified, symbol.kind
                                ),
                                LocationMeta::default(),
                            );
                            found_symbol = true;
                        }
                    }
                    if !found_symbol {
                        locations.insert(
                            format!("file:{path}"),
                            LocationMeta {
                                file_fallback: true,
                                fallback_reason: Some(file_fallback_reason(file)),
                            },
                        );
                    }
                }
            }
        }
        Ok(locations)
    }

    fn resolve_file(
        &self,
        repo: &Repository,
        historical_revision: &str,
        historical_path: &str,
    ) -> Result<Option<String>, GraphError> {
        if self.files.contains_key(historical_path) {
            return Ok(Some(historical_path.to_string()));
        }
        let historical = repo
            .find_commit(
                Oid::from_str(historical_revision)
                    .map_err(git_error("parse historical file revision"))?,
            )
            .and_then(|commit| commit.tree())
            .map_err(git_error("load historical file tree"))?;
        let target = repo
            .find_commit(self.revision)
            .and_then(|commit| commit.tree())
            .map_err(git_error("load target file tree"))?;
        let mut options = DiffOptions::new();
        let mut diff = repo
            .diff_tree_to_tree(Some(&historical), Some(&target), Some(&mut options))
            .map_err(git_error("diff historical path to target"))?;
        let mut find = DiffFindOptions::new();
        find.renames(true).copies(true);
        diff.find_similar(Some(&mut find))
            .map_err(git_error("detect path rename to target"))?;
        for delta in diff.deltas() {
            if delta.old_file().path().and_then(Path::to_str) == Some(historical_path)
                && let Some(path) = delta.new_file().path().and_then(Path::to_str)
                && self.files.contains_key(path)
            {
                return Ok(Some(path.to_string()));
            }
        }
        Ok(None)
    }

    fn resolve_symbol(&self, historical: &SymbolIdentity, mapped_path: &str) -> Option<RawSymbol> {
        let exact = self
            .symbols
            .get(mapped_path)
            .into_iter()
            .flatten()
            .filter(|symbol| {
                symbol.qualified == historical.qualified && symbol.kind == historical.kind
            })
            .cloned()
            .collect::<Vec<_>>();
        if exact.len() == 1 {
            return exact.into_iter().next();
        }
        let global = self
            .symbols
            .values()
            .flatten()
            .filter(|symbol| {
                symbol.qualified == historical.qualified && symbol.kind == historical.kind
            })
            .cloned()
            .collect::<Vec<_>>();
        if global.len() == 1 {
            return global.into_iter().next();
        }
        let signature = historical.signature.as_deref()?;
        let similar = self
            .symbols
            .values()
            .flatten()
            .filter(|symbol| {
                symbol.kind == historical.kind && symbol.signature.as_deref() == Some(signature)
            })
            .cloned()
            .collect::<Vec<_>>();
        (similar.len() == 1).then(|| similar[0].clone())
    }
}

fn file_fallback_reason(file: &FileChange) -> String {
    if file.after_fallback.is_some() {
        format!(
            "historical change had file-only evidence: {:?}",
            file.after_fallback
        )
    } else {
        "historical symbols were deleted, ambiguous, or unresolved at the target revision"
            .to_string()
    }
}

fn split_symbol_selector(selector: &str) -> Option<(&str, &str, &str)> {
    let rest = selector.strip_prefix("symbol:")?;
    let (path, symbol_kind) = rest.split_once('#')?;
    let (qualified, kind) = symbol_kind.rsplit_once(':')?;
    Some((path, qualified, kind))
}

fn parse_timestamp(value: &str) -> Result<i64, GraphError> {
    if let Some(seconds) = value.strip_prefix("unix:") {
        return seconds.parse::<i64>().map_err(|error| {
            GraphError::invalid_data("parse recommendation cutoff", error.to_string())
        });
    }
    parse_rfc3339(value).ok_or_else(|| {
        GraphError::invalid_data(
            "parse recommendation cutoff",
            format!("invalid RFC 3339 or unix timestamp: {value}"),
        )
    })
}

fn parse_rfc3339(value: &str) -> Option<i64> {
    let (date, time_zone) = value.split_once('T')?;
    let mut date_parts = date.split('-');
    let year = date_parts.next()?.parse::<i64>().ok()?;
    let month = date_parts.next()?.parse::<i64>().ok()?;
    let day = date_parts.next()?.parse::<i64>().ok()?;
    let zone_index = time_zone
        .find(['Z', '+'])
        .or_else(|| time_zone.get(1..)?.find('-').map(|index| index + 1))?;
    let (time, zone) = time_zone.split_at(zone_index);
    let mut time_parts = time.split(':');
    let hour = time_parts.next()?.parse::<i64>().ok()?;
    let minute = time_parts.next()?.parse::<i64>().ok()?;
    let second = time_parts.next()?.split('.').next()?.parse::<i64>().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let offset = if zone == "Z" {
        0
    } else {
        let sign = if zone.starts_with('+') {
            1
        } else if zone.starts_with('-') {
            -1
        } else {
            return None;
        };
        let mut parts = zone[1..].split(':');
        let hours = parts.next()?.parse::<i64>().ok()?;
        let minutes = parts.next()?.parse::<i64>().ok()?;
        sign * (hours * 3600 + minutes * 60)
    };
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let adjusted_month = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * adjusted_month + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn git_error(operation: &'static str) -> impl FnOnce(git2::Error) -> GraphError {
    move |error| GraphError::invalid_data(operation, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    use crate::{DeliveryImport, Provenance, TaskAssociation, TemporalFact};
    use tempfile::TempDir;

    #[test]
    fn lexical_similarity_is_directional_query_coverage() {
        assert_eq!(
            lexical_similarity("parser cache", "repair parser cache invalidation"),
            1.0
        );
        assert_eq!(
            lexical_similarity("parser cache extra", "parser cache"),
            2.0 / 3.0
        );
    }

    #[test]
    fn directional_association_uses_source_denominator() {
        let rows = vec![
            row(&["file:a", "file:b"]),
            row(&["file:a", "file:b"]),
            row(&["file:b"]),
            row(&["file:b"]),
        ];
        let prevalence = BTreeMap::from([("file:a".to_string(), 2), ("file:b".to_string(), 4)]);
        let mut scored = BTreeMap::new();
        add_associations(
            &rows,
            &prevalence,
            4,
            &BTreeMap::from([("file:a".to_string(), 1.0)]),
            &mut scored,
        );
        let association = scored["file:b"].association.as_ref().expect("association");
        assert_eq!(association.support, 2);
        assert_eq!(association.confidence, 1.0);
        assert_eq!(association.lift, 1.0);

        let mut reverse = BTreeMap::new();
        add_associations(
            &rows,
            &prevalence,
            4,
            &BTreeMap::from([("file:b".to_string(), 1.0)]),
            &mut reverse,
        );
        assert_eq!(
            reverse["file:a"]
                .association
                .as_ref()
                .expect("reverse association")
                .confidence,
            0.5
        );
    }

    #[test]
    fn broad_ambiguous_old_changes_are_discounted() {
        let focused_recent = weighted_change_score(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0);
        let broad_old = weighted_change_score(1.0, 1.0, 0.25, 0.5, 0.2, 1.0, 1.0);
        assert!(focused_recent > broad_old * 20.0);
        assert!(artifact_discount("file:Cargo.lock") < artifact_discount("file:src/lib.rs"));
    }

    #[test]
    fn stable_ties_sort_by_selector() {
        let scored = BTreeMap::from([
            (
                "file:b".to_string(),
                Accumulator {
                    score: 1.0,
                    ..Accumulator::default()
                },
            ),
            (
                "file:a".to_string(),
                Accumulator {
                    score: 1.0,
                    ..Accumulator::default()
                },
            ),
        ]);
        let result = finalize(scored, 0);
        assert_eq!(result[0].selector, "file:a");
    }

    #[test]
    fn cutoff_parser_handles_offsets_and_unix() {
        assert_eq!(parse_timestamp("unix:0").expect("unix"), 0);
        assert_eq!(
            parse_timestamp("1970-01-01T01:00:00+01:00").expect("offset"),
            0
        );
    }

    #[test]
    fn task_mode_excludes_self_and_future_and_resolves_renamed_live_destinations() {
        let fixture = TempDir::new().expect("fixture");
        git(fixture.path(), &["init", "-b", "main"]);
        git(
            fixture.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        git(fixture.path(), &["config", "user.name", "Test"]);
        fs::create_dir_all(fixture.path().join("src")).expect("src");
        fs::create_dir_all(fixture.path().join("tests")).expect("tests");
        fs::write(
            fixture.path().join("src/payment.rs"),
            "pub fn payment_cache() -> i32 { 1 }\n",
        )
        .expect("payment");
        fs::write(
            fixture.path().join("tests/payment.rs"),
            "#[test]\nfn payment_cache_test() {}\n",
        )
        .expect("test");
        commit_all(fixture.path(), "base");

        let before_history = head(fixture.path());
        fs::write(
            fixture.path().join("src/payment.rs"),
            "pub fn payment_cache() -> i32 { 2 }\n",
        )
        .expect("edit payment");
        fs::write(
            fixture.path().join("tests/payment.rs"),
            "#[test]\nfn payment_cache_test() { assert_eq!(2, 2); }\n",
        )
        .expect("edit test");
        fs::write(
            fixture.path().join("Cargo.toml"),
            "[package]\nname = \"payment-fixture\"\nversion = \"0.1.0\"\n",
        )
        .expect("metadata");
        commit_all(fixture.path(), "historical delivery");
        let history_revision = head(fixture.path());

        git(fixture.path(), &["mv", "src/payment.rs", "src/billing.rs"]);
        fs::write(
            fixture.path().join("src/secret.rs"),
            "pub fn unrelated_secret() {}\n",
        )
        .expect("secret");
        commit_all(fixture.path(), "target delivery");
        let target_revision = head(fixture.path());

        fs::remove_file(fixture.path().join("src/billing.rs")).expect("delete billing");
        fs::write(
            fixture.path().join("src/future_payment.rs"),
            "pub fn future_only_payment() -> &'static str { \"future\" }\n",
        )
        .expect("future");
        commit_all(fixture.path(), "future delivery");
        let future_revision = head(fixture.path());

        let index = HistoryIndex::open(fixture.path(), "main").expect("history");
        import(
            &index,
            before_history.as_str(),
            history_revision.as_str(),
            "D-HISTORY",
            "HIST-1",
            "Repair payment cache and tests",
            10,
        );
        import(
            &index,
            history_revision.as_str(),
            target_revision.as_str(),
            "D-TARGET",
            "TARGET-1",
            "Repair payment cache behavior",
            20,
        );
        import(
            &index,
            target_revision.as_str(),
            future_revision.as_str(),
            "D-FUTURE",
            "FUTURE-1",
            "Repair payment cache later",
            30,
        );

        let engine = RecommendationEngine::open(fixture.path(), "main").expect("engine");
        let result = engine
            .recommend(&RecommendationRequest {
                input: RecommendationInput::TaskId("TARGET-1".to_string()),
                level: RecommendationLevel::File,
                limit: Some(10),
                target_revision: Some(target_revision.clone()),
                cutoff: None,
                hybrid_hits: Vec::new(),
            })
            .expect("recommend target");
        assert_eq!(result.resolved_target_revision, target_revision);
        let billing = result
            .recommendations
            .iter()
            .find(|item| item.selector == "file:src/billing.rs")
            .expect("renamed live path");
        assert!(
            billing
                .supporting_delivery_ids
                .contains(&"D-HISTORY".to_string())
        );
        assert!(
            !billing
                .supporting_delivery_ids
                .contains(&"D-TARGET".to_string())
        );
        assert!(result.recommendations.iter().all(|item| {
            !item
                .supporting_delivery_ids
                .contains(&"D-FUTURE".to_string())
                && item.selector != "file:src/secret.rs"
        }));
        let metadata = result
            .recommendations
            .iter()
            .find(|item| item.selector == "file:Cargo.toml")
            .expect("metadata evidence retained");
        assert!(
            billing.score > metadata.score,
            "useful code must outrank ubiquitous metadata"
        );
        assert!(
            !result.structure_applied,
            "non-HEAD target must not use HEAD structure"
        );

        let symbols = engine
            .recommend(&RecommendationRequest {
                input: RecommendationInput::TaskId("TARGET-1".to_string()),
                level: RecommendationLevel::Symbol,
                limit: Some(10),
                target_revision: Some(target_revision),
                cutoff: None,
                hybrid_hits: Vec::new(),
            })
            .expect("recommend symbols");
        assert!(symbols.recommendations.iter().any(|item| {
            item.selector
                .contains("symbol:src/billing.rs#payment_cache:function")
        }));

        let current = engine
            .recommend(&RecommendationRequest {
                input: RecommendationInput::Query("payment cache".to_string()),
                level: RecommendationLevel::Symbol,
                limit: Some(20),
                target_revision: Some(future_revision),
                cutoff: Some("unix:15".to_string()),
                hybrid_hits: Vec::new(),
            })
            .expect("recommend current");
        assert!(current.recommendations.iter().all(|item| {
            !item.selector.contains("billing.rs")
                && !item
                    .supporting_delivery_ids
                    .contains(&"D-FUTURE".to_string())
        }));
    }

    fn row(locations: &[&str]) -> DeliveryLocations {
        DeliveryLocations {
            delivery_id: "d".to_string(),
            task_ids: BTreeSet::new(),
            similarity: 1.0,
            evidence_weight: 1.0,
            recency: 1.0,
            ambiguity: 1.0,
            breadth: 1.0,
            locations: locations
                .iter()
                .map(|value| ((*value).to_string(), LocationMeta::default()))
                .collect(),
        }
    }

    fn import(
        index: &HistoryIndex,
        before: &str,
        after: &str,
        delivery_id: &str,
        task_id: &str,
        title: &str,
        seconds: i64,
    ) {
        let source = Provenance {
            system: "test".to_string(),
            record_id: Some(delivery_id.to_string()),
        };
        let fact = |value: i64| TemporalFact {
            status: TemporalStatus::Known,
            timestamp: Some(format!("unix:{value}")),
            source: source.clone(),
        };
        index
            .import(DeliveryImport {
                schema_version: crate::DELIVERY_IMPORT_SCHEMA_VERSION,
                repository: index.repository().to_string(),
                landing_branch: "main".to_string(),
                before_revision: before.to_string(),
                after_revision: after.to_string(),
                delivery_id: delivery_id.to_string(),
                evidence: DeliveryEvidence::VerifiedDelivery,
                source: source.clone(),
                delivered_at: fact(seconds),
                captured_at: format!("unix:{}", seconds + 1),
                tasks: vec![TaskAssociation {
                    task_id: task_id.to_string(),
                    title: title.to_string(),
                    description: "Change behavior using verified code and tests".to_string(),
                    acceptance_criteria: vec!["Payment cache is covered".to_string()],
                    source: source.clone(),
                    created_at: fact(1),
                    snapshot_available_at: fact(2),
                    text_availability: TaskTextAvailability::KnownPreExecution,
                    captured_at: "unix:3".to_string(),
                }],
            })
            .expect("import delivery");
    }

    fn commit_all(root: &Path, message: &str) {
        git(root, &["add", "."]);
        git(root, &["commit", "-m", message]);
    }

    fn head(root: &Path) -> String {
        let output = Command::new("git")
            .current_dir(root)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("git head");
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .expect("utf8")
            .trim()
            .to_string()
    }

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

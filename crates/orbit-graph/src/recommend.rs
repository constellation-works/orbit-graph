//! Explainable destination recommendations learned from verified change history.
//!
//! The engine consumes only delivered changes from [`HistoryIndex`](crate::HistoryIndex).
//! Planned context-file selectors are deliberately absent from the request contract.
//! Callers may supply the target task's observed text or ranked task hits from an
//! external hybrid retriever; strict replay requires attested pre-execution text,
//! while live calls may use honestly labeled current observations. Standalone callers
//! get a deterministic lexical baseline over eligible historical task snapshots.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use git2::{
    Delta, DiffFindOptions, DiffOptions, ObjectType, Oid, Repository, TreeWalkMode, TreeWalkResult,
};
use serde::{Deserialize, Serialize};

use crate::extract::RawSymbol;
use crate::extract::history::{
    DeliveredChange, DeliveryEvidence, FileChange, SymbolIdentity, TaskAssociation,
    TaskTextAvailability, TemporalStatus, Timestamp, parse_timestamp, validate_task_association,
};
use crate::extract::languages;
use crate::store::history::PathLineageStep;
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

/// Ranking strategy used by live recommendations and chronological evaluation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationVariant {
    /// Task relevance, verified change evidence, co-change, lexical, and structure.
    #[default]
    Combined,
    /// Similar-task changed destinations only.
    TaskSearchOnly,
    /// Current-tree lexical matches and bounded structural neighbors only.
    GraphOnly,
    /// Query-independent historical destination frequency.
    Frequency,
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
    /// Ranking strategy. Ordinary callers use the combined strategy.
    #[serde(default)]
    pub variant: RecommendationVariant,
    /// Top-K bound. Defaults to 10 and must be between 1 and 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Git revision expression to resolve. Defaults to the current checkout commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_revision: Option<String>,
    /// Optional chronological cutoff (RFC 3339 or `unix:<seconds>`). Evidence must be
    /// strictly earlier; unknown delivery times fail closed when this is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cutoff: Option<String>,
    /// Optional authoritative task observation, valid only with
    /// [`RecommendationInput::TaskId`]. Explicit-cutoff replay requires attested
    /// pre-execution text; live calls may use honestly labeled current observations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_snapshot: Option<TaskAssociation>,
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
    /// Ranking strategy applied to this response.
    pub variant: RecommendationVariant,
    /// Fully resolved immutable target commit.
    pub resolved_target_revision: String,
    /// Explicit cutoff, or the request observation time with subsecond precision when omitted.
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
    index_dir: Option<PathBuf>,
}

impl RecommendationEngine {
    /// Open a standalone engine over one landing-branch history scope.
    pub fn open(repo_root: &Path, landing_branch: &str) -> Result<Self, GraphError> {
        let index = HistoryIndex::open(repo_root, landing_branch)?;
        Ok(Self {
            repo_root: index.repo_root().to_path_buf(),
            landing_branch: index.landing_branch().to_string(),
            index_dir: None,
        })
    }

    pub(crate) fn open_with_index_dir(
        repo_root: &Path,
        landing_branch: &str,
        index_dir: &Path,
    ) -> Result<Self, GraphError> {
        let index = HistoryIndex::open_with_index_dir(repo_root, landing_branch, index_dir)?;
        Ok(Self {
            repo_root: index.repo_root().to_path_buf(),
            landing_branch: index.landing_branch().to_string(),
            index_dir: Some(index_dir.to_path_buf()),
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
        let effective_cutoff = request
            .cutoff
            .clone()
            .map_or_else(current_observation_cutoff, Ok)?;
        let cutoff = parse_timestamp("recommendation cutoff", effective_cutoff.as_str())?;
        let index = match self.index_dir.as_deref() {
            Some(index_dir) => HistoryIndex::open_with_index_dir(
                self.repo_root.as_path(),
                self.landing_branch.as_str(),
                index_dir,
            )?,
            None => HistoryIndex::open(self.repo_root.as_path(), self.landing_branch.as_str())?,
        };
        let status = index.status()?;
        let freshness = freshness(&repo, status.cursor, target);
        let deliveries = index.deliveries()?;
        let lineage_steps = index.path_lineage()?;
        let resolver = TargetTree::load(&repo, target)?;
        let strict_replay = request.cutoff.is_some();
        let query = resolve_query(&deliveries, request, &cutoff, strict_replay, &repo, target)?;
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
        let (eligible, visible) = eligible_deliveries(
            &repo,
            deliveries,
            target,
            &cutoff,
            request.cutoff.is_some(),
            target_task,
            &mut fallbacks,
        )?;
        let mut lineage = PathLineage::new(&repo, target, &visible, lineage_steps)?;
        let mut scored = score_history(
            &repo,
            &resolver,
            &mut lineage,
            eligible.as_slice(),
            &HistoryScoreRequest {
                query: query.as_str(),
                hybrid: &hybrid,
                level: request.level,
                cutoff: &cutoff,
                strict_replay,
                variant: request.variant,
            },
        )?;
        fallbacks.extend(lineage.fallbacks());

        if matches!(
            request.variant,
            RecommendationVariant::Combined | RecommendationVariant::GraphOnly
        ) {
            add_lexical_baseline(&resolver, query.as_str(), request.level, &mut scored);
        }
        let checkout_head = repo
            .head()
            .ok()
            .and_then(|head| head.peel_to_commit().ok())
            .map(|c| c.id());
        let structure = if checkout_head == Some(target)
            && matches!(
                request.variant,
                RecommendationVariant::Combined | RecommendationVariant::GraphOnly
            ) {
            add_current_structure(
                self.repo_root.as_path(),
                self.index_dir.as_deref(),
                request.level,
                &resolver,
                &mut scored,
            )?
        } else {
            StructureEvidence::default()
        };
        if matches!(
            request.variant,
            RecommendationVariant::Combined | RecommendationVariant::GraphOnly
        ) {
            if checkout_head != Some(target) {
                fallbacks.push(RecommendationFallback {
                    kind: "structure_unavailable_for_revision".to_string(),
                    reason: "bounded structural expansion was skipped because the target is not the current checkout; HEAD structure was not reused".to_string(),
                });
            } else if !structure.applied {
                fallbacks.push(RecommendationFallback {
                    kind: "structure_unavailable".to_string(),
                    reason: "the current graph index contained no usable structural neighbors"
                        .to_string(),
                });
            }
        }
        if structure.stale_destinations > 0 {
            fallbacks.push(RecommendationFallback {
                kind: "stale_structure_excluded".to_string(),
                reason: format!(
                    "{} cached structural destination(s) were absent from the requested Git tree and excluded",
                    structure.stale_destinations
                ),
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
            variant: request.variant,
            resolved_target_revision: target.to_string(),
            effective_cutoff,
            source_freshness: freshness,
            structure_applied: structure.applied,
            fallbacks,
            recommendations,
        })
    }
}

pub(crate) fn current_observation_cutoff() -> Result<String, GraphError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            GraphError::invalid_data("capture recommendation observation time", error.to_string())
        })?;
    let seconds = i64::try_from(elapsed.as_secs()).map_err(|error| {
        GraphError::invalid_data("capture recommendation observation time", error.to_string())
    })?;
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = day_seconds / 3_600;
    let minute = (day_seconds % 3_600) / 60;
    let second = day_seconds % 60;
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:09}Z",
        elapsed.subsec_nanos()
    ))
}

fn civil_from_days(days_since_unix_epoch: i64) -> (i64, i64, i64) {
    let shifted = days_since_unix_epoch + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    (year, month, day)
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
        let _ = parse_timestamp("recommendation cutoff", cutoff)?;
    }
    if let Some(snapshot) = request.task_snapshot.as_ref() {
        validate_task_association(snapshot)?;
        match &request.input {
            RecommendationInput::TaskId(task_id) if task_id == &snapshot.task_id => {}
            RecommendationInput::TaskId(task_id) => {
                return Err(GraphError::invalid_data(
                    "validate recommendation task snapshot",
                    format!(
                        "snapshot task ID {} does not match requested task {task_id}",
                        snapshot.task_id
                    ),
                ));
            }
            RecommendationInput::Query(_) => {
                return Err(GraphError::invalid_data(
                    "validate recommendation task snapshot",
                    "a supplied task snapshot requires task-ID input",
                ));
            }
        }
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
    cutoff: &Timestamp,
    strict_replay: bool,
    repo: &Repository,
    target: Oid,
) -> Result<String, GraphError> {
    if let RecommendationInput::Query(query) = &request.input {
        return Ok(query.trim().to_string());
    }
    let RecommendationInput::TaskId(task_id) = &request.input else {
        unreachable!()
    };
    if let Some(snapshot) = request.task_snapshot.as_ref() {
        if !task_is_eligible(snapshot, cutoff, strict_replay) {
            return Err(GraphError::invalid_data(
                "resolve recommendation task query",
                format!(
                    "supplied snapshot for task {task_id} was not observable before the request cutoff under the selected live/replay policy"
                ),
            ));
        }
        return Ok(task_text(snapshot));
    }
    let mut snapshots = deliveries
        .iter()
        .filter_map(|delivery| {
            let after = Oid::from_str(delivery.delivery.after_revision.as_str()).ok()?;
            if after != target && !repo.graph_descendant_of(target, after).ok()? {
                return None;
            }
            delivery.delivery.tasks.iter().find(|task| {
                task.task_id == *task_id && task_is_eligible(task, cutoff, strict_replay)
            })
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
                "task {task_id} has no known pre-execution text strictly before the cutoff and no hybrid hits were supplied"
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
    cutoff: &Timestamp,
    explicit_cutoff: bool,
    target_task: Option<&str>,
    fallbacks: &mut Vec<RecommendationFallback>,
) -> Result<(Vec<EligibleDelivery>, Vec<VisibleDelivery>), GraphError> {
    let mut grouped: BTreeMap<(String, String), Vec<DeliveredChange>> = BTreeMap::new();
    let mut visible = Vec::new();
    let mut excluded_unknown_time = 0;
    for delivery in deliveries {
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
            if parse_timestamp("indexed delivery timestamp", timestamp)? >= *cutoff {
                continue;
            }
        }
        // Path lineage may use every delivery visible under the ancestry and
        // cutoff rules, including the target task's own boundary: its rename
        // steps describe Git history already contained in the target tree.
        visible.push(VisibleDelivery {
            delivery_id: delivery.delivery.delivery_id.clone(),
            before_revision: Oid::from_str(delivery.delivery.before_revision.as_str())
                .map_err(git_error("parse indexed delivery base revision"))?,
            after_revision: after,
        });
        let key = (
            delivery.delivery.before_revision.clone(),
            delivery.delivery.after_revision.clone(),
        );
        grouped.entry(key).or_default().push(delivery);
    }
    if excluded_unknown_time > 0 {
        fallbacks.push(RecommendationFallback {
            kind: "unknown_time_excluded".to_string(),
            reason: format!("{excluded_unknown_time} delivery record(s) with uncertain or unavailable landing time were excluded at the explicit cutoff"),
        });
    }
    let mut unique = Vec::new();
    for (_, mut equivalent) in grouped {
        if target_task.is_some_and(|task_id| {
            equivalent.iter().any(|delivery| {
                delivery
                    .delivery
                    .tasks
                    .iter()
                    .any(|task| task.task_id == task_id)
            })
        }) {
            continue;
        }
        equivalent.sort_by(|left, right| {
            evidence_rank(right.delivery.evidence)
                .cmp(&evidence_rank(left.delivery.evidence))
                .then_with(|| left.delivery.delivery_id.cmp(&right.delivery.delivery_id))
        });
        let source_delivery_ids = equivalent
            .iter()
            .map(|delivery| delivery.delivery.delivery_id.clone())
            .collect();
        unique.push(EligibleDelivery {
            change: equivalent.remove(0),
            source_delivery_ids,
        });
    }
    Ok((unique, visible))
}

fn evidence_rank(evidence: DeliveryEvidence) -> u8 {
    match evidence {
        DeliveryEvidence::VerifiedDelivery => 1,
        DeliveryEvidence::GitOnly => 0,
    }
}

/// A delivery on the target's ancestry that passed the request's cutoff rule.
#[derive(Debug, Clone)]
struct VisibleDelivery {
    delivery_id: String,
    before_revision: Oid,
    after_revision: Oid,
}

#[derive(Debug, Clone)]
struct EligibleDelivery {
    change: DeliveredChange,
    source_delivery_ids: BTreeSet<String>,
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
    source_delivery_ids: BTreeSet<String>,
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

struct HistoryScoreRequest<'a> {
    query: &'a str,
    hybrid: &'a BTreeMap<String, f64>,
    level: RecommendationLevel,
    cutoff: &'a Timestamp,
    strict_replay: bool,
    variant: RecommendationVariant,
}

fn score_history(
    repo: &Repository,
    resolver: &TargetTree,
    lineage: &mut PathLineage,
    deliveries: &[EligibleDelivery],
    request: &HistoryScoreRequest<'_>,
) -> Result<BTreeMap<String, Accumulator>, GraphError> {
    let mut rows = Vec::new();
    for eligible in deliveries {
        let delivery = &eligible.change;
        let mut similarity = 0.0_f64;
        let mut task_ids = BTreeSet::new();
        let eligible_tasks = delivery
            .delivery
            .tasks
            .iter()
            .filter(|task| task_is_eligible(task, request.cutoff, request.strict_replay))
            .collect::<Vec<_>>();
        for task in &eligible_tasks {
            let local = lexical_similarity(request.query, task_text(task).as_str());
            let relevance = f64::max(
                local,
                request
                    .hybrid
                    .get(task.task_id.as_str())
                    .copied()
                    .unwrap_or(0.0),
            );
            if relevance > 0.0 {
                task_ids.insert(task.task_id.clone());
            }
            similarity = similarity.max(relevance);
        }
        let after = Oid::from_str(delivery.delivery.after_revision.as_str())
            .map_err(git_error("parse delivery revision for recency"))?;
        let locations =
            resolver.locations_for_delivery(repo, lineage, delivery, after, request.level)?;
        if locations.is_empty() {
            continue;
        }
        let distance = lineage.distance(repo, after);
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
            ambiguity: 1.0 / (eligible_tasks.len().max(1) as f64).sqrt(),
            breadth: 1.0 / (delivery.files.len().max(1) as f64).sqrt(),
            locations,
            source_delivery_ids: eligible.source_delivery_ids.clone(),
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
    if request.variant == RecommendationVariant::Frequency {
        for row in &rows {
            for (selector, meta) in &row.locations {
                let entry = scored
                    .entry(selector.clone())
                    .or_insert_with(Accumulator::default);
                let contribution = 1.0 / history_total as f64;
                entry.score += contribution;
                entry.deliveries.insert(row.delivery_id.clone());
                entry.file_fallback |= meta.file_fallback;
                if entry.fallback_reason.is_none() {
                    entry.fallback_reason.clone_from(&meta.fallback_reason);
                }
                entry.reasons.push(RecommendationReason {
                    kind: "historical_frequency".to_string(),
                    contribution,
                    explanation: "query-independent eligible delivery frequency".to_string(),
                });
            }
        }
        return Ok(scored);
    }
    if request.variant == RecommendationVariant::GraphOnly {
        return Ok(scored);
    }
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
                explanation: format!("task similarity {:.3}, actual-change evidence {:.2}, recency {:.3}, broad/ambiguous/ubiquity/artifact discounts applied; equivalent source IDs: {}", row.similarity, row.evidence_weight, row.recency, row.source_delivery_ids.iter().cloned().collect::<Vec<_>>().join(", ")),
            });
            direct_strength
                .entry(selector.clone())
                .and_modify(|value| *value = f64::max(*value, contribution))
                .or_insert(contribution);
        }
    }
    if request.variant == RecommendationVariant::Combined {
        add_associations(
            &rows,
            &prevalence,
            history_total,
            &direct_strength,
            &mut scored,
        );
    }
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
        let mut best: Option<(f64, RecommendationAssociation, String)> = None;
        for (source, source_score) in &seeds {
            if *source == destination {
                continue;
            }
            let source_count = *prevalence.get(*source).unwrap_or(&0);
            if source_count == 0 {
                continue;
            }
            let supporting_rows = rows
                .iter()
                .filter(|row| {
                    row.locations.contains_key(*source) && row.locations.contains_key(destination)
                })
                .collect::<Vec<_>>();
            let support = supporting_rows.len();
            if support == 0 {
                continue;
            }
            let supporting_source_ids = supporting_rows
                .iter()
                .flat_map(|row| row.source_delivery_ids.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(", ");
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
            if best.as_ref().is_none_or(|(score, current, _)| {
                contribution > *score
                    || (contribution == *score && association.from_selector < current.from_selector)
            }) {
                best = Some((contribution, association, supporting_source_ids));
            }
        }
        if let Some((contribution, association, supporting_source_ids)) = best {
            let entry = scored.entry(destination.clone()).or_default();
            entry.score += contribution;
            entry.reasons.push(RecommendationReason {
                kind: "directional_cochange".to_string(),
                contribution,
                explanation: format!(
                    "{} predicts this destination with support {}, confidence {:.3}, lift {:.3}; source delivery IDs: {}",
                    association.from_selector,
                    association.support,
                    association.confidence,
                    association.lift,
                    supporting_source_ids
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
    index_dir: Option<&Path>,
    level: RecommendationLevel,
    resolver: &TargetTree,
    scored: &mut BTreeMap<String, Accumulator>,
) -> Result<StructureEvidence, GraphError> {
    let graph = match index_dir {
        Some(index_dir) => Graph::open_with_db_path(
            repo_root,
            index_dir
                .join(format!("graph.{}.db", crate::EXTRACTOR_VERSION))
                .as_path(),
            SyncPolicy::Manual,
        )?,
        None => Graph::open(repo_root, SyncPolicy::Manual)?,
    };
    let seeds = scored
        .iter()
        .filter(|(_, score)| score.score > 0.0)
        .take(20)
        .map(|(selector, score)| (selector.clone(), score.score))
        .collect::<Vec<_>>();
    if seeds.is_empty() {
        return Ok(StructureEvidence::default());
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
    let mut evidence = StructureEvidence::default();
    for (destination, contribution, source) in additions {
        if !resolver.selector_is_live(destination.as_str()) {
            evidence.stale_destinations += 1;
            continue;
        }
        evidence.applied = true;
        let entry = scored.entry(destination).or_default();
        entry.score += contribution;
        entry.reasons.push(RecommendationReason {
            kind: "current_structure".to_string(),
            contribution,
            explanation: format!("bounded caller/callee neighbor of {source}"),
        });
    }
    Ok(evidence)
}

#[derive(Debug, Clone, Copy, Default)]
struct StructureEvidence {
    applied: bool,
    stale_destinations: usize,
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

fn task_is_eligible(task: &TaskAssociation, cutoff: &Timestamp, strict_replay: bool) -> bool {
    (!strict_replay || task.text_availability == TaskTextAvailability::KnownPreExecution)
        && task.snapshot_available_at.status == TemporalStatus::Known
        && task
            .snapshot_available_at
            .timestamp
            .as_deref()
            .and_then(|value| parse_timestamp("task snapshot timestamp", value).ok())
            .is_some_and(|value| value < *cutoff)
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

/// Largest number of added or deleted files for which the single gap diff runs
/// similarity (inexact) rename detection. Larger gaps follow exact renames only,
/// so query-time work stays bounded regardless of how far the index lags.
const GAP_SIMILARITY_FILE_LIMIT: usize = 500;

#[cfg(test)]
thread_local! {
    /// Query-time tree diffs performed on this thread (test instrumentation).
    static QUERY_TREE_DIFFS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Forward path lineage from indexed deliveries to the target revision.
///
/// Built once per request from the rename and deletion steps each delivery
/// recorded at ingest (`history_path_lineage`). A path observed at a
/// delivery's landed revision is followed through every later visible
/// delivery's steps, oldest first; a deletion ends it. The only query-time Git
/// diff is one bounded, renames-only comparison from the newest visible
/// delivery to the target, computed lazily and memoized, covering commits the
/// index has not ingested (history cursor behind the target).
///
/// Visibility follows the request: live requests use every delivery on the
/// target's ancestry; strict replay uses only deliveries that pass the explicit
/// cutoff, so post-cutoff index records never steer path resolution. Renames by
/// excluded deliveries are then observable only through the gap diff, which
/// compares Git trees up to the target and therefore reveals nothing the
/// target revision does not already contain. Every resolved path must exist in
/// the target tree.
struct PathLineage {
    target: Oid,
    /// Memoized commits reachable from the target but not from the key.
    distances: BTreeMap<Oid, usize>,
    /// Per landed revision rename (`Some`) and deletion (`None`) maps, oldest first.
    steps: Vec<(usize, BTreeMap<String, Option<String>>)>,
    /// Newest visible delivery revision and its distance from the target.
    newest: Option<(Oid, usize)>,
    /// Visible delivery bases that are not another visible delivery's landing.
    unindexed_ranges: usize,
    gap: Option<GapRenames>,
    unresolved: usize,
}

/// Renames detected by the single memoized gap diff.
struct GapRenames {
    renames: BTreeMap<String, String>,
    exact_only: bool,
    added: usize,
    deleted: usize,
}

impl PathLineage {
    fn new(
        repo: &Repository,
        target: Oid,
        visible: &[VisibleDelivery],
        steps: Vec<PathLineageStep>,
    ) -> Result<Self, GraphError> {
        let mut lineage = Self {
            target,
            distances: BTreeMap::new(),
            steps: Vec::new(),
            newest: None,
            unindexed_ranges: 0,
            gap: None,
            unresolved: 0,
        };
        let visible_ids = visible
            .iter()
            .map(|delivery| (delivery.delivery_id.as_str(), delivery.after_revision))
            .collect::<BTreeMap<_, _>>();
        let mut by_revision: BTreeMap<Oid, BTreeMap<String, Option<String>>> = BTreeMap::new();
        for step in steps {
            let Some(after) = visible_ids.get(step.delivery_id.as_str()) else {
                continue;
            };
            let recorded = Oid::from_str(step.after_revision.as_str())
                .map_err(git_error("parse lineage delivery revision"))?;
            if recorded != *after {
                continue;
            }
            // Equivalent boundaries record identical steps; keep the first.
            by_revision
                .entry(*after)
                .or_default()
                .entry(step.old_path)
                .or_insert(step.new_path);
        }
        let mut ordered = by_revision
            .into_iter()
            .map(|(revision, renames)| (lineage.distance(repo, revision), renames))
            .collect::<Vec<_>>();
        ordered.sort_by_key(|(distance, _)| std::cmp::Reverse(*distance));
        lineage.steps = ordered;
        for delivery in visible {
            let distance = lineage.distance(repo, delivery.after_revision);
            if lineage
                .newest
                .is_none_or(|(_, newest_distance)| distance < newest_distance)
            {
                lineage.newest = Some((delivery.after_revision, distance));
            }
        }
        let landed = visible
            .iter()
            .map(|delivery| delivery.after_revision)
            .collect::<BTreeSet<_>>();
        let unlanded_bases = visible
            .iter()
            .map(|delivery| delivery.before_revision)
            .filter(|base| !landed.contains(base))
            .collect::<BTreeSet<_>>();
        // The oldest delivery's base always starts the indexed range.
        lineage.unindexed_ranges = unlanded_bases.len().saturating_sub(1);
        Ok(lineage)
    }

    /// Commits reachable from the target but not from `revision`, memoized.
    fn distance(&mut self, repo: &Repository, revision: Oid) -> usize {
        let target = self.target;
        *self
            .distances
            .entry(revision)
            .or_insert_with(|| commit_distance(repo, revision, target))
    }

    /// Map `path` as it existed at `after_revision` to its live descendant in
    /// the target tree, or `None` when it was deleted or cannot be followed.
    fn resolve(
        &mut self,
        repo: &Repository,
        tree: &TargetTree,
        after_revision: Oid,
        path: &str,
    ) -> Result<Option<String>, GraphError> {
        if tree.files.contains_key(path) {
            return Ok(Some(path.to_string()));
        }
        let origin = self.distance(repo, after_revision);
        let mut current = path.to_string();
        for (distance, renames) in &self.steps {
            if *distance >= origin {
                continue;
            }
            match renames.get(current.as_str()) {
                Some(Some(next)) => current.clone_from(next),
                Some(None) => return Ok(None),
                None => {}
            }
        }
        if tree.files.contains_key(current.as_str()) {
            return Ok(Some(current));
        }
        if let Some((newest, distance)) = self.newest
            && distance > 0
        {
            if self.gap.is_none() {
                self.gap = Some(gap_renames(repo, newest, self.target)?);
            }
            if let Some(next) = self
                .gap
                .as_ref()
                .and_then(|gap| gap.renames.get(current.as_str()))
                .filter(|next| tree.files.contains_key(next.as_str()))
            {
                return Ok(Some(next.clone()));
            }
        }
        self.unresolved += 1;
        Ok(None)
    }

    fn fallbacks(&self) -> Vec<RecommendationFallback> {
        let mut fallbacks = Vec::new();
        if let (Some(gap), Some((newest, distance))) = (self.gap.as_ref(), self.newest) {
            let (kind, detail) = if gap.exact_only {
                (
                    "path_lineage_gap_exact_only",
                    format!(
                        "the gap exceeded the {GAP_SIMILARITY_FILE_LIMIT}-file similarity budget, so only content-identical renames were followed"
                    ),
                )
            } else {
                (
                    "path_lineage_gap",
                    "renames there were followed with one bounded renames-only diff".to_string(),
                )
            };
            fallbacks.push(RecommendationFallback {
                kind: kind.to_string(),
                reason: format!(
                    "the newest indexed delivery {newest} is {distance} commit(s) behind the target; {detail} ({} rename(s) across {} deleted and {} added file(s)); sync history to record per-delivery lineage",
                    gap.renames.len(),
                    gap.deleted,
                    gap.added
                ),
            });
        }
        if self.unresolved > 0 && self.unindexed_ranges > 0 {
            fallbacks.push(RecommendationFallback {
                kind: "path_lineage_incomplete".to_string(),
                reason: format!(
                    "{} historical path(s) could not be followed to the target; {} unindexed range(s) between indexed deliveries carry no recorded renames",
                    self.unresolved, self.unindexed_ranges
                ),
            });
        }
        fallbacks
    }
}

/// One bounded renames-only diff from `from` to `to` (no copy detection).
fn gap_renames(repo: &Repository, from: Oid, to: Oid) -> Result<GapRenames, GraphError> {
    #[cfg(test)]
    QUERY_TREE_DIFFS.with(|count| count.set(count.get() + 1));
    let old_tree = repo
        .find_commit(from)
        .and_then(|commit| commit.tree())
        .map_err(git_error("load newest indexed delivery tree"))?;
    let new_tree = repo
        .find_commit(to)
        .and_then(|commit| commit.tree())
        .map_err(git_error("load recommendation target tree for lineage gap"))?;
    let mut options = DiffOptions::new();
    options.ignore_submodules(true);
    let mut diff = repo
        .diff_tree_to_tree(Some(&old_tree), Some(&new_tree), Some(&mut options))
        .map_err(git_error("diff unindexed history gap"))?;
    let (mut added, mut deleted) = (0, 0);
    for delta in diff.deltas() {
        match delta.status() {
            Delta::Added => added += 1,
            Delta::Deleted => deleted += 1,
            _ => {}
        }
    }
    let exact_only = added > GAP_SIMILARITY_FILE_LIMIT || deleted > GAP_SIMILARITY_FILE_LIMIT;
    if added > 0 && deleted > 0 {
        let mut find = DiffFindOptions::new();
        find.renames(true)
            .copies(false)
            .rename_limit(GAP_SIMILARITY_FILE_LIMIT)
            .exact_match_only(exact_only);
        diff.find_similar(Some(&mut find))
            .map_err(git_error("detect renames in unindexed history gap"))?;
    }
    let renames = diff
        .deltas()
        .filter(|delta| delta.status() == Delta::Renamed)
        .filter_map(|delta| {
            Some((
                delta.old_file().path()?.to_str()?.to_string(),
                delta.new_file().path()?.to_str()?.to_string(),
            ))
        })
        .collect();
    Ok(GapRenames {
        renames,
        exact_only,
        added,
        deleted,
    })
}

struct TargetTree {
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
                && let Ok(name) = entry.name()
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
        Ok(Self { files, symbols })
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

    fn selector_is_live(&self, selector: &str) -> bool {
        if let Some(path) = selector.strip_prefix("file:") {
            return self.files.contains_key(path);
        }
        let Some((path, qualified, kind)) = split_symbol_selector(selector) else {
            return false;
        };
        self.symbols.get(path).is_some_and(|symbols| {
            symbols
                .iter()
                .any(|symbol| symbol.qualified == qualified && symbol.kind == kind)
        })
    }

    fn locations_for_delivery(
        &self,
        repo: &Repository,
        lineage: &mut PathLineage,
        delivery: &DeliveredChange,
        after_revision: Oid,
        level: RecommendationLevel,
    ) -> Result<BTreeMap<String, LocationMeta>, GraphError> {
        let mut locations = BTreeMap::new();
        for file in &delivery.files {
            let Some(historical_path) = file.new_path.as_deref() else {
                continue;
            };
            let mapped_path = lineage.resolve(repo, self, after_revision, historical_path)?;
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

pub(crate) fn selector_is_live_at(
    repo_root: &Path,
    revision: &str,
    selector: &str,
) -> Result<bool, GraphError> {
    let repo = Repository::open(repo_root).map_err(|error| {
        GraphError::invalid_data("open selector validation repository", error.to_string())
    })?;
    let oid = Oid::from_str(revision).map_err(git_error("parse selector validation revision"))?;
    Ok(TargetTree::load(&repo, oid)?.selector_is_live(selector))
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
        assert_eq!(
            parse_timestamp("test", "unix:0").expect("unix"),
            parse_timestamp("test", "1970-01-01T01:00:00+01:00").expect("offset")
        );
        assert!(
            parse_timestamp("test", "2001-01-01T00:00:00.900Z").expect("later")
                > parse_timestamp("test", "2001-01-01T00:00:00.100Z").expect("earlier")
        );
        assert_eq!(
            parse_timestamp("test", "2001-01-01T02:30:00.100+02:30").expect("offset"),
            parse_timestamp("test", "2001-01-01T00:00:00.100Z").expect("utc")
        );
        for invalid in [
            "2026-02-30T00:00:00Z",
            "2026-01-01T00:00:00Zjunk",
            "2026-01-01T00:00:00+24:00",
            "unix:01",
            "unix:9223372036854775808",
        ] {
            assert!(
                parse_timestamp("test", invalid).is_err(),
                "accepted {invalid}"
            );
        }
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
                variant: RecommendationVariant::Combined,
                limit: Some(10),
                target_revision: Some(target_revision.clone()),
                cutoff: None,
                task_snapshot: None,
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
                variant: RecommendationVariant::Combined,
                limit: Some(10),
                target_revision: Some(target_revision),
                cutoff: None,
                task_snapshot: None,
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
                variant: RecommendationVariant::Combined,
                limit: Some(20),
                target_revision: Some(future_revision),
                cutoff: Some("unix:15".to_string()),
                task_snapshot: None,
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

    fn query_tree_diffs() -> usize {
        QUERY_TREE_DIFFS.with(std::cell::Cell::get)
    }

    fn reset_query_tree_diffs() {
        QUERY_TREE_DIFFS.with(|count| count.set(0));
    }

    fn fixture_repo() -> TempDir {
        let fixture = TempDir::new().expect("fixture");
        git(fixture.path(), &["init", "-b", "main"]);
        git(
            fixture.path(),
            &["config", "user.email", "test@example.invalid"],
        );
        git(fixture.path(), &["config", "user.name", "Test"]);
        fs::create_dir_all(fixture.path().join("src")).expect("src");
        fixture
    }

    fn query_request(query: &str, target: &str, cutoff: Option<&str>) -> RecommendationRequest {
        RecommendationRequest {
            input: RecommendationInput::Query(query.to_string()),
            level: RecommendationLevel::File,
            variant: RecommendationVariant::Combined,
            limit: Some(20),
            target_revision: Some(target.to_string()),
            cutoff: cutoff.map(str::to_string),
            task_snapshot: None,
            hybrid_hits: Vec::new(),
        }
    }

    fn historical_support<'a>(
        result: &'a RecommendationResult,
        selector: &str,
    ) -> Option<&'a Recommendation> {
        result.recommendations.iter().find(|item| {
            item.selector == selector
                && item
                    .reasons
                    .iter()
                    .any(|reason| reason.kind == "historical_change")
        })
    }

    #[test]
    fn rename_chain_and_deleted_files_resolve_from_persisted_lineage_without_query_diffs() {
        let fixture = fixture_repo();
        let root = fixture.path();
        fs::write(
            root.join("src/ledger.rs"),
            "pub fn ledger_total() -> i32 { 1 }\n",
        )
        .expect("ledger");
        fs::write(root.join("src/obsolete.rs"), "pub fn obsolete_path() {}\n").expect("obsolete");
        commit_all(root, "base");
        let base = head(root);

        fs::write(
            root.join("src/ledger.rs"),
            "pub fn ledger_total() -> i32 { 2 }\n",
        )
        .expect("edit ledger");
        fs::write(
            root.join("src/obsolete.rs"),
            "pub fn obsolete_path() { let _ = 1; }\n",
        )
        .expect("edit obsolete");
        commit_all(root, "delivery A");
        let delivery_a = head(root);

        git(root, &["mv", "src/ledger.rs", "src/accounts.rs"]);
        fs::remove_file(root.join("src/obsolete.rs")).expect("delete obsolete");
        commit_all(root, "delivery B");
        let delivery_b = head(root);

        fs::create_dir_all(root.join("src/books")).expect("books");
        git(root, &["mv", "src/accounts.rs", "src/books/accounts.rs"]);
        commit_all(root, "delivery C");
        let delivery_c = head(root);

        let index = HistoryIndex::open(root, "main").expect("history");
        import(
            &index,
            base.as_str(),
            delivery_a.as_str(),
            "D-A",
            "TASK-A",
            "Fix ledger total rounding",
            10,
        );
        import(
            &index,
            delivery_a.as_str(),
            delivery_b.as_str(),
            "D-B",
            "TASK-B",
            "Reorganize modules",
            20,
        );
        import(
            &index,
            delivery_b.as_str(),
            delivery_c.as_str(),
            "D-C",
            "TASK-C",
            "Group modules by domain",
            30,
        );
        let lineage = index.path_lineage().expect("lineage");
        assert!(lineage.iter().any(|step| step.delivery_id == "D-B"
            && step.old_path == "src/ledger.rs"
            && step.new_path.as_deref() == Some("src/accounts.rs")));
        assert!(lineage.iter().any(|step| step.delivery_id == "D-B"
            && step.old_path == "src/obsolete.rs"
            && step.new_path.is_none()));

        let engine = RecommendationEngine::open(root, "main").expect("engine");
        reset_query_tree_diffs();
        let result = engine
            .recommend(&query_request("ledger total rounding", &delivery_c, None))
            .expect("recommend");
        assert_eq!(
            query_tree_diffs(),
            0,
            "indexed lineage must resolve renames without query-time diffs"
        );
        let moved = historical_support(&result, "file:src/books/accounts.rs")
            .expect("rename chain resolves to the live path");
        assert!(moved.supporting_delivery_ids.contains(&"D-A".to_string()));
        assert!(
            result
                .recommendations
                .iter()
                .all(|item| !item.selector.contains("obsolete")
                    && !item.selector.contains("src/ledger.rs")),
            "deleted and superseded paths must not be recommended"
        );
        assert!(
            result
                .fallbacks
                .iter()
                .all(|fallback| !fallback.kind.starts_with("path_lineage")),
            "{:?}",
            result.fallbacks
        );
    }

    #[test]
    fn cursor_gap_uses_one_memoized_renames_only_diff() {
        let fixture = fixture_repo();
        let root = fixture.path();
        for name in ["alpha", "beta"] {
            fs::write(
                root.join(format!("src/{name}.rs")),
                format!("pub fn {name}_quota() -> i32 {{ 1 }}\n"),
            )
            .expect("write");
        }
        commit_all(root, "base");
        let base = head(root);
        for name in ["alpha", "beta"] {
            fs::write(
                root.join(format!("src/{name}.rs")),
                format!("pub fn {name}_quota() -> i32 {{ 2 }}\n"),
            )
            .expect("edit");
        }
        commit_all(root, "delivery");
        let delivery = head(root);
        git(root, &["mv", "src/alpha.rs", "src/alpha_quota.rs"]);
        git(root, &["mv", "src/beta.rs", "src/beta_quota.rs"]);
        commit_all(root, "unindexed rename");
        let target = head(root);

        let index = HistoryIndex::open(root, "main").expect("history");
        import(
            &index,
            base.as_str(),
            delivery.as_str(),
            "D-QUOTA",
            "TASK-QUOTA",
            "Raise quota limits",
            10,
        );
        let engine = RecommendationEngine::open(root, "main").expect("engine");
        reset_query_tree_diffs();
        let result = engine
            .recommend(&query_request("raise quota limits", &target, None))
            .expect("recommend");
        assert_eq!(query_tree_diffs(), 1, "the gap diff is computed once");
        for selector in ["file:src/alpha_quota.rs", "file:src/beta_quota.rs"] {
            assert!(
                historical_support(&result, selector).is_some(),
                "{selector} missing from {:?}",
                result.recommendations
            );
        }
        assert!(
            result
                .fallbacks
                .iter()
                .any(|fallback| fallback.kind == "path_lineage_gap"),
            "{:?}",
            result.fallbacks
        );
    }

    #[test]
    fn strict_replay_ignores_lineage_recorded_after_the_cutoff() {
        let fixture = fixture_repo();
        let root = fixture.path();
        fs::write(
            root.join("src/meter.rs"),
            "pub fn meter_reading() -> i32 { 1 }\n",
        )
        .expect("meter");
        commit_all(root, "base");
        let base = head(root);
        fs::write(
            root.join("src/meter.rs"),
            "pub fn meter_reading() -> i32 { 2 }\n",
        )
        .expect("edit meter");
        commit_all(root, "delivery A");
        let delivery_a = head(root);
        git(root, &["mv", "src/meter.rs", "src/gauge.rs"]);
        commit_all(root, "delivery B");
        let delivery_b = head(root);

        let index = HistoryIndex::open(root, "main").expect("history");
        import(
            &index,
            base.as_str(),
            delivery_a.as_str(),
            "D-A",
            "TASK-A",
            "Fix meter reading",
            10,
        );
        import(
            &index,
            delivery_a.as_str(),
            delivery_b.as_str(),
            "D-B",
            "TASK-B",
            "Rename meter module",
            30,
        );
        let engine = RecommendationEngine::open(root, "main").expect("engine");

        reset_query_tree_diffs();
        let live = engine
            .recommend(&query_request("meter reading", &delivery_b, None))
            .expect("live");
        assert_eq!(query_tree_diffs(), 0, "live mode follows indexed lineage");
        assert!(historical_support(&live, "file:src/gauge.rs").is_some());

        reset_query_tree_diffs();
        let replay = engine
            .recommend(&query_request(
                "meter reading",
                &delivery_b,
                Some("unix:20"),
            ))
            .expect("replay");
        assert_eq!(
            query_tree_diffs(),
            1,
            "post-cutoff lineage is not consulted; only the target tree is compared"
        );
        let gauge = historical_support(&replay, "file:src/gauge.rs")
            .expect("path contained in the target revision still resolves");
        assert_eq!(gauge.supporting_delivery_ids, vec!["D-A".to_string()]);
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
            source_delivery_ids: BTreeSet::from(["d".to_string()]),
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

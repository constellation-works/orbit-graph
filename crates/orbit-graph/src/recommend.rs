//! Explainable destination recommendations learned from verified change history.
//!
//! The engine consumes only delivered changes from [`HistoryIndex`](crate::HistoryIndex).
//! Planned context-file selectors are deliberately absent from the request contract.
//! Callers may supply the target task's observed text or ranked task hits from an
//! external hybrid retriever; strict replay requires attested pre-execution text,
//! while live calls may use honestly labeled current observations. Standalone callers
//! get a deterministic lexical baseline over eligible historical task snapshots.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use git2::{
    Delta, DiffFindOptions, DiffOptions, ObjectType, Oid, Repository, TreeWalkMode, TreeWalkResult,
};
use orbit_graph_extract::RawSymbol;
use orbit_graph_extract::history::{
    DeliveredChange, DeliveryEvidence, FileChange, SymbolIdentity, TaskAssociation,
    TaskTextAvailability, TemporalStatus, Timestamp, parse_timestamp, validate_task_association,
};
use orbit_graph_extract::languages;
use serde::{Deserialize, Serialize};

use crate::store::history::PathLineageStep;
use crate::{Graph, GraphError, HistoryIndex};

/// Default number of ranked destinations returned by recommendation queries.
pub const DEFAULT_RECOMMENDATION_LIMIT: usize = 10;
#[cfg(test)]
thread_local! {
    static RECOMMEND_AFTER_TARGET: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}
pub(crate) const MAX_RECOMMENDATION_LIMIT: usize = 100;

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

impl RecommendationVariant {
    /// Whether this variant reads the current tree's structure: symbols,
    /// the lexical baseline and structural neighbours.
    const fn uses_structure(self) -> bool {
        match self {
            Self::Combined | Self::GraphOnly => true,
            Self::TaskSearchOnly | Self::Frequency => false,
        }
    }
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
    /// Override for Git-only commit-text weight. `None` uses the production
    /// weight. Zero disables commit text. Must be finite and non-negative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_text_weight: Option<f64>,
    /// Override for the commit-text similarity exponent. `None` uses the
    /// production exponent. Must be finite and non-negative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_text_exponent: Option<f64>,
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
    /// Explicit file-fallback reason in symbol mode, otherwise `null`.
    pub fallback_reason: Option<String>,
    /// Stable task IDs contributing evidence.
    pub supporting_task_ids: Vec<String>,
    /// Stable delivery IDs contributing evidence.
    pub supporting_delivery_ids: Vec<String>,
    /// Evidence cardinalities.
    pub counts: RecommendationCounts,
    /// Strongest directional association expansion, or `null` when none.
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
    /// Number of ranked destinations before the limit was applied. Always
    /// known here; typed as optional to match the other capped lists.
    pub total: Option<usize>,
    /// Whether `total` exceeds the destinations returned.
    pub truncated: bool,
}

/// Repository-scoped recommendation engine.
#[derive(Debug, Clone)]
pub struct RecommendationEngine {
    repo_root: PathBuf,
    landing_branch: String,
    index_dir: Option<PathBuf>,
    structure_index: StructureIndex,
}

/// Where combined and graph-only ranking read current code structure from.
///
/// Public only for the `orbit-graph` CLI's plugin protocol, which builds the
/// published variants for [`RecommendationEngine::open_with_index_dir`]; it is
/// not part of the documented library surface.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub enum StructureIndex {
    /// The repository-local database selected by branch and commit, as the
    /// command-line interface maintains it with `orbit-graph sync`.
    RepositoryLocal,
    /// A complete published database whose files were indexed while the
    /// checkout was at `revision`.
    Published {
        /// Database file to read.
        db_path: PathBuf,
        /// Checkout commit when the index was built, if the branch was born.
        revision: Option<String>,
    },
    /// No usable index; structure is skipped with this explicit fallback.
    Unavailable {
        /// Fallback kind reported to the caller.
        kind: &'static str,
        /// Actionable explanation.
        reason: String,
    },
}

impl RecommendationEngine {
    /// Open a standalone engine over one landing-branch history scope.
    ///
    /// Recommending only reads: the history index and the graph index are
    /// opened read-only and nothing is created (STD-01 §R31). A history index
    /// that `orbit-graph history sync` has not built is
    /// [`GraphError::IndexMissing`].
    pub fn open(repo_root: &Path, landing_branch: &str) -> Result<Self, GraphError> {
        let index = HistoryIndex::open_read_only(repo_root, landing_branch)?;
        Ok(Self {
            repo_root: index.repo_root().to_path_buf(),
            landing_branch: index.landing_branch().to_string(),
            index_dir: None,
            structure_index: StructureIndex::RepositoryLocal,
        })
    }

    /// Open an engine whose history index lives in `index_dir` and whose
    /// structural evidence comes from `structure_index`.
    ///
    /// Public only for the `orbit-graph` CLI's plugin protocol, which keeps its
    /// indexes in its own state directory; library callers use
    /// [`RecommendationEngine::open`].
    #[doc(hidden)]
    pub fn open_with_index_dir(
        repo_root: &Path,
        landing_branch: &str,
        index_dir: &Path,
        structure_index: StructureIndex,
    ) -> Result<Self, GraphError> {
        let index =
            HistoryIndex::open_read_only_with_index_dir(repo_root, landing_branch, index_dir)?;
        Ok(Self {
            repo_root: index.repo_root().to_path_buf(),
            landing_branch: index.landing_branch().to_string(),
            index_dir: Some(index_dir.to_path_buf()),
            structure_index,
        })
    }

    /// Rank live destinations at the requested revision.
    pub fn recommend(
        &self,
        request: &RecommendationRequest,
    ) -> Result<RecommendationResult, GraphError> {
        validate_request(request)?;
        let repo = Repository::open(self.repo_root.as_path())
            .map_err(|error| GraphError::git("open repository for recommendations", error))?;
        let observed_head = repo.head().and_then(|head| {
            let branch = if head.is_branch() {
                head.shorthand().unwrap_or("HEAD").to_string()
            } else {
                "HEAD".to_string()
            };
            head.peel_to_commit().map(|commit| (branch, commit.id()))
        });
        let (checkout_branch, checkout_head, head_error) = match observed_head {
            Ok((branch, oid)) => (branch, Some(oid), None),
            Err(error) => ("HEAD".to_string(), None, Some(error.to_string())),
        };
        let target = resolve_target(&repo, request.target_revision.as_deref(), checkout_head)?;
        #[cfg(test)]
        RECOMMEND_AFTER_TARGET.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        let effective_cutoff = request
            .cutoff
            .clone()
            .map_or_else(current_observation_cutoff, Ok)?;
        let cutoff = parse_timestamp("recommendation cutoff", effective_cutoff.as_str())?;
        let index = match self.index_dir.as_deref() {
            Some(index_dir) => HistoryIndex::open_read_only_with_index_dir(
                self.repo_root.as_path(),
                self.landing_branch.as_str(),
                index_dir,
            )?,
            None => HistoryIndex::open_read_only(
                self.repo_root.as_path(),
                self.landing_branch.as_str(),
            )?,
        };
        let status = index.status()?;
        let freshness = freshness(&repo, status.cursor, target);
        let deliveries = index.deliveries()?;
        let lineage_steps = index.path_lineage()?;
        let mut resolver = TargetTree::load(&repo, target)?;
        if request.level == RecommendationLevel::Symbol || request.variant.uses_structure() {
            resolver.ensure_symbols(&repo, index.database_path().parent())?;
        }
        let strict_replay = request.cutoff.is_some();
        let commit_text = commit_text_policy(request)?;
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
                free_text_query: matches!(request.input, RecommendationInput::Query(_)),
                commit_text,
            },
        )?;
        fallbacks.extend(lineage.fallbacks());
        let commit_text_destinations = scored
            .values()
            .filter(|entry| {
                entry
                    .reasons
                    .iter()
                    .any(|reason| reason.kind == "historical_change_commit_text")
            })
            .count();
        if commit_text_destinations > 0 {
            fallbacks.push(RecommendationFallback {
                kind: "git_commit_text_used".to_string(),
                reason: format!(
                    "{commit_text_destinations} destination(s) draw relevance from Git-only commit messages: post-execution text written with the change, exponent {:.3} and weighted {:.2} relative to task text and never used in strict replay",
                    commit_text.exponent, commit_text.weight
                ),
            });
        }

        if request.variant.uses_structure() {
            add_lexical_baseline(&resolver, query.as_str(), request.level, &mut scored);
        }
        let uses_structure = request.variant.uses_structure();
        let mut structure_fallback = None;
        let structure = if checkout_head == Some(target) && uses_structure {
            match &self.structure_index {
                StructureIndex::RepositoryLocal => {
                    match Graph::open_existing_for_pinned_target(
                        self.repo_root.as_path(),
                        checkout_branch.as_str(),
                        target.to_string().as_str(),
                    ) {
                        Ok(graph) => {
                            add_current_structure(&graph, request.level, &resolver, &mut scored)?
                        }
                        // An unsynced graph index is reported, never read as an
                        // index with no neighbors (STD-02 §R29).
                        Err(GraphError::IndexMissing { reason, .. }) => {
                            structure_fallback = Some(RecommendationFallback {
                                kind: "structure_index_missing".to_string(),
                                reason,
                            });
                            StructureEvidence::default()
                        }
                        Err(error) => return Err(error),
                    }
                }
                StructureIndex::Published { db_path, revision }
                    if revision.as_deref() == Some(target.to_string().as_str()) =>
                {
                    // A published plugin index is only ever read (STD-01 R31).
                    let graph = Graph::open_read_only(self.repo_root.as_path(), db_path)?;
                    add_current_structure(&graph, request.level, &resolver, &mut scored)?
                }
                StructureIndex::Published { revision, .. } => {
                    structure_fallback = Some(RecommendationFallback {
                        kind: "structure_index_stale".to_string(),
                        reason: format!(
                            "the code-graph index was built at {} but the target is {target}; run the graph_sync maintenance operation to refresh it",
                            revision.as_deref().unwrap_or("an unborn branch")
                        ),
                    });
                    StructureEvidence::default()
                }
                StructureIndex::Unavailable { kind, reason } => {
                    structure_fallback = Some(RecommendationFallback {
                        kind: (*kind).to_string(),
                        reason: reason.clone(),
                    });
                    StructureEvidence::default()
                }
            }
        } else {
            StructureEvidence::default()
        };
        if uses_structure {
            if let Some(fallback) = structure_fallback {
                fallbacks.push(fallback);
            } else if let Some(error) = &head_error {
                fallbacks.push(RecommendationFallback {
                    kind: "structure_head_unknown".to_string(),
                    reason: format!("bounded structural expansion was skipped because checkout HEAD is unknown: {error}"),
                });
            } else if checkout_head != Some(target) {
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
        let ranked = recommendations.len();
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
            truncated: ranked > recommendations.len(),
            recommendations,
            total: Some(ranked),
        })
    }
}

/// The current time as an RFC 3339 UTC timestamp with nanosecond precision
/// (`YYYY-MM-DDTHH:MM:SS.nnnnnnnnnZ`), the form recommendations record as
/// their effective cutoff when a request names none.
///
/// # Errors
///
/// [`GraphError::InvalidData`] when the system clock is before the Unix epoch
/// or too far past it to represent.
///
/// # Examples
///
/// ```
/// let now = orbit_graph::current_observation_cutoff()?;
/// assert!(now.ends_with('Z'));
/// # Ok::<(), orbit_graph::GraphError>(())
/// ```
pub fn current_observation_cutoff() -> Result<String, GraphError> {
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
        return Err(GraphError::invalid_input(
            "validate recommendation request",
            "query",
            "query or task ID must be non-empty",
        ));
    }
    let limit = request.limit.unwrap_or(DEFAULT_RECOMMENDATION_LIMIT);
    if !(1..=MAX_RECOMMENDATION_LIMIT).contains(&limit) {
        return Err(GraphError::invalid_input(
            "validate recommendation request",
            "limit",
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
                return Err(GraphError::invalid_input(
                    "validate recommendation task snapshot",
                    "task_snapshot",
                    format!(
                        "snapshot task ID {} does not match requested task {task_id}",
                        snapshot.task_id
                    ),
                ));
            }
            RecommendationInput::Query(_) => {
                return Err(GraphError::invalid_input(
                    "validate recommendation task snapshot",
                    "task_snapshot",
                    "a supplied task snapshot requires task-ID input",
                ));
            }
        }
    }
    commit_text_policy(request)?;
    Ok(())
}

fn resolve_target(
    repo: &Repository,
    revision: Option<&str>,
    checkout_head: Option<Oid>,
) -> Result<Oid, GraphError> {
    let object = match revision {
        Some(revision) => repo.revparse_single(revision).map_err(|error| {
            if error.code() == git2::ErrorCode::NotFound {
                GraphError::not_found(
                    "resolve requested recommendation revision",
                    format!("revision {revision:?}"),
                )
            } else {
                GraphError::git("resolve requested recommendation revision", error)
            }
        })?,
        None => {
            return checkout_head.ok_or_else(|| {
                GraphError::invalid_data(
                    "resolve current checkout revision",
                    "checkout HEAD is unknown; cannot resolve recommendation target",
                )
            });
        }
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
    let relation = parsed.map(|oid| repo.graph_descendant_of(target, oid));
    let reason = match (status, relation) {
        (RecommendationFreshnessStatus::Current, _) => {
            "history cursor equals the resolved target revision".to_string()
        }
        (_, Some(Ok(true))) => "history cursor is behind the resolved target revision".to_string(),
        (_, Some(Ok(false))) => {
            "history cursor is off the resolved target revision ancestry".to_string()
        }
        (_, Some(Err(error))) => {
            format!("history cursor ancestry is unknown because Git failed: {error}")
        }
        (_, None) => "history cursor ancestry is unknown because its revision could not be parsed"
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
    let task_id = match &request.input {
        RecommendationInput::Query(query) => return Ok(query.trim().to_string()),
        RecommendationInput::TaskId(task_id) => task_id,
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
        return Err(GraphError::invalid_input(
            "validate hybrid task hits",
            "hybrid_hits",
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
        DeliveryEvidence::CallerAttested => 0,
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
    /// Live-only relevance from a Git-only delivery's own commit message.
    commit_text: Option<CommitTextEvidence>,
}

/// Query overlap with a Git-only delivery's commit message.
///
/// The message is post-execution text (`git_commit_message` provenance): it
/// was written with the change. It is used only by live combined requests,
/// down-weighted relative to task text, and never in strict replay.
#[derive(Debug, Clone)]
struct CommitTextEvidence {
    similarity: f64,
    /// `[ABC-123]` identifiers cited by the message; non-authoritative hints.
    cited_task_ids: Vec<String>,
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
    /// Free-text query request. Commit text is only matched against these:
    /// a task-ID request's own Git-only delivery is not associated with the
    /// task, so its message could otherwise describe the held-out change.
    free_text_query: bool,
    /// Live commit-text weight and exponent. Weight zero disables the signal.
    commit_text: CommitTextPolicy,
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
        let distance = lineage.distance(repo, after)?;
        let commit_text = (request.commit_text.enabled
            && similarity <= 0.0
            && request.free_text_query
            && !request.strict_replay
            && request.variant == RecommendationVariant::Combined
            && delivery.delivery.evidence == DeliveryEvidence::GitOnly)
            .then(|| commit_message_text(repo, after))
            .flatten()
            .and_then(|text| {
                let similarity = lexical_similarity(request.query, text.as_str());
                (similarity > 0.0).then(|| CommitTextEvidence {
                    similarity,
                    cited_task_ids: cited_task_ids(text.as_str()),
                })
            });
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
            commit_text,
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
        let source_ids = row
            .source_delivery_ids
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let (relevance, kind, explanation) = if row.similarity > 0.0 {
            (
                row.similarity,
                "historical_change",
                format!(
                    "task similarity {:.3}, actual-change evidence {:.2}, recency {:.3}, broad/ambiguous/ubiquity/artifact discounts applied; equivalent source IDs: {source_ids}",
                    row.similarity, row.evidence_weight, row.recency
                ),
            )
        } else if let Some(commit) = row.commit_text.as_ref() {
            let hints = if commit.cited_task_ids.is_empty() {
                String::new()
            } else {
                format!(
                    "; message cites {} (non-authoritative)",
                    commit.cited_task_ids.join(", ")
                )
            };
            (
                commit_text_relevance(commit.similarity, request.commit_text),
                "historical_change_commit_text",
                format!(
                    "post_execution git_commit_message similarity {:.3} exponent {:.3} weight {:.2} (not task text), Git-only evidence {:.2}, recency {:.3}, broad/ubiquity/artifact discounts applied; source IDs: {source_ids}{hints}",
                    commit.similarity,
                    request.commit_text.exponent,
                    request.commit_text.weight,
                    row.evidence_weight,
                    row.recency
                ),
            )
        } else {
            continue;
        };
        for (selector, meta) in &row.locations {
            let ubiquity = 1.0
                / (1.0
                    + 2.0
                        * (*prevalence.get(selector).unwrap_or(&0) as f64 / history_total as f64));
            let artifact = artifact_discount(selector);
            let contribution = weighted_change_score(
                relevance,
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
                kind: kind.to_string(),
                contribution,
                explanation: explanation.clone(),
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

/// Seeds considered for directional co-change, strongest direct evidence
/// first. Bounds the association pass on large histories; a destination's
/// best association almost always comes from a strong seed, because the
/// contribution is proportional to the seed's direct strength.
const ASSOCIATION_SEED_LIMIT: usize = 256;

/// Best association found so far for one destination.
struct BestAssociation<'a> {
    contribution: f64,
    source: &'a str,
    support: usize,
    source_count: usize,
}

/// Add at most one `directional_cochange` reason per destination: the
/// strongest association from a seed (a selector with direct evidence) to a
/// destination it co-occurred with, ties broken by the smaller seed selector.
///
/// Work is proportional to the deliveries each seed occurs in rather than to
/// every (destination, seed, delivery) triple: per-selector delivery lists are
/// built once, each seed counts co-occurring destinations over its own
/// deliveries only, and the supporting delivery IDs are materialized once per
/// destination for the winning seed.
fn add_associations(
    rows: &[DeliveryLocations],
    prevalence: &BTreeMap<String, usize>,
    history_total: usize,
    direct: &BTreeMap<String, f64>,
    scored: &mut BTreeMap<String, Accumulator>,
) {
    let mut seeds = direct
        .iter()
        .filter(|(_, score)| **score > 0.0)
        .map(|(selector, score)| (selector.as_str(), *score))
        .collect::<Vec<_>>();
    seeds.sort_by(|left, right| right.1.total_cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    seeds.truncate(ASSOCIATION_SEED_LIMIT);
    if seeds.is_empty() {
        return;
    }
    // Ascending row indices per selector (rows are visited in order).
    let mut rows_by_selector: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        for selector in row.locations.keys() {
            rows_by_selector
                .entry(selector.as_str())
                .or_default()
                .push(index);
        }
    }
    let mut best: BTreeMap<&str, BestAssociation<'_>> = BTreeMap::new();
    let mut support: HashMap<&str, usize> = HashMap::new();
    for &(source, source_score) in &seeds {
        let source_count = prevalence.get(source).copied().unwrap_or(0);
        let Some(source_rows) = rows_by_selector.get(source) else {
            continue;
        };
        if source_count == 0 {
            continue;
        }
        support.clear();
        for &index in source_rows {
            for destination in rows[index].locations.keys() {
                if destination != source {
                    *support.entry(destination.as_str()).or_default() += 1;
                }
            }
        }
        for (&destination, &destination_support) in &support {
            let destination_count = prevalence.get(destination).copied().unwrap_or(0);
            let confidence = destination_support as f64 / source_count as f64;
            let lift = confidence / (destination_count as f64 / history_total as f64);
            let contribution = source_score * confidence * lift.ln_1p() * 0.25;
            let replace = best.get(destination).is_none_or(|current| {
                contribution > current.contribution
                    || (contribution == current.contribution && source < current.source)
            });
            if replace {
                best.insert(
                    destination,
                    BestAssociation {
                        contribution,
                        source,
                        support: destination_support,
                        source_count,
                    },
                );
            }
        }
    }
    for (destination, winner) in best {
        let destination_count = prevalence.get(destination).copied().unwrap_or(0);
        let confidence = winner.support as f64 / winner.source_count as f64;
        let lift = confidence / (destination_count as f64 / history_total as f64);
        let supporting_source_ids = intersect_sorted(
            rows_by_selector
                .get(winner.source)
                .map_or(&[][..], Vec::as_slice),
            rows_by_selector
                .get(destination)
                .map_or(&[][..], Vec::as_slice),
        )
        .flat_map(|index| rows[index].source_delivery_ids.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
        let association = RecommendationAssociation {
            from_selector: winner.source.to_string(),
            support: winner.support,
            source_count: winner.source_count,
            destination_count,
            confidence,
            lift,
        };
        let contribution = winner.contribution;
        let entry = scored.entry(destination.to_string()).or_default();
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

/// Elements present in both ascending slices, in order.
fn intersect_sorted<'a>(left: &'a [usize], right: &'a [usize]) -> impl Iterator<Item = usize> + 'a {
    let mut right = right.iter().copied().peekable();
    left.iter().copied().filter(move |value| {
        while right.next_if(|candidate| candidate < value).is_some() {}
        right.next_if_eq(value).is_some()
    })
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
    graph: &Graph,
    level: RecommendationLevel,
    resolver: &TargetTree,
    scored: &mut BTreeMap<String, Accumulator>,
) -> Result<StructureEvidence, GraphError> {
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

/// Weight of commit-message relevance relative to task-text relevance.
///
/// Live evaluation overrides this per request. The production value is the
/// measured setting recorded in `docs/design/change-recommendations.md`.
pub(crate) const COMMIT_TEXT_WEIGHT: f64 = 0.5;

/// Exponent on commit-message query coverage. `2.0` squares similarity, so a
/// partial overlap is much weaker than a message that covers the query.
pub(crate) const COMMIT_TEXT_EXPONENT: f64 = 2.0;

/// How a live free-text request turns commit-message overlap into relevance.
#[derive(Debug, Clone, Copy)]
struct CommitTextPolicy {
    /// `false` when the weight is zero: commit messages are not read.
    enabled: bool,
    weight: f64,
    exponent: f64,
}

fn commit_text_policy(request: &RecommendationRequest) -> Result<CommitTextPolicy, GraphError> {
    let weight = request.commit_text_weight.unwrap_or(COMMIT_TEXT_WEIGHT);
    let exponent = request.commit_text_exponent.unwrap_or(COMMIT_TEXT_EXPONENT);
    if !weight.is_finite() || weight < 0.0 {
        return Err(GraphError::invalid_input(
            "validate recommendation request",
            "commit_text_weight",
            format!("commit_text_weight must be finite and non-negative; got {weight}"),
        ));
    }
    if !exponent.is_finite() || exponent < 0.0 {
        return Err(GraphError::invalid_input(
            "validate recommendation request",
            "commit_text_exponent",
            format!("commit_text_exponent must be finite and non-negative; got {exponent}"),
        ));
    }
    Ok(CommitTextPolicy {
        enabled: weight > 0.0,
        weight,
        exponent,
    })
}

/// Relevance of a commit message: `weight × similarity^exponent`.
fn commit_text_relevance(similarity: f64, policy: CommitTextPolicy) -> f64 {
    policy.weight * similarity.powf(policy.exponent)
}

/// Whether `text` cites a bracketed task id such as `[ORB-13229]`.
///
/// Squash-merge subjects in this constellation restate the task title and
/// append that id. The live evaluation uses the marker to split cohorts
/// without reading an external task store.
pub(crate) fn subject_cites_task_id(text: &str) -> bool {
    !cited_task_ids(text).is_empty()
}

/// Upper bound on commit-message bytes considered per delivery.
const COMMIT_TEXT_MAX_BYTES: usize = 4_096;

/// Attribution and bookkeeping trailer keys (compared case-insensitively)
/// removed from a commit message's final paragraph. Keys starting with
/// `orbit-` are removed too. Other `Key: value` lines, such as `Scope: …` or a
/// squashed `fix: …` subject, are message content and are kept.
const KNOWN_TRAILER_KEYS: &[&str] = &[
    "acked-by",
    "cc",
    "change-id",
    "claude-session",
    "co-authored-by",
    "helped-by",
    "implemented-by",
    "planned-by",
    "reported-by",
    "reviewed-by",
    "signed-off-by",
    "suggested-by",
    "tested-by",
];

/// Bounded subject and body of a commit, without its known trailers.
///
/// Read from the immutable commit object of the delivery's landed revision,
/// so existing history indexes need no re-extraction.
fn commit_message_text(repo: &Repository, revision: Oid) -> Option<String> {
    let commit = repo.find_commit(revision).ok()?;
    let message = String::from_utf8_lossy(commit.message_bytes()).into_owned();
    let mut paragraphs = message
        .trim()
        .split("\n\n")
        .map(|paragraph| paragraph.trim().to_string())
        .filter(|paragraph| !paragraph.is_empty())
        .collect::<Vec<_>>();
    if paragraphs.len() > 1
        && let Some(last) = paragraphs.pop()
    {
        let kept = last
            .lines()
            .filter(|line| !is_known_trailer_line(line))
            .collect::<Vec<_>>()
            .join("\n");
        if !kept.trim().is_empty() {
            paragraphs.push(kept);
        }
    }
    let mut text = paragraphs.join("\n\n");
    if text.len() > COMMIT_TEXT_MAX_BYTES {
        let mut end = COMMIT_TEXT_MAX_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    (!text.trim().is_empty()).then_some(text)
}

/// A `Key: value` line whose key is a known attribution or bookkeeping
/// trailer ([`KNOWN_TRAILER_KEYS`] or an `orbit-` key).
fn is_known_trailer_line(line: &str) -> bool {
    line.trim().split_once(": ").is_some_and(|(key, value)| {
        let key = key.to_ascii_lowercase();
        !value.trim().is_empty()
            && key
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
            && (KNOWN_TRAILER_KEYS.contains(&key.as_str())
                || key
                    .strip_prefix("orbit-")
                    .is_some_and(|rest| !rest.is_empty()))
    })
}

/// Bracketed `[ABC-123]` identifiers, as squash-merge subjects cite tasks.
fn cited_task_ids(text: &str) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for candidate in text.split('[').skip(1) {
        let Some((id, _)) = candidate.split_once(']') else {
            continue;
        };
        let Some((prefix, number)) = id.split_once('-') else {
            continue;
        };
        if prefix
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_uppercase())
            && prefix
                .chars()
                .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
            && !number.is_empty()
            && number.chars().all(|ch| ch.is_ascii_digit())
        {
            ids.insert(id.to_string());
        }
    }
    ids.into_iter().collect()
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

fn commit_distance(repo: &Repository, from: Oid, to: Oid) -> Result<usize, GraphError> {
    if from == to {
        return Ok(0);
    }
    let mut walk = repo
        .revwalk()
        .map_err(git_error("compute commit distance: distance unknown"))?;
    walk.push(to)
        .map_err(git_error("compute commit distance: distance unknown"))?;
    walk.hide(from)
        .map_err(git_error("compute commit distance: distance unknown"))?;
    walk.try_fold(0usize, |count, step| {
        step.map(|_| count + 1)
            .map_err(git_error("compute commit distance: distance unknown"))
    })
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
        lineage.index_first_parent_distances(
            repo,
            visible
                .iter()
                .map(|delivery| delivery.after_revision)
                .collect(),
        )?;
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
        let mut ordered = Vec::new();
        for (revision, renames) in by_revision {
            ordered.push((lineage.distance(repo, revision)?, renames));
        }
        ordered.sort_by_key(|(distance, _)| std::cmp::Reverse(*distance));
        lineage.steps = ordered;
        for delivery in visible {
            let distance = lineage.distance(repo, delivery.after_revision)?;
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

    /// Seed exact distances for the target's first-parent chain in one pass.
    ///
    /// Along the chain `c0 = target, c1, c2, ...` each commit's reachable set
    /// contains its first parent's, so `distance(ck)` is the running sum of
    /// `|reach(ci) \ reach(ci+1)|`: one for an ordinary commit, plus the side
    /// commits a merge introduces. This equals [`commit_distance`] exactly but
    /// costs one step per chain commit instead of one revwalk per delivery.
    /// The walk stops once every wanted revision is seen or the root is reached;
    /// revisions off the chain fall back to [`commit_distance`] on demand.
    fn index_first_parent_distances(
        &mut self,
        repo: &Repository,
        mut wanted: BTreeSet<Oid>,
    ) -> Result<(), GraphError> {
        let mut current = repo
            .find_commit(self.target)
            .map_err(git_error("index first-parent distances: distance unknown"))?;
        let mut distance = 0;
        loop {
            self.distances.insert(current.id(), distance);
            wanted.remove(&current.id());
            if wanted.is_empty() {
                return Ok(());
            }
            if current.parent_count() == 0 {
                return Ok(());
            }
            let parent = current
                .parent(0)
                .map_err(git_error("index first-parent distances: distance unknown"))?;
            distance += if current.parent_count() == 1 {
                1
            } else {
                commit_distance(repo, parent.id(), current.id())?
            };
            current = parent;
        }
    }

    /// Commits reachable from the target but not from `revision`, memoized.
    fn distance(&mut self, repo: &Repository, revision: Oid) -> Result<usize, GraphError> {
        if let Some(distance) = self.distances.get(&revision) {
            return Ok(*distance);
        }
        let distance = commit_distance(repo, revision, self.target)?;
        self.distances.insert(revision, distance);
        Ok(distance)
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
        let origin = self.distance(repo, after_revision)?;
        let mut current = path.to_string();
        // A current file can reuse an old spelling after the historical file
        // was renamed or deleted, so follow identity before checking the tree.
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

/// Layout version of the on-disk target symbol cache.
const TARGET_SYMBOL_CACHE_FORMAT: u32 = 1;

/// Current-extractor cache files kept per index directory (most recently used
/// first).
const TARGET_SYMBOL_CACHE_KEEP: usize = 4;

/// A temp file whose writer process is gone is removed once it is this old.
const TARGET_SYMBOL_CACHE_DEAD_TEMP_AGE: Duration = Duration::from_secs(60);

/// A temp file is removed once it is this old, whatever its pid says (the pid
/// may have been reused, or belong to another PID namespace).
const TARGET_SYMBOL_CACHE_STALE_TEMP_AGE: Duration = Duration::from_secs(60 * 60);

const TARGET_SYMBOL_CACHE_PREFIX: &str = "recommend-target.";

static NO_SYMBOLS: BTreeMap<String, Vec<RawSymbol>> = BTreeMap::new();

#[cfg(test)]
thread_local! {
    /// Full target-tree symbol extractions performed on this thread (test instrumentation).
    static TARGET_SYMBOL_EXTRACTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Blob paths of the immutable target tree plus, when a request needs them,
/// the symbols the language extractors find in those blobs.
///
/// Walking the tree is cheap; parsing every blob is not. Symbols are therefore
/// loaded only for requests that read them (lexical, structural, or symbol
/// level), and the extracted table is cached on disk keyed by the target
/// *tree* OID and [`crate::EXTRACTOR_VERSION`], so repeated requests at an
/// unchanged revision skip parsing. Free-text signatures are redacted before
/// the table is cached or used, so a credential in a default argument is not
/// durable and cached results stay identical to a cold extraction.
struct TargetTree {
    tree: Oid,
    files: BTreeMap<String, Oid>,
    symbols: Option<BTreeMap<String, Vec<RawSymbol>>>,
}

impl TargetTree {
    /// Walk the target tree's blob paths without parsing any of them.
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
        Ok(Self {
            tree: tree.id(),
            files,
            symbols: None,
        })
    }

    /// Load the symbol table, from `cache_dir` when a valid entry exists.
    ///
    /// Cache reads and writes are best effort: a missing, stale, corrupt, or
    /// unwritable cache falls back to extraction and never fails the request.
    fn ensure_symbols(
        &mut self,
        repo: &Repository,
        cache_dir: Option<&Path>,
    ) -> Result<(), GraphError> {
        if self.symbols.is_some() {
            return Ok(());
        }
        let cache_path = cache_dir.map(|dir| target_symbol_cache_path(dir, self.tree));
        if let Some((path, symbols)) = cache_path.as_deref().and_then(|path| {
            read_target_symbol_cache(path, self.tree).map(|symbols| (path, symbols))
        }) {
            touch_target_symbol_cache(path);
            self.symbols = Some(symbols);
            return Ok(());
        }
        let mut symbols = self.extract_symbols(repo)?;
        redact_cached_signatures(&mut symbols);
        if let (Some(dir), Some(path)) = (cache_dir, cache_path.as_deref()) {
            write_target_symbol_cache(dir, path, self.tree, &symbols);
        }
        self.symbols = Some(symbols);
        Ok(())
    }

    fn extract_symbols(
        &self,
        repo: &Repository,
    ) -> Result<BTreeMap<String, Vec<RawSymbol>>, GraphError> {
        #[cfg(test)]
        TARGET_SYMBOL_EXTRACTIONS.with(|count| count.set(count.get() + 1));
        let extractors = languages::extractors();
        let mut symbols = BTreeMap::new();
        for (path, oid) in &self.files {
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
        Ok(symbols)
    }

    /// Symbols by path; empty unless [`Self::ensure_symbols`] ran.
    fn symbols(&self) -> &BTreeMap<String, Vec<RawSymbol>> {
        debug_assert!(
            self.symbols.is_some(),
            "target symbols read before they were loaded"
        );
        self.symbols.as_ref().unwrap_or(&NO_SYMBOLS)
    }

    fn baseline_candidates(&self, level: RecommendationLevel) -> Vec<(String, String)> {
        match level {
            RecommendationLevel::File => self
                .files
                .keys()
                .map(|path| {
                    let mut text = path.clone();
                    if let Some(symbols) = self.symbols().get(path) {
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
                .symbols()
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
        self.symbols().get(path).is_some_and(|symbols| {
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
            .symbols()
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
            .symbols()
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
            .symbols()
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

#[derive(Serialize, Deserialize)]
struct TargetSymbolCache {
    format: u32,
    extractor_version: u32,
    tree: String,
    files: Vec<CachedFileSymbols>,
}

#[derive(Serialize, Deserialize)]
struct CachedFileSymbols {
    path: String,
    symbols: Vec<CachedSymbol>,
}

#[derive(Serialize, Deserialize)]
struct CachedSymbol {
    /// Present only when it differs from the owning file path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_path: Option<String>,
    name: String,
    qualified: String,
    kind: String,
    span_start: usize,
    span_end: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_symbol: Option<String>,
}

/// Mask credential-shaped literals in signatures before the table is cached.
///
/// Names and qualified names are identifiers and are not passed through the
/// redactor. Redacting here, rather than only in the JSON writer, keeps a
/// cache hit identical to the cold path that just extracted these symbols.
fn redact_cached_signatures(symbols: &mut BTreeMap<String, Vec<RawSymbol>>) {
    for file_symbols in symbols.values_mut() {
        for symbol in file_symbols {
            if let Some(signature) = symbol.signature.as_mut()
                && let std::borrow::Cow::Owned(redacted) = crate::redaction::redact(signature)
            {
                *signature = redacted;
            }
        }
    }
}

fn target_symbol_cache_path(dir: &Path, tree: Oid) -> PathBuf {
    dir.join(format!(
        "{TARGET_SYMBOL_CACHE_PREFIX}{}.{tree}.json",
        crate::EXTRACTOR_VERSION
    ))
}

/// Read a cache entry, rejecting any whose recorded format, extractor version,
/// or tree differs from what its name promises.
fn read_target_symbol_cache(path: &Path, tree: Oid) -> Option<BTreeMap<String, Vec<RawSymbol>>> {
    let bytes = std::fs::read(path).ok()?;
    let cache: TargetSymbolCache = serde_json::from_slice(bytes.as_slice()).ok()?;
    if cache.format != TARGET_SYMBOL_CACHE_FORMAT
        || cache.extractor_version != crate::EXTRACTOR_VERSION
        || cache.tree != tree.to_string()
    {
        return None;
    }
    Some(
        cache
            .files
            .into_iter()
            .map(|file| {
                let symbols = file
                    .symbols
                    .into_iter()
                    .map(|symbol| RawSymbol {
                        file_path: symbol.file_path.unwrap_or_else(|| file.path.clone()),
                        name: symbol.name,
                        qualified: symbol.qualified,
                        kind: symbol.kind,
                        span_start: symbol.span_start,
                        span_end: symbol.span_end,
                        signature: symbol.signature,
                        parent_symbol: symbol.parent_symbol,
                    })
                    .collect();
                (file.path, symbols)
            })
            .collect(),
    )
}

/// Atomically publish a cache entry through [`crate::atomic_write`] and
/// prune stale entries. Failures are ignored: the cache only saves work.
fn write_target_symbol_cache(
    dir: &Path,
    path: &Path,
    tree: Oid,
    symbols: &BTreeMap<String, Vec<RawSymbol>>,
) {
    let cache = TargetSymbolCache {
        format: TARGET_SYMBOL_CACHE_FORMAT,
        extractor_version: crate::EXTRACTOR_VERSION,
        tree: tree.to_string(),
        files: symbols
            .iter()
            .map(|(file_path, symbols)| CachedFileSymbols {
                path: file_path.clone(),
                symbols: symbols
                    .iter()
                    .map(|symbol| CachedSymbol {
                        file_path: (symbol.file_path != *file_path)
                            .then(|| symbol.file_path.clone()),
                        name: symbol.name.clone(),
                        qualified: symbol.qualified.clone(),
                        kind: symbol.kind.clone(),
                        span_start: symbol.span_start,
                        span_end: symbol.span_end,
                        signature: symbol.signature.clone(),
                        parent_symbol: symbol.parent_symbol.clone(),
                    })
                    .collect(),
            })
            .collect(),
    };
    let Ok(bytes) = serde_json::to_vec(&cache) else {
        return;
    };
    if crate::atomic_write(path, bytes.as_slice()).is_err() {
        return;
    }
    prune_target_symbol_caches(dir, path);
}

/// Record a warm hit as the entry's last use, so pruning keeps the entries
/// that are actually read. Best effort.
fn touch_target_symbol_cache(path: &Path) {
    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

/// A file name this cache owns, parsed from its exact grammar:
/// `recommend-target.<extractor u32>.<40 hex tree OID>.json`, or the same name
/// plus `.tmp-<pid>-<n>` (`.tmp-<pid>` from older writers) while it is being
/// written. Anything else in the directory is not ours and is never deleted
/// (STD-03 §R29).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetSymbolCacheName {
    Entry { extractor_version: u32 },
    Temp { extractor_version: u32, pid: u32 },
}

fn parse_target_symbol_cache_name(name: &str) -> Option<TargetSymbolCacheName> {
    fn decimal<T: std::str::FromStr>(value: &str) -> Option<T> {
        (!value.is_empty()
            && value.bytes().all(|byte| byte.is_ascii_digit())
            && (value == "0" || !value.starts_with('0')))
        .then(|| value.parse().ok())
        .flatten()
    }
    let rest = name.strip_prefix(TARGET_SYMBOL_CACHE_PREFIX)?;
    let (version, rest) = rest.split_once('.')?;
    let extractor_version = decimal::<u32>(version)?;
    let (tree, suffix) = rest.split_once('.')?;
    if tree.len() != 40
        || !tree
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    if suffix == "json" {
        return Some(TargetSymbolCacheName::Entry { extractor_version });
    }
    // `json.tmp-<pid>-<n>` from `atomic_write`, or `json.tmp-<pid>` as
    // writers before it named their temp files.
    let pid = crate::atomic_write_temp_pid(suffix, "json")
        .or_else(|| decimal::<u32>(suffix.strip_prefix("json.tmp-")?))?;
    Some(TargetSymbolCacheName::Temp {
        extractor_version,
        pid,
    })
}

/// Whether `pid` is known not to be a running process. Unknown (no `/proc`)
/// is never "gone"; such temp files age out instead.
fn process_is_gone(pid: u32) -> bool {
    let proc_root = Path::new("/proc");
    proc_root.join("self").exists() && !proc_root.join(pid.to_string()).exists()
}

/// Prune the cache directory after a write:
///
/// - only names matching the cache grammar are considered;
/// - entries for *older* extractor versions are removed, while newer ones
///   belong to a newer binary sharing the directory and are left alone
///   (STD-03 §R10);
/// - temp files left by a crashed writer are removed when their pid is gone
///   (after a short grace) or once they are an hour old;
/// - of the current version's entries, `keep` and the most recently used
///   others are retained, up to [`TARGET_SYMBOL_CACHE_KEEP`].
fn prune_target_symbol_caches(dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    let mut retained = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(parse_target_symbol_cache_name)
        else {
            continue;
        };
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
        let age = now.duration_since(modified).unwrap_or_default();
        match name {
            TargetSymbolCacheName::Temp { pid, .. } => {
                let stale = age >= TARGET_SYMBOL_CACHE_STALE_TEMP_AGE
                    || (age >= TARGET_SYMBOL_CACHE_DEAD_TEMP_AGE && process_is_gone(pid));
                if stale {
                    let _ = std::fs::remove_file(path.as_path());
                }
            }
            TargetSymbolCacheName::Entry { extractor_version }
                if extractor_version < crate::EXTRACTOR_VERSION =>
            {
                let _ = std::fs::remove_file(path.as_path());
            }
            TargetSymbolCacheName::Entry { extractor_version }
                if extractor_version == crate::EXTRACTOR_VERSION && path != keep =>
            {
                retained.push((modified, path));
            }
            TargetSymbolCacheName::Entry { .. } => {}
        }
    }
    retained.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, path) in retained.into_iter().skip(TARGET_SYMBOL_CACHE_KEEP - 1) {
        let _ = std::fs::remove_file(path);
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
    let repo = Repository::open(repo_root)
        .map_err(|error| GraphError::git("open selector validation repository", error))?;
    let oid = Oid::from_str(revision).map_err(git_error("parse selector validation revision"))?;
    let mut tree = TargetTree::load(&repo, oid)?;
    if !selector.starts_with("file:") {
        tree.ensure_symbols(&repo, None)?;
    }
    Ok(tree.selector_is_live(selector))
}

fn git_error(operation: &'static str) -> impl FnOnce(git2::Error) -> GraphError {
    move |error| GraphError::git(operation, error)
}

#[cfg(test)]
#[path = "recommend/tests/mod.rs"]
mod tests;

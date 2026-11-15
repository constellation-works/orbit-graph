//! The evaluation readout: per-variant metrics with coverage, the split by
//! cases whose delivery mostly added files, paired differences between the
//! word index and every other ranking with a bootstrap interval, and the
//! context-index design's gates G1 and G2 (§10.5).
//!
//! Everything here is a pure function of the per-case rankings the
//! evaluation collected, so a parameter grid re-reads the same cases under
//! each word-index configuration without ranking anything twice.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::{RecommendationLevel, RecommendationVariant, WordIndexOptions};

/// Reciprocal-rank-fusion constant: `score = Σ 1 / (RRF_K + rank)`.
pub(super) const RRF_K: f64 = 60.0;

/// Bootstrap resamples behind every paired interval.
pub(super) const BOOTSTRAP_RESAMPLES: usize = 2_000;

/// Fixed seed, so the same cases give the same interval on every run.
const BOOTSTRAP_SEED: u64 = 0x0b17_9a4b_5eed_2026;

/// The cutoff the gates are defined at (design §10.5: recall@10).
pub(super) const GATE_K: usize = 10;

/// G1's smallest word-index coverage: half the test tasks.
pub(super) const GATE_MIN_COVERAGE: f64 = 0.5;

/// Every ranking the evaluation scores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationVariant {
    /// `recommend`'s combined ranking.
    Combined,
    /// `recommend`'s similar-task ranking.
    TaskSearchOnly,
    /// `recommend`'s current-tree lexical and structural ranking.
    GraphOnly,
    /// `recommend`'s query-independent delivery frequency.
    Frequency,
    /// `recommend`'s word index.
    WordIndex,
    /// BM25 over the files (file level) or the symbols (symbol level) of the
    /// tree at the fold's cutoff commit. No history.
    Bm25Code,
    /// Paths and symbol names the task text writes that resolve at the
    /// case's target revision, in the order written.
    ExplicitMentions,
    /// Explicit mentions first, then the word index's ranking.
    WordIndexPlusMentions,
    /// The word index and BM25 over code fused by reciprocal rank.
    WordIndexBm25Rrf,
}

impl EvaluationVariant {
    /// Every variant, in report order.
    pub const ALL: [Self; 9] = [
        Self::Combined,
        Self::TaskSearchOnly,
        Self::GraphOnly,
        Self::Frequency,
        Self::WordIndex,
        Self::Bm25Code,
        Self::ExplicitMentions,
        Self::WordIndexPlusMentions,
        Self::WordIndexBm25Rrf,
    ];

    /// The `recommend` variant behind this row, if it is one.
    pub(super) const fn recommendation(self) -> Option<RecommendationVariant> {
        match self {
            Self::Combined => Some(RecommendationVariant::Combined),
            Self::TaskSearchOnly => Some(RecommendationVariant::TaskSearchOnly),
            Self::GraphOnly => Some(RecommendationVariant::GraphOnly),
            Self::Frequency => Some(RecommendationVariant::Frequency),
            Self::WordIndex => Some(RecommendationVariant::WordIndex),
            Self::Bm25Code
            | Self::ExplicitMentions
            | Self::WordIndexPlusMentions
            | Self::WordIndexBm25Rrf => None,
        }
    }

    /// The JSON name.
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Combined => "combined",
            Self::TaskSearchOnly => "task_search_only",
            Self::GraphOnly => "graph_only",
            Self::Frequency => "frequency",
            Self::WordIndex => "word_index",
            Self::Bm25Code => "bm25_code",
            Self::ExplicitMentions => "explicit_mentions",
            Self::WordIndexPlusMentions => "word_index_plus_mentions",
            Self::WordIndexBm25Rrf => "word_index_bm25_rrf",
        }
    }

    /// Whether this row is derived from the word index's ranking, and so
    /// follows the word-index configuration a readout uses.
    const fn reads_word_index(self) -> bool {
        matches!(
            self,
            Self::WordIndex | Self::WordIndexPlusMentions | Self::WordIndexBm25Rrf
        )
    }
}

pub(super) const LEVELS: [RecommendationLevel; 2] =
    [RecommendationLevel::File, RecommendationLevel::Symbol];

pub(super) const fn level_index(level: RecommendationLevel) -> usize {
    match level {
        RecommendationLevel::File => 0,
        RecommendationLevel::Symbol => 1,
    }
}

/// One ranking of one case at one level, best first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct CaseRun {
    /// Ranked selectors, possibly deeper than K (combinations read past it).
    pub(super) selectors: Vec<String>,
    /// Query latency in microseconds.
    pub(super) latency_micros: u128,
}

/// Everything the readout needs from one evaluated case.
#[derive(Debug, Clone, Default)]
pub(super) struct CaseScores {
    /// Position of the case's fold in cutoff order.
    pub(super) fold: usize,
    /// Whether more than half of the delivery's changed files were added.
    pub(super) mostly_added: bool,
    /// Held-out truth by level ([`level_index`]).
    pub(super) truth: [BTreeSet<String>; 2],
    /// Selectors any ranking returned that are absent at the target revision.
    pub(super) not_live: BTreeSet<String>,
    /// Rankings that do not depend on the word-index configuration, plus the
    /// word index under the corpus's own options.
    pub(super) runs: BTreeMap<(EvaluationVariant, usize), CaseRun>,
    /// The word index under each grid cell, by level.
    pub(super) grid: Vec<[CaseRun; 2]>,
}

/// Which word-index configuration a readout uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WordIndexSource {
    /// The corpus's `word_index` options.
    Base,
    /// One grid cell.
    Cell(usize),
}

impl CaseScores {
    /// The ranking of `variant` at `level` under `source`.
    pub(super) fn run(
        &self,
        variant: EvaluationVariant,
        level: RecommendationLevel,
        source: WordIndexSource,
    ) -> CaseRun {
        let level = level_index(level);
        let stored = |variant| {
            self.runs
                .get(&(variant, level))
                .cloned()
                .unwrap_or_default()
        };
        if !variant.reads_word_index() {
            return stored(variant);
        }
        let word_index = match source {
            WordIndexSource::Base => stored(EvaluationVariant::WordIndex),
            WordIndexSource::Cell(cell) => self
                .grid
                .get(cell)
                .map(|runs| runs[level].clone())
                .unwrap_or_default(),
        };
        match variant {
            EvaluationVariant::WordIndexPlusMentions => {
                let mentions = stored(EvaluationVariant::ExplicitMentions);
                prepend(&mentions, &word_index)
            }
            EvaluationVariant::WordIndexBm25Rrf => {
                let bm25 = stored(EvaluationVariant::Bm25Code);
                reciprocal_rank_fusion(&word_index, &bm25)
            }
            _ => word_index,
        }
    }
}

/// `first`'s selectors, then `second`'s not already listed.
pub(super) fn prepend(first: &CaseRun, second: &CaseRun) -> CaseRun {
    let mut seen = BTreeSet::new();
    let selectors = first
        .selectors
        .iter()
        .chain(&second.selectors)
        .filter(|selector| seen.insert(selector.as_str()))
        .cloned()
        .collect();
    CaseRun {
        selectors,
        latency_micros: first.latency_micros + second.latency_micros,
    }
}

/// Reciprocal rank fusion: each selector scores `Σ 1 / (RRF_K + rank)` over
/// the rankings it appears in (rank from 1); best first, ties by selector.
pub(super) fn reciprocal_rank_fusion(left: &CaseRun, right: &CaseRun) -> CaseRun {
    let mut scores: BTreeMap<&str, f64> = BTreeMap::new();
    for run in [left, right] {
        for (index, selector) in run.selectors.iter().enumerate() {
            *scores.entry(selector.as_str()).or_default() += 1.0 / (RRF_K + (index + 1) as f64);
        }
    }
    let mut ranked = scores.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    CaseRun {
        selectors: ranked
            .into_iter()
            .map(|(selector, _)| selector.to_string())
            .collect(),
        latency_micros: left.latency_micros + right.latency_micros,
    }
}

/// One case's result for one ranking at one level.
struct CaseOutcome {
    relevant: usize,
    hits: usize,
    returned: usize,
    stale: usize,
    latency_micros: u128,
}

impl CaseOutcome {
    fn new(case: &CaseScores, run: &CaseRun, level: RecommendationLevel, k: usize) -> Self {
        let truth = &case.truth[level_index(level)];
        let top = &run.selectors[..run.selectors.len().min(k)];
        Self {
            relevant: truth.len(),
            hits: top
                .iter()
                .filter(|selector| truth.contains(selector.as_str()))
                .count(),
            returned: top.len(),
            stale: top
                .iter()
                .filter(|selector| case.not_live.contains(selector.as_str()))
                .count(),
            latency_micros: run.latency_micros,
        }
    }

    fn recall(&self) -> Option<f64> {
        ratio(self.hits, self.relevant)
    }

    fn precision(&self, k: usize) -> f64 {
        self.hits as f64 / k as f64
    }
}

/// Aggregate metrics for one ranking and destination level.
#[derive(Debug, Clone, Serialize)]
pub struct EvaluationMetrics {
    /// Ranking.
    pub variant: EvaluationVariant,
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
    /// Recommendations returned across all cases: the stale-rate denominator.
    pub returned: usize,
    /// Micro-averaged (pooled) recall at K; `None` (`null`) when `relevant` is 0.
    pub recall_at_k: Option<f64>,
    /// Precision at K, with K slots per evaluated case as denominator;
    /// `None` (`null`) when `cases` is 0.
    pub precision_at_k: Option<f64>,
    /// Cases with at least one relevant destination at this level.
    pub cases_with_truth: usize,
    /// Recall at K averaged over `cases_with_truth`; `None` (`null`) when
    /// there are none. Paired differences and the gates read this.
    pub mean_recall_at_k: Option<f64>,
    /// Cases with a non-empty answer.
    pub cases_answered: usize,
    /// `cases_answered / cases`; `None` (`null`) when `cases` is 0.
    pub coverage: Option<f64>,
    /// Recommendations absent from the immutable target tree divided by
    /// returned results; `None` (`null`) when `returned` is 0.
    pub stale_result_rate: Option<f64>,
    /// Mean query latency in milliseconds; `None` (`null`) with no queries.
    /// A combination's latency is the sum of its parts'.
    pub mean_latency_ms: Option<f64>,
    /// Maximum query latency in milliseconds; `None` (`null`) with no queries.
    pub max_latency_ms: Option<f64>,
}

/// Metrics of `variant` at `level` over `cases`.
pub(super) fn metrics(
    cases: &[&CaseScores],
    variant: EvaluationVariant,
    level: RecommendationLevel,
    source: WordIndexSource,
    k: usize,
) -> EvaluationMetrics {
    let outcomes = cases
        .iter()
        .map(|case| CaseOutcome::new(case, &case.run(variant, level, source), level, k))
        .collect::<Vec<_>>();
    let relevant = outcomes.iter().map(|outcome| outcome.relevant).sum();
    let true_positives = outcomes.iter().map(|outcome| outcome.hits).sum();
    let returned = outcomes.iter().map(|outcome| outcome.returned).sum();
    let stale = outcomes.iter().map(|outcome| outcome.stale).sum();
    let recalls = outcomes
        .iter()
        .filter_map(CaseOutcome::recall)
        .collect::<Vec<_>>();
    let answered = outcomes
        .iter()
        .filter(|outcome| outcome.returned > 0)
        .count();
    let latencies = outcomes
        .iter()
        .map(|outcome| outcome.latency_micros)
        .collect::<Vec<_>>();
    EvaluationMetrics {
        variant,
        level,
        k,
        cases: outcomes.len(),
        relevant,
        true_positives,
        returned,
        recall_at_k: ratio(true_positives, relevant),
        precision_at_k: ratio(true_positives, outcomes.len() * k),
        cases_with_truth: recalls.len(),
        mean_recall_at_k: mean(recalls.as_slice()),
        cases_answered: answered,
        coverage: ratio(answered, outcomes.len()),
        stale_result_rate: ratio(stale, returned),
        mean_latency_ms: mean_latency_ms(latencies.as_slice()),
        max_latency_ms: latencies
            .iter()
            .max()
            .map(|micros| *micros as f64 / 1_000.0),
    }
}

/// One row per variant and level.
pub(super) fn all_metrics(
    cases: &[&CaseScores],
    source: WordIndexSource,
    k: usize,
) -> Vec<EvaluationMetrics> {
    EvaluationVariant::ALL
        .into_iter()
        .flat_map(|variant| LEVELS.map(|level| metrics(cases, variant, level, source, k)))
        .collect()
}

/// Metrics over the cases on one side of the added-files split.
#[derive(Debug, Clone, Serialize)]
pub struct SplitMetrics {
    /// `mostly_added_files`: more than half of the delivery's changed files
    /// were added, so most of its work is outside any truth an index over
    /// history can name. `mostly_existing_files`: the rest.
    pub split: String,
    /// Cases on this side.
    pub cases: usize,
    /// One row per variant and level.
    pub metrics: Vec<EvaluationMetrics>,
}

fn split_metrics(cases: &[&CaseScores], source: WordIndexSource, k: usize) -> Vec<SplitMetrics> {
    [
        ("mostly_added_files", true),
        ("mostly_existing_files", false),
    ]
    .into_iter()
    .map(|(split, added)| {
        let side = cases
            .iter()
            .copied()
            .filter(|case| case.mostly_added == added)
            .collect::<Vec<_>>();
        SplitMetrics {
            split: split.to_string(),
            cases: side.len(),
            metrics: all_metrics(side.as_slice(), source, k),
        }
    })
    .collect()
}

/// Per-case metric a paired difference compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairedMetric {
    /// Recall at K, over the cases with truth at the level.
    RecallAtK,
    /// Precision at K, over every case.
    PrecisionAtK,
}

/// Mean and bootstrap interval of a per-case difference.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct PairedStats {
    pub(super) cases: usize,
    pub(super) mean: Option<f64>,
    pub(super) interval: Option<(f64, f64)>,
}

impl PairedStats {
    /// The interval lies above zero.
    fn above_zero(&self) -> bool {
        self.interval.is_some_and(|(low, _)| low > 0.0)
    }

    /// The interval lies below zero.
    fn below_zero(&self) -> bool {
        self.interval.is_some_and(|(_, high)| high < 0.0)
    }
}

/// `left − right` per case, over the same cases, with a 95% percentile
/// bootstrap interval.
pub(super) fn paired(
    cases: &[&CaseScores],
    left: EvaluationVariant,
    right: EvaluationVariant,
    level: RecommendationLevel,
    metric: PairedMetric,
    source: WordIndexSource,
    k: usize,
) -> PairedStats {
    let differences = cases
        .iter()
        .filter_map(|case| {
            let left = CaseOutcome::new(case, &case.run(left, level, source), level, k);
            let right = CaseOutcome::new(case, &case.run(right, level, source), level, k);
            match metric {
                PairedMetric::RecallAtK => Some(left.recall()? - right.recall()?),
                PairedMetric::PrecisionAtK => Some(left.precision(k) - right.precision(k)),
            }
        })
        .collect::<Vec<_>>();
    PairedStats {
        cases: differences.len(),
        mean: mean(differences.as_slice()),
        interval: bootstrap_interval(differences.as_slice()),
    }
}

/// Word index minus one other ranking, per case.
#[derive(Debug, Clone, Serialize)]
pub struct PairedDifference {
    /// The ranking `word_index` is compared with.
    pub baseline: EvaluationVariant,
    /// File or symbol level.
    pub level: RecommendationLevel,
    /// Per-case metric compared.
    pub metric: PairedMetric,
    /// Cases paired: those with truth at the level for recall, every case
    /// for precision.
    pub cases: usize,
    /// Mean of `word_index − baseline`; `None` (`null`) with no cases.
    pub mean_difference: Option<f64>,
    /// 2.5th percentile of the bootstrap means; `None` (`null`) with fewer
    /// than two cases.
    pub interval_low: Option<f64>,
    /// 97.5th percentile of the bootstrap means; `None` (`null`) with fewer
    /// than two cases.
    pub interval_high: Option<f64>,
    /// `word_index_higher` or `baseline_higher` when the interval excludes
    /// zero, `no_difference_shown` when it includes it, `unevaluable` when
    /// there is no interval.
    pub verdict: String,
}

fn paired_differences(
    cases: &[&CaseScores],
    source: WordIndexSource,
    k: usize,
) -> Vec<PairedDifference> {
    let mut out = Vec::new();
    for baseline in EvaluationVariant::ALL {
        if baseline == EvaluationVariant::WordIndex {
            continue;
        }
        for level in LEVELS {
            for metric in [PairedMetric::RecallAtK, PairedMetric::PrecisionAtK] {
                let stats = paired(
                    cases,
                    EvaluationVariant::WordIndex,
                    baseline,
                    level,
                    metric,
                    source,
                    k,
                );
                let verdict = if stats.interval.is_none() {
                    "unevaluable"
                } else if stats.above_zero() {
                    "word_index_higher"
                } else if stats.below_zero() {
                    "baseline_higher"
                } else {
                    "no_difference_shown"
                };
                out.push(PairedDifference {
                    baseline,
                    level,
                    metric,
                    cases: stats.cases,
                    mean_difference: stats.mean,
                    interval_low: stats.interval.map(|(low, _)| low),
                    interval_high: stats.interval.map(|(_, high)| high),
                    verdict: verdict.to_string(),
                });
            }
        }
    }
    out
}

/// Metrics, the added-files split and the paired differences over one set
/// of cases under one word-index configuration.
#[derive(Debug, Clone, Serialize)]
pub struct Readout {
    /// Cases read.
    pub cases: usize,
    /// One row per variant and level.
    pub metrics: Vec<EvaluationMetrics>,
    /// The same rows over each side of the added-files split.
    pub split_metrics: Vec<SplitMetrics>,
    /// `word_index` minus each other ranking, per level and metric.
    pub paired_differences: Vec<PairedDifference>,
}

pub(super) fn readout(cases: &[&CaseScores], source: WordIndexSource, k: usize) -> Readout {
    Readout {
        cases: cases.len(),
        metrics: all_metrics(cases, source, k),
        split_metrics: split_metrics(cases, source, k),
        paired_differences: paired_differences(cases, source, k),
    }
}

/// Outcome of one gate. An unevaluable gate counts as not passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateStatus {
    /// The gate's condition holds.
    Passed,
    /// The gate's condition does not hold.
    Failed,
    /// The numbers the gate needs do not exist.
    Unevaluable,
}

/// One gate of design §10.5.
#[derive(Debug, Clone, Serialize)]
pub struct GateResult {
    /// `G1` (keep the index) or `G2` (which ranking the plugin keeps).
    pub gate: String,
    /// Passed, failed or unevaluable.
    pub status: GateStatus,
    /// The numbers behind the status, in words.
    pub reason: String,
    /// G2: the ranking the plugin keeps, or `null` when none won.
    pub keep: Option<EvaluationVariant>,
    /// G1 when not passed: the history-free or combined ranking with the
    /// highest mean file recall@10, the candidate for the slot instead.
    pub alternative: Option<EvaluationVariant>,
    /// G2: combinations that beat both the word index and each existing
    /// `recommend` ranking, so both parts would stay.
    pub combinations_beating_both: Vec<EvaluationVariant>,
}

/// Gates G1 and G2 over one set of cases.
#[derive(Debug, Clone, Serialize)]
pub struct GateReadout {
    /// `all_cases` without a grid; `headline_folds` with one, read on the
    /// selected cell.
    pub scope: String,
    /// Word-index options the gates were read with.
    pub word_index_options: Option<WordIndexOptions>,
    /// Cases read.
    pub cases: usize,
    /// G1 then G2. G3 needs pilot data and G4 option settings this
    /// evaluation does not have, so neither is reported.
    pub results: Vec<GateResult>,
}

fn describe(stats: &PairedStats) -> String {
    match (stats.mean, stats.interval) {
        (Some(mean), Some((low, high))) => {
            format!(
                "{mean:+.3} [{low:+.3}, {high:+.3}] over {} cases",
                stats.cases
            )
        }
        _ => format!("no interval ({} cases)", stats.cases),
    }
}

fn unevaluable(gate: &str, reason: String) -> GateResult {
    GateResult {
        gate: gate.to_string(),
        status: GateStatus::Unevaluable,
        reason,
        keep: None,
        alternative: None,
        combinations_beating_both: Vec::new(),
    }
}

/// G1 and G2, or both unevaluable with `reason` when the cases to read them
/// on do not exist.
pub(super) fn gates(
    cases: &[&CaseScores],
    source: WordIndexSource,
    k: usize,
    unavailable: Option<&str>,
) -> Vec<GateResult> {
    let blocker = unavailable.map(str::to_string).or_else(|| {
        (k != GATE_K).then(|| {
            format!("the gates are defined on recall@{GATE_K}; this corpus scores k = {k}")
        })
    });
    if let Some(reason) = blocker {
        return vec![unevaluable("G1", reason.clone()), unevaluable("G2", reason)];
    }
    vec![gate_one(cases, source, k), gate_two(cases, source, k)]
}

fn file_recall(
    cases: &[&CaseScores],
    left: EvaluationVariant,
    right: EvaluationVariant,
    source: WordIndexSource,
    k: usize,
) -> PairedStats {
    paired(
        cases,
        left,
        right,
        RecommendationLevel::File,
        PairedMetric::RecallAtK,
        source,
        k,
    )
}

fn gate_one(cases: &[&CaseScores], source: WordIndexSource, k: usize) -> GateResult {
    use EvaluationVariant::{
        Bm25Code, Frequency, WordIndex, WordIndexBm25Rrf, WordIndexPlusMentions,
    };
    let frequency = file_recall(cases, WordIndex, Frequency, source, k);
    let bm25 = file_recall(cases, WordIndex, Bm25Code, source, k);
    let coverage = metrics(cases, WordIndex, RecommendationLevel::File, source, k).coverage;
    let numbers = format!(
        "word_index − frequency file recall@{k}: {}; word_index − bm25_code: {}; word_index file coverage {}",
        describe(&frequency),
        describe(&bm25),
        coverage.map_or_else(|| "n/a".to_string(), |value| format!("{value:.3}")),
    );
    if frequency.interval.is_none() || bm25.interval.is_none() || coverage.is_none() {
        return unevaluable(
            "G1",
            format!("fewer than two cases with file truth to pair; {numbers}"),
        );
    }
    let passed = frequency.above_zero()
        && bm25.above_zero()
        && coverage.is_some_and(|value| value >= GATE_MIN_COVERAGE);
    let alternative = (!passed)
        .then(|| {
            [Bm25Code, WordIndexPlusMentions, WordIndexBm25Rrf]
                .into_iter()
                .map(|variant| {
                    let recall = metrics(cases, variant, RecommendationLevel::File, source, k)
                        .mean_recall_at_k;
                    (variant, recall.unwrap_or(f64::NEG_INFINITY))
                })
                .reduce(|best, next| if next.1 > best.1 { next } else { best })
                .map(|(variant, _)| variant)
        })
        .flatten();
    GateResult {
        gate: "G1".to_string(),
        status: if passed {
            GateStatus::Passed
        } else {
            GateStatus::Failed
        },
        reason: if passed {
            format!(
                "the word index beats frequency and BM25 over code at coverage of at least half the cases: {numbers}"
            )
        } else {
            format!(
                "keep the index only if both intervals lie above zero at coverage of at least {GATE_MIN_COVERAGE}: {numbers}"
            )
        },
        keep: None,
        alternative,
        combinations_beating_both: Vec::new(),
    }
}

fn gate_two(cases: &[&CaseScores], source: WordIndexSource, k: usize) -> GateResult {
    use EvaluationVariant::{
        Combined, TaskSearchOnly, WordIndex, WordIndexBm25Rrf, WordIndexPlusMentions,
    };
    let existing = [Combined, TaskSearchOnly];
    let against =
        existing.map(|variant| (variant, file_recall(cases, WordIndex, variant, source, k)));
    let numbers = against
        .iter()
        .map(|(variant, stats)| {
            format!(
                "word_index − {} file recall@{k}: {}",
                variant.name(),
                describe(stats)
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    if against.iter().any(|(_, stats)| stats.interval.is_none()) {
        return unevaluable(
            "G2",
            format!("fewer than two cases with file truth to pair; {numbers}"),
        );
    }
    let combinations_beating_both = [WordIndexPlusMentions, WordIndexBm25Rrf]
        .into_iter()
        .filter(|combination| {
            std::iter::once(WordIndex)
                .chain(existing)
                .all(|other| file_recall(cases, *combination, other, source, k).above_zero())
        })
        .collect::<Vec<_>>();
    let (status, keep, verdict) = if against.iter().all(|(_, stats)| stats.above_zero()) {
        (
            GateStatus::Passed,
            Some(WordIndex),
            "the word index beats every existing recommend ranking; the losers are removed"
                .to_string(),
        )
    } else {
        let winner = against
            .iter()
            .filter(|(_, stats)| stats.below_zero())
            .min_by(|a, b| {
                a.1.mean
                    .unwrap_or_default()
                    .total_cmp(&b.1.mean.unwrap_or_default())
            })
            .map(|(variant, _)| *variant);
        let verdict = match winner {
            Some(variant) => format!(
                "{} beats the word index; the word index is the loser",
                variant.name()
            ),
            None => "no ranking wins against every other: no difference shown".to_string(),
        };
        (GateStatus::Failed, winner, verdict)
    };
    let combinations = if combinations_beating_both.is_empty() {
        String::new()
    } else {
        format!(
            "; {} beat(s) both parts, so both would stay",
            combinations_beating_both
                .iter()
                .map(|variant| variant.name())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    GateResult {
        gate: "G2".to_string(),
        status,
        reason: format!("{verdict}: {numbers}{combinations}"),
        keep,
        alternative: None,
        combinations_beating_both,
    }
}

/// 95% percentile-bootstrap interval of the mean of `values`, or `None`
/// with fewer than two values (there is nothing to resample).
pub(super) fn bootstrap_interval(values: &[f64]) -> Option<(f64, f64)> {
    if values.len() < 2 {
        return None;
    }
    let mut state = BOOTSTRAP_SEED;
    let count = values.len();
    let mut means = (0..BOOTSTRAP_RESAMPLES)
        .map(|_| {
            let total: f64 = (0..count)
                .map(|_| {
                    let draw = splitmix64(&mut state);
                    values[(draw % count as u64) as usize]
                })
                .sum();
            total / count as f64
        })
        .collect::<Vec<_>>();
    means.sort_by(f64::total_cmp);
    let at = |quantile: f64| {
        let rank = (quantile * BOOTSTRAP_RESAMPLES as f64).ceil() as usize;
        means[rank.clamp(1, BOOTSTRAP_RESAMPLES) - 1]
    };
    Some((at(0.025), at(0.975)))
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// `numerator / denominator`, or `None` when there is nothing to divide by:
/// "no data" is not a measured 0 (STD-02 §R29).
pub(super) fn ratio(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator != 0).then(|| numerator as f64 / denominator as f64)
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

/// Mean of the latency samples in milliseconds, or `None` with no samples.
fn mean_latency_ms(samples_micros: &[u128]) -> Option<f64> {
    if samples_micros.is_empty() {
        return None;
    }
    let total: u128 = samples_micros.iter().sum();
    Some(total as f64 / samples_micros.len() as f64 / 1_000.0)
}

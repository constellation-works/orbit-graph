use std::collections::{BTreeMap, BTreeSet};

use crate::RecommendationLevel;
use crate::evaluation::readout::{
    CaseRun, CaseScores, EvaluationVariant, GATE_K, GateStatus, PairedMetric, WordIndexSource,
    bootstrap_interval, gates, metrics, paired, prepend, readout, reciprocal_rank_fusion,
};

use EvaluationVariant::{
    Bm25Code, Combined, ExplicitMentions, Frequency, GraphOnly, TaskSearchOnly, WordIndex,
    WordIndexBm25Rrf, WordIndexPlusMentions,
};

fn run(selectors: &[&str]) -> CaseRun {
    CaseRun {
        selectors: selectors
            .iter()
            .map(|selector| (*selector).to_string())
            .collect(),
        latency_micros: 1_000,
    }
}

/// A case whose file truth is `truth`, with each listed ranking at file level
/// and empty rankings for the rest.
fn case(fold: usize, truth: &[&str], file_runs: &[(EvaluationVariant, &[&str])]) -> CaseScores {
    let mut runs = BTreeMap::new();
    for variant in EvaluationVariant::ALL {
        if matches!(variant, WordIndexPlusMentions | WordIndexBm25Rrf) {
            continue;
        }
        runs.insert((variant, 0), CaseRun::default());
        runs.insert((variant, 1), CaseRun::default());
    }
    for (variant, selectors) in file_runs {
        runs.insert((*variant, 0), run(selectors));
    }
    CaseScores {
        fold,
        mostly_added: false,
        truth: [
            truth
                .iter()
                .map(|selector| (*selector).to_string())
                .collect(),
            BTreeSet::new(),
        ],
        not_live: BTreeSet::new(),
        runs,
        grid: Vec::new(),
    }
}

#[test]
fn combinations_put_mentions_first_and_fuse_by_reciprocal_rank() {
    let merged = prepend(
        &run(&["file:b", "file:x"]),
        &run(&["file:a", "file:b", "file:c"]),
    );
    assert_eq!(merged.selectors, ["file:b", "file:x", "file:a", "file:c"]);
    assert_eq!(merged.latency_micros, 2_000);

    // b is second in both lists, so 2/62 beats a's and c's 1/61 each.
    let fused = reciprocal_rank_fusion(&run(&["file:a", "file:b"]), &run(&["file:c", "file:b"]));
    assert_eq!(fused.selectors, ["file:b", "file:a", "file:c"]);

    let scores = case(
        0,
        &["file:a"],
        &[
            (WordIndex, &["file:w", "file:a"]),
            (ExplicitMentions, &["file:m"]),
            (Bm25Code, &["file:a"]),
        ],
    );
    let file = RecommendationLevel::File;
    assert_eq!(
        scores
            .run(WordIndexPlusMentions, file, WordIndexSource::Base)
            .selectors,
        ["file:m", "file:w", "file:a"]
    );
    assert_eq!(
        scores
            .run(WordIndexBm25Rrf, file, WordIndexSource::Base)
            .selectors,
        ["file:a", "file:w"]
    );
}

#[test]
fn grid_cells_replace_the_word_index_and_its_combinations_only() {
    let mut scores = case(
        0,
        &["file:a"],
        &[(WordIndex, &["file:base"]), (ExplicitMentions, &["file:m"])],
    );
    scores.grid.push([run(&["file:cell"]), CaseRun::default()]);
    let file = RecommendationLevel::File;
    let cell = WordIndexSource::Cell(0);
    assert_eq!(scores.run(WordIndex, file, cell).selectors, ["file:cell"]);
    assert_eq!(
        scores.run(WordIndexPlusMentions, file, cell).selectors,
        ["file:m", "file:cell"]
    );
    assert_eq!(
        scores.run(ExplicitMentions, file, cell).selectors,
        ["file:m"]
    );
}

#[test]
fn metrics_report_coverage_mean_recall_and_stale_results_within_k() {
    let mut first = case(
        0,
        &["file:a", "file:b"],
        &[(WordIndex, &["file:a", "file:z", "file:b"])],
    );
    first.not_live.insert("file:z".to_string());
    let second = case(0, &["file:c"], &[(WordIndex, &[])]);
    let cases = [&first, &second];
    let row = metrics(
        &cases,
        WordIndex,
        RecommendationLevel::File,
        WordIndexSource::Base,
        2,
    );
    assert_eq!(row.cases, 2);
    assert_eq!(row.relevant, 3);
    assert_eq!(row.true_positives, 1);
    assert_eq!(row.returned, 2);
    assert_eq!(row.recall_at_k, Some(1.0 / 3.0));
    assert_eq!(row.precision_at_k, Some(0.25));
    assert_eq!(row.cases_with_truth, 2);
    // (1/2 + 0) / 2: per case, then averaged.
    assert_eq!(row.mean_recall_at_k, Some(0.25));
    assert_eq!(row.cases_answered, 1);
    assert_eq!(row.coverage, Some(0.5));
    assert_eq!(row.stale_result_rate, Some(0.5));
    assert_eq!(row.mean_latency_ms, Some(1.0));

    let symbol = metrics(
        &cases,
        WordIndex,
        RecommendationLevel::Symbol,
        WordIndexSource::Base,
        2,
    );
    assert_eq!(symbol.cases_with_truth, 0);
    assert_eq!(symbol.recall_at_k, None);
    assert_eq!(symbol.mean_recall_at_k, None);
    assert_eq!(symbol.coverage, Some(0.0));

    let empty = metrics(
        &[],
        WordIndex,
        RecommendationLevel::File,
        WordIndexSource::Base,
        2,
    );
    assert_eq!(empty.coverage, None);
    assert_eq!(empty.precision_at_k, None);
}

#[test]
fn bootstrap_interval_is_deterministic_and_needs_two_cases() {
    assert_eq!(bootstrap_interval(&[]), None);
    assert_eq!(bootstrap_interval(&[0.4]), None);
    assert_eq!(bootstrap_interval(&[0.5, 0.5, 0.5]), Some((0.5, 0.5)));
    let values = [0.0, 1.0, 0.5, 0.25, 0.75, 1.0, 0.0, 0.5];
    let interval = bootstrap_interval(&values).expect("interval");
    assert_eq!(bootstrap_interval(&values), Some(interval));
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    assert!(interval.0 < mean && mean < interval.1, "{interval:?}");
    assert!(interval.0 >= 0.0 && interval.1 <= 1.0, "{interval:?}");
}

/// Cases where the word index finds the truth, frequency and BM25 do not,
/// and `combined` finds it as well as the word index.
fn winning_cases(count: usize) -> Vec<CaseScores> {
    (0..count)
        .map(|index| {
            let truth = format!("file:t{index}");
            let truth = truth.as_str();
            case(
                0,
                &[truth],
                &[
                    (WordIndex, &[truth]),
                    (Frequency, &["file:hot"]),
                    (Bm25Code, &["file:other"]),
                    (Combined, &["file:other"]),
                    (TaskSearchOnly, &["file:other"]),
                    (GraphOnly, &[truth]),
                ],
            )
        })
        .collect()
}

#[test]
fn paired_differences_pair_the_same_cases() {
    let cases = winning_cases(4);
    let cases = cases.iter().collect::<Vec<_>>();
    let stats = paired(
        &cases,
        WordIndex,
        Frequency,
        RecommendationLevel::File,
        PairedMetric::RecallAtK,
        WordIndexSource::Base,
        GATE_K,
    );
    assert_eq!(stats.cases, 4);
    assert_eq!(stats.mean, Some(1.0));
    assert_eq!(stats.interval, Some((1.0, 1.0)));

    let report = readout(&cases, WordIndexSource::Base, GATE_K);
    assert_eq!(report.cases, 4);
    assert_eq!(report.metrics.len(), EvaluationVariant::ALL.len() * 2);
    let verdict = |baseline, metric| {
        report
            .paired_differences
            .iter()
            .find(|row| {
                row.baseline == baseline
                    && row.level == RecommendationLevel::File
                    && row.metric == metric
            })
            .map(|row| row.verdict.as_str())
            .expect("paired row")
    };
    assert_eq!(
        verdict(Frequency, PairedMetric::RecallAtK),
        "word_index_higher"
    );
    assert_eq!(
        verdict(GraphOnly, PairedMetric::RecallAtK),
        "no_difference_shown"
    );
    assert_eq!(
        report
            .split_metrics
            .iter()
            .map(|split| (split.split.as_str(), split.cases))
            .collect::<Vec<_>>(),
        [("mostly_added_files", 0), ("mostly_existing_files", 4)]
    );
    // No symbol truth: recall has nothing to pair.
    assert!(
        report
            .paired_differences
            .iter()
            .filter(|row| {
                row.level == RecommendationLevel::Symbol && row.metric == PairedMetric::RecallAtK
            })
            .all(|row| row.verdict == "unevaluable" && row.cases == 0)
    );
}

#[test]
fn gate_one_and_two_pass_when_the_word_index_wins_with_coverage() {
    let cases = winning_cases(5);
    let cases = cases.iter().collect::<Vec<_>>();
    let results = gates(&cases, WordIndexSource::Base, GATE_K, None);
    assert_eq!(results[0].gate, "G1");
    assert_eq!(
        results[0].status,
        GateStatus::Passed,
        "{}",
        results[0].reason
    );
    assert_eq!(results[0].alternative, None);
    assert_eq!(results[1].gate, "G2");
    assert_eq!(
        results[1].status,
        GateStatus::Passed,
        "{}",
        results[1].reason
    );
    assert_eq!(results[1].keep, Some(WordIndex));
}

#[test]
fn gate_one_fails_below_half_coverage_and_names_an_alternative() {
    // Two cases answered and won; four more the word index leaves empty
    // while BM25 finds them.
    let mut cases = winning_cases(2);
    for index in 0..4 {
        let truth = format!("file:u{index}");
        cases.push(case(
            0,
            &[truth.as_str()],
            &[
                (WordIndex, &[]),
                (Bm25Code, &[truth.as_str()]),
                (Combined, &[truth.as_str()]),
            ],
        ));
    }
    let cases = cases.iter().collect::<Vec<_>>();
    let results = gates(&cases, WordIndexSource::Base, GATE_K, None);
    assert_eq!(
        results[0].status,
        GateStatus::Failed,
        "{}",
        results[0].reason
    );
    assert!(results[0].alternative.is_some());
    assert_eq!(
        results[1].status,
        GateStatus::Failed,
        "{}",
        results[1].reason
    );
}

#[test]
fn gate_two_keeps_an_existing_ranking_that_beats_the_word_index() {
    let cases = (0..4)
        .map(|index| {
            let truth = format!("file:t{index}");
            case(
                0,
                &[truth.as_str()],
                &[
                    (WordIndex, &["file:wrong"]),
                    (Combined, &[truth.as_str()]),
                    (TaskSearchOnly, &["file:wrong"]),
                ],
            )
        })
        .collect::<Vec<_>>();
    let cases = cases.iter().collect::<Vec<_>>();
    let results = gates(&cases, WordIndexSource::Base, GATE_K, None);
    assert_eq!(results[1].status, GateStatus::Failed);
    assert_eq!(results[1].keep, Some(Combined), "{}", results[1].reason);
}

#[test]
fn gates_are_unevaluable_off_k_ten_without_pairs_or_when_told() {
    let cases = winning_cases(3);
    let cases = cases.iter().collect::<Vec<_>>();
    for results in [
        gates(&cases, WordIndexSource::Base, 5, None),
        gates(&cases[..1], WordIndexSource::Base, GATE_K, None),
        gates(
            &cases,
            WordIndexSource::Base,
            GATE_K,
            Some("no headline cases"),
        ),
    ] {
        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .all(|result| result.status == GateStatus::Unevaluable),
            "{results:?}"
        );
    }
    assert!(
        gates(&cases, WordIndexSource::Base, 5, None)[0]
            .reason
            .contains("k = 5")
    );
}

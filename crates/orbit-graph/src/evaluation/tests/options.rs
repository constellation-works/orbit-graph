//! The word-index option grid and gate G4 (design §10.5).

use std::collections::{BTreeMap, BTreeSet};

use git2::Oid;
use orbit_graph_extract::history::parse_timestamp;

use crate::evaluation::readout::{
    CaseRun, CaseScores, EvaluationVariant, GATE_K, GateStatus, OptionComparison, WordIndexOption,
    gate_four,
};
use crate::evaluation::{Fold, WordIndexGrid, grid_cells, sweep};
use crate::{WordIndexOptions, WordIndexPostingWeight, WordIndexSmoothingUnit};

const TRUTH_SYMBOL: &str = "symbol:src/t.rs#t:function";

fn run(selectors: &[&str]) -> CaseRun {
    CaseRun {
        selectors: selectors
            .iter()
            .map(|selector| (*selector).to_string())
            .collect(),
        latency_micros: 1_000,
    }
}

/// A case in `fold` whose file truth cell 1 finds when `cell_one_wins` and
/// cell 0 finds otherwise. Both cells find the symbol truth.
fn case(fold: usize, index: usize, cell_one_wins: bool) -> CaseScores {
    let truth = format!("file:src/t{index}.rs");
    let mut runs = BTreeMap::new();
    for variant in EvaluationVariant::ALL {
        runs.insert((variant, 0), CaseRun::default());
        runs.insert((variant, 1), CaseRun::default());
    }
    let (found, missed) = (run(&[truth.as_str()]), run(&["file:src/wrong.rs"]));
    let symbols = run(&[TRUTH_SYMBOL]);
    let (zero, one) = if cell_one_wins {
        (missed, found)
    } else {
        (found, missed)
    };
    CaseScores {
        fold,
        mostly_added: false,
        truth: [
            BTreeSet::from([truth]),
            BTreeSet::from([TRUTH_SYMBOL.to_string()]),
        ],
        not_live: BTreeSet::new(),
        runs,
        grid: vec![[zero, symbols.clone()], [one, symbols]],
    }
}

fn folds() -> Vec<Fold> {
    ["selection", "headline"]
        .into_iter()
        .map(|id| Fold {
            id: id.to_string(),
            cutoff: "2026-01-01T00:00:00Z".to_string(),
            cutoff_time: parse_timestamp("cutoff", "2026-01-01T00:00:00Z").expect("timestamp"),
            revision: Oid::ZERO_SHA1,
            declared: true,
        })
        .collect()
}

/// Presence and `symbol_fraction`, both symbol-first.
fn posting_weight_grid() -> WordIndexGrid {
    WordIndexGrid {
        selection_folds: 1,
        symbol_first: vec![true],
        posting_weight: vec![
            WordIndexPostingWeight::Presence,
            WordIndexPostingWeight::SymbolFraction,
        ],
        ..WordIndexGrid::default()
    }
}

#[test]
fn grid_cells_normalize_dedupe_and_skip_options_without_symbol_first() {
    let grid = WordIndexGrid {
        selection_folds: 1,
        symbol_first: vec![false, true],
        posting_weight: vec![
            WordIndexPostingWeight::Presence,
            WordIndexPostingWeight::DiffShare,
        ],
        smoothing_unit: vec![
            WordIndexSmoothingUnit::Tasks,
            WordIndexSmoothingUnit::Relative,
        ],
        added_weight: vec![1.0, 0.5],
        ..WordIndexGrid::default()
    };
    let cells = grid_cells(&WordIndexOptions::default(), Some(&grid)).expect("cells");
    // Without symbol-first only the baseline survives; with it, presence
    // keeps one smoothing unit (2 cells by α) and diff_share keeps both (4).
    assert_eq!(cells.len(), 7, "{cells:?}");
    assert_eq!(cells[0], WordIndexOptions::default());
    assert!(
        cells
            .iter()
            .filter(|cell| cell.posting_weight == WordIndexPostingWeight::Presence)
            .all(|cell| cell.smoothing_unit == WordIndexSmoothingUnit::Tasks)
    );
    let distinct = cells.iter().collect::<Vec<_>>();
    for (index, cell) in distinct.iter().enumerate() {
        assert!(!distinct[..index].contains(cell), "duplicate {cell:?}");
    }

    let invalid = WordIndexGrid {
        added_weight: vec![2.0],
        ..grid.clone()
    };
    assert!(grid_cells(&WordIndexOptions::default(), Some(&invalid)).is_err());
    let unusable = WordIndexGrid {
        selection_folds: 1,
        symbol_first: vec![false],
        posting_weight: vec![WordIndexPostingWeight::SymbolFraction],
        ..WordIndexGrid::default()
    };
    assert!(grid_cells(&WordIndexOptions::default(), Some(&unusable)).is_err());
    let oversized = WordIndexGrid {
        window_size: (1..=300).collect(),
        ..grid
    };
    assert!(grid_cells(&WordIndexOptions::default(), Some(&oversized)).is_err());
}

#[test]
fn gate_four_keeps_a_setting_that_wins_on_the_headline_folds() {
    let cases = (0..3)
        .map(|index| case(0, index, true))
        .chain((3..8).map(|index| case(1, index, true)))
        .collect::<Vec<_>>();
    let cases = cases.iter().collect::<Vec<_>>();
    let grid = posting_weight_grid();
    let cells = grid_cells(&WordIndexOptions::default(), Some(&grid)).expect("cells");
    let (report, gates) = sweep(&cases, &folds(), &cells, &grid, GATE_K);
    assert_eq!(report.selected_cell, Some(1));
    assert_eq!(report.kept_cell, Some(1));
    let g4 = |option: &str| {
        gates
            .results
            .iter()
            .find(|result| result.gate == "G4" && result.option.as_deref() == Some(option))
            .expect("G4 result")
    };
    let posting = g4("posting_weight");
    assert_eq!(posting.status, GateStatus::Passed, "{}", posting.reason);
    assert_eq!(
        posting.selected_setting.as_deref(),
        Some("symbol_fraction with smoothing_unit tasks")
    );
    assert_eq!(posting.kept_setting, posting.selected_setting);
    // The grid holds α at 1 throughout: there is nothing to compare.
    let added = g4("added_weight");
    assert_eq!(added.status, GateStatus::Unevaluable);
    assert!(added.reason.contains("does not sweep"), "{}", added.reason);
    assert_eq!(gates.word_index_options, Some(cells[1]));
    assert_eq!(gates.selected_word_index_options, Some(cells[1]));
    // G1 and G2 come first and are read on the kept configuration.
    assert_eq!(gates.results[0].gate, "G1");
    assert_eq!(gates.results[1].gate, "G2");
    assert!(gates.results[0].option.is_none());
}

#[test]
fn gate_four_drops_a_setting_that_loses_on_the_headline_folds() {
    // Cell 1 wins every selection case and loses every headline case.
    let cases = (0..3)
        .map(|index| case(0, index, true))
        .chain((3..8).map(|index| case(1, index, false)))
        .collect::<Vec<_>>();
    let cases = cases.iter().collect::<Vec<_>>();
    let grid = posting_weight_grid();
    let cells = grid_cells(&WordIndexOptions::default(), Some(&grid)).expect("cells");
    let (report, gates) = sweep(&cases, &folds(), &cells, &grid, GATE_K);
    assert_eq!(report.selected_cell, Some(1));
    assert_eq!(report.kept_cell, Some(0));
    let posting = gates
        .results
        .iter()
        .find(|result| result.option.as_deref() == Some("posting_weight"))
        .expect("G4 result");
    assert_eq!(posting.status, GateStatus::Failed, "{}", posting.reason);
    assert_eq!(posting.kept_setting.as_deref(), Some("presence"));
    assert_eq!(gates.word_index_options, Some(cells[0]));
    assert_eq!(gates.selected_word_index_options, Some(cells[1]));
    // The headline readout is the kept cell's.
    assert!(report.headline.is_some());
}

#[test]
fn gate_four_fails_at_baseline_and_is_unevaluable_without_numbers() {
    let cases = (0..4).map(|index| case(1, index, true)).collect::<Vec<_>>();
    let cases = cases.iter().collect::<Vec<_>>();
    let selected = WordIndexOptions {
        symbol_first: true,
        ..WordIndexOptions::default()
    };
    let at_baseline = gate_four(
        &cases,
        WordIndexOption::AddedWeight,
        Some(&selected),
        OptionComparison::AtBaseline,
        GATE_K,
    );
    assert_eq!(at_baseline.status, GateStatus::Failed);
    assert_eq!(at_baseline.kept_setting.as_deref(), Some("1"));
    let unavailable = gate_four(
        &cases,
        WordIndexOption::AddedWeight,
        None,
        OptionComparison::Unavailable("no grid".to_string()),
        GATE_K,
    );
    assert_eq!(unavailable.status, GateStatus::Unevaluable);
    assert_eq!(unavailable.reason, "no grid");
    let off_k = gate_four(
        &cases,
        WordIndexOption::PostingWeight,
        Some(&selected),
        OptionComparison::Cells {
            selected: 1,
            baseline: 0,
        },
        5,
    );
    assert_eq!(off_k.status, GateStatus::Unevaluable);
    let one_case = gate_four(
        &cases[..1],
        WordIndexOption::PostingWeight,
        Some(&selected),
        OptionComparison::Cells {
            selected: 1,
            baseline: 0,
        },
        GATE_K,
    );
    assert_eq!(
        one_case.status,
        GateStatus::Unevaluable,
        "{}",
        one_case.reason
    );
}

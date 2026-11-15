//! Symbol-first mode, posting weights, smoothing units and `added_weight`
//! (design §2.4, §2.5, §5.4 and §5.5) on small fixture windows.

use super::*;
use crate::recommend::word_index::{
    self, ContextRecord, ContextStatus, Leaf, REMAINDER_PREFIX, WindowTask, file_label,
};
use orbit_graph_extract::history::{
    FileChange, FileChangeKind, FileFallbackReason, FileSideLines, RevisionSide, SymbolAttribution,
    SymbolChange, SymbolIdentity, SymbolMatchConfidence,
};

/// A record of `changed` lines on a context of `length` lines on each side.
fn record(status: ContextStatus, changed: u32, length: u32) -> ContextRecord {
    let (added, deleted, len_before, len_after) = match status {
        ContextStatus::Added => (changed, 0, 0, length),
        ContextStatus::Deleted => (0, changed, length, 0),
        ContextStatus::Modified => (changed - changed / 2, changed / 2, length, length),
    };
    ContextRecord {
        added,
        deleted,
        len_before,
        len_after,
        status,
    }
}

/// A window task with `words` as its terms and `files` as its file records,
/// each counted in the task's file-level `diff_share` total.
fn task(index: usize, words: &[&str], files: &[(&str, ContextRecord)]) -> WindowTask {
    let mut task = WindowTask {
        task_id: format!("T-{index}"),
        delivery_id: format!("D-{index}"),
        terms: words.iter().map(|word| (*word).to_string()).collect(),
        ..WindowTask::default()
    };
    for (path, record) in files {
        task.files.insert(format!("file:{path}"), *record);
        task.count_file(record);
    }
    task
}

fn options(posting_weight: WordIndexPostingWeight) -> WordIndexOptions {
    WordIndexOptions {
        max_term_share: 1.0,
        symbol_boost: 1.0,
        symbol_first: true,
        posting_weight,
        ..WordIndexOptions::default()
    }
}

fn file_score(window: &[WindowTask], query: &str, options: &WordIndexOptions, file: &str) -> f64 {
    let outcome = word_index::score(
        window,
        BTreeMap::new(),
        query,
        RecommendationLevel::File,
        options,
    );
    outcome
        .scored
        .get(&format!("file:{file}"))
        .map_or(0.0, |entry| entry.score)
}

fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-12, "{actual} != {expected}");
}

/// Four tasks; `parser` co-occurs with `a.rs` in two of them. Each touches
/// `a.rs` (100 lines over both sides) with a different number of lines.
fn weighted_window() -> Vec<WindowTask> {
    let modified = ContextStatus::Modified;
    vec![
        task(0, &["parser"], &[("a.rs", record(modified, 10, 50))]),
        task(1, &["parser"], &[("a.rs", record(modified, 30, 50))]),
        task(2, &["cache"], &[("a.rs", record(modified, 5, 50))]),
        task(3, &["cache"], &[("b.rs", record(modified, 10, 50))]),
    ]
}

#[test]
fn presence_counts_each_task_once() {
    // t = 2, T(a) = 3, E = 2 · 3 / 4.
    let window = weighted_window();
    let presence = options(WordIndexPostingWeight::Presence);
    close(
        file_score(&window, "parser", &presence, "a.rs"),
        (3.0_f64 / 2.5).ln(),
    );
    // The legacy mode (no symbol-first) scores the same window identically.
    let legacy = WordIndexOptions {
        symbol_first: false,
        ..presence
    };
    close(
        file_score(&window, "parser", &legacy, "a.rs"),
        (3.0_f64 / 2.5).ln(),
    );
}

#[test]
fn symbol_fraction_weighs_by_share_of_the_context_changed() {
    // x = 0.1, 0.3 with `parser` and 0.05 without: t = 0.4, T = 0.45,
    // E = 2 · 0.45 / 4 = 0.225.
    let window = weighted_window();
    let weighted = options(WordIndexPostingWeight::SymbolFraction);
    close(
        file_score(&window, "parser", &weighted, "a.rs"),
        (1.4_f64 / 1.225).ln(),
    );
}

#[test]
fn relative_smoothing_divides_by_the_context_mean_weight() {
    // Mean 0.15: x = 2/3, 2 and 1/3, so t = 8/3, T = 3 and E = 1.5.
    let window = weighted_window();
    let relative = WordIndexOptions {
        smoothing_unit: WordIndexSmoothingUnit::Relative,
        ..options(WordIndexPostingWeight::SymbolFraction)
    };
    close(
        file_score(&window, "parser", &relative, "a.rs"),
        (11.0_f64 / 3.0 / 2.5).ln(),
    );
}

#[test]
fn smoothing_units_are_identical_under_presence() {
    let window = weighted_window();
    let tasks = options(WordIndexPostingWeight::Presence);
    let relative = WordIndexOptions {
        smoothing_unit: WordIndexSmoothingUnit::Relative,
        ..tasks
    };
    assert_eq!(
        file_score(&window, "parser", &tasks, "a.rs"),
        file_score(&window, "parser", &relative, "a.rs")
    );
    assert_eq!(relative.normalized(), tasks);
}

#[test]
fn symbol_fraction_of_an_empty_context_with_changes_is_one() {
    // A context whose length is 0 on both sides (a binary or emptied file
    // recorded as such) weighs 1 when it changed lines.
    let empty = ContextRecord {
        added: 1,
        len_before: 0,
        len_after: 0,
        ..ContextRecord::default()
    };
    let window = vec![
        task(0, &["blob"], &[("a.bin", empty)]),
        task(1, &["blob"], &[("a.bin", empty)]),
        task(
            2,
            &["cache"],
            &[("b.rs", record(ContextStatus::Modified, 4, 4))],
        ),
        task(
            3,
            &["cache"],
            &[("b.rs", record(ContextStatus::Modified, 4, 4))],
        ),
    ];
    let weighted = options(WordIndexPostingWeight::SymbolFraction);
    // t = 2, T = 2, E = 1.
    close(
        file_score(&window, "blob", &weighted, "a.bin"),
        (3.0_f64 / 2.0).ln(),
    );
}

/// `T-0` creates `a.rs` (30 lines) and modifies `b.rs` (10 lines).
fn diff_share_window() -> Vec<WindowTask> {
    let (added, modified) = (ContextStatus::Added, ContextStatus::Modified);
    vec![
        task(
            0,
            &["parser"],
            &[
                ("a.rs", record(added, 30, 30)),
                ("b.rs", record(modified, 10, 40)),
            ],
        ),
        task(
            1,
            &["parser"],
            &[
                ("a.rs", record(modified, 10, 40)),
                ("b.rs", record(modified, 10, 40)),
            ],
        ),
        task(2, &["cache"], &[("a.rs", record(modified, 10, 40))]),
        task(3, &["cache"], &[("c.rs", record(modified, 10, 40))]),
    ]
}

#[test]
fn diff_share_takes_the_task_total_after_added_weight() {
    let window = diff_share_window();
    // α = 1: T-0 shares 0.75 and 0.25; T-1 0.5 and 0.5; T-2 1.
    let full = options(WordIndexPostingWeight::DiffShare);
    close(
        file_score(&window, "parser", &full, "a.rs"),
        (2.25_f64 / (1.0 + 2.0 * 2.25 / 4.0)).ln(),
    );
    close(
        file_score(&window, "parser", &full, "b.rs"),
        (1.75_f64 / (1.0 + 2.0 * 0.75 / 4.0)).ln(),
    );
    // α = 0.5: T-0's total is 0.5 · 30 + 10 = 25, so `a.rs` weighs
    // 0.5 · 30 / 25 = 0.6 and `b.rs` 10 / 25 = 0.4; the shares still sum to 1.
    let halved = WordIndexOptions {
        added_weight: 0.5,
        ..full
    };
    close(
        file_score(&window, "parser", &halved, "a.rs"),
        (2.1_f64 / (1.0 + 2.0 * 2.1 / 4.0)).ln(),
    );
    close(
        file_score(&window, "parser", &halved, "b.rs"),
        (1.9_f64 / (1.0 + 2.0 * 0.9 / 4.0)).ln(),
    );
}

#[test]
fn diff_share_relative_normalizes_before_added_weight() {
    let window = diff_share_window();
    let relative = WordIndexOptions {
        added_weight: 0.5,
        smoothing_unit: WordIndexSmoothingUnit::Relative,
        ..options(WordIndexPostingWeight::DiffShare)
    };
    // Base weights before α: T-0 30/25 = 1.2, T-1 0.5, T-2 1; mean 2.7 / 3 =
    // 0.9. Relative: 4/3, 5/9, 10/9; α halves T-0's: 2/3.
    let t: f64 = 2.0 / 3.0 + 5.0 / 9.0;
    let mass = t + 10.0 / 9.0;
    close(
        file_score(&window, "parser", &relative, "a.rs"),
        ((t + 1.0) / (1.0 + 2.0 * mass / 4.0)).ln(),
    );
}

#[test]
fn added_weight_zero_drops_contexts_only_ever_created() {
    let added = ContextStatus::Added;
    let modified = ContextStatus::Modified;
    let window = vec![
        task(
            0,
            &["parser"],
            &[
                ("new.rs", record(added, 5, 5)),
                ("old.rs", record(modified, 4, 9)),
            ],
        ),
        task(
            1,
            &["parser"],
            &[
                ("new.rs", record(added, 5, 5)),
                ("old.rs", record(modified, 4, 9)),
            ],
        ),
        task(2, &["cache"], &[("other.rs", record(modified, 4, 9))]),
        task(3, &["cache"], &[("other.rs", record(modified, 4, 9))]),
    ];
    let full = options(WordIndexPostingWeight::Presence);
    assert!(file_score(&window, "parser", &full, "new.rs") > 0.0);
    let none = WordIndexOptions {
        added_weight: 0.0,
        ..full
    };
    assert_eq!(file_score(&window, "parser", &none, "new.rs"), 0.0);
    close(
        file_score(&window, "parser", &none, "old.rs"),
        (3.0_f64 / 2.0).ln(),
    );
    // The explanation names how many tasks created the context.
    let outcome = word_index::score(
        &window,
        BTreeMap::new(),
        "parser",
        RecommendationLevel::File,
        &full,
    );
    let explanation = &outcome.scored["file:new.rs"].reasons[0].explanation;
    assert!(
        explanation.contains("modified by 0, created by 2"),
        "{explanation}"
    );
}

#[test]
fn a_remainder_is_scored_but_never_a_selector_nor_boosted() {
    let symbol = "symbol:src/a.rs#parse:function";
    let remainder = format!("{REMAINDER_PREFIX}src/a.rs");
    let modified = record(ContextStatus::Modified, 4, 20);
    let window = (0..4)
        .map(|index| {
            let words: &[&str] = if index < 2 { &["parser"] } else { &["cache"] };
            let path = if index < 3 { "src/a.rs" } else { "src/b.rs" };
            let mut task = task(index, words, &[(path, modified)]);
            if index < 2 {
                task.add_leaf(
                    remainder.clone(),
                    Leaf {
                        record: modified,
                        ..Leaf::default()
                    },
                );
            }
            if index == 0 {
                task.add_leaf(
                    symbol.to_string(),
                    Leaf {
                        record: modified,
                        ..Leaf::default()
                    },
                );
            }
            task
        })
        .collect::<Vec<_>>();
    let boosted = WordIndexOptions {
        symbol_boost: 1.5,
        min_support: 1,
        ..options(WordIndexPostingWeight::Presence)
    };
    let symbols = word_index::score(
        &window,
        BTreeMap::new(),
        "parser",
        RecommendationLevel::Symbol,
        &boosted,
    );
    assert!(symbols.scored.contains_key(symbol));
    assert!(
        symbols
            .scored
            .keys()
            .all(|selector| !selector.starts_with(REMAINDER_PREFIX)),
        "{:?}",
        symbols.scored.keys()
    );
    let files = word_index::score(
        &window,
        BTreeMap::new(),
        "parser",
        RecommendationLevel::File,
        &boosted,
    );
    assert!(
        files
            .scored
            .keys()
            .all(|selector| selector.starts_with("file:"))
    );
    let file = &files.scored["file:src/a.rs"];
    let kinds = file
        .reasons
        .iter()
        .map(|reason| reason.kind.as_str())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&"word_index_remainder"), "{kinds:?}");
    let remainder_reason = file
        .reasons
        .iter()
        .find(|reason| reason.kind == "word_index_remainder")
        .expect("remainder reason");
    assert_eq!(remainder_reason.contribution, 0.0);
    // The boost is the symbol's score, not the (higher) remainder's: the
    // remainder co-occurs with `parser` twice, the symbol once.
    let boost = file
        .reasons
        .iter()
        .find(|reason| reason.kind == "word_index_symbol_boost")
        .expect("symbol boost");
    close(boost.contribution, 0.5 * symbols.scored[symbol].score);
    assert!(boost.explanation.contains(symbol));
}

#[test]
fn options_that_need_symbol_first_are_rejected_without_it() {
    let legacy = WordIndexOptions::default();
    assert!(legacy.validate().is_ok());
    for posting_weight in [
        WordIndexPostingWeight::SymbolFraction,
        WordIndexPostingWeight::DiffShare,
    ] {
        let weighted = WordIndexOptions {
            posting_weight,
            ..legacy
        };
        assert!(weighted.validate().is_err());
        assert!(
            WordIndexOptions {
                symbol_first: true,
                ..weighted
            }
            .validate()
            .is_ok()
        );
    }
    let halved = WordIndexOptions {
        added_weight: 0.5,
        ..legacy
    };
    assert!(halved.validate().is_err());
    for added_weight in [-0.1, 1.5, f64::NAN] {
        assert!(
            WordIndexOptions {
                symbol_first: true,
                added_weight,
                ..legacy
            }
            .validate()
            .is_err(),
            "{added_weight}"
        );
    }
}

#[test]
fn baseline_options_serialize_as_before() {
    let legacy = serde_json::to_value(WordIndexOptions::default()).expect("serialize");
    for field in [
        "symbol_first",
        "posting_weight",
        "smoothing_unit",
        "added_weight",
    ] {
        assert!(legacy.get(field).is_none(), "{field}");
    }
    let weighted = WordIndexOptions {
        symbol_first: true,
        posting_weight: WordIndexPostingWeight::DiffShare,
        smoothing_unit: WordIndexSmoothingUnit::Relative,
        added_weight: 0.25,
        ..WordIndexOptions::default()
    };
    let value = serde_json::to_value(weighted).expect("serialize");
    assert_eq!(value["posting_weight"], "diff_share");
    assert_eq!(value["smoothing_unit"], "relative");
    let parsed: WordIndexOptions = serde_json::from_value(value).expect("parse");
    assert_eq!(parsed, weighted);
}

// --- file_label ----------------------------------------------------------

fn identity(side: RevisionSide, name: &str, lines: (u32, u32)) -> SymbolIdentity {
    SymbolIdentity {
        revision: "rev".to_string(),
        side,
        file_path: "src/a.rs".to_string(),
        name: name.to_string(),
        qualified: name.to_string(),
        kind: "function".to_string(),
        span_start: 0,
        span_end: 0,
        signature: None,
        parent: None,
        line_start: Some(lines.0),
        line_end: Some(lines.1),
    }
}

fn attribution(
    side: RevisionSide,
    name: &str,
    lines: (u32, u32),
    changed: &[u32],
) -> SymbolAttribution {
    SymbolAttribution {
        symbol: identity(side, name, lines),
        changed_lines: changed.to_vec(),
    }
}

fn change(before: Option<SymbolAttribution>, after: Option<SymbolAttribution>) -> SymbolChange {
    SymbolChange {
        before,
        after,
        match_confidence: SymbolMatchConfidence::Exact,
        reason: String::new(),
        live_after: true,
    }
}

fn side(line_count: u32, covered: u32, whitespace: &[u32]) -> Option<FileSideLines> {
    Some(FileSideLines {
        line_count,
        named_symbol_covered_lines: covered,
        whitespace_only_lines: whitespace.to_vec(),
    })
}

/// `src/a.rs`: 20 lines before, 22 after; `parse` spans lines 3–8 before
/// and 3–10 after, `helper` is new at 15–17. Deleted lines 4, 5 and 12;
/// added lines 4–7 and 15–17, of which line 7 is whitespace only.
fn modified_file() -> FileChange {
    use RevisionSide::{After, Before};
    FileChange {
        old_path: Some("src/a.rs".to_string()),
        new_path: Some("src/a.rs".to_string()),
        kind: FileChangeKind::Modified,
        additions: 7,
        deletions: 3,
        symbols: vec![
            change(
                Some(attribution(Before, "parse", (3, 8), &[4, 5])),
                Some(attribution(After, "parse", (3, 10), &[4, 5, 6, 7])),
            ),
            change(
                None,
                Some(attribution(After, "helper", (15, 17), &[15, 16, 17])),
            ),
        ],
        before_fallback: None,
        after_fallback: None,
        before_lines: side(20, 6, &[]),
        after_lines: side(22, 11, &[7]),
    }
}

#[test]
fn file_label_assigns_lines_to_symbols_and_the_remainder() {
    let label = file_label(&modified_file()).expect("label");
    let modified = ContextStatus::Modified;
    assert_eq!(
        label.symbols,
        vec![
            (
                0,
                ContextRecord {
                    added: 3,
                    deleted: 2,
                    len_before: 6,
                    len_after: 8,
                    status: modified,
                }
            ),
            (
                1,
                ContextRecord {
                    added: 3,
                    deleted: 0,
                    len_before: 0,
                    len_after: 3,
                    status: ContextStatus::Added,
                }
            ),
        ]
    );
    // Deleted line 12 is outside every symbol; every significant added
    // line is inside one.
    assert_eq!(
        label.remainder,
        ContextRecord {
            added: 0,
            deleted: 1,
            len_before: 14,
            len_after: 11,
            status: modified,
        }
    );
    assert_eq!(
        label.file,
        ContextRecord {
            added: 6,
            deleted: 3,
            len_before: 20,
            len_after: 22,
            status: modified,
        }
    );
}

#[test]
fn a_whitespace_only_symbol_change_is_no_change() {
    let mut file = modified_file();
    file.before_lines = side(20, 6, &[4, 5, 12]);
    file.after_lines = side(22, 11, &[4, 5, 6, 7]);
    let label = file_label(&file).expect("label");
    assert_eq!(label.symbols.len(), 1);
    assert_eq!(label.symbols[0].0, 1);
    assert_eq!(label.remainder.changed(), 0);
    assert_eq!((label.file.added, label.file.deleted), (3, 0));
}

#[test]
fn a_pure_rename_changes_no_lines_and_has_no_leaves() {
    let mut file = modified_file();
    file.kind = FileChangeKind::Renamed;
    file.new_path = Some("src/b.rs".to_string());
    file.additions = 0;
    file.deletions = 0;
    let label = file_label(&file).expect("label");
    assert!(label.symbols.is_empty());
    assert_eq!(label.remainder.changed(), 0);
    assert_eq!(label.file.changed(), 0);
    assert_eq!(label.file.status, ContextStatus::Modified);
}

#[test]
fn a_binary_side_is_one_changed_line_of_a_one_line_file() {
    let file = FileChange {
        old_path: None,
        new_path: Some("logo.png".to_string()),
        kind: FileChangeKind::Added,
        additions: 0,
        deletions: 0,
        symbols: Vec::new(),
        before_fallback: None,
        after_fallback: Some(FileFallbackReason::Binary),
        before_lines: side(0, 0, &[]),
        after_lines: side(1, 0, &[]),
    };
    let label = file_label(&file).expect("label");
    let expected = ContextRecord {
        added: 1,
        deleted: 0,
        len_before: 0,
        len_after: 1,
        status: ContextStatus::Added,
    };
    assert_eq!(label.file, expected);
    assert_eq!(label.remainder, expected);
}

#[test]
fn a_payload_without_line_lengths_has_no_label() {
    let mut file = modified_file();
    file.after_lines = None;
    assert!(file_label(&file).is_none());
    let mut file = modified_file();
    if let Some(after) = file.symbols[1].after.as_mut() {
        after.symbol.line_end = None;
    }
    assert!(file_label(&file).is_none());
}

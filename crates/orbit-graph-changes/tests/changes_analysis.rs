//! Coverage for the agent-facing change analysis: the working-tree head,
//! the default base, input validation before indexing, bounded regrouping,
//! and cleanup of cancelled builds.

#![allow(clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use git2::Repository;
use orbit_graph::Confidence;
use orbit_graph_changes::analysis::{
    AnalysisError, ChangesBounds, ChangesRange, ChangesRequest, analyse,
};
use orbit_graph_changes::changes::{ChangeStatus, ChangedSymbols};
use orbit_graph_changes::filters::FilterSet;
use orbit_graph_changes::report::{
    ReportDeadline, ReportLimits, ReportOptions, UnanalysedReason, build_report_bounded,
};
use orbit_graph_changes::snapshot::{
    BuildState, BuildStatus, Comparison, ComparisonMode, ComparisonOptions, ComparisonOutcome,
    ComparisonProgress, SnapshotError, SnapshotSide, WORKING_TREE_ID, default_base,
};
use tempfile::TempDir;

mod common;

use common::{Fixture, build_fixture, commit_files, fingerprint_working_tree};

/// A helper with one production caller and one test caller, all changed
/// between base and head.
fn build_callers_fixture() -> Fixture {
    let dir = TempDir::new().expect("create fixture repository");
    let repo = Repository::init(dir.path()).expect("init fixture repository");
    let base = commit_files(
        &repo,
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"tool\"\nversion = \"0.1.0\"\n",
            ),
            (
                "src/lib.rs",
                "pub fn helper() -> i32 {\n    1\n}\n\npub fn entry() -> i32 {\n    helper()\n}\n",
            ),
            (
                "tests/helper.rs",
                "use tool::helper;\n\n#[test]\nfn helper_is_positive() {\n    \
                 assert!(helper() > 0);\n}\n",
            ),
        ],
        "base: helper, caller, and test",
        0,
    );
    let head = commit_files(
        &repo,
        dir.path(),
        &[(
            "src/lib.rs",
            "pub fn helper() -> i32 {\n    2\n}\n\npub fn entry() -> i32 {\n    helper() + 1\n}\n",
        )],
        "head: change helper and entry",
        1,
    );
    drop(repo);
    Fixture::from_parts(dir, base, head)
}

fn request(fixture: &Fixture, range: ChangesRange, cache: &TempDir) -> ChangesRequest {
    ChangesRequest {
        repository: fixture.path().to_path_buf(),
        range,
        selection: Vec::new(),
        filters: FilterSet::default(),
        min_confidence: Confidence::FuzzyName,
        bounds: ChangesBounds::default(),
        cache: ComparisonOptions {
            cache_dir: Some(cache.path().join("snapshots")),
            no_cache: false,
            ..ComparisonOptions::default()
        },
    }
}

fn revisions(fixture: &Fixture) -> ChangesRange {
    ChangesRange::Revisions {
        base: fixture.base.clone(),
        head: fixture.head.clone(),
    }
}

#[test]
fn working_tree_head_includes_uncommitted_and_untracked_edits_without_writing() {
    let fixture = build_fixture();
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn entry() -> i32 {\n    7\n}\n\npub fn added() -> i32 {\n    entry()\n}\n",
    )
    .expect("edit tracked file");
    fs::write(
        fixture.path().join("src/extra.rs"),
        "pub fn extra() -> i32 {\n    3\n}\n",
    )
    .expect("write untracked file");
    let index_before = fs::read(fixture.path().join(".git/index")).expect("read index");
    let tree_before = fingerprint_working_tree(fixture.path());

    let outcome = Comparison::open_working_tree(
        fixture.path(),
        fixture.base.as_str(),
        &ComparisonOptions::default(),
        &NoProgress,
    )
    .expect("open working-tree comparison");
    let ComparisonOutcome::Ready(comparison) = outcome else {
        panic!("an uncancelled build must be ready");
    };

    assert_eq!(comparison.mode(), ComparisonMode::WorkingTree);
    assert_eq!(comparison.mode().label(), "working_tree");
    assert_eq!(comparison.base().commit_sha(), fixture.base);
    assert_eq!(comparison.head().commit_sha(), WORKING_TREE_ID);
    assert_eq!(comparison.head().cache_outcome().label(), "disabled");
    assert!(comparison.head().cache_note().is_some());
    assert!(!comparison.head().tree_is_cached());

    let changed = ChangedSymbols::compute(&comparison).expect("changed symbols");
    let status_of = |name: &str| {
        changed
            .symbols
            .iter()
            .find(|symbol| symbol.primary_selector().contains(name))
            .map(|symbol| symbol.status)
    };
    assert_eq!(status_of("#removed_helper:"), Some(ChangeStatus::Removed));
    assert_eq!(status_of("#added:"), Some(ChangeStatus::Added));
    assert_eq!(
        status_of("src/extra.rs#extra:"),
        Some(ChangeStatus::Added),
        "an untracked, non-ignored file is part of the working tree"
    );

    drop(comparison);
    assert_eq!(fingerprint_working_tree(fixture.path()), tree_before);
    assert_eq!(
        fs::read(fixture.path().join(".git/index")).expect("read index"),
        index_before,
        "the Git index is read, never written"
    );
}

#[test]
fn a_clean_working_tree_against_head_has_no_changes() {
    let fixture = build_fixture();
    let outcome = Comparison::open_working_tree(
        fixture.path(),
        "HEAD",
        &ComparisonOptions {
            cache_dir: None,
            no_cache: true,
            ..ComparisonOptions::default()
        },
        &NoProgress,
    )
    .expect("open working-tree comparison");
    let ComparisonOutcome::Ready(comparison) = outcome else {
        panic!("an uncancelled build must be ready");
    };
    let changed = ChangedSymbols::compute(&comparison).expect("changed symbols");
    assert!(changed.symbols.is_empty(), "{:?}", changed.symbols);
}

#[test]
fn default_base_prefers_the_upstream_then_main() {
    let fixture = build_fixture();
    let repo = Repository::open(fixture.path()).expect("open fixture");
    let base = repo
        .find_commit(git2::Oid::from_str(fixture.base.as_str()).expect("oid"))
        .expect("base commit");
    let head = repo
        .find_commit(git2::Oid::from_str(fixture.head.as_str()).expect("oid"))
        .expect("head commit");
    // Check out `feature` first: the fixture's initial branch name follows
    // libgit2's default and may itself be `main`, which cannot be moved while
    // checked out.
    repo.branch("feature", &head, true).expect("feature branch");
    repo.set_head("refs/heads/feature")
        .expect("check out feature");
    repo.branch("main", &base, true).expect("main branch");

    let chosen = default_base(fixture.path()).expect("default base");
    assert_eq!(chosen.source, "main");
    assert_eq!(chosen.reference, "main");
    assert_eq!(chosen.merge_base, fixture.base);

    repo.reference("refs/remotes/origin/trunk", base.id(), true, "fixture")
        .expect("remote-tracking ref");
    // The remote maps `refs/heads/trunk` to `refs/remotes/origin/trunk`
    // through its default fetch refspec; it is never contacted.
    repo.remote("origin", "file:///nonexistent/origin.git")
        .expect("origin remote");
    let mut config = repo
        .config()
        .and_then(|config| config.open_level(git2::ConfigLevel::Local))
        .expect("repository-local config");
    config
        .set_str("branch.feature.remote", "origin")
        .expect("set remote");
    config
        .set_str("branch.feature.merge", "refs/heads/trunk")
        .expect("set merge");
    let chosen = default_base(fixture.path()).expect("default base");
    assert_eq!(chosen.source, "upstream");
    assert_eq!(chosen.reference, "origin/trunk");
    assert_eq!(chosen.merge_base, fixture.base);
}

#[test]
fn no_default_base_is_a_typed_error_naming_what_was_tried() {
    let dir = TempDir::new().expect("create repository");
    let repo = Repository::init(dir.path()).expect("init repository");
    repo.set_head("refs/heads/feature")
        .expect("unborn feature branch");
    commit_files(
        &repo,
        dir.path(),
        &[("src/lib.rs", "pub fn a() {}\n")],
        "only",
        0,
    );

    match default_base(dir.path()) {
        Err(SnapshotError::NoDefaultBase { tried }) => {
            assert_eq!(tried, vec!["main".to_string(), "master".to_string()]);
        }
        other => panic!("expected NoDefaultBase, got {other:?}"),
    }
}

#[test]
fn a_bad_ref_on_either_side_fails_before_anything_is_indexed() {
    let fixture = build_fixture();
    let cache = TempDir::new().expect("cache directory");
    for (base, head) in [
        ("no-such-ref", fixture.head.as_str()),
        (fixture.base.as_str(), "no-such-ref"),
    ] {
        let error = Comparison::open_with_options(
            fixture.path(),
            base,
            head,
            &ComparisonOptions {
                cache_dir: Some(cache.path().join("snapshots")),
                no_cache: false,
                ..ComparisonOptions::default()
            },
        )
        .err()
        .expect("a bad ref must fail");
        assert!(matches!(error, SnapshotError::Revision { .. }), "{error:?}");
        assert!(
            !cache.path().join("snapshots").exists(),
            "nothing may be built or cached for a request that cannot succeed"
        );
    }
}

#[test]
fn invalid_requests_fail_before_the_repository_is_touched() {
    let fixture = build_fixture();
    let cache = TempDir::new().expect("cache directory");

    let mut bad_bounds = request(&fixture, revisions(&fixture), &cache);
    bad_bounds.bounds.depth = 0;
    assert!(matches!(
        analyse(&bad_bounds),
        Err(AnalysisError::InvalidBound { name: "depth", .. })
    ));

    let mut bad_selection = request(&fixture, revisions(&fixture), &cache);
    bad_selection.selection = vec!["file:src/lib.rs".to_string()];
    assert!(matches!(
        analyse(&bad_selection),
        Err(AnalysisError::InvalidSelection { .. })
    ));

    let bad_ref = request(
        &fixture,
        ChangesRange::Revisions {
            base: "no-such-ref".to_string(),
            head: fixture.head.clone(),
        },
        &cache,
    );
    assert!(matches!(
        analyse(&bad_ref),
        Err(AnalysisError::Snapshot(SnapshotError::Revision { .. }))
    ));

    assert!(!cache.path().join("snapshots").exists());
    assert!(!fixture.path().join(".orbit-graph").exists());
}

#[test]
fn analysis_groups_labelled_callers_and_tests_per_changed_symbol() {
    let fixture = build_callers_fixture();
    let cache = TempDir::new().expect("cache directory");
    let document = analyse(&request(&fixture, revisions(&fixture), &cache)).expect("analyse");

    assert_eq!(document.schema_version, 1);
    assert!(document.complete);
    assert!(document.incomplete.is_none());
    assert_eq!(document.summary.analysed_symbols, document.symbols.len());
    let helper = document
        .symbols
        .iter()
        .find(|symbol| symbol.selector.contains("#helper:"))
        .expect("helper is a changed symbol");
    assert_eq!(helper.status, ChangeStatus::Modified);
    assert!(
        helper
            .callers
            .iter()
            .any(|caller| caller.caller.selector.contains("#entry:")),
        "entry calls helper: {:?}",
        helper.callers
    );
    for caller in &helper.callers {
        assert!(!caller.confidence.is_empty());
        assert!(
            ["call_path", "import_relationship", "reference_path"]
                .contains(&caller.source.as_str())
        );
        assert_eq!(caller.evidence.from.selector, caller.caller.selector);
    }
    assert!(
        helper
            .candidate_tests
            .iter()
            .any(|test| test.test.label.starts_with("tests/helper.rs")),
        "the test that calls helper is a candidate: {:?}",
        helper.candidate_tests
    );
    for test in &helper.candidate_tests {
        assert!(!test.confidence.is_empty());
        if test.source.label() == "call_path" {
            assert!(
                test.evidence.is_some(),
                "a call_path candidate cites its path"
            );
        }
    }
    assert!(
        document
            .tests
            .iter()
            .any(|test| test.changed_symbols.contains(&helper.selector))
    );
    assert_eq!(document.timings.base_cache.as_deref(), Some("miss"));

    let warm = analyse(&request(&fixture, revisions(&fixture), &cache)).expect("warm analyse");
    assert_eq!(warm.timings.base_cache.as_deref(), Some("hit"));
    assert_eq!(warm.timings.head_cache.as_deref(), Some("hit"));
    assert_eq!(
        warm.symbols, document.symbols,
        "a warm call gives the same answer"
    );
}

#[test]
fn every_cap_that_cuts_a_list_is_signalled() {
    let fixture = build_callers_fixture();
    let cache = TempDir::new().expect("cache directory");
    let mut capped = request(&fixture, revisions(&fixture), &cache);
    capped.bounds.max_symbols = 1;
    capped.bounds.max_callers = 1;
    let document = analyse(&capped).expect("analyse");

    assert!(document.complete, "a symbol cap is a bound, not a budget");
    assert!(document.truncated);
    assert_eq!(document.symbols.len(), 1);
    assert!(!document.not_analysed.is_empty());
    assert!(
        document
            .not_analysed
            .iter()
            .all(|symbol| symbol.reason == "max_symbols")
    );
    assert!(
        document
            .truncation
            .iter()
            .any(|flag| flag.bound == "max_symbols" && flag.value == 1)
    );
    for symbol in &document.symbols {
        assert!(symbol.callers.len() <= 1);
        if symbol.callers_found > symbol.callers.len() {
            assert!(document.truncation.iter().any(|flag| {
                flag.bound == "max_callers" && flag.what == format!("callers:{}", symbol.selector)
            }));
        }
    }
}

/// A response ceiling shrinks the document in signalled steps, and a
/// document that already fits is left untouched.
#[test]
fn fitting_to_a_byte_ceiling_cuts_in_signalled_steps() {
    let fixture = build_callers_fixture();
    let cache = TempDir::new().expect("cache directory");
    let document = analyse(&request(&fixture, revisions(&fixture), &cache)).expect("analyse");
    let size = serde_json::to_vec(&document).expect("serialize").len();

    let mut untouched = document.clone();
    assert!(untouched.fit_to_bytes(size));
    assert_eq!(untouched, document, "a fitting document is not cut");

    let mut fitted = document.clone();
    let ceiling = size * 3 / 4;
    assert!(fitted.fit_to_bytes(ceiling));
    assert!(serde_json::to_vec(&fitted).expect("serialize").len() <= ceiling);
    assert!(fitted.truncated);
    assert!(
        fitted
            .truncation
            .iter()
            .any(|flag| flag.bound == "max_response_bytes" && flag.value == ceiling as u64),
        "{:?}",
        fitted.truncation
    );
    assert_eq!(fitted.summary.analysed_symbols, fitted.symbols.len());
    assert_eq!(
        fitted.symbols.len() + fitted.not_analysed.len(),
        document.symbols.len() + document.not_analysed.len(),
        "a symbol cut from the analysis is listed as not analysed"
    );

    let mut hopeless = document;
    assert!(
        !hopeless.fit_to_bytes(16),
        "a ceiling below the envelope is reported, not met by silent loss"
    );
    assert!(hopeless.symbols.is_empty());
    assert!(hopeless.truncated);
}

#[test]
fn unmatched_selection_is_reported_not_ignored() {
    let fixture = build_callers_fixture();
    let cache = TempDir::new().expect("cache directory");
    let mut selected = request(&fixture, revisions(&fixture), &cache);
    selected.selection = vec!["symbol:src/lib.rs#nothing_here:function".to_string()];
    let document = analyse(&selected).expect("analyse");
    assert!(document.symbols.is_empty());
    assert_eq!(document.unmatched_selection, selected.selection);
    assert_eq!(
        document.summary.not_selected_symbols,
        document.summary.changed_symbols
    );
}

#[test]
fn a_passed_deadline_leaves_symbols_unanalysed_and_says_so() {
    let fixture = build_callers_fixture();
    let outcome = Comparison::open_with_progress(
        fixture.path(),
        fixture.base.as_str(),
        fixture.head.as_str(),
        &ComparisonOptions {
            cache_dir: None,
            no_cache: true,
            ..ComparisonOptions::default()
        },
        &NoProgress,
    )
    .expect("open comparison");
    let ComparisonOutcome::Ready(comparison) = outcome else {
        panic!("an uncancelled build must be ready");
    };
    let built = build_report_bounded(
        &comparison,
        &ReportOptions::default(),
        &ReportLimits {
            skip_outbound: true,
            max_symbols: None,
            deadline: Some(ReportDeadline {
                at: Instant::now(),
                budget_ms: 1_000,
            }),
        },
    )
    .expect("bounded report");
    assert!(!built.unanalysed.is_empty());
    assert!(
        built
            .unanalysed
            .iter()
            .all(|(_, reason)| *reason == UnanalysedReason::TimeBudget)
    );
    assert!(built.report.evidence_paths.is_empty());
    assert!(
        built
            .report
            .scope
            .truncated
            .iter()
            .any(|flag| flag.bound == "time_budget_ms" && flag.value == 1_000)
    );
}

/// Reports nothing and never cancels.
struct NoProgress;

impl ComparisonProgress for NoProgress {
    fn on_status(&self, _side: SnapshotSide, _status: &BuildStatus) {}

    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Cancels a build once either side starts indexing.
#[derive(Default)]
struct CancelWhenIndexing {
    cancelled: AtomicBool,
}

impl ComparisonProgress for CancelWhenIndexing {
    fn on_status(&self, _side: SnapshotSide, status: &BuildStatus) {
        if status.state == BuildState::Indexing {
            self.cancelled.store(true, Ordering::SeqCst);
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

#[test]
fn a_cancelled_build_leaves_no_staging_directory_behind() {
    let fixture = build_callers_fixture();
    let cache = TempDir::new().expect("cache directory");
    let cache_dir = cache.path().join("snapshots");
    let outcome = Comparison::open_with_progress(
        fixture.path(),
        fixture.base.as_str(),
        fixture.head.as_str(),
        &ComparisonOptions {
            cache_dir: Some(cache_dir.clone()),
            no_cache: false,
            ..ComparisonOptions::default()
        },
        &CancelWhenIndexing::default(),
    )
    .expect("a cancelled build is not an error");
    assert!(matches!(outcome, ComparisonOutcome::Cancelled));
    let leftovers = staging_entries(cache_dir.as_path());
    assert!(leftovers.is_empty(), "staging left behind: {leftovers:?}");
}

fn staging_entries(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".staging-"))
        .collect()
}

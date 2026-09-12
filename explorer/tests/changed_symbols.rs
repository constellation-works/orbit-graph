//! The changed-symbol slice and its evidence, across the whole fixture corpus.
//!
//! Every case in `tests/fixtures/change-explorer/` is built into a real
//! temporary Git repository, opened as a [`Comparison`], and reconciled against
//! that case's `expected.json`.
//!
//! Reconciliation is by **snapshot presence**, not by the manifest's `change`
//! label, because the two vocabularies answer different questions. The manifest
//! records presence per named snapshot (`present_in` / `absent_in`) and labels
//! the change relative to whichever baseline the case is about — for
//! `branch-divergence` that baseline is the merge base, which this milestone
//! deliberately does not compute (`direct_base_head` is the only mode). The
//! payload answers the direct base-to-head question. Presence is the claim both
//! agree on, so the shared assertions check presence and each case then asserts
//! its own pairing outcome explicitly.

#![allow(clippy::expect_used)]

use std::collections::BTreeSet;

use orbit_graph_explorer::changes::{ChangeStatus, ChangedSymbols, Pairing, PairingEvidence};
use orbit_graph_explorer::evidence::{
    CandidateSource, EvidenceCategory, EvidenceCollector, EvidenceReport,
};
use orbit_graph_explorer::snapshot::{Comparison, SnapshotSide};
use serde_json::Value;

mod common;

use common::corpus::{self, CorpusCase};

#[test]
fn direct_call_reports_only_the_changed_body() {
    let case = Case::open("direct-call");
    case.assert_manifest_presence();

    let helper = case.single("symbol:src/lib.rs#helper:function");
    assert_eq!(helper.status, ChangeStatus::Modified);
    assert_eq!(helper.pairing, Pairing::SameSelector);
    assert_eq!(helper.pairing_evidence, PairingEvidence::Selector);

    // `entry` sits in the same changed file but is byte-identical, so it is not
    // a changed symbol.
    assert!(
        case.changed
            .entries_for("symbol:src/lib.rs#entry:function")
            .is_empty(),
        "byte-identical neighbours must not be reported: {:?}",
        case.selectors()
    );

    // The manifest's caller and test evidence must survive the body change on
    // both sides, unchanged.
    for side in [SnapshotSide::Base, SnapshotSide::Head] {
        let report = case.evidence(side, "symbol:src/lib.rs#helper:function");
        assert_edge(
            &report,
            "src/lib.rs",
            6,
            EvidenceCategory::ResolvedCall,
            "exact",
            "call",
        );
        assert_edge(
            &report,
            "tests/test_lib.rs",
            2,
            EvidenceCategory::ObservedReference,
            "same_module",
            "call",
        );
        assert_eq!(report.commit_sha, case.sha(side));
    }

    let candidates = case.candidate_tests(SnapshotSide::Head, "symbol:src/lib.rs#helper:function");
    assert!(
        candidates
            .iter()
            .any(|(source, selector, _)| *source == CandidateSource::CallPath
                && selector == "symbol:tests/test_lib.rs#test_helper:function"),
        "{candidates:?}"
    );
}

#[test]
fn ambiguous_same_name_resolves_each_qualified_call_to_its_own_target() {
    let case = Case::open("ambiguous-same-name");
    case.assert_manifest_presence();

    let run = case.single("symbol:src/a.rs#run:function");
    assert_eq!(run.status, ChangeStatus::Modified);
    // Only `a::run` changed; the identically named `b::run` and the unchanged
    // caller file must not be dragged in.
    assert_eq!(
        case.selectors(),
        BTreeSet::from(["symbol:src/a.rs#run:function".to_string()])
    );

    // Since ORB-12372 the resolver matches a qualified cross-file call
    // (`a::run()` / `b::run()`) against the candidate's qualification, so each
    // target gets exactly its own call site as a resolved `exact` call and the
    // other site never appears for it.
    for (target, own_line, other_line) in [
        ("symbol:src/a.rs#run:function", 5, 9),
        ("symbol:src/b.rs#run:function", 9, 5),
    ] {
        let report = case.evidence(SnapshotSide::Head, target);
        assert_edge(
            &report,
            "src/lib.rs",
            own_line,
            EvidenceCategory::ResolvedCall,
            "exact",
            "call",
        );
        assert!(
            report
                .paths
                .iter()
                .flat_map(|path| path.edges.iter())
                .all(|edge| edge.source.line != Some(other_line)),
            "a qualified same-name target must not attract the other module's call site: {report:?}"
        );
        assert!(
            report
                .paths
                .iter()
                .flat_map(|path| path.edges.iter())
                .all(|edge| edge.category != EvidenceCategory::HeuristicMatch),
            "qualified same-name calls resolve exactly; no heuristic evidence expected: {report:?}"
        );
    }
}

#[test]
fn changed_signature_is_labelled_signature_changed() {
    let case = Case::open("changed-signature");
    case.assert_manifest_presence();

    let process = case.single("symbol:mod.py#process:function");
    assert_eq!(process.status, ChangeStatus::SignatureChanged);
    assert_eq!(process.pairing, Pairing::SignatureChanged);
    assert_eq!(process.pairing_evidence, PairingEvidence::Signature);
    let note = process.note.clone().unwrap_or_default();
    assert!(note.contains("def process(x)"), "{note}");
    assert!(note.contains("def process(x, y=0)"), "{note}");

    // Callers keep calling with the original arity and must stay valid on both
    // sides, so the manifest's reference evidence is asserted on both.
    for side in [SnapshotSide::Base, SnapshotSide::Head] {
        let report = case.evidence(side, "symbol:mod.py#process:function");
        assert_edge(
            &report,
            "mod.py",
            6,
            EvidenceCategory::ResolvedCall,
            "exact",
            "call",
        );
        assert_edge(
            &report,
            "test_process.py",
            5,
            EvidenceCategory::ResolvedCall,
            "import_resolved",
            "call",
        );
        assert_edge(
            &report,
            "test_process.py",
            1,
            EvidenceCategory::ImportRelationship,
            "import_resolved",
            "use",
        );

        // The known gap: a `getattr` call is invisible to the extractor, so
        // there is no edge from `test_naming.py` at any confidence.
        assert!(
            report
                .paths
                .iter()
                .flat_map(|path| path.edges.iter())
                .all(|edge| edge.source.file != "test_naming.py"),
            "dynamic dispatch must not produce an edge: {report:?}"
        );
    }

    // That blind spot is disclosed rather than hidden: `test_naming.py` still
    // reaches the candidate list, through the two weaker sources only.
    let candidates = case.candidate_tests(SnapshotSide::Head, "symbol:mod.py#process:function");
    assert!(
        candidates.iter().any(|(source, selector, category)| {
            *source == CandidateSource::ImportRelationship
                && selector == "file:test_naming.py"
                && *category == EvidenceCategory::ImportRelationship
        }),
        "{candidates:?}"
    );
    assert!(
        candidates.iter().any(|(source, selector, category)| {
            *source == CandidateSource::NamingHeuristic
                && selector == "symbol:test_naming.py#test_process_dynamic:function"
                && *category == EvidenceCategory::HeuristicMatch
        }),
        "{candidates:?}"
    );
    // A heuristic candidate is never promoted to a call-path candidate.
    assert!(
        candidates.iter().all(|(source, selector, _)| {
            *source != CandidateSource::CallPath || !selector.contains("test_naming.py")
        }),
        "{candidates:?}"
    );
}

#[test]
fn removed_symbol_is_base_evidence_only() {
    let case = Case::open("removed-symbol");
    case.assert_manifest_presence();

    let removed = case.single("symbol:src/lib.rs#old_helper:function");
    assert_eq!(removed.status, ChangeStatus::Removed);
    assert_eq!(removed.supporting_snapshots, vec!["base".to_string()]);
    assert!(removed.head.is_none());
    assert_eq!(
        removed.base.as_ref().map(|entry| entry.commit_sha.as_str()),
        Some(case.base_sha.as_str())
    );

    // The rewritten caller changed too, and is reported as `modified`.
    let caller = case.single("symbol:src/lib.rs#caller:function");
    assert_eq!(caller.status, ChangeStatus::Modified);

    let base = case.evidence(SnapshotSide::Base, "symbol:src/lib.rs#old_helper:function");
    assert_edge(
        &base,
        "src/lib.rs",
        6,
        EvidenceCategory::ResolvedCall,
        "exact",
        "call",
    );
    let head = case.evidence(SnapshotSide::Head, "symbol:src/lib.rs#old_helper:function");
    assert!(!head.resolved, "{head:?}");
    assert!(head.paths.is_empty(), "{head:?}");
    assert!(
        head.no_path_reasons
            .iter()
            .any(|reason| reason.contains("did not resolve")),
        "an unresolved target must say so: {:?}",
        head.no_path_reasons
    );
}

#[test]
fn renamed_file_pairs_as_moved_not_as_an_unrelated_remove_and_add() {
    let case = Case::open("renamed-file");
    case.assert_manifest_presence();

    let pairs = [
        (
            "symbol:formatter.py#format_value:function",
            "symbol:formatting/formatter.py#format_value:function",
            "byte-identical",
        ),
        (
            "symbol:helper.py#helper_fn:function",
            "symbol:helpers.py#helper_fn:function",
            "content also changed",
        ),
    ];
    for (base_selector, head_selector, expected_note) in pairs {
        let moved = case.single(base_selector);
        assert!(
            matches!(moved.status, ChangeStatus::Moved | ChangeStatus::Uncertain),
            "the ladder must pair a renamed file's symbols, not report an unrelated remove and \
             add: {moved:?}"
        );
        assert_eq!(moved.status, ChangeStatus::Moved);
        assert_eq!(moved.pairing, Pairing::Moved);
        assert_eq!(moved.pairing_evidence, PairingEvidence::GitRename);
        assert_eq!(
            moved.head.as_ref().map(|entry| entry.selector.as_str()),
            Some(head_selector)
        );
        assert_eq!(
            moved.supporting_snapshots,
            vec!["base".to_string(), "head".to_string()]
        );
        let note = moved.note.clone().unwrap_or_default();
        assert!(note.contains(expected_note), "{note}");
        // The known gap stays visible: the pairing came from Git, because the
        // index has no cross-path symbol identity of its own.
        assert!(note.contains("cross-path symbol identity"), "{note}");

        // Each entry accounts for both selectors, so neither side is also
        // reported as a bare removal or addition.
        assert_eq!(case.changed.entries_for(base_selector).len(), 1);
        assert_eq!(case.changed.entries_for(head_selector).len(), 1);
    }
    assert_eq!(case.changed.symbols.len(), 2, "{:?}", case.selectors());

    // The callers in `main.py` keep resolving by name at the same confidence on
    // both sides, at the paths the manifest records.
    for (side, selector, use_line) in [
        (
            SnapshotSide::Base,
            "symbol:formatter.py#format_value:function",
            1,
        ),
        (
            SnapshotSide::Head,
            "symbol:formatting/formatter.py#format_value:function",
            1,
        ),
        (SnapshotSide::Base, "symbol:helper.py#helper_fn:function", 2),
        (
            SnapshotSide::Head,
            "symbol:helpers.py#helper_fn:function",
            2,
        ),
    ] {
        let report = case.evidence(side, selector);
        assert_edge(
            &report,
            "main.py",
            6,
            EvidenceCategory::ResolvedCall,
            "import_resolved",
            "call",
        );
        assert_edge(
            &report,
            "main.py",
            use_line,
            EvidenceCategory::ImportRelationship,
            "import_resolved",
            "use",
        );
    }
}

#[test]
fn changed_test_does_not_implicate_the_untouched_source() {
    let case = Case::open("changed-test");
    case.assert_manifest_presence();

    let test = case.single("symbol:tests/add_test.rs#test_add:function");
    assert_eq!(test.status, ChangeStatus::Modified);
    assert_eq!(
        case.selectors(),
        BTreeSet::from(["symbol:tests/add_test.rs#test_add:function".to_string()]),
        "only the test changed"
    );

    // The known gap: a call written as a macro argument produces no reference,
    // so `add` has no call-path candidate even though the test clearly
    // exercises it. The naming heuristic discloses the link without inventing
    // an edge.
    let report = case.evidence(SnapshotSide::Head, "symbol:src/lib.rs#add:function");
    assert!(report.resolved, "{report:?}");
    assert!(
        report.paths.is_empty(),
        "a call inside a macro invocation must not produce an edge: {report:?}"
    );
    assert!(!report.no_path_reasons.is_empty(), "{report:?}");
    assert!(
        report
            .no_path_reasons
            .iter()
            .any(|reason| reason.contains("macro")),
        "{:?}",
        report.no_path_reasons
    );

    let candidates = case.candidate_tests(SnapshotSide::Head, "symbol:src/lib.rs#add:function");
    assert!(
        candidates
            .iter()
            .all(|(source, _, _)| *source != CandidateSource::CallPath),
        "{candidates:?}"
    );
    assert!(
        candidates.iter().any(|(source, selector, category)| {
            *source == CandidateSource::NamingHeuristic
                && selector == "symbol:tests/add_test.rs#test_add:function"
                && *category == EvidenceCategory::HeuristicMatch
        }),
        "the naming heuristic must disclose the link the extractor cannot see: {candidates:?}"
    );
}

#[test]
fn unsupported_language_contributes_nothing_rather_than_a_false_edge() {
    let case = Case::open("generated-unsupported");
    case.assert_manifest_presence();

    let config_value = case.single("symbol:src/lib.rs#config_value:function");
    assert_eq!(config_value.status, ChangeStatus::Modified);

    // `schema.proto` textually mentions `config_value`, but Protocol Buffers
    // has no extractor. The mention must produce no symbol, no edge, and no
    // candidate test.
    for side in [SnapshotSide::Base, SnapshotSide::Head] {
        let report = case.evidence(side, "symbol:src/lib.rs#config_value:function");
        assert!(
            report
                .paths
                .iter()
                .flat_map(|path| path.edges.iter())
                .all(|edge| edge.source.file != "schema.proto"),
            "{report:?}"
        );
    }
    assert!(
        case.selectors()
            .iter()
            .all(|selector| !selector.contains("schema.proto")),
        "{:?}",
        case.selectors()
    );
    // `schema.proto` is byte-identical across the change, so it is not a
    // changed path and there is nothing to report as out of scope.
    assert!(
        case.changed.out_of_scope.is_empty(),
        "{:?}",
        case.changed.out_of_scope
    );
}

#[test]
fn mutual_recursion_resolves_each_direction_without_looping() {
    let case = Case::open("cycle");
    case.assert_manifest_presence();

    let is_even = case.single("symbol:src/lib.rs#is_even:function");
    assert_eq!(is_even.status, ChangeStatus::Modified);
    assert_eq!(
        case.selectors(),
        BTreeSet::from(["symbol:src/lib.rs#is_even:function".to_string()]),
        "`is_odd` is byte-identical and must not be reported"
    );

    for side in [SnapshotSide::Base, SnapshotSide::Head] {
        assert_edge(
            &case.evidence(side, "symbol:src/lib.rs#is_even:function"),
            "src/lib.rs",
            13,
            EvidenceCategory::ResolvedCall,
            "exact",
            "call",
        );
        assert_edge(
            &case.evidence(side, "symbol:src/lib.rs#is_odd:function"),
            "src/lib.rs",
            5,
            EvidenceCategory::ResolvedCall,
            "exact",
            "call",
        );
    }
}

#[test]
fn branch_divergence_is_reported_as_the_direct_reading_it_is() {
    let case = Case::open("branch-divergence");
    case.assert_manifest_presence();

    // This milestone compares the two revisions the user named. The manifest's
    // `direct_diff_note` states exactly this reading: `base_only` looks removed
    // and `head_only` looks added.
    let base_only = case.single("symbol:src/lib.rs#base_only:function");
    assert_eq!(base_only.status, ChangeStatus::Removed);
    assert_eq!(base_only.supporting_snapshots, vec!["base".to_string()]);
    let head_only = case.single("symbol:src/lib.rs#head_only:function");
    assert_eq!(head_only.status, ChangeStatus::Added);
    assert_eq!(head_only.supporting_snapshots, vec!["head".to_string()]);

    // Two symbols with different names in a file Git did not rename are not
    // paired: the ladder's `renamed` rung needs Git evidence, and guessing here
    // would invent a rename that did not happen.
    assert!(
        case.changed
            .symbols
            .iter()
            .all(|symbol| symbol.status != ChangeStatus::Renamed),
        "{:?}",
        case.changed.symbols
    );
    assert_eq!(case.changed.symbols.len(), 2, "{:?}", case.selectors());
    assert_eq!(case.comparison.mode().label(), "direct_base_head");
}

#[test]
fn every_case_produces_a_deterministic_payload() {
    for case_id in corpus::CASES {
        let case = Case::open(case_id);
        let again = ChangedSymbols::compute(&case.comparison).expect("recompute changed symbols");
        assert_eq!(
            case.changed, again,
            "case `{case_id}` must produce an identical payload on a second computation"
        );
        assert_eq!(case.changed.schema_version, 1);
        for symbol in &case.changed.symbols {
            assert!(
                symbol.base.is_some()
                    || symbol.head.is_some()
                    || !symbol.uncertain_candidates.is_empty(),
                "case `{case_id}` produced an entry with no side and no candidate: {symbol:?}"
            );
            if symbol.status == ChangeStatus::Uncertain {
                assert!(
                    !symbol.uncertain_candidates.is_empty(),
                    "case `{case_id}`: an uncertain entry must list every candidate: {symbol:?}"
                );
            }
            for side in [symbol.base.as_ref(), symbol.head.as_ref()]
                .into_iter()
                .flatten()
            {
                let expected = if side.snapshot == "base" {
                    case.base_sha.as_str()
                } else {
                    case.head_sha.as_str()
                };
                assert_eq!(
                    side.commit_sha, expected,
                    "case `{case_id}`: every occurrence names its own snapshot SHA"
                );
            }
        }
    }
}

#[test]
fn a_changed_unsupported_file_is_out_of_scope_never_removed() {
    // No corpus case changes an unsupported file, so this builds the situation
    // directly: a path the extractor has no grammar for, edited between the two
    // revisions.
    let repository = tempfile::TempDir::new().expect("create repository");
    let repo = git2::Repository::init(repository.path()).expect("init repository");
    let base = commit_all(
        &repo,
        repository.path(),
        &[
            ("src/lib.rs", "pub fn entry() -> i32 {\n    7\n}\n"),
            (
                "schema.proto",
                "// generated reference to entry\nmessage Config {}\n",
            ),
        ],
        "base: add an unsupported file",
        0,
    );
    let head = commit_all(
        &repo,
        repository.path(),
        &[
            ("src/lib.rs", "pub fn entry() -> i32 {\n    7\n}\n"),
            (
                "schema.proto",
                "// generated reference to entry\nmessage Config { int32 id = 1; }\n",
            ),
        ],
        "head: edit the unsupported file",
        1,
    );
    drop(repo);

    let comparison =
        Comparison::open(repository.path(), base.as_str(), head.as_str()).expect("open comparison");
    let changed = ChangedSymbols::compute(&comparison).expect("compute changed symbols");

    assert!(
        changed.symbols.is_empty(),
        "an unindexed file has no symbols to report: {:?}",
        changed.symbols
    );
    let paths: Vec<(&str, &str, &str)> = changed
        .out_of_scope
        .iter()
        .map(|entry| {
            (
                entry.path.as_str(),
                entry.reason.label(),
                entry.snapshot.as_str(),
            )
        })
        .collect();
    assert_eq!(
        paths,
        vec![
            ("schema.proto", "unsupported_language", "base"),
            ("schema.proto", "unsupported_language", "head"),
        ],
        "a changed path the extractor never indexed is out of scope on both sides"
    );
}

/// Write `files`, stage everything, and commit with a fixed identity and time.
fn commit_all(
    repo: &git2::Repository,
    root: &std::path::Path,
    files: &[(&str, &str)],
    message: &str,
    offset: i64,
) -> String {
    for (path, contents) in files {
        let target = root.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).expect("create directory");
        }
        std::fs::write(target.as_path(), contents).expect("write file");
    }
    let mut index = repo.index().expect("open index");
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .expect("stage tree");
    index.write().expect("write index");
    let tree_id = index.write_tree().expect("write tree");
    let tree = repo.find_tree(tree_id).expect("find tree");
    let when = git2::Time::new(1_700_000_000 + offset, 0);
    let author = git2::Signature::new("Fixture Author", "fixture@example.invalid", &when)
        .expect("signature");
    let parent = repo
        .head()
        .ok()
        .and_then(|head| head.target())
        .map(|oid| repo.find_commit(oid).expect("find parent commit"));
    let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
    repo.commit(
        Some("HEAD"),
        &author,
        &author,
        message,
        &tree,
        parents.as_slice(),
    )
    .expect("create commit")
    .to_string()
}

/// One built corpus case, with its comparison and computed payload.
struct Case {
    case: CorpusCase,
    comparison: Comparison,
    changed: ChangedSymbols,
    base_sha: String,
    head_sha: String,
}

impl Case {
    fn open(case_id: &str) -> Self {
        let case = corpus::build_case(case_id);
        let comparison = Comparison::open(case.repository.as_path(), case.base(), case.head())
            .unwrap_or_else(|error| panic!("case `{case_id}`: open comparison: {error}"));
        let changed = ChangedSymbols::compute(&comparison)
            .unwrap_or_else(|error| panic!("case `{case_id}`: compute changed symbols: {error}"));
        let base_sha = case.base().to_string();
        let head_sha = case.head().to_string();
        Self {
            case,
            comparison,
            changed,
            base_sha,
            head_sha,
        }
    }

    fn sha(&self, side: SnapshotSide) -> String {
        match side {
            SnapshotSide::Base => self.base_sha.clone(),
            SnapshotSide::Head => self.head_sha.clone(),
        }
    }

    /// Every selector the payload mentions, on either side.
    fn selectors(&self) -> BTreeSet<String> {
        let mut selectors = BTreeSet::new();
        for symbol in &self.changed.symbols {
            for side in [symbol.base.as_ref(), symbol.head.as_ref()]
                .into_iter()
                .flatten()
            {
                selectors.insert(side.selector.clone());
            }
            for candidate in &symbol.uncertain_candidates {
                selectors.insert(candidate.symbol.selector.clone());
            }
        }
        selectors
    }

    /// The single payload entry mentioning `selector`.
    fn single(&self, selector: &str) -> &orbit_graph_explorer::changes::ChangedSymbol {
        let entries = self.changed.entries_for(selector);
        assert_eq!(
            entries.len(),
            1,
            "case `{}`: expected exactly one entry for `{selector}`, found {}: {:?}",
            self.case.case_id,
            entries.len(),
            self.selectors()
        );
        entries[0]
    }

    fn evidence(&self, side: SnapshotSide, selector: &str) -> EvidenceReport {
        let mut collector =
            EvidenceCollector::new(&self.comparison, side).expect("build evidence collector");
        collector
            .evidence(selector, Default::default())
            .unwrap_or_else(|error| {
                panic!(
                    "case `{}`: evidence for `{selector}` on {side}: {error}",
                    self.case.case_id
                )
            })
    }

    fn candidate_tests(
        &self,
        side: SnapshotSide,
        selector: &str,
    ) -> Vec<(CandidateSource, String, EvidenceCategory)> {
        let mut collector =
            EvidenceCollector::new(&self.comparison, side).expect("build evidence collector");
        let candidates = collector
            .candidate_tests(
                selector,
                Default::default(),
                self.changed.out_of_scope.clone(),
            )
            .unwrap_or_else(|error| {
                panic!(
                    "case `{}`: candidate tests for `{selector}` on {side}: {error}",
                    self.case.case_id
                )
            });
        assert_eq!(candidates.query_options.depth, 1);
        candidates
            .candidates
            .into_iter()
            .map(|candidate| {
                assert_eq!(candidate.label, candidate.source.corpus_label());
                (
                    candidate.source,
                    candidate.test.selector,
                    candidate.category,
                )
            })
            .collect()
    }

    /// Reconcile the payload with the manifest's per-snapshot presence claims.
    ///
    /// A symbol the manifest says is present only in base must appear on the
    /// payload's base side, and likewise for head; a symbol present in both must
    /// either appear on both sides or not be reported at all, because a symbol
    /// that survives unchanged is not a changed symbol. A symbol the manifest
    /// lists under `unchanged_symbols` must not be reported at all.
    fn assert_manifest_presence(&self) {
        for entry in self.case.manifest_array("changed_symbols") {
            let selector = entry["selector"]
                .as_str()
                .expect("changed_symbols[].selector");
            let present = snapshot_list(entry, "present_in");
            let absent = snapshot_list(entry, "absent_in");

            let entries = self.changed.entries_for(selector);
            let sides: BTreeSet<String> = entries
                .iter()
                .flat_map(|symbol| {
                    [symbol.base.as_ref(), symbol.head.as_ref()]
                        .into_iter()
                        .flatten()
                })
                .filter(|side| side.selector == selector)
                .map(|side| side.snapshot.clone())
                .collect();

            for side in ["base", "head"] {
                if present.contains(side) {
                    assert!(
                        sides.contains(side)
                            || (present.contains("base") && present.contains("head")),
                        "case `{}`: `{selector}` is present in `{side}` per the manifest but the \
                         payload does not carry it there: {:?}",
                        self.case.case_id,
                        self.selectors()
                    );
                }
                if absent.contains(side) {
                    assert!(
                        !sides.contains(side),
                        "case `{}`: `{selector}` is absent from `{side}` per the manifest but the \
                         payload claims it there",
                        self.case.case_id
                    );
                }
            }
        }

        for entry in self.case.manifest_array("unchanged_symbols") {
            let selector = entry["selector"]
                .as_str()
                .expect("unchanged_symbols[].selector");
            assert!(
                self.changed.entries_for(selector).is_empty(),
                "case `{}`: `{selector}` is stable across every snapshot and must not be reported \
                 as changed",
                self.case.case_id
            );
        }
    }
}

fn snapshot_list(entry: &Value, field: &str) -> BTreeSet<String> {
    entry
        .get(field)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Assert one edge, matching the manifest's `expected_references` shape.
fn assert_edge(
    report: &EvidenceReport,
    file: &str,
    line: usize,
    category: EvidenceCategory,
    confidence: &str,
    relationship: &str,
) {
    let found = report
        .paths
        .iter()
        .flat_map(|path| path.edges.iter())
        .any(|edge| {
            edge.source.file == file
                && edge.source.line == Some(line)
                && edge.category == category
                && edge.confidence == confidence
                && edge.relationship == relationship
                && edge.snapshot == report.target.snapshot
                && edge.commit_sha == report.commit_sha
        });
    assert!(
        found,
        "missing {category:?}/{confidence}/{relationship} edge at {file}:{line}; report: {report:?}"
    );
}

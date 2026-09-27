use super::*;
use std::fs;

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
            commit_text_weight: None,
            commit_text_exponent: None,
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
            commit_text_weight: None,
            commit_text_exponent: None,
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
            commit_text_weight: None,
            commit_text_exponent: None,
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
        commit_text_weight: None,
        commit_text_exponent: None,
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
fn first_parent_distances_match_revwalk_distances_across_merges() {
    let fixture = fixture_repo();
    let root = fixture.path();
    fs::write(root.join("src/a.rs"), "fn a() {}\n").expect("a");
    commit_all(root, "base");
    git(root, &["checkout", "-b", "side"]);
    for index in 0..3 {
        fs::write(root.join(format!("src/side{index}.rs")), "fn s() {}\n").expect("side");
        commit_all(root, "side");
    }
    git(root, &["checkout", "main"]);
    fs::write(root.join("src/b.rs"), "fn b() {}\n").expect("b");
    commit_all(root, "main");
    git(root, &["merge", "--no-ff", "-m", "merge side", "side"]);
    fs::write(root.join("src/c.rs"), "fn c() {}\n").expect("c");
    commit_all(root, "after merge");
    let repo = Repository::open(root).expect("repo");
    let target = repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .expect("head")
        .id();
    let mut chain = Vec::new();
    let mut current = repo.find_commit(target).expect("target commit");
    loop {
        chain.push(current.id());
        let Ok(parent) = current.parent(0) else { break };
        current = parent;
    }
    let side_tip = repo
        .revparse_single("side")
        .and_then(|object| object.peel_to_commit())
        .expect("side")
        .id();
    let mut lineage = PathLineage::new(&repo, target, &[], Vec::new()).expect("lineage");
    lineage
        .index_first_parent_distances(&repo, chain.iter().copied().collect())
        .expect("index distances");
    for oid in chain.iter().copied().chain([side_tip]) {
        assert_eq!(
            lineage
                .distance(&repo, oid)
                .map_err(|error| error.to_string()),
            commit_distance(&repo, oid, target).map_err(|error| error.to_string()),
            "distance mismatch for {oid}"
        );
    }
}

#[test]
fn commit_distance_names_an_unknown_distance_when_git_cannot_walk() {
    let fixture = TempDir::new().expect("fixture");
    git(fixture.path(), &["init", "-b", "main"]);
    git(
        fixture.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    git(fixture.path(), &["config", "user.name", "Test"]);
    fs::write(fixture.path().join("a.rs"), "fn value() {}\n").expect("write");
    commit_all(fixture.path(), "root");
    let repo = Repository::open(fixture.path()).expect("repo");
    let target = Oid::from_str(head(fixture.path()).as_str()).expect("target");
    let missing = Oid::from_str("0000000000000000000000000000000000000000").expect("oid");
    let error = commit_distance(&repo, missing, target).expect_err("missing commit");
    assert!(error.to_string().contains("distance unknown"), "{error}");
}

#[test]
fn recommendation_keeps_pinned_target_and_graph_family_after_checkout_moves() {
    let fixture = fixture_repo();
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn value() -> i32 { 1 }\n",
    )
    .expect("write root");
    commit_all(fixture.path(), "root");
    let earlier = head(fixture.path());
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn value() -> i32 { 2 }\n",
    )
    .expect("write target");
    commit_all(fixture.path(), "target");
    let target = head(fixture.path());
    HistoryIndex::open(fixture.path(), "main").expect("history");
    Graph::open(fixture.path(), crate::SyncPolicy::Manual)
        .expect("graph")
        .sync(crate::SyncMode::Full)
        .expect("sync graph");
    let moved = fixture.path().to_path_buf();
    RECOMMEND_AFTER_TARGET.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            git(&moved, &["checkout", "--detach", earlier.as_str()]);
        }));
    });
    let engine = RecommendationEngine::open(fixture.path(), "main").expect("engine");
    let mut request = query_request("value", target.as_str(), None);
    request.target_revision = None;
    let result = engine.recommend(&request).expect("recommend pinned target");
    assert_eq!(result.resolved_target_revision, target);
    assert!(
        result.fallbacks.iter().all(|fallback| {
            fallback.kind != "structure_unavailable_for_revision"
                && fallback.kind != "structure_index_missing"
        }),
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

fn target_symbol_extractions() -> usize {
    TARGET_SYMBOL_EXTRACTIONS.with(std::cell::Cell::get)
}

fn cache_files(root: &Path) -> Vec<String> {
    let mut names = fs::read_dir(root.join(".orbit-graph"))
        .expect("index dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.starts_with(TARGET_SYMBOL_CACHE_PREFIX))
        .collect::<Vec<_>>();
    names.sort();
    names
}

/// Everything except the live observation time, which differs per call.
fn comparable(result: &RecommendationResult) -> String {
    let mut value = serde_json::to_value(result).expect("serialize result");
    value["effective_cutoff"] = serde_json::Value::Null;
    value.to_string()
}

fn cache_fixture() -> (TempDir, String, String) {
    let fixture = fixture_repo();
    let root = fixture.path();
    fs::write(
            root.join("src/invoice.rs"),
            "pub struct Invoice;\nimpl Invoice {\n    pub fn invoice_total(&self) -> i32 { 1 }\n}\npub fn render_invoice() {}\n",
        )
        .expect("invoice");
    fs::write(
        root.join("src/tax.py"),
        "def invoice_tax():\n    return 1\n",
    )
    .expect("tax");
    commit_all(root, "base");
    let base = head(root);
    fs::write(
            root.join("src/invoice.rs"),
            "pub struct Invoice;\nimpl Invoice {\n    pub fn invoice_total(&self) -> i32 { 2 }\n}\npub fn render_invoice() {}\n",
        )
        .expect("edit invoice");
    commit_all(root, "delivery");
    let delivery = head(root);
    let index = HistoryIndex::open(root, "main").expect("history");
    import(
        &index,
        base.as_str(),
        delivery.as_str(),
        "D-INVOICE",
        "TASK-INVOICE",
        "Fix invoice total",
        10,
    );
    (fixture, base, delivery)
}

#[test]
fn target_symbol_cache_is_reused_and_results_match_uncached_extraction() {
    let (fixture, _base, delivery) = cache_fixture();
    let root = fixture.path();
    let engine = RecommendationEngine::open(root, "main").expect("engine");
    for level in [RecommendationLevel::File, RecommendationLevel::Symbol] {
        for name in cache_files(root) {
            fs::remove_file(root.join(".orbit-graph").join(name)).expect("clear cache");
        }
        let mut request = query_request("invoice total", &delivery, None);
        request.level = level;
        let before = target_symbol_extractions();
        let cold = engine.recommend(&request).expect("cold");
        assert_eq!(target_symbol_extractions(), before + 1, "cold call parses");
        let warm = engine.recommend(&request).expect("warm");
        assert_eq!(
            target_symbol_extractions(),
            before + 1,
            "warm call must reuse the cache"
        );
        assert!(!cold.recommendations.is_empty());
        assert_eq!(comparable(&cold), comparable(&warm), "{level:?}");
    }
    let names = cache_files(root);
    assert_eq!(names.len(), 1, "{names:?}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(root.join(".orbit-graph").join(&names[0]))
            .expect("cache metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    // File-level history-only variants never parse the target tree.
    let before = target_symbol_extractions();
    for variant in [
        RecommendationVariant::TaskSearchOnly,
        RecommendationVariant::Frequency,
    ] {
        let mut request = query_request("invoice total", &delivery, None);
        request.variant = variant;
        engine.recommend(&request).expect("history-only variant");
    }
    assert_eq!(target_symbol_extractions(), before);
}

#[test]
fn target_symbol_cache_redacts_default_argument_credentials() {
    let token = "ghp_12345678901234567890";
    let fixture = fixture_repo();
    let root = fixture.path();
    fs::write(
        root.join("src/client.py"),
        "def connect(token=\"placeholder\"):\n    return token\n\ndef label(name=\"service\"):\n    return name\n",
    )
    .expect("before");
    commit_all(root, "before");
    let before = head(root);
    fs::write(
        root.join("src/client.py"),
        format!(
            "def connect(token=\"{token}\"):\n    return token\n\ndef label(name=\"service\"):\n    return name\n"
        ),
    )
    .expect("after");
    commit_all(root, "after");
    let after = head(root);
    let index = HistoryIndex::open(root, "main").expect("history");
    import(
        &index,
        before.as_str(),
        after.as_str(),
        "D-SECRET",
        "TASK-SECRET",
        "Rotate the client token default",
        10,
    );
    let engine = RecommendationEngine::open(root, "main").expect("engine");
    let mut request = query_request("label service", after.as_str(), None);
    request.level = RecommendationLevel::Symbol;
    let cold = engine.recommend(&request).expect("cold recommend");
    let warm = engine.recommend(&request).expect("warm recommend");
    assert_eq!(comparable(&cold), comparable(&warm));
    assert!(
        cold.recommendations
            .iter()
            .any(|item| item.selector.contains("label")),
        "benign identifier should stay recommendable: {cold:?}"
    );

    let names = cache_files(root);
    assert_eq!(names.len(), 1, "{names:?}");
    let cache = fs::read(root.join(".orbit-graph").join(&names[0])).expect("read cache");
    let cache_text = String::from_utf8(cache.clone()).expect("cache utf8");
    assert!(cache_text.contains("[REDACTED_SECRET]"), "{cache_text}");
    assert!(!cache_text.contains(token), "{cache_text}");
    assert!(
        cache_text.contains("name=\\\"service\\\"") || cache_text.contains("name=\"service\""),
        "{cache_text}"
    );
    assert!(cache_text.contains("\"name\":\"connect\""), "{cache_text}");
    assert!(cache_text.contains("\"name\":\"label\""), "{cache_text}");
    assert!(
        cache_text.contains("\"qualified\":\"connect\""),
        "{cache_text}"
    );
    assert!(
        cache_text.contains("\"qualified\":\"label\""),
        "{cache_text}"
    );

    let conn = rusqlite::Connection::open(index.database_path()).expect("history db");
    let payload: String = conn
        .query_row("SELECT payload_json FROM history_deliveries", [], |row| {
            row.get(0)
        })
        .expect("payload");
    let signature: String = conn
        .query_row(
            "SELECT signature FROM history_symbols WHERE name='connect' AND signature LIKE '%REDACTED_SECRET%'",
            [],
            |row| row.get(0),
        )
        .expect("redacted history signature");
    drop(conn);
    assert!(!payload.contains(token), "{payload}");
    assert!(signature.contains("[REDACTED_SECRET]"));
    assert!(!signature.contains(token));
    let history_bytes = fs::read(index.database_path()).expect("history bytes");
    assert!(
        !history_bytes
            .windows(token.len())
            .any(|window| window == token.as_bytes())
    );
    assert!(
        !cache
            .windows(token.len())
            .any(|window| window == token.as_bytes())
    );
}

#[test]
fn target_symbol_cache_invalidates_on_revision_and_extractor_version() {
    let (fixture, base, delivery) = cache_fixture();
    let root = fixture.path();
    let engine = RecommendationEngine::open(root, "main").expect("engine");
    let request = query_request("invoice total", &delivery, None);
    let reference = engine.recommend(&request).expect("populate cache");
    let populated = target_symbol_extractions();
    let names = cache_files(root);
    assert_eq!(names.len(), 1);
    let cache_path = root.join(".orbit-graph").join(&names[0]);

    // A different target tree is a different key.
    engine
        .recommend(&query_request("invoice total", &base, None))
        .expect("other revision");
    assert_eq!(target_symbol_extractions(), populated + 1);
    assert_eq!(cache_files(root).len(), 2);
    engine.recommend(&request).expect("original revision");
    assert_eq!(
        target_symbol_extractions(),
        populated + 1,
        "the original revision's entry is still valid"
    );

    // An entry recorded by another extractor version is never trusted.
    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(&cache_path).expect("read cache")).expect("json");
    document["extractor_version"] = serde_json::json!(crate::EXTRACTOR_VERSION + 1);
    document["files"] = serde_json::json!([]);
    fs::write(&cache_path, document.to_string()).expect("tamper");
    let rebuilt = engine.recommend(&request).expect("version mismatch");
    assert_eq!(target_symbol_extractions(), populated + 2);
    assert_eq!(comparable(&rebuilt), comparable(&reference));

    // Corrupt entries fall back to extraction without failing the request.
    fs::write(&cache_path, b"{not json").expect("corrupt");
    let recovered = engine.recommend(&request).expect("corrupt cache");
    assert_eq!(target_symbol_extractions(), populated + 3);
    assert_eq!(comparable(&recovered), comparable(&reference));

    // Entries named for an older extractor version are pruned on the next
    // write; a newer binary's entries are left alone.
    let entry_name = |version: u32| {
        format!(
            "{TARGET_SYMBOL_CACHE_PREFIX}{version}.{}.json",
            "0".repeat(40)
        )
    };
    let older = root
        .join(".orbit-graph")
        .join(entry_name(crate::EXTRACTOR_VERSION - 1));
    let newer = root
        .join(".orbit-graph")
        .join(entry_name(crate::EXTRACTOR_VERSION + 1));
    fs::write(&older, b"{}").expect("older entry");
    fs::write(&newer, b"{}").expect("newer entry");
    fs::remove_file(&cache_path).expect("drop entry");
    engine.recommend(&request).expect("rewrite");
    assert!(!older.exists(), "older extractor versions are pruned");
    assert!(
        newer.exists(),
        "a newer binary's entries are not ours to prune"
    );

    // A warm hit records the entry's last use.
    let long_ago = SystemTime::now() - Duration::from_secs(24 * 60 * 60);
    set_mtime(&cache_path, long_ago);
    engine.recommend(&request).expect("warm hit");
    assert_eq!(target_symbol_extractions(), populated + 4);
    let used = fs::metadata(&cache_path)
        .and_then(|metadata| metadata.modified())
        .expect("mtime");
    assert!(used > long_ago + Duration::from_secs(60 * 60));
}

fn set_mtime(path: &Path, when: SystemTime) {
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(when))
        .expect("set mtime");
}

#[test]
fn target_symbol_cache_names_follow_an_exact_grammar() {
    let tree = "0123456789abcdef0123456789abcdef01234567";
    let version = crate::EXTRACTOR_VERSION;
    assert_eq!(
        parse_target_symbol_cache_name(&format!("recommend-target.{version}.{tree}.json")),
        Some(TargetSymbolCacheName::Entry {
            extractor_version: version
        })
    );
    assert_eq!(
        parse_target_symbol_cache_name(&format!("recommend-target.7.{tree}.json.tmp-4242")),
        Some(TargetSymbolCacheName::Temp {
            extractor_version: 7,
            pid: 4242
        })
    );
    for foreign in [
        "recommend-target.notes.json".to_string(),
        format!("recommend-target.{version}.{}.json", &tree[..39]),
        format!("recommend-target.{version}.{}.json", tree.to_uppercase()),
        format!("recommend-target.0{version}.{tree}.json"),
        format!("recommend-target.+{version}.{tree}.json"),
        format!("recommend-target.{version}.{tree}.json.bak"),
        format!("recommend-target.{version}.{tree}.json.tmp-"),
        format!("recommend-target.{version}.{tree}.json.tmp-12x"),
        format!("recommend-target.99999999999.{tree}.json"),
        format!("Recommend-target.{version}.{tree}.json"),
        format!("recommend-target.{version}.{tree}.jsonl"),
    ] {
        assert_eq!(parse_target_symbol_cache_name(&foreign), None, "{foreign}");
    }
}

#[test]
fn target_symbol_cache_pruning_is_scoped_by_ownership_version_and_last_use() {
    let dir = TempDir::new().expect("dir");
    let dir = dir.path();
    let version = crate::EXTRACTOR_VERSION;
    let tree = |n: usize| format!("{n:040x}");
    let now = SystemTime::now();
    let make = |name: String, age_secs: u64| {
        let path = dir.join(name);
        fs::write(&path, b"{}").expect("write");
        set_mtime(&path, now - Duration::from_secs(age_secs));
        path
    };
    // Current-version entries: the kept (just written) one plus four others
    // whose last use differs from their names' order.
    let kept = make(format!("recommend-target.{version}.{}.json", tree(0)), 0);
    let used_recently = make(format!("recommend-target.{version}.{}.json", tree(1)), 10);
    let used_second = make(format!("recommend-target.{version}.{}.json", tree(4)), 20);
    let used_third = make(format!("recommend-target.{version}.{}.json", tree(2)), 30);
    let least_recent = make(format!("recommend-target.{version}.{}.json", tree(3)), 40);
    let older_version = make(
        format!("recommend-target.{}.{}.json", version - 1, tree(5)),
        0,
    );
    let newer_version = make(
        format!("recommend-target.{}.{}.json", version + 1, tree(6)),
        4_000,
    );
    // Temp files: an own-pid writer in progress, a crashed writer, and an
    // hour-old one whose pid cannot be trusted.
    let own_pid = std::process::id();
    let in_flight = make(
        format!("recommend-target.{version}.{}.json.tmp-{own_pid}", tree(7)),
        120,
    );
    let mut child = std::process::Command::new("true").spawn().expect("spawn");
    let dead_pid = child.id();
    child.wait().expect("reap");
    let crashed = make(
        format!("recommend-target.{version}.{}.json.tmp-{dead_pid}", tree(8)),
        120,
    );
    let fresh_crash = make(
        format!("recommend-target.{version}.{}.json.tmp-{dead_pid}", tree(9)),
        1,
    );
    let hour_old = make(
        format!("recommend-target.{version}.{}.json.tmp-{own_pid}", tree(10)),
        2 * 60 * 60,
    );
    // Names outside the grammar are never touched, however old.
    let foreign = [
        make("recommend-target.notes.json".to_string(), 99_999),
        make(
            format!("recommend-target.{}.{}.json.bak", version - 1, tree(11)),
            99_999,
        ),
        make("graph.owned.json".to_string(), 99_999),
    ];

    prune_target_symbol_caches(dir, &kept);

    for path in [
        &kept,
        &used_recently,
        &used_second,
        &used_third,
        &newer_version,
        &in_flight,
        &fresh_crash,
    ] {
        assert!(path.exists(), "kept {}", path.display());
    }
    for path in [&least_recent, &older_version, &hour_old] {
        assert!(!path.exists(), "pruned {}", path.display());
    }
    for path in &foreign {
        assert!(path.exists(), "foreign {}", path.display());
    }
    // Without /proc a pid's liveness is unknown, so the crashed writer's
    // file ages out instead of being removed early.
    assert_eq!(
        crashed.exists(),
        !Path::new("/proc/self").exists(),
        "dead-pid temp file"
    );
}

#[test]
fn git_only_commit_text_ranks_touched_files_in_live_mode_only() {
    let fixture = fixture_repo();
    let root = fixture.path();
    fs::write(
        root.join("src/limits.rs"),
        "pub fn burst_window() -> u32 { 10 }\n",
    )
    .expect("limits");
    fs::write(
        root.join("src/quota_report.rs"),
        "pub fn tenant_quota_report() {}\n",
    )
    .expect("report");
    fs::write(root.join("src/other.rs"), "pub fn other() {}\n").expect("other");
    commit_all(root, "base");
    fs::write(
        root.join("src/limits.rs"),
        "pub fn burst_window() -> u32 { 20 }\n",
    )
    .expect("edit limits");
    git(
        root,
        &[
            "commit",
            "-am",
            "fix: Raise tenant quota ceiling for burst traffic [ORB-4242]\n\nThe ceiling now tracks the burst window.\n\nCo-Authored-By: Someone <someone@example.invalid>",
        ],
    );
    fs::write(root.join("src/other.rs"), "pub fn other() -> u8 { 1 }\n").expect("edit other");
    commit_all(root, "chore: unrelated cleanup");
    let target = head(root);
    let index = HistoryIndex::open(root, "main").expect("history");
    index.sync(Some(10)).expect("sync");

    let engine = RecommendationEngine::open(root, "main").expect("engine");
    let query = "raise tenant quota ceiling burst traffic";
    let live = engine
        .recommend(&query_request(query, &target, None))
        .expect("live");
    let limits = live
        .recommendations
        .iter()
        .find(|item| item.selector == "file:src/limits.rs")
        .expect("commit-text destination");
    let commit_reason = limits
        .reasons
        .iter()
        .find(|reason| reason.kind == "historical_change_commit_text")
        .expect("labelled commit-text reason");
    assert!(commit_reason.explanation.contains("post_execution"));
    assert!(commit_reason.explanation.contains("ORB-4242"));
    assert!(
        limits.supporting_task_ids.is_empty(),
        "cited IDs are hints, not task evidence"
    );
    let report = live
        .recommendations
        .iter()
        .find(|item| item.selector == "file:src/quota_report.rs")
        .expect("lexical-only neighbour");
    assert!(
        report
            .reasons
            .iter()
            .all(|reason| reason.kind == "current_tree_lexical")
    );
    assert!(limits.rank < report.rank, "{:?}", live.recommendations);
    assert!(live.recommendations.iter().all(|item| {
        item.selector != "file:src/other.rs"
            || item
                .reasons
                .iter()
                .all(|reason| reason.kind != "historical_change_commit_text")
    }));
    assert!(
        live.fallbacks
            .iter()
            .any(|fallback| fallback.kind == "git_commit_text_used")
    );

    // A live task-ID request never reads commit text: the target task's
    // own Git-only delivery carries no task association, so its message
    // could describe the change being predicted. The snapshot's text
    // matches the commit message exactly, so only the free-text guard
    // excludes it.
    let source = Provenance {
        system: "test".to_string(),
        record_id: Some("QUOTA-1".to_string()),
    };
    let known = |value: &str| TemporalFact {
        status: TemporalStatus::Known,
        timestamp: Some(value.to_string()),
        source: source.clone(),
    };
    let by_task = engine
        .recommend(&RecommendationRequest {
            input: RecommendationInput::TaskId("QUOTA-1".to_string()),
            level: RecommendationLevel::File,
            variant: RecommendationVariant::Combined,
            limit: Some(10),
            target_revision: Some(target.clone()),
            cutoff: None,
            task_snapshot: Some(TaskAssociation {
                task_id: "QUOTA-1".to_string(),
                title: "Raise tenant quota ceiling for burst traffic".to_string(),
                description: "Raise tenant quota ceiling burst traffic".to_string(),
                acceptance_criteria: Vec::new(),
                source: source.clone(),
                created_at: known("unix:1"),
                snapshot_available_at: known("unix:2"),
                text_availability: TaskTextAvailability::PostExecution,
                captured_at: "unix:3".to_string(),
            }),
            hybrid_hits: Vec::new(),
            commit_text_weight: None,
            commit_text_exponent: None,
        })
        .expect("task-ID request");
    assert!(
        by_task.recommendations.iter().all(|item| item
            .reasons
            .iter()
            .all(|reason| reason.kind != "historical_change_commit_text")),
        "{:?}",
        by_task.recommendations
    );
    assert!(
        by_task
            .fallbacks
            .iter()
            .all(|fallback| fallback.kind != "git_commit_text_used")
    );

    // Task-search-only ranks task text only.
    let mut task_only = query_request(query, &target, None);
    task_only.variant = RecommendationVariant::TaskSearchOnly;
    assert!(
        engine
            .recommend(&task_only)
            .expect("task-search-only")
            .recommendations
            .is_empty()
    );

    // Strict replay and task-ID requests never score commit text, even
    // for a delivery that reaches ranking (strict eligibility would
    // already exclude Git-only landing times).
    let repo = Repository::open(root).expect("repo");
    let target_oid = Oid::from_str(target.as_str()).expect("oid");
    let tree = TargetTree::load(&repo, target_oid).expect("tree");
    let deliveries = index
        .deliveries()
        .expect("deliveries")
        .into_iter()
        .map(|change| EligibleDelivery {
            source_delivery_ids: BTreeSet::from([change.delivery.delivery_id.clone()]),
            change,
        })
        .collect::<Vec<_>>();
    let cutoff = parse_timestamp("test", "unix:99999999999").expect("cutoff");
    for (strict_replay, free_text_query) in [(false, true), (true, true), (false, false)] {
        let mut lineage = PathLineage::new(&repo, target_oid, &[], Vec::new()).expect("lineage");
        let hybrid = BTreeMap::new();
        let scored = score_history(
            &repo,
            &tree,
            &mut lineage,
            deliveries.as_slice(),
            &HistoryScoreRequest {
                query,
                hybrid: &hybrid,
                level: RecommendationLevel::File,
                cutoff: &cutoff,
                strict_replay,
                variant: RecommendationVariant::Combined,
                free_text_query,
                commit_text: CommitTextPolicy {
                    enabled: true,
                    weight: COMMIT_TEXT_WEIGHT,
                    exponent: COMMIT_TEXT_EXPONENT,
                },
            },
        )
        .expect("score");
        let used = scored.values().any(|entry| {
            entry
                .reasons
                .iter()
                .any(|reason| reason.kind == "historical_change_commit_text")
        });
        assert_eq!(
            used,
            free_text_query && !strict_replay,
            "strict_replay={strict_replay} free_text_query={free_text_query}"
        );
    }
    let replay = engine
        .recommend(&query_request(query, &target, Some("unix:99999999999")))
        .expect("replay");
    assert!(replay.recommendations.iter().all(|item| {
        item.reasons
            .iter()
            .all(|reason| reason.kind != "historical_change_commit_text")
    }));

    let mut disabled = query_request(query, &target, None);
    disabled.commit_text_weight = Some(0.0);
    let disabled = engine.recommend(&disabled).expect("weight zero");
    assert!(
        disabled.recommendations.iter().all(|item| {
            item.reasons
                .iter()
                .all(|reason| reason.kind != "historical_change_commit_text")
        }),
        "weight 0 disables commit text"
    );
    let mut linear = query_request(query, &target, None);
    linear.commit_text_exponent = Some(1.0);
    let linear = engine.recommend(&linear).expect("linear exponent");
    assert!(linear.recommendations.iter().any(|item| {
        item.reasons.iter().any(|reason| {
            reason.kind == "historical_change_commit_text"
                && reason.explanation.contains("exponent 1.000")
        })
    }));
}

/// The exhaustive (destination × seed × delivery) association pass that
/// `add_associations` replaced, kept as the reference it must match.
fn reference_associations(
    rows: &[DeliveryLocations],
    prevalence: &BTreeMap<String, usize>,
    history_total: usize,
    direct: &BTreeMap<String, f64>,
) -> BTreeMap<String, (f64, RecommendationAssociation, String)> {
    let mut out = BTreeMap::new();
    for (destination, destination_count) in prevalence {
        let mut best: Option<(f64, RecommendationAssociation, String)> = None;
        for (source, source_score) in direct.iter().filter(|(_, score)| **score > 0.0) {
            if source == destination {
                continue;
            }
            let source_count = *prevalence.get(source).unwrap_or(&0);
            let supporting = rows
                .iter()
                .filter(|row| {
                    row.locations.contains_key(source) && row.locations.contains_key(destination)
                })
                .collect::<Vec<_>>();
            if source_count == 0 || supporting.is_empty() {
                continue;
            }
            let ids = supporting
                .iter()
                .flat_map(|row| row.source_delivery_ids.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(", ");
            let confidence = supporting.len() as f64 / source_count as f64;
            let lift = confidence / (*destination_count as f64 / history_total as f64);
            let contribution = source_score * confidence * lift.ln_1p() * 0.25;
            let association = RecommendationAssociation {
                from_selector: source.clone(),
                support: supporting.len(),
                source_count,
                destination_count: *destination_count,
                confidence,
                lift,
            };
            if best.as_ref().is_none_or(|(score, current, _)| {
                contribution > *score
                    || (contribution == *score && association.from_selector < current.from_selector)
            }) {
                best = Some((contribution, association, ids));
            }
        }
        if let Some(found) = best {
            out.insert(destination.clone(), found);
        }
    }
    out
}

fn association_rows(count: usize) -> Vec<DeliveryLocations> {
    // Deterministic overlapping deliveries over 23 selectors, with equal
    // direct strengths to exercise the selector tie-break.
    (0..count)
        .map(|index| {
            let members = (0..23)
                .filter(|selector| {
                    (index * 7 + selector * 3) % 5 < 2 || selector % 11 == index % 11
                })
                .map(|selector| format!("file:s{selector:02}.rs"))
                .collect::<Vec<_>>();
            let mut delivery = row(&members.iter().map(String::as_str).collect::<Vec<_>>());
            delivery.delivery_id = format!("d{index}");
            delivery.source_delivery_ids =
                BTreeSet::from([format!("d{index}"), format!("alias-{}", index % 3)]);
            delivery
        })
        .collect()
}

#[test]
fn indexed_associations_match_the_exhaustive_reference() {
    let rows = association_rows(40);
    let mut prevalence = BTreeMap::new();
    for delivery in &rows {
        for selector in delivery.locations.keys() {
            *prevalence.entry(selector.clone()).or_insert(0) += 1;
        }
    }
    let direct = prevalence
        .keys()
        .enumerate()
        .filter(|(index, _)| index % 4 != 3)
        .map(|(index, selector)| {
            (
                selector.clone(),
                [0.3, 0.3, 0.0, 0.7][index % 4] + 0.01 * (index % 2) as f64,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut scored = BTreeMap::new();
    add_associations(&rows, &prevalence, rows.len(), &direct, &mut scored);
    let expected = reference_associations(&rows, &prevalence, rows.len(), &direct);
    assert!(!expected.is_empty());
    assert_eq!(scored.len(), expected.len());
    for (destination, (contribution, association, ids)) in expected {
        let entry = scored.get(&destination).expect("destination scored");
        assert_eq!(
            entry.score.to_bits(),
            contribution.to_bits(),
            "{destination}"
        );
        assert_eq!(
            entry.association.as_ref(),
            Some(&association),
            "{destination}"
        );
        assert_eq!(entry.reasons.len(), 1);
        assert!(
            entry.reasons[0]
                .explanation
                .ends_with(&format!("source delivery IDs: {ids}"))
        );
    }
}

#[test]
fn associations_consider_only_the_strongest_seeds() {
    // Every seed co-occurs with its own destination only; the weakest seed
    // falls beyond the limit, so its destination gets no association.
    let rows = (0..=ASSOCIATION_SEED_LIMIT)
        .map(|index| {
            let seed = format!("file:seed{index:04}.rs");
            let destination = format!("file:dest{index:04}.rs");
            row(&[seed.as_str(), destination.as_str()])
        })
        .collect::<Vec<_>>();
    let prevalence = rows
        .iter()
        .flat_map(|delivery| delivery.locations.keys().cloned())
        .map(|selector| (selector, 1))
        .collect::<BTreeMap<_, _>>();
    let direct = (0..=ASSOCIATION_SEED_LIMIT)
        .map(|index| (format!("file:seed{index:04}.rs"), 1.0 / (index + 1) as f64))
        .collect::<BTreeMap<_, _>>();
    let mut scored = BTreeMap::new();
    add_associations(&rows, &prevalence, rows.len(), &direct, &mut scored);
    assert!(scored.contains_key("file:dest0000.rs"));
    assert!(scored.contains_key(&format!("file:dest{:04}.rs", ASSOCIATION_SEED_LIMIT - 1)));
    assert!(!scored.contains_key(&format!("file:dest{ASSOCIATION_SEED_LIMIT:04}.rs")));
}

#[test]
fn commit_text_drops_trailers_bounds_length_and_parses_cited_ids() {
    let policy = CommitTextPolicy {
        enabled: true,
        weight: COMMIT_TEXT_WEIGHT,
        exponent: COMMIT_TEXT_EXPONENT,
    };
    assert!((commit_text_relevance(1.0, policy) - COMMIT_TEXT_WEIGHT).abs() < f64::EPSILON);
    assert!(commit_text_relevance(0.5, policy) < 0.5 * commit_text_relevance(1.0, policy));
    assert!(is_known_trailer_line(
        "Co-Authored-By: A <a@example.invalid>"
    ));
    assert!(is_known_trailer_line(
        "Signed-off-by: X <x@example.invalid>"
    ));
    assert!(is_known_trailer_line("Orbit-Run: jrun-1"));
    assert!(is_known_trailer_line("planned-by: claude"));
    assert!(!is_known_trailer_line("Scope: auto-task delete"));
    assert!(!is_known_trailer_line("fix: repair the parser cache"));
    assert!(!is_known_trailer_line("Orbit-: empty suffix"));
    assert!(!is_known_trailer_line("Co-Authored-By:"));
    assert!(!is_known_trailer_line("plain prose without a key"));
    assert_eq!(
        cited_task_ids("feat: x [ORB-13007] (#2612) [DANI-1] [not-an-id] [ORB-]"),
        vec!["DANI-1".to_string(), "ORB-13007".to_string()]
    );
    let fixture = fixture_repo();
    let root = fixture.path();
    fs::write(root.join("src/a.rs"), "fn a() {}\n").expect("a");
    commit_all(root, "base");
    fs::write(root.join("src/a.rs"), "fn a() { }\n").expect("edit");
    let long_body = "é".repeat(COMMIT_TEXT_MAX_BYTES);
    git(
        root,
        &[
            "commit",
            "-am",
            &format!("subject line\n\n{long_body}\n\nSigned-off-by: X <x@example.invalid>"),
        ],
    );
    let repo = Repository::open(root).expect("repo");
    let oid = Oid::from_str(head(root).as_str()).expect("oid");
    let text = commit_message_text(&repo, oid).expect("text");
    assert!(text.starts_with("subject line"));
    assert!(text.len() <= COMMIT_TEXT_MAX_BYTES);
    assert!(!text.contains("Signed-off-by"));

    // A final paragraph of message content that merely looks like
    // trailers is kept; only the known trailer lines are removed.
    fs::write(root.join("src/a.rs"), "fn a() {  }\n").expect("edit again");
    git(
        root,
        &[
            "commit",
            "-am",
            "Squash of two changes (#12)\n\nScope: auto-task delete\nfix: keep opt-out durable\nCo-authored-by: A <a@example.invalid>\nPlanned-by: claude",
        ],
    );
    let oid = Oid::from_str(head(root).as_str()).expect("oid");
    assert_eq!(
        commit_message_text(&repo, oid).as_deref(),
        Some("Squash of two changes (#12)\n\nScope: auto-task delete\nfix: keep opt-out durable")
    );
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
        commit_text: None,
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
    let output = crate::tests::support::git_command(root)
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
    let output = crate::tests::support::git_command(root)
        .args(args)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

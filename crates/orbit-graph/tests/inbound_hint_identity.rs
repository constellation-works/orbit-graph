//! Inbound queries treat row IDs as a hint and qualified names as identity.

#![allow(clippy::expect_used)]
#![allow(
    clippy::disallowed_methods,
    reason = "fixtures use fs::write, which the production lint forbids"
)]

use orbit_graph::{
    Graph, GraphError, ImpactDirection, RefConfidence, RefKind, RefOpts, Selector, SyncPolicy,
};
use rusqlite::{Connection, params};

struct Fixture {
    graph: Graph,
    conn: Connection,
    target: i64,
    duplicate: i64,
    unrelated: i64,
    _root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary worktree");
        let mut options = git2::RepositoryInitOptions::new();
        options.initial_head("main");
        let repo = git2::Repository::init_opts(root.path(), &options).expect("discovery boundary");
        repo.config()
            .and_then(|config| config.open_level(git2::ConfigLevel::Local))
            .and_then(|mut config| {
                config.set_str(
                    "core.excludesFile",
                    if cfg!(windows) { "NUL" } else { "/dev/null" },
                )
            })
            .expect("isolate host excludes");
        let graph = Graph::open(root.path(), SyncPolicy::Manual).expect("open graph");
        let conn = Connection::open(graph.db_path().path()).expect("open fixture database");
        for file in ["target.rs", "duplicate.rs", "unrelated.rs", "caller.rs"] {
            let content = "fn caller() { Target(); }\n";
            std::fs::write(root.path().join(file), content).expect("write source");
            conn.execute(
                "INSERT INTO files VALUES (?1, x'00', 1, 'rust', ?2, 2)",
                params![
                    file,
                    i64::try_from(content.len()).expect("source length fits")
                ],
            )
            .expect("insert file");
        }
        let target = insert_symbol(&conn, "target.rs", "Target", "crate::Target");
        let duplicate = insert_symbol(&conn, "duplicate.rs", "Target", "crate::Target");
        let unrelated = insert_symbol(&conn, "unrelated.rs", "Other", "crate::Other");
        insert_symbol(&conn, "caller.rs", "caller", "crate::caller");
        Self {
            graph,
            conn,
            target,
            duplicate,
            unrelated,
            _root: root,
        }
    }

    fn insert_ref(&self, hint: Option<i64>) {
        self.conn
            .execute(
                "INSERT INTO refs (from_file, from_span_start, from_span_end, target_name,
                target_qualified, target_symbol_hint, kind, confidence)
             VALUES ('caller.rs', 14, 20, 'Target', 'crate::Target', ?1, 'call', 'exact')",
                [hint],
            )
            .expect("insert reference");
    }

    fn assert_inbound(&self, file: &str, name: &str, expected: usize) {
        let selector = Selector::Symbol {
            path: file.to_string(),
            symbol: name.to_string(),
            kind: "function".to_string(),
        };
        for confidence in [RefConfidence::Exact, RefConfidence::FuzzyName] {
            for kind in [None, Some(RefKind::Call)] {
                let refs = self
                    .graph
                    .refs(&selector, &RefOpts { confidence, kind })
                    .expect("query inbound refs");
                assert_eq!(
                    refs.refs.len(),
                    expected,
                    "refs {file}, {confidence:?}, {kind:?}"
                );
                assert!(!refs.fallback_used, "no fuzzy reference exists");
            }
            for direction in [ImpactDirection::Inbound, ImpactDirection::Both] {
                let impact = self
                    .graph
                    .impact_with_direction(&selector, 1, confidence, direction)
                    .expect("query inbound impact");
                assert_eq!(
                    impact.visited_nodes, expected,
                    "impact {file}, {confidence:?}, {direction:?}"
                );
                if expected != 0 {
                    assert_eq!(impact.touched[0].file.as_deref(), Some("caller.rs"));
                }
                assert!(!impact.fallback_used, "no fuzzy reference exists");
            }
        }
    }
}

fn insert_symbol(conn: &Connection, file: &str, name: &str, qualified: &str) -> i64 {
    conn.execute(
        "INSERT INTO symbols (file_path, name, qualified, kind, span_start, span_end)
         VALUES (?1, ?2, ?3, 'function', 0, 25)",
        params![file, name, qualified],
    )
    .expect("insert symbol");
    conn.last_insert_rowid()
}

#[test]
fn inbound_queries_recover_missing_target_hints() {
    let fixture = Fixture::new();
    fixture.insert_ref(Some(999));
    fixture.assert_inbound("target.rs", "Target", 1);
}

#[test]
fn inbound_queries_recover_reused_target_hints_without_following_unrelated_symbols() {
    let fixture = Fixture::new();
    fixture.insert_ref(Some(fixture.unrelated));
    fixture.assert_inbound("target.rs", "Target", 1);
    fixture.assert_inbound("unrelated.rs", "Other", 0);
}

#[test]
fn inbound_queries_use_valid_target_hints_to_distinguish_duplicate_qualified_names() {
    let fixture = Fixture::new();
    fixture.insert_ref(Some(fixture.duplicate));
    fixture.assert_inbound("target.rs", "Target", 0);
    fixture.assert_inbound("duplicate.rs", "Target", 1);
}

#[test]
fn inbound_queries_accept_matching_and_absent_target_hints() {
    for hinted in [false, true] {
        let fixture = Fixture::new();
        fixture.insert_ref(hinted.then_some(fixture.target));
        fixture.assert_inbound("target.rs", "Target", 1);
    }
}

#[test]
fn corrupt_stored_reference_confidence_is_invalid_data_across_queries() {
    let fixture = Fixture::new();
    fixture.insert_ref(Some(fixture.target));
    fixture
        .conn
        .execute("UPDATE refs SET confidence = 'corrupt-confidence'", [])
        .expect("corrupt stored confidence");
    fixture
        .conn
        .execute(
            "INSERT INTO commands SELECT 'caller', file_path, span_start, id
         FROM symbols WHERE qualified = 'crate::caller'",
            [],
        )
        .expect("insert command handler");
    let target = Selector::Symbol {
        path: "target.rs".to_string(),
        symbol: "Target".to_string(),
        kind: "function".to_string(),
    };
    let caller = Selector::Symbol {
        path: "caller.rs".to_string(),
        symbol: "caller".to_string(),
        kind: "function".to_string(),
    };
    for error in [
        fixture
            .graph
            .refs(&target, &RefOpts::default())
            .expect_err("reject confidence in refs"),
        fixture
            .graph
            .impact_with_direction(&target, 1, RefConfidence::Exact, ImpactDirection::Inbound)
            .expect_err("reject confidence in impact"),
        fixture
            .graph
            .callees(&caller)
            .expect_err("reject confidence in callees"),
        fixture
            .graph
            .trace("caller", 1, RefConfidence::Exact)
            .expect_err("reject confidence in trace"),
    ] {
        assert!(matches!(error, GraphError::InvalidData { .. }), "{error:?}");
    }
}

#[test]
fn corrupt_stored_reference_kind_is_invalid_data_in_inbound_queries() {
    let fixture = Fixture::new();
    fixture.insert_ref(Some(fixture.target));
    fixture
        .conn
        .execute("UPDATE refs SET kind = 'corrupt-kind'", [])
        .expect("corrupt stored kind");
    let target = Selector::Symbol {
        path: "target.rs".to_string(),
        symbol: "Target".to_string(),
        kind: "function".to_string(),
    };
    for error in [
        fixture
            .graph
            .refs(&target, &RefOpts::default())
            .expect_err("reject kind in refs"),
        fixture
            .graph
            .impact_with_direction(&target, 1, RefConfidence::Exact, ImpactDirection::Inbound)
            .expect_err("reject kind in impact"),
    ] {
        assert!(matches!(error, GraphError::InvalidData { .. }), "{error:?}");
    }
}

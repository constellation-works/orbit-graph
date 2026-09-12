use rusqlite::{Connection, params};

use crate::query::tests::support::{
    TestWorktree, assert_json_matches_fixture, insert_file, insert_symbol, open_connection,
    open_graph,
};
use crate::{CalleeEdge, CalleeOpts, RefConfidence, RefKind, Selector, SyncPolicy};

#[test]
fn callees_result_shape_matches_golden_fixture() {
    let result = vec![
        CalleeEdge {
            target_name: "foo".to_string(),
            target_qualified: Some("crate::foo".to_string()),
            confidence: RefConfidence::Exact,
            line: 2,
        },
        CalleeEdge {
            target_name: "dynamic".to_string(),
            target_qualified: None,
            confidence: RefConfidence::FuzzyName,
            line: 4,
        },
    ];

    assert_json_matches_fixture(&result, include_str!("callees.golden.json"));
}

#[test]
fn mixed_confidence_edges_return_source_lines() {
    let worktree = TestWorktree::new("callees-mixed");
    let source = "fn caller() {\n    exact_call();\n    fuzzy_call();\n    imported_call();\n}\n";
    worktree.write("src/lib.rs", source);
    let graph = open_graph(&worktree, SyncPolicy::Manual);
    let conn = open_connection(&worktree);
    seed_caller(&conn, source);

    insert_call_ref(
        &conn,
        source.find("exact_call").expect("exact call span"),
        "exact_call",
        Some("crate::exact_call"),
        "exact",
    );
    insert_call_ref(
        &conn,
        source.find("fuzzy_call").expect("fuzzy call span"),
        "fuzzy_call",
        None,
        "fuzzy_name",
    );
    insert_call_ref(
        &conn,
        source.find("imported_call").expect("imported call span"),
        "imported_call",
        Some("other::imported_call"),
        "import_resolved",
    );

    let edges = graph
        .callees(&caller_selector())
        .expect("query mixed callees");

    assert_eq!(
        edges,
        vec![
            CalleeEdge {
                target_name: "exact_call".to_string(),
                target_qualified: Some("crate::exact_call".to_string()),
                confidence: RefConfidence::Exact,
                line: 2,
            },
            CalleeEdge {
                target_name: "fuzzy_call".to_string(),
                target_qualified: None,
                confidence: RefConfidence::FuzzyName,
                line: 3,
            },
            CalleeEdge {
                target_name: "imported_call".to_string(),
                target_qualified: Some("other::imported_call".to_string()),
                confidence: RefConfidence::ImportResolved,
                line: 4,
            },
        ]
    );
}

#[test]
fn options_filter_by_confidence_and_kind_without_changing_unfiltered_results() {
    let worktree = TestWorktree::new("callees-options");
    let source = "fn caller() {\n    exact_call();\n    fuzzy_call();\n}\n";
    worktree.write("src/lib.rs", source);
    let graph = open_graph(&worktree, SyncPolicy::Manual);
    let conn = open_connection(&worktree);
    seed_caller(&conn, source);
    insert_call_ref(
        &conn,
        source.find("exact_call").expect("exact"),
        "exact_call",
        Some("crate::exact_call"),
        "exact",
    );
    insert_call_ref(
        &conn,
        source.find("fuzzy_call").expect("fuzzy"),
        "fuzzy_call",
        None,
        "fuzzy_name",
    );

    let unfiltered = graph
        .callees(&caller_selector())
        .expect("unfiltered callees");
    assert_eq!(unfiltered.len(), 2);
    let exact = graph
        .callees_with_options(
            &caller_selector(),
            &CalleeOpts {
                confidence: RefConfidence::Exact,
                kind: Some(RefKind::Call),
            },
        )
        .expect("filtered callees");
    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0].confidence, RefConfidence::Exact);
    let wrong_kind = graph
        .callees_with_options(
            &caller_selector(),
            &CalleeOpts {
                confidence: RefConfidence::FuzzyName,
                kind: Some(RefKind::Type),
            },
        )
        .expect("kind-filtered callees");
    assert!(wrong_kind.is_empty());
}

#[test]
fn leaf_symbol_with_no_call_refs_returns_empty_edges() {
    let worktree = TestWorktree::new("callees-leaf");
    let source = "fn caller() {\n}\n";
    worktree.write("src/lib.rs", source);
    let graph = open_graph(&worktree, SyncPolicy::Manual);
    let conn = open_connection(&worktree);
    seed_caller(&conn, source);

    let edges = graph
        .callees(&caller_selector())
        .expect("query leaf callees");

    assert!(edges.is_empty());
}

#[test]
fn null_qualified_edges_are_preserved_and_ordered_by_span() {
    let worktree = TestWorktree::new("callees-null-qualified");
    let source = "fn caller() {\n    first();\n    second();\n}\n";
    worktree.write("src/lib.rs", source);
    let graph = open_graph(&worktree, SyncPolicy::Manual);
    let conn = open_connection(&worktree);
    seed_caller(&conn, source);
    insert_call_ref(
        &conn,
        source.find("second").expect("second span"),
        "second",
        None,
        "fuzzy_name",
    );
    insert_call_ref(
        &conn,
        source.find("first").expect("first span"),
        "first",
        None,
        "fuzzy_name",
    );

    let edges = graph
        .callees(&caller_selector())
        .expect("query null-qualified callees");

    let names = edges
        .iter()
        .map(|edge| (edge.target_name.as_str(), edge.target_qualified.as_deref()))
        .collect::<Vec<_>>();
    assert_eq!(names, vec![("first", None), ("second", None)]);
}

fn seed_caller(conn: &Connection, source: &str) {
    insert_file(conn, "src/lib.rs", "rust", source);
    insert_symbol(
        conn,
        "src/lib.rs",
        "caller",
        "crate::caller",
        "function",
        0,
        source.len(),
    );
}

fn caller_selector() -> Selector {
    Selector::Symbol {
        path: "src/lib.rs".to_string(),
        symbol: "caller".to_string(),
        kind: "function".to_string(),
    }
}

fn insert_call_ref(
    conn: &Connection,
    span_start: usize,
    target_name: &str,
    target_qualified: Option<&str>,
    confidence: &str,
) {
    conn.execute(
        "INSERT INTO refs (
            from_file, from_span_start, from_span_end, target_name, target_qualified,
            target_symbol_hint, kind, confidence
         ) VALUES ('src/lib.rs', ?1, ?2, ?3, ?4, NULL, 'call', ?5)",
        params![
            i64::try_from(span_start).expect("span start fits"),
            i64::try_from(span_start + target_name.len()).expect("span end fits"),
            target_name,
            target_qualified,
            confidence
        ],
    )
    .expect("insert call ref");
}

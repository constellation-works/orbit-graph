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
                hide_unresolved: false,
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
                hide_unresolved: false,
            },
        )
        .expect("kind-filtered callees");
    assert!(wrong_kind.is_empty());
}

#[test]
fn hide_unresolved_omits_only_unresolved_calls_without_a_callable_definition() {
    let worktree = TestWorktree::new("callees-hide-unresolved");
    let source = "fn caller() {\n    resolved();\n    local_fn();\n    map_err();\n    Err();\n    map_err();\n}\n";
    worktree.write("src/lib.rs", source);
    let graph = open_graph(&worktree, SyncPolicy::Manual);
    let conn = open_connection(&worktree);
    seed_caller(&conn, source);
    // `local_fn` has an indexed function of that name; `Err` only shares its
    // name with an associated type alias, which is not a call target.
    insert_symbol(
        &conn,
        "src/lib.rs",
        "local_fn",
        "crate::local_fn",
        "function",
        0,
        1,
    );
    insert_symbol(
        &conn,
        "src/lib.rs",
        "Err",
        "<Parser>::Err",
        "type_alias",
        0,
        1,
    );
    insert_call_ref(
        &conn,
        source.find("resolved").expect("resolved"),
        "resolved",
        Some("other::resolved"),
        "import_resolved",
    );
    for name in ["local_fn", "map_err", "Err"] {
        insert_call_ref(
            &conn,
            source.find(name).expect("call span"),
            name,
            None,
            "fuzzy_name",
        );
    }
    insert_call_ref(
        &conn,
        source.rfind("map_err").expect("second map_err"),
        "map_err",
        None,
        "fuzzy_name",
    );

    let hidden = graph
        .callees_report(
            &caller_selector(),
            &CalleeOpts {
                hide_unresolved: true,
                ..CalleeOpts::all()
            },
        )
        .expect("filtered callees report");
    let names = hidden
        .callees
        .iter()
        .map(|edge| edge.target_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["resolved", "local_fn"]);
    assert_eq!(hidden.hidden_unresolved, 3);

    let all = graph
        .callees_report(&caller_selector(), &CalleeOpts::all())
        .expect("unfiltered callees report");
    assert_eq!(all.callees.len(), 5);
    assert_eq!(all.hidden_unresolved, 0);
    assert_eq!(
        graph.callees(&caller_selector()).expect("callees"),
        all.callees,
        "the unfiltered entry point is unchanged"
    );
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

fn populate_callees_fixture(conn: &Connection, file_path: &str) {
    let content = "0123456789\n".repeat(10);
    conn.execute(
        "INSERT INTO files (path, content_hash, mtime_ns, lang, byte_len, extracted_at)
                 VALUES (?1, x'00', 1, 'rust', ?2, 2)",
        params![
            file_path,
            i64::try_from(content.len()).expect("content length fits")
        ],
    )
    .expect("insert file");

    // Outer function span: 0..100
    conn.execute(
                "INSERT INTO symbols (id, file_path, name, qualified, kind, span_start, span_end, signature, parent_symbol)
                 VALUES (1, ?1, 'outer', 'crate::outer', 'function', 0, 100, 'fn outer()', NULL)",
                [file_path],
            )
            .expect("insert outer symbol");

    // Nested inner function span: 20..50 (contained in outer)
    conn.execute(
                "INSERT INTO symbols (id, file_path, name, qualified, kind, span_start, span_end, signature, parent_symbol)
                 VALUES (2, ?1, 'inner', 'crate::outer::inner', 'function', 20, 50, 'fn inner()', 1)",
                [file_path],
            )
            .expect("insert inner symbol");

    // 4 direct calls inside outer but outside inner (spans 10-15, 55-60, 70-75, 80-85)
    let calls = [
        (10, 15, "foo", Some("crate::foo"), "exact"),
        (55, 60, "bar", None, "fuzzy_name"),
        (70, 75, "baz", Some("crate::baz"), "same_module"),
        (80, 85, "quux", Some("other::quux"), "import_resolved"),
    ];
    for (i, (start, end, name, qual, conf)) in calls.iter().enumerate() {
        conn.execute(
                    "INSERT INTO refs (id, from_file, from_span_start, from_span_end, target_name, target_qualified, target_symbol_hint, kind, confidence)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, 'call', ?7)",
                    params![i as i64 + 10, file_path, *start, *end, name, qual, conf],
                )
                .expect("insert direct call ref");
    }

    // 1 nested call inside inner (span 30-35, contained in both)
    conn.execute(
                "INSERT INTO refs (id, from_file, from_span_start, from_span_end, target_name, target_qualified, target_symbol_hint, kind, confidence)
                 VALUES (99, ?1, 30, 35, 'nested_call', 'crate::nested_call', NULL, 'call', 'exact')",
                [file_path],
            )
            .expect("insert nested call ref");
}

#[test]
fn query_callees_function_with_five_call_sites_returns_five_edges_including_nested_via_span_containment()
 {
    let worktree = TestWorktree::new("callees-five");
    let file_path = "src/example.rs";
    worktree.write(file_path, &"0123456789\n".repeat(10));

    let graph = open_graph(&worktree, SyncPolicy::Manual);
    let conn = open_connection(&worktree);
    populate_callees_fixture(&conn, file_path);

    let sel = Selector::Symbol {
        path: file_path.to_string(),
        symbol: "outer".to_string(),
        kind: "function".to_string(),
    };
    let edges: Vec<CalleeEdge> = graph.callees(&sel).expect("callees query");

    assert_eq!(
        edges.len(),
        5,
        "expected 5 callees (4 direct + 1 nested via containment)"
    );

    let names: Vec<_> = edges.iter().map(|e| e.target_name.as_str()).collect();
    assert!(names.contains(&"foo"));
    assert!(names.contains(&"nested_call")); // attributed to outer via span containment
    let nested = edges
        .iter()
        .find(|e| e.target_name == "nested_call")
        .unwrap();
    assert_eq!(nested.confidence, RefConfidence::Exact);
    assert_eq!(nested.line, 3);
}

#[test]
fn query_callees_unknown_symbol_selector_returns_empty_vec_not_error() {
    let worktree = TestWorktree::new("callees-miss");
    worktree.write("src/lib.rs", "pub fn present() {}\n");
    let graph = open_graph(&worktree, SyncPolicy::Manual);

    let sel = Selector::Symbol {
        path: "src/lib.rs".to_string(),
        symbol: "absent".to_string(),
        kind: "function".to_string(),
    };
    let edges: Vec<CalleeEdge> = graph.callees(&sel).expect("callees on missing symbol");
    assert!(edges.is_empty());
}

#[test]
fn query_callees_non_symbol_selector_returns_empty_vec() {
    let worktree = TestWorktree::new("callees-non-sym");
    worktree.write("src/lib.rs", "// empty\n");
    let graph = open_graph(&worktree, SyncPolicy::Manual);

    let file_sel = Selector::File {
        path: "src/lib.rs".to_string(),
    };
    let edges: Vec<CalleeEdge> = graph.callees(&file_sel).expect("callees on file sel");
    assert!(edges.is_empty());

    let dir_sel = Selector::Dir {
        path: "src".to_string(),
    };
    let edges2: Vec<CalleeEdge> = graph.callees(&dir_sel).expect("callees on dir sel");
    assert!(edges2.is_empty());
}

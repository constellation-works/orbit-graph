use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::extract::RawRef;
use rusqlite::{Connection, params};

use crate::sync::pass1::ExtractedFileRefs;
use crate::{
    EXTRACTOR_VERSION, Graph, RefConfidence, RefOpts, Selector, SyncMode, SyncPolicy,
    resolve_db_path,
};

#[test]
fn exact_resolution_prefers_unambiguous_same_file_symbol() {
    let worktree = TestWorktree::new("exact");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write(
        "src/lib.rs",
        r#"
fn helper() {}

fn caller() {
    helper();
}
"#,
    );

    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    let row = call_ref(&conn, "src/lib.rs", "helper");
    assert_eq!(row.target_qualified.as_deref(), Some("helper"));
    assert_eq!(row.confidence, super::CONFIDENCE_EXACT);
    assert!(row.target_symbol_hint.is_some());
}

#[test]
fn import_resolved_resolution_uses_explicit_import() {
    let worktree = TestWorktree::new("import");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write(
        "src/caller.rs",
        r#"
use imported::target;

mod caller {
    fn run() {
        target();
    }
}
"#,
    );
    // L-0050: file paths do not currently contribute Rust module-qualified symbol names.
    worktree.write(
        "src/imported.rs",
        r#"
mod imported {
    pub fn target() {}
}
"#,
    );

    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    let row = call_ref(&conn, "src/caller.rs", "target");
    assert_eq!(row.target_qualified.as_deref(), Some("imported::target"));
    assert_eq!(row.confidence, super::CONFIDENCE_IMPORT_RESOLVED);
    assert!(row.target_symbol_hint.is_some());
}

#[test]
fn qualified_cross_file_resolution_is_exact() {
    let worktree = TestWorktree::new("same-module");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write(
        "src/caller.rs",
        r#"
mod shared {
    fn run() {
        target();
    }
}
"#,
    );
    worktree.write(
        "src/target.rs",
        r#"
mod shared {
    pub fn target() {}
}
"#,
    );

    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    let row = call_ref(&conn, "src/caller.rs", "target");
    assert_eq!(row.target_qualified.as_deref(), Some("shared::target"));
    assert_eq!(row.confidence, super::CONFIDENCE_EXACT);
    assert!(row.target_symbol_hint.is_some());
}

#[test]
fn fuzzy_name_resolution_leaves_target_qualified_and_hint_null() {
    let worktree = TestWorktree::new("fuzzy");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write(
        "src/caller.rs",
        r#"
fn run() {
    duplicate();
}
"#,
    );
    worktree.write("src/left.rs", "pub fn duplicate() {}\n");
    worktree.write("src/right.rs", "pub fn duplicate() {}\n");

    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    let row = call_ref(&conn, "src/caller.rs", "duplicate");
    assert!(row.target_qualified.is_none());
    assert!(row.target_symbol_hint.is_none());
    assert_eq!(row.confidence, super::CONFIDENCE_FUZZY_NAME);
}

#[test]
fn import_resolved_outranks_same_module_candidate() {
    let worktree = TestWorktree::new("import-outranks-same-module");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write(
        "src/caller.rs",
        r#"
use chosen::run;
use shared::*;

mod shared {
    fn caller() {
        run();
    }
}
"#,
    );
    worktree.write(
        "src/same_module.rs",
        r#"
mod shared {
    pub fn run() {}
}
"#,
    );
    worktree.write(
        "src/imported.rs",
        r#"
mod chosen {
    pub fn run() {}
}
"#,
    );

    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    let row = call_ref(&conn, "src/caller.rs", "run");
    assert_eq!(row.target_qualified.as_deref(), Some("chosen::run"));
    assert_eq!(row.confidence, super::CONFIDENCE_IMPORT_RESOLVED);
}

#[test]
fn rust_grouped_super_and_whole_module_imports_resolve() {
    let worktree = TestWorktree::new("rust-import-forms");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write(
        "src/nested/caller.rs",
        "use crate::grouped::{grouped, other};\nuse super::sibling::sibling;\nuse crate::whole::*;\nfn caller() { grouped(); other(); sibling(); whole::whole(); }\n",
    );
    worktree.write("src/grouped.rs", "pub fn grouped() {}\npub fn other() {}\n");
    worktree.write("src/nested/sibling.rs", "pub fn sibling() {}\n");
    worktree.write("src/whole.rs", "pub fn whole() {}\n");
    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    for name in ["grouped", "other", "sibling", "whole"] {
        let row = call_ref(&conn, "src/nested/caller.rs", name);
        assert_eq!(row.confidence, super::CONFIDENCE_IMPORT_RESOLVED, "{name}");
        assert!(row.target_symbol_hint.is_some(), "{name}");
    }
}

#[test]
fn python_from_module_and_aliased_module_imports_resolve() {
    let worktree = TestWorktree::new("python-import-forms");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write(
        "caller.py",
        "from direct import direct\nimport module\nimport package.aliased as alias\ndef caller():\n    direct()\n    module.module_fn()\n    alias.alias_fn()\n",
    );
    worktree.write("direct.py", "def direct():\n    pass\n");
    worktree.write("module.py", "def module_fn():\n    pass\n");
    worktree.write("package/aliased.py", "def alias_fn():\n    pass\n");
    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    for name in ["direct", "module_fn", "alias_fn"] {
        let row = call_ref(&conn, "caller.py", name);
        assert_eq!(row.confidence, super::CONFIDENCE_IMPORT_RESOLVED, "{name}");
        assert!(row.target_symbol_hint.is_some(), "{name}");
    }
}

#[test]
fn ambiguous_explicit_imports_do_not_choose_a_target() {
    let worktree = TestWorktree::new("ambiguous-explicit-imports");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write(
        "src/caller.rs",
        "use crate::left::run;\nuse crate::right::run;\nfn caller() { run(); }\n",
    );
    worktree.write("src/left.rs", "pub fn run() {}\n");
    worktree.write("src/right.rs", "pub fn run() {}\n");
    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    let row = call_ref(&conn, "src/caller.rs", "run");
    assert_ref(&row, None, super::CONFIDENCE_FUZZY_NAME);
}

#[test]
fn fuzzy_refs_are_null_and_non_fuzzy_refs_are_populated_in_three_file_sync() {
    let worktree = TestWorktree::new("three-file-tiers");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write(
        "src/caller.rs",
        r#"
use imported::imported_call;

fn local_call() {}

mod shared {
    fn caller() {
        local_call();
        imported_call();
        module_call();
        ambiguous_call();
    }
}
"#,
    );
    worktree.write(
        "src/defs_one.rs",
        r#"
mod imported {
    pub fn imported_call() {}
}

mod shared {
    pub fn module_call() {}
    pub fn ambiguous_call() {}
}
"#,
    );
    worktree.write(
        "src/defs_two.rs",
        r#"
mod shared {
    pub fn ambiguous_call() {}
}
"#,
    );

    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    let rows = call_refs_by_name(&conn, "src/caller.rs");
    assert_ref(
        rows.get("local_call").expect("local call ref"),
        Some("local_call"),
        super::CONFIDENCE_EXACT,
    );
    assert_ref(
        rows.get("imported_call").expect("imported call ref"),
        Some("imported::imported_call"),
        super::CONFIDENCE_IMPORT_RESOLVED,
    );
    assert_ref(
        rows.get("module_call").expect("same module call ref"),
        Some("shared::module_call"),
        super::CONFIDENCE_EXACT,
    );
    assert_ref(
        rows.get("ambiguous_call").expect("ambiguous call ref"),
        None,
        super::CONFIDENCE_FUZZY_NAME,
    );
}

#[test]
fn duplicate_import_targets_remain_fuzzy_and_unhinted() {
    let worktree = TestWorktree::new("ambiguous-hint");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    drop(graph);
    let conn = open_test_connection(worktree.path());
    insert_file(&conn, "src/caller.rs");
    insert_file(&conn, "src/left.rs");
    insert_file(&conn, "src/right.rs");
    insert_symbol(&conn, "src/left.rs", "target", "dupe::target");
    insert_symbol(&conn, "src/right.rs", "target", "dupe::target");
    insert_import(&conn, "src/caller.rs", "dupe", Some("target"));
    drop(conn);

    super::run(
        graph_db_path(worktree.path()).as_path(),
        SyncMode::Full,
        vec![ExtractedFileRefs {
            file_path: "src/caller.rs".to_string(),
            refs: vec![raw_ref("src/caller.rs", "target")],
        }],
        &super::Definitions::default(),
        None,
        0,
        None,
    )
    .expect("run pass2");

    let conn = open_test_connection(worktree.path());
    let row = call_ref(&conn, "src/caller.rs", "target");
    assert_eq!(row.target_qualified, None);
    assert_eq!(row.confidence, super::CONFIDENCE_FUZZY_NAME);
    assert!(row.target_symbol_hint.is_none());
}

#[test]
fn incremental_sync_rewrites_only_changed_file_refs() {
    let worktree = TestWorktree::new("incremental-preserve");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write(
        "src/a.rs",
        r#"
fn stable() {}

fn caller() {
    stable();
}
"#,
    );
    worktree.write(
        "src/b.rs",
        r#"
fn before() {}

fn caller() {
    before();
}
"#,
    );
    graph.sync(SyncMode::Full).expect("initial sync");

    let conn = open_test_connection(worktree.path());
    let before = refs_for_file(&conn, "src/a.rs");
    drop(conn);

    std::thread::sleep(Duration::from_millis(5));
    worktree.write(
        "src/b.rs",
        r#"
fn after() {}

fn caller() {
    after();
}
"#,
    );
    graph.sync(SyncMode::Auto).expect("incremental sync");

    let conn = open_test_connection(worktree.path());
    assert_eq!(refs_for_file(&conn, "src/a.rs"), before);
}

#[test]
fn pass2_failure_rolls_back_ref_rewrites_and_meta_update() {
    let worktree = TestWorktree::new("rollback");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    drop(graph);
    let conn = open_test_connection(worktree.path());
    insert_file(&conn, "src/a.rs");
    insert_ref_row(&conn, "src/a.rs", "old", Some("old"), 1, "exact");
    drop(conn);

    let result = super::run(
        graph_db_path(worktree.path()).as_path(),
        SyncMode::Full,
        vec![
            ExtractedFileRefs {
                file_path: "src/a.rs".to_string(),
                refs: vec![raw_ref("src/a.rs", "new")],
            },
            ExtractedFileRefs {
                file_path: "src/missing.rs".to_string(),
                refs: vec![raw_ref("src/missing.rs", "missing")],
            },
        ],
        &super::Definitions::default(),
        None,
        0,
        None,
    );

    assert!(result.is_err());
    let conn = open_test_connection(worktree.path());
    let refs = refs_for_file(&conn, "src/a.rs");
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].target_name, "old");
    assert_eq!(meta_value(&conn, "last_full_build_at"), 0);
}

#[test]
fn refs_sharing_a_name_in_one_file_resolve_by_their_own_receiver() {
    // Pass 2 resolves a file's refs once per distinct resolution key. The
    // receiver-bearing call and the plain call share a file and a name, so
    // the key must still keep them apart, and repeated plain calls must all
    // receive the same result.
    let worktree = TestWorktree::new("memo-receiver");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write(
        "scripts/fixture.py",
        r#"
def append(value):
    return value
"#,
    );
    worktree.write(
        "sims/mixed.py",
        r#"
def run():
    rows = []
    rows.append(1)
    append(1)
    append(2)
    return rows
"#,
    );

    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    let appends = refs_for_file(&conn, "sims/mixed.py")
        .into_iter()
        .filter(|row| row.target_name == "append" && row.kind == "call")
        .collect::<Vec<_>>();
    assert_eq!(appends.len(), 3, "{appends:?}");
    let expected_hint = symbol_id(&conn, "scripts/fixture.py", "append");
    let (method, plain) = appends.split_first().expect("method call first");
    assert_eq!(method.target_qualified, None, "{method:?}");
    assert_eq!(method.target_symbol_hint, None, "{method:?}");
    assert_eq!(method.confidence, super::CONFIDENCE_FUZZY_NAME);
    for row in plain {
        assert_eq!(row.target_qualified.as_deref(), Some("append"), "{row:?}");
        assert_eq!(row.target_symbol_hint, Some(expected_hint), "{row:?}");
        assert_eq!(row.confidence, super::CONFIDENCE_SAME_MODULE, "{row:?}");
    }
}

#[test]
fn runtime_invocation_beside_a_same_named_python_call_stays_unresolved() {
    // Through the sync entry point, in both source orders: a program named
    // like a function is never resolved, and the call of that function still
    // resolves, in the same file (`exact`) and from another file of the same
    // module (`same_module`).
    let invocation = "    subprocess.run([\"git\", \"status\"])\n";
    let call = "    git()\n";
    let definition = "def git(*args):\n    return args\n\n\n";
    for (case, local_definition, confidence) in [
        ("same-file", definition, super::CONFIDENCE_EXACT),
        ("same-module", "", super::CONFIDENCE_SAME_MODULE),
    ] {
        let worktree = TestWorktree::new(&format!("runtime-invocation-{case}"));
        let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
        let files = [
            ("tools/invocation_first.py", invocation, call),
            ("tools/call_first.py", call, invocation),
        ];
        for (file, first, second) in files {
            worktree.write(
                file,
                &format!("import subprocess\n\n\n{local_definition}def run():\n{first}{second}"),
            );
        }
        if local_definition.is_empty() {
            worktree.write("tools/helpers.py", definition);
        }

        graph.sync(SyncMode::Full).expect("sync graph");

        let conn = open_test_connection(worktree.path());
        for (file, _, _) in files {
            let definition_file = if local_definition.is_empty() {
                "tools/helpers.py"
            } else {
                file
            };
            assert_runtime_invocation_and_call(&conn, file, definition_file, confidence);
        }
    }
}

#[test]
fn runtime_invocation_and_call_sharing_every_other_key_field_resolve_independently() {
    // Pass 2 resolves a file's refs once per distinct resolution key. Java,
    // Kotlin and C# calls carry no extracted qualified name, so such a call
    // and a runtime invocation of the same program agree on every key field
    // but the kind. Whichever comes first must not decide the other.
    let worktree = TestWorktree::new("runtime-invocation-key");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    drop(graph);
    let files = ["src/InvocationFirst.java", "src/CallFirst.java"];
    let conn = open_test_connection(worktree.path());
    for file in files {
        insert_file(&conn, file);
        insert_symbol(&conn, file, "git", "git");
    }
    drop(conn);

    let invocation = RawRef {
        kind: super::RUNTIME_INVOCATION_KIND.to_string(),
        ..raw_ref(files[0], "git")
    };
    let call = raw_ref(files[0], "git");
    super::run(
        graph_db_path(worktree.path()).as_path(),
        SyncMode::Full,
        vec![
            ExtractedFileRefs {
                file_path: files[0].to_string(),
                refs: vec![invocation.clone(), call.clone()],
            },
            ExtractedFileRefs {
                file_path: files[1].to_string(),
                refs: vec![
                    RawRef {
                        from_file: files[1].to_string(),
                        ..call
                    },
                    RawRef {
                        from_file: files[1].to_string(),
                        ..invocation
                    },
                ],
            },
        ],
        &super::Definitions::default(),
        None,
        0,
        None,
    )
    .expect("run pass2");

    let conn = open_test_connection(worktree.path());
    for file in files {
        assert_runtime_invocation_and_call(&conn, file, file, super::CONFIDENCE_EXACT);
    }
}

fn assert_runtime_invocation_and_call(
    conn: &Connection,
    file: &str,
    definition_file: &str,
    confidence: &str,
) {
    let invocation = ref_by_kind(conn, file, "git", super::RUNTIME_INVOCATION_KIND);
    assert_eq!(invocation.target_qualified, None, "{file}: {invocation:?}");
    assert_eq!(
        invocation.target_symbol_hint, None,
        "{file}: {invocation:?}"
    );
    assert_eq!(
        invocation.confidence,
        super::CONFIDENCE_FUZZY_NAME,
        "{file}: {invocation:?}"
    );

    let call = ref_by_kind(conn, file, "git", "call");
    assert!(
        call.target_qualified.is_some(),
        "{file}: call should resolve: {call:?}"
    );
    assert_eq!(
        call.target_symbol_hint,
        Some(symbol_id(conn, definition_file, "git")),
        "{file}: {call:?}"
    );
    assert_eq!(call.confidence, confidence, "{file}: {call:?}");
}

#[test]
fn allocation_free_module_matching_agrees_with_the_joined_strings() {
    let paths = [
        "", "a", "b", "a::b", "b::a", "x::a::b", "xa::b", "a::bb", "::a", "é::ü", "a::b::c",
    ];
    for target in paths {
        for module in paths {
            for symbol in paths {
                assert_eq!(
                    super::is_module_symbol(target, module, symbol),
                    super::join_module_symbol(module, symbol) == target,
                    "target={target:?} module={module:?} symbol={symbol:?}"
                );
            }
            assert_eq!(
                super::is_module_suffix(target, module),
                target.ends_with(&format!("::{module}")),
                "path={target:?} module={module:?}"
            );
        }
    }
}

#[test]
fn a_rewritten_file_reusing_an_old_id_for_another_symbol_marks_that_id_replaced() {
    fn defined(name: &str, id: i64) -> super::DefinedSymbol {
        super::DefinedSymbol {
            name: name.to_string(),
            qualified: name.to_string(),
            kind: "function".to_string(),
            id,
        }
    }
    // `gamma` was added above `alpha` and took its freed id.
    let old = [defined("alpha", 4), defined("beta", 5)];
    let new = [defined("alpha", 5), defined("beta", 6), defined("gamma", 4)];
    let mut dependents = super::Dependents::default();
    dependents.add_file("src/zdefs.rs", &old, &new);
    assert_eq!(
        dependents.replaced,
        [(4, "alpha".to_string()), (5, "beta".to_string())]
            .into_iter()
            .collect()
    );

    // An id that still names the same, unique definition keeps its hint;
    // one shared by two equal definitions does not, since the ladder orders
    // equal candidates by id.
    let old = [defined("alpha", 4), defined("twin", 5), defined("twin", 6)];
    let new = [defined("alpha", 4), defined("twin", 5), defined("twin", 6)];
    let mut dependents = super::Dependents::default();
    dependents.add_file("src/zdefs.rs", &old, &new);
    assert_eq!(
        dependents.replaced,
        [(5, "twin".to_string()), (6, "twin".to_string())]
            .into_iter()
            .collect()
    );
    assert!(dependents.names.is_empty());
}

fn assert_ref(row: &StoredRef, target_qualified: Option<&str>, confidence: &str) {
    assert_eq!(row.target_qualified.as_deref(), target_qualified);
    assert_eq!(row.confidence, confidence);
    if confidence == super::CONFIDENCE_FUZZY_NAME {
        assert!(row.target_symbol_hint.is_none());
    } else {
        assert!(row.target_qualified.is_some());
    }
}

fn raw_ref(from_file: &str, target_name: &str) -> RawRef {
    RawRef {
        from_file: from_file.to_string(),
        from_span_start: 0,
        from_span_end: target_name.len(),
        target_name: target_name.to_string(),
        target_qualified: None,
        kind: "call".to_string(),
        confidence: super::CONFIDENCE_FUZZY_NAME.to_string(),
        unresolved_receiver: None,
        spelled_path: false,
    }
}

fn insert_file(conn: &Connection, rel: &str) {
    conn.execute(
        "INSERT INTO files (path, content_hash, mtime_ns, lang, byte_len, extracted_at)
         VALUES (?1, x'00', 1, 'rust', 12, 2)",
        params![rel],
    )
    .expect("insert file");
}

fn insert_symbol(conn: &Connection, rel: &str, name: &str, qualified: &str) {
    conn.execute(
        "INSERT INTO symbols (
            file_path, name, qualified, kind, span_start, span_end, signature, parent_symbol
         ) VALUES (?1, ?2, ?3, 'function', 0, 1, NULL, NULL)",
        params![rel, name, qualified],
    )
    .expect("insert symbol");
}

fn insert_import(
    conn: &Connection,
    from_file: &str,
    target_path: &str,
    target_symbol: Option<&str>,
) {
    conn.execute(
        "INSERT INTO imports (from_file, target_path, target_symbol)
         VALUES (?1, ?2, ?3)",
        params![from_file, target_path, target_symbol],
    )
    .expect("insert import");
}

fn insert_ref_row(
    conn: &Connection,
    from_file: &str,
    target_name: &str,
    target_qualified: Option<&str>,
    target_symbol_hint: i64,
    confidence: &str,
) {
    conn.execute(
        "INSERT INTO refs (
            from_file, from_span_start, from_span_end, target_name, target_qualified,
            target_symbol_hint, kind, confidence
         ) VALUES (?1, 0, 1, ?2, ?3, ?4, 'call', ?5)",
        params![
            from_file,
            target_name,
            target_qualified,
            target_symbol_hint,
            confidence
        ],
    )
    .expect("insert ref");
}

fn call_ref(conn: &Connection, from_file: &str, target_name: &str) -> StoredRef {
    let mut rows = query_refs(
        conn,
        "SELECT id, from_file, from_span_start, from_span_end, target_name, target_qualified,
                target_symbol_hint, kind, confidence
         FROM refs
         WHERE from_file = ?1 AND target_name = ?2 AND kind = 'call'
         ORDER BY id",
        params![from_file, target_name],
    );
    assert_eq!(rows.len(), 1, "expected one call ref for {target_name}");
    rows.remove(0)
}

fn call_refs_by_name(conn: &Connection, from_file: &str) -> BTreeMap<String, StoredRef> {
    query_refs(
        conn,
        "SELECT id, from_file, from_span_start, from_span_end, target_name, target_qualified,
                target_symbol_hint, kind, confidence
         FROM refs
         WHERE from_file = ?1 AND kind = 'call'
         ORDER BY target_name, id",
        params![from_file],
    )
    .into_iter()
    .map(|row| (row.target_name.clone(), row))
    .collect()
}

fn ref_by_kind(conn: &Connection, from_file: &str, target_name: &str, kind: &str) -> StoredRef {
    let mut rows = query_refs(
        conn,
        "SELECT id, from_file, from_span_start, from_span_end, target_name, target_qualified,
                target_symbol_hint, kind, confidence
         FROM refs
         WHERE from_file = ?1 AND target_name = ?2 AND kind = ?3
         ORDER BY id",
        params![from_file, target_name, kind],
    );
    assert_eq!(rows.len(), 1, "expected one {kind} ref for {target_name}");
    rows.remove(0)
}

fn symbol_id(conn: &Connection, file_path: &str, qualified: &str) -> i64 {
    conn.query_row(
        "SELECT id FROM symbols WHERE file_path = ?1 AND qualified = ?2",
        params![file_path, qualified],
        |row| row.get(0),
    )
    .expect("read symbol id")
}

fn refs_for_file(conn: &Connection, from_file: &str) -> Vec<StoredRef> {
    query_refs(
        conn,
        "SELECT id, from_file, from_span_start, from_span_end, target_name, target_qualified,
                target_symbol_hint, kind, confidence
         FROM refs
         WHERE from_file = ?1
         ORDER BY id",
        params![from_file],
    )
}

fn query_refs<P>(conn: &Connection, sql: &str, params: P) -> Vec<StoredRef>
where
    P: rusqlite::Params,
{
    conn.prepare(sql)
        .expect("prepare refs query")
        .query_map(params, |row| {
            Ok(StoredRef {
                id: row.get(0)?,
                from_file: row.get(1)?,
                from_span_start: row.get(2)?,
                from_span_end: row.get(3)?,
                target_name: row.get(4)?,
                target_qualified: row.get(5)?,
                target_symbol_hint: row.get(6)?,
                kind: row.get(7)?,
                confidence: row.get(8)?,
            })
        })
        .expect("query refs")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect refs")
}

fn open_test_connection(worktree: &Path) -> Connection {
    let conn = Connection::open(graph_db_path(worktree)).expect("open graph database");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("enable foreign keys");
    conn
}

fn graph_db_path(worktree: &Path) -> PathBuf {
    resolve_db_path(worktree, "HEAD", EXTRACTOR_VERSION)
        .path()
        .to_path_buf()
}

fn meta_value(conn: &Connection, key: &str) -> i64 {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
        row.get::<_, String>(0)
    })
    .expect("read meta value")
    .parse()
    .expect("meta value is integer")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredRef {
    id: i64,
    from_file: String,
    from_span_start: i64,
    from_span_end: i64,
    target_name: String,
    target_qualified: Option<String>,
    target_symbol_hint: Option<i64>,
    kind: String,
    confidence: String,
}

struct TestWorktree {
    path: PathBuf,
}

impl TestWorktree {
    fn new(name: &str) -> Self {
        let mut path = std::env::temp_dir();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos();
        path.push(format!(
            "orbit-graph-pass2-{name}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create test worktree");
        Self { path }
    }

    fn path(&self) -> &Path {
        self.path.as_path()
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.path.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directory");
        }
        fs::write(path, content).expect("write file");
    }
}

impl Drop for TestWorktree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn type_with_impl_blocks_still_resolves_cross_file_references() {
    let worktree = TestWorktree::new("impl-blocks");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    // The enum carries one inherent and one trait impl; both are stored as `impl`
    // symbols named `Complexity`, so they must not defeat ref resolution.
    worktree.write(
        "src/model.rs",
        r#"
pub enum Complexity {
    Low,
    High,
}

pub fn describe(value: Complexity) -> &'static str {
    match value {
        Complexity::Low => "low",
        Complexity::High => "high",
    }
}

impl Complexity {
    pub fn is_low(&self) -> bool {
        matches!(self, Complexity::Low)
    }
}

impl std::fmt::Display for Complexity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(describe(*self))
    }
}
"#,
    );
    worktree.write(
        "src/caller.rs",
        r#"
use crate::model::Complexity;

fn run(value: Complexity) -> bool {
    value.is_low()
}
"#,
    );

    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    let enum_id = symbol_id(&conn, "src/model.rs", "Complexity");
    for kind in ["use", "type"] {
        let row = ref_by_kind(&conn, "src/caller.rs", "Complexity", kind);
        assert_eq!(
            row.target_qualified.as_deref(),
            Some("Complexity"),
            "{kind}"
        );
        assert_eq!(row.confidence, super::CONFIDENCE_IMPORT_RESOLVED, "{kind}");
        assert_eq!(row.target_symbol_hint, Some(enum_id), "{kind}");
    }

    // Same-file references keep pointing at the enum rather than an impl block.
    for row in refs_for_file(&conn, "src/model.rs")
        .into_iter()
        .filter(|row| row.target_name == "Complexity")
    {
        assert_eq!(row.confidence, super::CONFIDENCE_EXACT);
        assert_eq!(row.target_symbol_hint, Some(enum_id));
    }
}

#[test]
fn dispatching_method_call_does_not_resolve_to_the_calling_file() {
    let worktree = TestWorktree::new("method-dispatch");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write(
        "src/exec.rs",
        r#"
pub trait Execute {
    fn execute(self);
}
"#,
    );
    worktree.write(
        "src/list.rs",
        r#"
use crate::exec::Execute;

pub struct ListArgs;

impl Execute for ListArgs {
    fn execute(self) {}
}
"#,
    );
    worktree.write(
        "src/teardown.rs",
        r#"
use crate::exec::Execute;

pub struct TeardownArgs;

impl Execute for TeardownArgs {
    fn execute(self) {}
}
"#,
    );
    // The dispatcher's own method has the same short name as every handler it
    // dispatches to, which is exactly the shape that used to self-resolve.
    worktree.write(
        "src/command.rs",
        r#"
use crate::exec::Execute;

pub struct WorkspaceCommand {
    command: WorkspaceSubcommand,
}

pub enum WorkspaceSubcommand {
    List(ListArgs),
    Teardown(TeardownArgs),
}

impl Execute for WorkspaceCommand {
    fn execute(self) {
        self.log();
        match self.command {
            WorkspaceSubcommand::List(args) => args.execute(),
            WorkspaceSubcommand::Teardown(args) => args.execute(),
        }
    }
}

impl WorkspaceCommand {
    fn log(&self) {}
}
"#,
    );
    worktree.write(
        "src/main.rs",
        "mod command;\nmod exec;\nmod list;\nmod teardown;\n\nfn main() {}\n",
    );

    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    let dispatch_refs = refs_for_file(&conn, "src/command.rs")
        .into_iter()
        .filter(|row| row.kind == "call" && row.target_name == "execute")
        .collect::<Vec<_>>();
    assert_eq!(dispatch_refs.len(), 2, "expected both dispatch lines");
    for row in &dispatch_refs {
        assert_eq!(row.target_qualified, None, "{row:?}");
        assert_eq!(row.target_symbol_hint, None, "{row:?}");
        assert_eq!(row.confidence, super::CONFIDENCE_FUZZY_NAME, "{row:?}");
    }

    // A `self` receiver still names the enclosing type, so the same-file rung
    // keeps resolving it.
    let logged = call_ref(&conn, "src/command.rs", "log");
    assert_eq!(logged.confidence, super::CONFIDENCE_EXACT);
    assert_eq!(
        logged.target_symbol_hint,
        Some(symbol_id(
            &conn,
            "src/command.rs",
            "<WorkspaceCommand>::log"
        ))
    );

    // The dispatcher no longer reports its own dispatch lines as inbound refs.
    let dispatcher = graph
        .refs(
            &Selector::from_str("symbol:src/command.rs#execute:method")
                .expect("parse dispatcher selector"),
            &RefOpts::default(),
        )
        .expect("query dispatcher refs");
    assert!(
        dispatcher.refs.is_empty(),
        "dispatcher references itself: {:?}",
        dispatcher.refs
    );

    // The real handler is reachable from the dispatch site at the confidence
    // the resolver can honestly claim: a name-only match.
    let handler = graph
        .refs(
            &Selector::from_str("symbol:src/teardown.rs#execute:method")
                .expect("parse handler selector"),
            &RefOpts::default(),
        )
        .expect("query handler refs");
    let fallback = handler.fallback.expect("fuzzy fallback for the handler");
    assert_eq!(fallback.confidence, RefConfidence::FuzzyName);
    assert!(
        fallback
            .refs
            .iter()
            .any(|entry| entry.file == "src/command.rs"),
        "dispatch site missing from the handler's fallback: {:?}",
        fallback.refs
    );
}

#[test]
fn impl_self_calls_resolve_to_the_enclosing_type_and_trait_defaults_fall_back() {
    let worktree = TestWorktree::new("impl-self-calls");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write(
        "src/lib.rs",
        r#"
trait DefaultRun {
    fn default_run(&self) {}
}

struct A;
struct B;

impl A {
    fn run(&self) { self.run(); }
    fn helper(&self) { Self::run(self); }
}

impl B {
    fn run(&self) {}
}

impl DefaultRun for A {
    fn call_default(&self) { self.default_run(); }
}
"#,
    );

    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    let runs = refs_for_file(&conn, "src/lib.rs")
        .into_iter()
        .filter(|row| row.kind == "call" && row.target_name == "run")
        .collect::<Vec<_>>();
    assert_eq!(runs.len(), 2, "expected self.run and Self::run");
    for row in runs {
        assert_eq!(row.target_qualified.as_deref(), Some("<A>::run"), "{row:?}");
        assert_eq!(
            row.target_symbol_hint,
            Some(symbol_id(&conn, "src/lib.rs", "<A>::run"))
        );
        assert_eq!(row.confidence, super::CONFIDENCE_EXACT, "{row:?}");
    }

    let fallback = call_ref(&conn, "src/lib.rs", "default_run");
    // There is no `<A>::default_run` inherent method. The qualified attempt
    // therefore falls through to the pre-existing short-name resolution.
    assert_eq!(
        fallback.target_qualified.as_deref(),
        Some("DefaultRun::default_run")
    );
    assert_eq!(fallback.confidence, super::CONFIDENCE_EXACT);
}

#[test]
fn python_attribute_call_does_not_resolve_to_a_same_module_function() {
    let worktree = TestWorktree::new("python-attribute");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write(
        "scripts/fixture.py",
        r#"
def append(value):
    return value
"#,
    );
    // An ordinary list `.append(` call must not be read as a call of the
    // unrelated module-level `append` function.
    worktree.write(
        "sims/method.py",
        r#"
def run():
    rows = []
    rows.append(1)
    return rows
"#,
    );
    // A plain call of the same name is still name-resolvable: only the
    // receiver-bearing form loses the short-name rungs.
    worktree.write(
        "sims/plain.py",
        r#"
def run():
    return append(1)
"#,
    );

    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    let method = call_ref(&conn, "sims/method.py", "append");
    assert_eq!(method.target_qualified, None);
    assert_eq!(method.target_symbol_hint, None);
    assert_eq!(method.confidence, super::CONFIDENCE_FUZZY_NAME);

    let plain = call_ref(&conn, "sims/plain.py", "append");
    assert_eq!(plain.target_qualified.as_deref(), Some("append"));
    assert_eq!(
        plain.target_symbol_hint,
        Some(symbol_id(&conn, "scripts/fixture.py", "append"))
    );
    assert_eq!(plain.confidence, super::CONFIDENCE_SAME_MODULE);
}

fn assert_resolves_to(
    row: &StoredRef,
    conn: &Connection,
    file: &str,
    qualified: &str,
    confidence: &str,
) {
    assert_eq!(row.target_qualified.as_deref(), Some(qualified), "{row:?}");
    assert_eq!(
        row.target_symbol_hint,
        Some(symbol_id(conn, file, qualified)),
        "{row:?}"
    );
    assert_eq!(row.confidence, confidence, "{row:?}");
}

#[test]
fn typed_receiver_and_scoped_type_calls_resolve_to_that_types_member() {
    let worktree = TestWorktree::new("typed-members");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write(
        "src/runtime.rs",
        r#"
pub struct OrbitRuntime;

impl OrbitRuntime {
    pub fn enable_plugin(&self) {}
}
"#,
    );
    // Same member names on other types: a name-only rung would have to guess.
    worktree.write(
        "src/registry.rs",
        r#"
pub struct Registry;

impl Registry {
    pub fn enable_plugin(&self) {}
    pub fn load() -> Self { Registry }
}
"#,
    );
    worktree.write(
        "src/config.rs",
        r#"
pub struct ResolvedConfig;

impl ResolvedConfig {
    pub fn load() -> Self { ResolvedConfig }
}
"#,
    );
    worktree.write(
        "src/cli.rs",
        r#"
use crate::config::ResolvedConfig;
use crate::runtime::OrbitRuntime;

fn load() {}

fn run(runtime: &OrbitRuntime) {
    runtime.enable_plugin();
    let _ = ResolvedConfig::load();
    let _ = std::sync::Mutex::new(());
    let _ = Unknown::load();
}
"#,
    );
    worktree.write(
        "src/lib.rs",
        "mod cli;\nmod config;\nmod registry;\nmod runtime;\n",
    );

    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    assert_resolves_to(
        &call_ref(&conn, "src/cli.rs", "enable_plugin"),
        &conn,
        "src/runtime.rs",
        "<OrbitRuntime>::enable_plugin",
        super::CONFIDENCE_EXACT,
    );

    let loads = refs_for_file(&conn, "src/cli.rs")
        .into_iter()
        .filter(|row| row.kind == "call" && row.target_name == "load")
        .collect::<Vec<_>>();
    assert_eq!(loads.len(), 2, "{loads:?}");
    assert_resolves_to(
        &loads[0],
        &conn,
        "src/config.rs",
        "<ResolvedConfig>::load",
        super::CONFIDENCE_EXACT,
    );
    // `Unknown::load()` can only name `Unknown`'s member, which is not
    // indexed: it must not fall back to the same-file `load` function.
    assert_ref(&loads[1], None, super::CONFIDENCE_FUZZY_NAME);
}

#[test]
fn trait_impl_and_dyn_trait_receivers_resolve_by_type() {
    let worktree = TestWorktree::new("trait-members");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write(
        "src/exec.rs",
        r#"
pub trait Execute {
    fn execute(self);
}

pub trait Store {
    fn save(&self);
}
"#,
    );
    worktree.write(
        "src/list.rs",
        r#"
use crate::exec::{Execute, Store};

pub struct ListArgs;

impl Execute for ListArgs {
    fn execute(self) {}
}

pub struct Disk;

impl Store for Disk {
    fn save(&self) {}
}
"#,
    );
    worktree.write(
        "src/teardown.rs",
        r#"
use crate::exec::Execute;

pub struct TeardownArgs;

impl Execute for TeardownArgs {
    fn execute(self) {}
}
"#,
    );
    worktree.write(
        "src/command.rs",
        r#"
use crate::exec::{Execute, Store};
use crate::list::ListArgs;

fn dispatch(args: ListArgs, store: &dyn Store, other: Box<dyn Execute>) {
    args.execute();
    store.save();
}
"#,
    );
    worktree.write(
        "src/main.rs",
        "mod command;\nmod exec;\nmod list;\nmod teardown;\n\nfn main() {}\n",
    );

    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    assert_resolves_to(
        &call_ref(&conn, "src/command.rs", "execute"),
        &conn,
        "src/list.rs",
        "<ListArgs as Execute>::execute",
        super::CONFIDENCE_EXACT,
    );
    // A `dyn Trait` receiver dispatches through the trait: the declaration is
    // the only honest target.
    assert_resolves_to(
        &call_ref(&conn, "src/command.rs", "save"),
        &conn,
        "src/exec.rs",
        "Store::save",
        super::CONFIDENCE_EXACT,
    );
}

#[test]
fn same_named_types_narrow_by_import_then_stay_fuzzy() {
    let worktree = TestWorktree::new("ambiguous-types");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    for module in ["alpha", "beta"] {
        worktree.write(
            &format!("src/{module}.rs"),
            r#"
pub struct Config;

impl Config {
    pub fn load() -> Self { Config }
}
"#,
        );
    }
    worktree.write(
        "src/imported.rs",
        r#"
use crate::beta::Config;

fn read() {
    let _ = Config::load();
}
"#,
    );
    worktree.write(
        "src/unimported.rs",
        r#"
fn load() {}

fn read(config: Config) {
    let _ = Config::load();
}
"#,
    );
    worktree.write(
        "src/lib.rs",
        "mod alpha;\nmod beta;\nmod imported;\nmod unimported;\n",
    );

    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    assert_resolves_to(
        &call_ref(&conn, "src/imported.rs", "load"),
        &conn,
        "src/beta.rs",
        "<Config>::load",
        super::CONFIDENCE_IMPORT_RESOLVED,
    );
    // Two `Config::load` candidates and no import to choose: neither is
    // picked, and the same-file `load` function is not a fallback.
    assert_ref(
        &call_ref(&conn, "src/unimported.rs", "load"),
        None,
        super::CONFIDENCE_FUZZY_NAME,
    );
}

#[test]
fn free_function_paths_never_resolve_to_a_same_named_method() {
    let worktree = TestWorktree::new("free-vs-member");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write(
        "src/adapter.rs",
        r#"
use crate::application::plugin;

pub struct OrbitRuntime;

impl OrbitRuntime {
    pub fn enable_plugin(&self) {
        plugin::enable_plugin(self);
    }

    pub fn helper(&self) {}

    pub fn run(&self) {
        helper();
    }
}

pub trait Store {
    fn get_task(&self);
    fn load(&self) {
        self.get_task();
    }
}

fn get_task() {}
"#,
    );
    worktree.write(
        "src/lib.rs",
        "mod adapter;\nmod application;\n\nfn helper() {}\n",
    );
    worktree.write("src/application/mod.rs", "pub mod plugin;\n");
    worktree.write(
        "src/application/plugin.rs",
        "pub fn enable_plugin(runtime: &crate::adapter::OrbitRuntime) {}\n",
    );

    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    // `plugin::enable_plugin(self)` is the lifecycle function, never the
    // method it is written in.
    assert_resolves_to(
        &call_ref(&conn, "src/adapter.rs", "enable_plugin"),
        &conn,
        "src/application/plugin.rs",
        "enable_plugin",
        super::CONFIDENCE_IMPORT_RESOLVED,
    );
    // A bare `helper()` call cannot name the same-file `<OrbitRuntime>::helper`
    // method either.
    let helper = call_ref(&conn, "src/adapter.rs", "helper");
    assert_ne!(
        helper.target_symbol_hint,
        Some(symbol_id(&conn, "src/adapter.rs", "<OrbitRuntime>::helper")),
        "{helper:?}"
    );
    // `self.get_task()` in a trait's default body is a method call, so the
    // free-item rule must not narrow it to the free `get_task` function.
    let method = call_ref(&conn, "src/adapter.rs", "get_task");
    assert_ne!(
        method.target_symbol_hint,
        Some(symbol_id(&conn, "src/adapter.rs", "get_task")),
        "{method:?}"
    );
}

#[test]
fn module_path_calls_only_match_same_module_items_under_that_path() {
    let worktree = TestWorktree::new("module-path-calls");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write("src/process/mod.rs", "pub mod ids;\n");
    // Both files hold a `tests` module, so they share a module prefix and the
    // name-only same-module rung would otherwise pair them.
    worktree.write(
        "src/process/ids.rs",
        "mod tests {\n    pub fn id() -> u32 { 7 }\n}\n",
    );
    worktree.write(
        "src/process/lock.rs",
        "pub fn label() -> u32 { 1 }\npub fn tally() -> u32 { 2 }\n",
    );
    worktree.write(
        "src/process/owner.rs",
        r#"
mod tests {
    fn owner() -> u32 {
        std::process::id() + crate::process::lock::label()
    }

    fn total() -> u32 {
        crate::counts::tally()
    }
}
"#,
    );
    worktree.write(
        "src/lib.rs",
        "mod process;\npub use process::lock as counts;\n",
    );

    graph.sync(SyncMode::Full).expect("sync graph");

    let conn = open_test_connection(worktree.path());
    // `std::process::id()` shares the `process` module name with the local
    // `id`, but the spelled path is `std::process`: not a same-module match.
    assert_ref(
        &call_ref(&conn, "src/process/owner.rs", "id"),
        None,
        super::CONFIDENCE_FUZZY_NAME,
    );
    let label = call_ref(&conn, "src/process/owner.rs", "label");
    assert_eq!(
        label.target_symbol_hint,
        Some(symbol_id(&conn, "src/process/lock.rs", "label")),
        "{label:?}"
    );
    // A crate-rooted path through a `use m as alias` re-export names no
    // module the symbol table knows; the plain same-module rule still pairs it.
    let tally = call_ref(&conn, "src/process/owner.rs", "tally");
    assert_eq!(
        tally.target_symbol_hint,
        Some(symbol_id(&conn, "src/process/lock.rs", "tally")),
        "{tally:?}"
    );
}

/// Syncs `files` into a fresh worktree and returns its connection.
fn sync_rust_files(name: &str, files: &[(&str, &str)]) -> (TestWorktree, Connection) {
    let worktree = TestWorktree::new(name);
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    for (path, body) in files {
        worktree.write(path, body);
    }
    graph.sync(SyncMode::Full).expect("sync graph");
    let conn = open_test_connection(worktree.path());
    (worktree, conn)
}

fn calls_named(conn: &Connection, from_file: &str, target_name: &str) -> Vec<StoredRef> {
    let calls = refs_for_file(conn, from_file)
        .into_iter()
        .filter(|row| row.kind == "call" && row.target_name == target_name)
        .collect::<Vec<_>>();
    assert!(!calls.is_empty(), "no call ref for {target_name}");
    calls
}

#[test]
fn member_calls_narrow_to_the_named_types_module_before_picking_a_tier() {
    // a::Foo has an inherent `m`, b::Foo only a trait impl `m`; the caller
    // names b::Foo by import and by a written path. The inherent tier must
    // not be picked across types before the module narrows.
    let (_worktree, conn) = sync_rust_files(
        "member-scope-tier",
        &[
            (
                "src/a.rs",
                "pub struct Foo;\nimpl Foo { pub fn m(&self) {} }\n",
            ),
            (
                "src/b.rs",
                "pub trait Tr { fn m(&self); }\npub struct Foo;\nimpl Tr for Foo { fn m(&self) {} }\n",
            ),
            (
                "src/caller.rs",
                "use crate::b::{Foo, Tr};\nfn f(x: Foo) { x.m(); }\nfn g(y: crate::b::Foo) { y.m(); }\n",
            ),
            (
                "src/written.rs",
                "mod inner { pub struct Foo; impl Foo { pub fn m(&self) {} } }\nfn f(x: &crate::a::Foo) { x.m(); }\n",
            ),
            ("src/lib.rs", "mod a;\nmod b;\nmod caller;\nmod written;\n"),
        ],
    );
    for row in calls_named(&conn, "src/caller.rs", "m") {
        assert_resolves_to(
            &row,
            &conn,
            "src/b.rs",
            "<Foo as Tr>::m",
            super::CONFIDENCE_IMPORT_RESOLVED,
        );
    }
    // The written path beats a same-file look-alike in an inline module.
    for row in calls_named(&conn, "src/written.rs", "m") {
        assert_resolves_to(
            &row,
            &conn,
            "src/a.rs",
            "<Foo>::m",
            super::CONFIDENCE_IMPORT_RESOLVED,
        );
    }
}

#[test]
fn external_types_never_resolve_to_a_local_look_alike() {
    let (_worktree, conn) = sync_rust_files(
        "member-external-type",
        &[
            (
                "src/net.rs",
                "pub struct Client;\nimpl Client { pub fn new() -> Self { Client } pub fn get(&self) {} }\n",
            ),
            (
                "src/caller.rs",
                "use reqwest::Client;\nfn f(c: &reqwest::Client) { c.get(); }\nfn g() { let c = Client::new(); c.get(); }\n",
            ),
            ("src/lib.rs", "mod net;\nmod caller;\n"),
        ],
    );
    for name in ["new", "get"] {
        for row in calls_named(&conn, "src/caller.rs", name) {
            assert_ref(&row, None, super::CONFIDENCE_FUZZY_NAME);
        }
    }
}

#[test]
fn use_as_aliases_resolve_to_the_renamed_type() {
    let (_worktree, conn) = sync_rust_files(
        "member-use-alias",
        &[
            (
                "src/a.rs",
                "pub struct Foo;\nimpl Foo { pub fn new() -> Self { Foo } pub fn m(&self) {} }\n",
            ),
            (
                "src/c.rs",
                "pub struct Bar;\nimpl Bar { pub fn new() -> Self { Bar } pub fn m(&self) {} }\n",
            ),
            (
                "src/caller.rs",
                "use crate::a::Foo as Bar;\nfn f(x: Bar) { x.m(); }\nfn g() { let y = Bar::new(); y.m(); }\n",
            ),
            ("src/lib.rs", "mod a;\nmod c;\nmod caller;\n"),
        ],
    );
    for name in ["new", "m"] {
        for row in calls_named(&conn, "src/caller.rs", name) {
            assert_eq!(
                row.target_symbol_hint,
                Some(symbol_id(&conn, "src/a.rs", &format!("<Foo>::{name}"))),
                "{row:?}"
            );
        }
    }
}

#[test]
fn trait_object_receivers_target_the_trait_not_a_same_named_struct() {
    let (_worktree, conn) = sync_rust_files(
        "member-dyn-trait",
        &[
            ("src/api.rs", "pub trait Store { fn save(&self); }\n"),
            (
                "src/mem.rs",
                "pub struct Store;\nimpl Store { pub fn save(&self) {} }\n",
            ),
            (
                "src/caller.rs",
                "use crate::api::Store;\nfn f(s: &dyn Store) { s.save(); }\nfn g(s: impl Store) { s.save(); }\n",
            ),
            ("src/lib.rs", "mod api;\nmod mem;\nmod caller;\n"),
        ],
    );
    for row in calls_named(&conn, "src/caller.rs", "save") {
        assert_eq!(
            row.target_symbol_hint,
            Some(symbol_id(&conn, "src/api.rs", "Store::save")),
            "{row:?}"
        );
    }
}

#[test]
fn type_parameter_receivers_stay_unresolved() {
    let (_worktree, conn) = sync_rust_files(
        "member-type-parameter",
        &[
            (
                "src/ext.rs",
                "pub trait Ext { fn m(&self); }\nimpl<T: std::fmt::Debug> Ext for T { fn m(&self) {} }\n",
            ),
            (
                "src/caller.rs",
                "pub trait Tr { fn m(&self); }\nfn g<T: Tr>(x: T) { x.m(); }\npub struct W<U>(U);\nimpl<U: Tr> W<U> { fn h(&self, u: U) { u.m(); } }\n",
            ),
            ("src/lib.rs", "mod ext;\nmod caller;\n"),
        ],
    );
    for row in calls_named(&conn, "src/caller.rs", "m") {
        assert_ref(&row, None, super::CONFIDENCE_FUZZY_NAME);
    }
}

#[test]
fn trait_default_members_resolve_to_the_trait_declaration() {
    let (_worktree, conn) = sync_rust_files(
        "member-trait-default",
        &[
            (
                "src/caller.rs",
                "pub trait Greet { fn hi() {} fn hey(&self) {} }\npub struct Foo;\nimpl Greet for Foo {}\nfn f(x: Foo) { Foo::hi(); x.hey(); }\n",
            ),
            ("src/lib.rs", "mod caller;\n"),
        ],
    );
    for name in ["hi", "hey"] {
        let row = call_ref(&conn, "src/caller.rs", name);
        assert_resolves_to(
            &row,
            &conn,
            "src/caller.rs",
            &format!("Greet::{name}"),
            super::CONFIDENCE_EXACT,
        );
    }
}

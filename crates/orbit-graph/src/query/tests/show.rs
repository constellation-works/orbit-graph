use std::fs;
use std::path::PathBuf;

use crate::extract::Selector;
use rusqlite::{Connection, params};
use serde_json::json;

use super::{DEFAULT_SHOW_MAX_BYTES, NodeMetadata, NodeView, SourceSpan};
use crate::GraphError;
use crate::SyncPolicy;
use crate::query::tests::support::{
    TestWorktree, assert_json_matches_fixture, graph_db_path, insert_file, insert_symbol,
    open_connection, open_graph,
};
use crate::sync::sync_leader_count;

#[test]
fn show_result_shape_matches_golden_fixture() {
    let result = NodeView {
        bytes: b"pub fn handler() {}\n".to_vec(),
        metadata: NodeMetadata {
            file: "src/lib.rs".to_string(),
            span: SourceSpan { start: 0, end: 20 },
            kind: "function".to_string(),
            name: Some("handler".to_string()),
            qualified: Some("crate::handler".to_string()),
            truncated: false,
        },
    };

    assert_json_matches_fixture(&result, include_str!("show.golden.json"));
}

#[test]
fn show_resolves_symbol_file_module_and_command_selectors() {
    let worktree = TestWorktree::new("show-selectors");
    let source = "pub mod api {\n    pub fn handler() {\n        println!(\"hi\");\n    }\n}\n";
    worktree.write("src/lib.rs", source);
    let graph = open_graph(&worktree, SyncPolicy::Manual);
    let conn = open_connection(&worktree);
    insert_file(&conn, "src/lib.rs", "rust", source);
    let module_start = source.find("pub mod api").expect("module start");
    let module_end = source.len();
    insert_symbol(
        &conn,
        "src/lib.rs",
        "api",
        "crate::api",
        "module",
        module_start,
        module_end,
    );
    let handler_start = source.find("pub fn handler").expect("handler start");
    let handler_end = source[handler_start..]
        .find("\n    }\n")
        .map(|offset| handler_start + offset + "\n    }".len())
        .expect("handler end");
    let handler_id = insert_symbol(
        &conn,
        "src/lib.rs",
        "handler",
        "crate::api::handler",
        "function",
        handler_start,
        handler_end,
    );
    insert_command(
        &conn,
        "serve",
        "src/lib.rs",
        handler_start,
        Some(handler_id),
    );

    let symbol = graph
        .show(
            &"symbol:src/lib.rs#handler:function"
                .parse()
                .expect("symbol selector"),
            DEFAULT_SHOW_MAX_BYTES,
        )
        .expect("show symbol")
        .expect("symbol resolves");
    assert_eq!(symbol.metadata.file, "src/lib.rs");
    assert_eq!(symbol.metadata.kind, "function");
    assert_eq!(symbol.metadata.name.as_deref(), Some("handler"));
    assert_eq!(
        symbol.metadata.qualified.as_deref(),
        Some("crate::api::handler")
    );
    assert_eq!(
        &symbol.bytes,
        &source.as_bytes()[handler_start..handler_end]
    );

    let file = graph
        .show(
            &"file:src/lib.rs".parse().expect("file selector"),
            DEFAULT_SHOW_MAX_BYTES,
        )
        .expect("show file")
        .expect("file resolves");
    assert_eq!(file.metadata.kind, "file");
    assert_eq!(
        file.metadata.span,
        SourceSpan {
            start: 0,
            end: source.len()
        }
    );
    assert_eq!(file.bytes, source.as_bytes());

    let module = graph
        .show(
            &Selector::Module {
                qualified: "crate::api".to_string(),
            },
            DEFAULT_SHOW_MAX_BYTES,
        )
        .expect("show module")
        .expect("module resolves");
    assert_eq!(module.metadata.kind, "module");
    assert_eq!(module.metadata.qualified.as_deref(), Some("crate::api"));
    assert_eq!(&module.bytes, &source.as_bytes()[module_start..module_end]);

    let command = graph
        .show(
            &Selector::Command {
                name: "serve".to_string(),
            },
            DEFAULT_SHOW_MAX_BYTES,
        )
        .expect("show command")
        .expect("command resolves");
    assert_eq!(command.metadata.kind, "command");
    assert_eq!(command.metadata.name.as_deref(), Some("serve"));
    assert_eq!(
        command.metadata.qualified.as_deref(),
        Some("crate::api::handler")
    );
    assert_eq!(
        &command.bytes,
        &source.as_bytes()[handler_start..handler_end]
    );
}

#[test]
fn show_command_selector_uses_handler_symbol_file_when_handler_lives_elsewhere() {
    let worktree = TestWorktree::new("show-command-cross-file");
    let main_source =
        "#[derive(Subcommand)]\npub enum WorkspaceCommand {\n    Teardown(TeardownArgs),\n}\n";
    let handler_source = "impl Execute for TeardownArgs {\n    fn execute(self) {\n        println!(\"{}\", self.workspace);\n    }\n}\n";
    worktree.write("src/main.rs", main_source);
    worktree.write("src/teardown.rs", handler_source);
    let graph = open_graph(&worktree, SyncPolicy::Manual);
    let conn = open_connection(&worktree);
    insert_file(&conn, "src/main.rs", "rust", main_source);
    insert_file(&conn, "src/teardown.rs", "rust", handler_source);
    let command_span_start = main_source.find("Teardown").expect("command variant start");
    let handler_start = handler_source.find("fn execute").expect("handler start");
    let handler_end = handler_source.len();
    let handler_id = insert_symbol(
        &conn,
        "src/teardown.rs",
        "execute",
        "<TeardownArgs as Execute>::execute",
        "method",
        handler_start,
        handler_end,
    );
    insert_command(
        &conn,
        "workspace teardown",
        "src/main.rs",
        command_span_start,
        Some(handler_id),
    );

    let command = graph
        .show(
            &Selector::Command {
                name: "workspace teardown".to_string(),
            },
            DEFAULT_SHOW_MAX_BYTES,
        )
        .expect("show command")
        .expect("command resolves");

    assert_eq!(command.metadata.file, "src/teardown.rs");
    assert_eq!(command.metadata.kind, "command");
    assert_eq!(command.metadata.name.as_deref(), Some("workspace teardown"));
    assert_eq!(
        command.metadata.qualified.as_deref(),
        Some("<TeardownArgs as Execute>::execute")
    );
    assert_eq!(
        command.metadata.span,
        SourceSpan {
            start: handler_start,
            end: handler_end
        }
    );
    assert_eq!(
        &command.bytes,
        &handler_source.as_bytes()[handler_start..handler_end]
    );
}

#[test]
fn show_truncates_source_when_max_bytes_is_shorter_than_span() {
    let worktree = TestWorktree::new("show-truncate");
    let source = "pub fn long_body() {\n    let message = \"abcdef\";\n}\n";
    worktree.write("src/lib.rs", source);
    let graph = open_graph(&worktree, SyncPolicy::Manual);
    let conn = open_connection(&worktree);
    insert_file(&conn, "src/lib.rs", "rust", source);
    insert_symbol(
        &conn,
        "src/lib.rs",
        "long_body",
        "crate::long_body",
        "function",
        0,
        source.len(),
    );

    let view = graph
        .show(
            &"symbol:src/lib.rs#long_body:function"
                .parse()
                .expect("selector"),
            7,
        )
        .expect("show symbol")
        .expect("symbol resolves");

    assert_eq!(view.bytes, b"pub fn ");
    assert_eq!(
        view.metadata.span,
        SourceSpan {
            start: 0,
            end: source.len()
        }
    );
    assert!(view.metadata.truncated);
}

#[test]
fn show_serializes_truncated_utf8_source_on_char_boundary() {
    let worktree = TestWorktree::new("show-truncate-utf8");
    let source = "pub fn cafe() -> &'static str { \"cafeé\" }\n";
    worktree.write("src/lib.rs", source);
    let graph = open_graph(&worktree, SyncPolicy::Manual);
    let conn = open_connection(&worktree);
    insert_file(&conn, "src/lib.rs", "rust", source);
    insert_symbol(
        &conn,
        "src/lib.rs",
        "cafe",
        "crate::cafe",
        "function",
        0,
        source.len(),
    );
    let max_bytes = source.find("é").expect("accent byte") + 1;

    let view = graph
        .show(
            &"symbol:src/lib.rs#cafe:function".parse().expect("selector"),
            max_bytes,
        )
        .expect("show symbol")
        .expect("symbol resolves");
    let json = serde_json::to_value(&view).expect("serialize view");

    assert!(
        json["source"]
            .as_str()
            .expect("source string")
            .ends_with("cafe")
    );
    assert!(json.get("bytes").is_none());
    assert!(view.metadata.truncated);
}

#[test]
fn show_serializes_non_utf8_source_with_labeled_byte_fallback() {
    let worktree = TestWorktree::new("show-non-utf8");
    let source = b"valid prefix \xFF invalid\n";
    let path = worktree.path().join("src/lib.bin");
    fs::create_dir_all(path.parent().expect("source parent")).expect("create source parent");
    fs::write(path.as_path(), source).expect("write non-utf8 source");
    let graph = open_graph(&worktree, SyncPolicy::Manual);
    let conn = open_connection(&worktree);
    conn.execute(
        "INSERT INTO files (path, content_hash, mtime_ns, lang, byte_len, extracted_at)
         VALUES (?1, x'00', 1, ?2, ?3, 2)",
        params![
            "src/lib.bin",
            "binary",
            i64::try_from(source.len()).expect("source length fits")
        ],
    )
    .expect("insert non-utf8 file row");

    let view = graph
        .show(
            &"file:src/lib.bin".parse().expect("file selector"),
            DEFAULT_SHOW_MAX_BYTES,
        )
        .expect("show file")
        .expect("file resolves");
    let json = serde_json::to_value(&view).expect("serialize view");

    assert_eq!(
        json["source"],
        json!({
            "encoding": "bytes",
            "bytes": source
        })
    );
    assert!(json.get("bytes").is_none());
}

#[test]
fn show_missing_selector_returns_none() {
    let worktree = TestWorktree::new("show-missing");
    let source = "pub fn present() {}\n";
    worktree.write("src/lib.rs", source);
    let graph = open_graph(&worktree, SyncPolicy::Manual);
    let conn = open_connection(&worktree);
    insert_file(&conn, "src/lib.rs", "rust", source);

    let missing = graph
        .show(
            &"symbol:src/lib.rs#missing:function"
                .parse()
                .expect("selector"),
            DEFAULT_SHOW_MAX_BYTES,
        )
        .expect("show missing");

    assert!(missing.is_none());
}

#[test]
fn show_calls_ensure_synced_at_entry() {
    let worktree = TestWorktree::new("show-ensure");
    worktree.write("src/lib.rs", "pub fn auto_sync_show() {}\n");
    let graph = open_graph(&worktree, SyncPolicy::OnRead);
    let db_path = graph_db_path(&worktree);

    let view = graph
        .show(
            &"file:src/lib.rs".parse().expect("file selector"),
            DEFAULT_SHOW_MAX_BYTES,
        )
        .expect("show with on-read sync")
        .expect("file resolves");

    assert_eq!(sync_leader_count(db_path.as_path()), 1);
    assert_eq!(view.metadata.file, "src/lib.rs");
}

#[test]
fn show_unresolved_dir_selector_returns_none() {
    let worktree = TestWorktree::new("show-dir");
    let graph = open_graph(&worktree, SyncPolicy::Manual);

    let view = graph
        .show(
            &"dir:src".parse().expect("dir selector"),
            DEFAULT_SHOW_MAX_BYTES,
        )
        .expect("show dir");

    assert!(view.is_none());
}

#[test]
fn show_rejects_database_paths_outside_the_worktree_before_reading() {
    let worktree = TestWorktree::new("show-confine");
    let stamp = worktree
        .path()
        .file_name()
        .expect("worktree name")
        .to_string_lossy()
        .into_owned();
    let parent_marker = "PATH_TRAVERSAL_MARKER_12899";
    let absolute_marker = "ABSOLUTE_PATH_MARKER_12899";
    let parent_name = format!("outside-{stamp}.rs");
    let parent_stored = format!("../{parent_name}");
    let parent_path = worktree.path().join(&parent_stored);
    let parent_source = format!("{parent_marker}\n");
    fs::write(&parent_path, &parent_source).expect("write parent-traversal source");
    let _remove_parent = DeleteOnDrop(parent_path);

    let absolute_path = std::env::temp_dir().join(format!("absolute-{stamp}.rs"));
    let absolute_source = format!("{absolute_marker}\n");
    fs::write(&absolute_path, &absolute_source).expect("write absolute source");
    let _remove_absolute = DeleteOnDrop(absolute_path.clone());
    let absolute_stored = absolute_path.to_string_lossy().into_owned();

    let inside = "pub fn ok() -> i32 { 1 }\n";
    worktree.write("src/ok.rs", inside);
    let graph = open_graph(&worktree, SyncPolicy::Manual);
    let conn = open_connection(&worktree);
    insert_file(&conn, "src/ok.rs", "rust", inside);
    insert_symbol(
        &conn,
        "src/ok.rs",
        "ok",
        "crate::ok",
        "function",
        0,
        inside.len(),
    );

    insert_file(&conn, &parent_stored, "rust", &parent_source);
    insert_symbol(
        &conn,
        &parent_stored,
        "leak",
        "crate::leak",
        "function",
        0,
        parent_source.len(),
    );
    insert_symbol(
        &conn,
        &parent_stored,
        "leaked_mod",
        "crate::leaked_mod",
        "module",
        0,
        parent_source.len(),
    );
    insert_command(&conn, "leaked-cmd", &parent_stored, 0, None);

    insert_file(&conn, &absolute_stored, "rust", &absolute_source);
    insert_symbol(
        &conn,
        &absolute_stored,
        "absolute_leak",
        "crate::absolute_leak",
        "function",
        0,
        absolute_source.len(),
    );

    let parent_symbol = format!("symbol:{parent_stored}#leak:function");
    let parent_file = format!("file:{parent_stored}");
    let absolute_symbol = format!("symbol:{absolute_stored}#absolute_leak:function");
    for selector in [
        parent_symbol.as_str(),
        parent_file.as_str(),
        "module:crate::leaked_mod",
        "command:leaked-cmd",
        absolute_symbol.as_str(),
    ] {
        assert_outside_source_rejected(&graph, selector, parent_marker);
        assert_outside_source_rejected(&graph, selector, absolute_marker);
    }

    let inside_view = graph
        .show(
            &"symbol:src/ok.rs#ok:function"
                .parse()
                .expect("inside selector"),
            DEFAULT_SHOW_MAX_BYTES,
        )
        .expect("show inside file")
        .expect("inside file resolves");
    let inside_source = std::str::from_utf8(&inside_view.bytes).expect("inside utf-8");
    assert!(inside_source.contains("pub fn ok"));
    assert!(!inside_source.contains(parent_marker));
    assert!(!inside_source.contains(absolute_marker));
}

fn assert_outside_source_rejected(graph: &crate::Graph, selector: &str, marker: &str) {
    let error = graph
        .show(
            &selector.parse().expect("outside selector"),
            DEFAULT_SHOW_MAX_BYTES,
        )
        .expect_err("database path outside the worktree must be rejected");
    match error {
        GraphError::InvalidData { operation, reason } => {
            assert_eq!(operation, "resolve graph source path");
            assert!(reason.contains("worktree"), "{reason}");
            assert!(!reason.contains(marker), "{reason}");
        }
        other => panic!("expected invalid source path, got {other}"),
    }
}

struct DeleteOnDrop(PathBuf);

impl Drop for DeleteOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn insert_command(
    conn: &Connection,
    name: &str,
    file_path: &str,
    span_start: usize,
    handler_symbol: Option<i64>,
) {
    conn.execute(
        "INSERT INTO commands (name, file_path, span_start, handler_symbol)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            name,
            file_path,
            i64::try_from(span_start).expect("command span fits"),
            handler_symbol
        ],
    )
    .expect("insert command row");
}

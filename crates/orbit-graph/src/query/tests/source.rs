use std::fs::{self, OpenOptions};
use std::path::Path;
use std::time::{Duration, Instant};

use rusqlite::{Connection, params};

use super::{bytes_read, reset_bytes_read};
use crate::query::tests::support::{
    TestWorktree, insert_file, insert_symbol, open_connection, open_graph,
};
use crate::sync::scanner::MAX_FILE_BYTES;
use crate::{
    CalleeEdge, Match, RefConfidence, RefOpts, SearchKind, SearchQuery, Selector, SyncPolicy,
};

const PROMPT_LIMIT: Duration = Duration::from_secs(1);

#[test]
fn show_keeps_a_raw_cut_when_the_whole_span_is_not_utf8() {
    let worktree = TestWorktree::new("show-raw-cut");
    let source = b"yy\xc3\xffZZ";
    let path = worktree.path().join("src/lib.bin");
    fs::create_dir_all(path.parent().expect("parent")).expect("parent");
    fs::write(&path, source).expect("write");
    let graph = open_graph(&worktree, SyncPolicy::Manual);
    let conn = open_connection(&worktree);
    conn.execute(
        "INSERT INTO files (path, content_hash, mtime_ns, lang, byte_len, extracted_at)
         VALUES ('src/lib.bin', x'00', 1, 'binary', ?1, 2)",
        params![i64::try_from(source.len()).expect("len")],
    )
    .expect("insert file");

    let view = graph
        .show(&"file:src/lib.bin".parse().expect("selector"), 3)
        .expect("show")
        .expect("file");

    assert_eq!(view.bytes, b"yy\xc3");
    assert!(view.metadata.truncated);
}

#[test]
fn tiny_show_budget_does_not_read_a_file_past_the_sync_cap() {
    let worktree = TestWorktree::new("show-bounded");
    let prefix = "pub fn cafe() -> &'static str { \"cafeé\" }\n";
    let relative = "src/lib.rs";
    let path = worktree.path().join(relative);
    fs::create_dir_all(path.parent().expect("parent")).expect("parent");
    fs::write(&path, prefix).expect("write prefix");
    let grown = MAX_FILE_BYTES + (1024 * 1024);
    OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open")
        .set_len(grown)
        .expect("grow past the sync cap");
    let graph = open_graph(&worktree, SyncPolicy::Manual);
    let conn = open_connection(&worktree);
    insert_file_len(&conn, relative, grown);
    let accent = prefix.find("é").expect("accent") + 1;

    reset_bytes_read();
    let started = Instant::now();
    let view = graph
        .show(&"file:src/lib.rs".parse().expect("selector"), accent)
        .expect("show grown file")
        .expect("file");
    assert!(started.elapsed() < PROMPT_LIMIT, "show read the grown file");
    let read = bytes_read();

    let source = std::str::from_utf8(view.bytes.as_slice()).expect("utf-8 window");
    assert!(source.ends_with("cafe"), "{source}");
    assert!(view.metadata.truncated);
    assert_eq!(
        view.metadata.span.end,
        usize::try_from(grown).expect("grown fits usize")
    );
    assert!(read <= u64::try_from(accent + 3).expect("budget"), "{read}");
    assert!(read < grown, "show read {read} of {grown} bytes");

    let marker = "TARGET";
    let marker_at = 4096usize;
    let mut body = vec![b'a'; marker_at + marker.len()];
    body[marker_at..].copy_from_slice(marker.as_bytes());
    let symbol_path = worktree.path().join("src/mid.rs");
    fs::write(&symbol_path, &body).expect("write mid");
    OpenOptions::new()
        .write(true)
        .open(&symbol_path)
        .expect("open mid")
        .set_len(grown)
        .expect("grow mid");
    insert_file_len(&conn, "src/mid.rs", grown);
    insert_symbol(
        &conn,
        "src/mid.rs",
        "target",
        "crate::target",
        "function",
        marker_at,
        marker_at + marker.len(),
    );

    reset_bytes_read();
    let view = graph
        .show(
            &"symbol:src/mid.rs#target:function"
                .parse()
                .expect("selector"),
            4,
        )
        .expect("show mid-file span")
        .expect("symbol");
    assert_eq!(view.bytes, b"TARG");
    assert!(view.metadata.truncated);
    let read = bytes_read();
    assert!(
        read < u64::try_from(marker_at).expect("offset"),
        "show read the {read}-byte prefix before the span"
    );
    assert!(read < grown);
}

#[test]
fn line_readers_refuse_stale_oversize_files_and_keep_normal_locations() {
    let worktree = TestWorktree::new("source-oversize");
    let source = "fn caller() {\n    caller();\n}\n";
    let caller = "src/lib.rs";
    worktree.write(caller, source);
    let ok = "fn ok() {}\n";
    worktree.write("src/ok.rs", ok);
    let graph = open_graph(&worktree, SyncPolicy::Manual);
    let conn = open_connection(&worktree);
    index_caller(&conn, caller, source);
    insert_file(&conn, "src/ok.rs", "rust", ok);
    insert_symbol(
        &conn,
        "src/ok.rs",
        "ok",
        "crate::ok",
        "function",
        0,
        ok.len(),
    );

    assert_eq!(symbol_line(&graph, "caller"), 1);
    assert_eq!(callee_line(&graph, caller), 2);
    let refs = graph
        .refs(&symbol_selector(caller, "caller"), &RefOpts::default())
        .expect("refs");
    assert_eq!(refs.refs.len(), 1);
    assert_eq!(refs.refs[0].file, caller);
    assert_eq!(refs.refs[0].line, 2);
    assert_eq!(refs.refs[0].snippet, "caller();");
    assert_eq!(symbol_line(&graph, "ok"), 1);

    let grown = MAX_FILE_BYTES + 1;
    OpenOptions::new()
        .write(true)
        .open(worktree.path().join(caller))
        .expect("open caller")
        .set_len(grown)
        .expect("grow caller");

    let cap = MAX_FILE_BYTES.to_string();
    for (label, error) in [
        (
            "search",
            graph
                .search(&symbol_query("caller"))
                .expect_err("oversize search"),
        ),
        (
            "refs",
            graph
                .refs(&symbol_selector(caller, "caller"), &RefOpts::default())
                .expect_err("oversize refs"),
        ),
        (
            "callees",
            graph
                .callees(&symbol_selector(caller, "caller"))
                .expect_err("oversize callees"),
        ),
    ] {
        let rendered = error.to_string();
        assert!(rendered.contains(caller), "{label}: {rendered}");
        assert!(rendered.contains(cap.as_str()), "{label}: {rendered}");
        assert!(
            rendered.contains("grew after it was indexed"),
            "{label}: {rendered}"
        );
    }
    reset_bytes_read();
    let _ = graph
        .search(&symbol_query("caller"))
        .expect_err("oversize search reads nothing");
    assert_eq!(bytes_read(), 0, "oversize refusal read the file body");

    assert_eq!(symbol_line(&graph, "ok"), 1);
}

#[cfg(unix)]
#[test]
fn fifo_replacement_refuses_show_and_line_queries_promptly() {
    let worktree = TestWorktree::new("source-fifo");
    let source = "def secret():\n    secret()\n";
    let relative = "secret.py";
    worktree.write(relative, source);
    let graph = open_graph(&worktree, SyncPolicy::Manual);
    let conn = open_connection(&worktree);
    index_caller(&conn, relative, source);

    assert_eq!(
        graph
            .show(&file_selector(relative), source.len())
            .expect("show file")
            .expect("indexed file")
            .bytes,
        source.as_bytes()
    );
    assert_eq!(symbol_line(&graph, "secret"), 1);
    assert_eq!(callee_line(&graph, relative), 2);
    let refs = graph
        .refs(&symbol_selector(relative, "secret"), &RefOpts::default())
        .expect("refs");
    assert_eq!(refs.refs[0].file, relative);
    assert_eq!(refs.refs[0].snippet, "secret()");

    replace_with_fifo(worktree.path().join(relative).as_path());

    assert_prompt_refusal("show", || graph.show(&file_selector(relative), 1));
    assert_prompt_refusal("search", || graph.search(&symbol_query("secret")));
    assert_prompt_refusal("refs", || {
        graph.refs(&symbol_selector(relative, "secret"), &RefOpts::default())
    });
    assert_prompt_refusal("callees", || {
        graph.callees(&symbol_selector(relative, "secret"))
    });
}

fn index_caller(conn: &Connection, file: &str, source: &str) {
    insert_file(conn, file, "rust", source);
    insert_symbol(
        conn,
        file,
        name_of(file),
        &format!("crate::{}", name_of(file)),
        "function",
        0,
        source.len(),
    );
    let call = if file.ends_with(".py") {
        "secret()"
    } else {
        "caller();"
    };
    let span_start = source.rfind(call).expect("call span");
    conn.execute(
        "INSERT INTO refs (
            from_file, from_span_start, from_span_end, target_name, target_qualified,
            target_symbol_hint, kind, confidence
         ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, 'call', 'exact')",
        params![
            file,
            i64::try_from(span_start).expect("span"),
            i64::try_from(span_start + call.len()).expect("span end"),
            name_of(file),
            format!("crate::{}", name_of(file))
        ],
    )
    .expect("insert call ref");
}

fn name_of(file: &str) -> &'static str {
    if file.ends_with(".py") {
        "secret"
    } else {
        "caller"
    }
}

fn insert_file_len(conn: &Connection, path: &str, byte_len: u64) {
    conn.execute(
        "INSERT INTO files (path, content_hash, mtime_ns, lang, byte_len, extracted_at)
         VALUES (?1, x'00', 1, 'rust', ?2, 2)",
        params![path, i64::try_from(byte_len).expect("length fits")],
    )
    .expect("insert grown file");
}

fn symbol_query(name: &str) -> SearchQuery {
    SearchQuery {
        query: name.to_string(),
        kind: Some(SearchKind::Symbol),
        lang: None,
        limit: Some(5),
    }
}

fn symbol_selector(file: &str, name: &str) -> Selector {
    Selector::Symbol {
        path: file.to_string(),
        symbol: name.to_string(),
        kind: "function".to_string(),
    }
}

fn file_selector(file: &str) -> Selector {
    Selector::File {
        path: file.to_string(),
    }
}

fn symbol_line(graph: &crate::Graph, name: &str) -> usize {
    let result = graph.search(&symbol_query(name)).expect("search");
    let Match::Symbol { line, .. } = result
        .matches
        .iter()
        .find(|item| matches!(item, Match::Symbol { name: found, .. } if found == name))
        .expect("symbol match")
    else {
        panic!("symbol match");
    };
    *line
}

fn callee_line(graph: &crate::Graph, file: &str) -> usize {
    let edges = graph
        .callees(&symbol_selector(file, name_of(file)))
        .expect("callees");
    assert_eq!(
        edges,
        vec![CalleeEdge {
            target_name: name_of(file).to_string(),
            target_qualified: Some(format!("crate::{}", name_of(file))),
            confidence: RefConfidence::Exact,
            line: 2,
        }]
    );
    edges[0].line
}

fn assert_prompt_refusal<T: std::fmt::Debug>(
    label: &str,
    run: impl FnOnce() -> Result<T, crate::GraphError>,
) {
    let started = Instant::now();
    let error = run().expect_err(label);
    let elapsed = started.elapsed();
    assert!(elapsed < PROMPT_LIMIT, "{label} blocked for {elapsed:?}");
    let rendered = error.to_string();
    assert!(rendered.contains("fifo"), "{label}: {rendered}");
    assert!(rendered.contains("regular file"), "{label}: {rendered}");
}

#[cfg(unix)]
fn replace_with_fifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    fs::remove_file(path).expect("remove indexed file");
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("fifo path");
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
    assert_eq!(rc, 0, "mkfifo: {}", std::io::Error::last_os_error());
}

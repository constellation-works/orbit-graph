use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};

use super::{
    DefaultExtractorBackend, ExtractFileError, ExtractedSourceFile, ExtractorBackend, Pass1Limits,
    SilencedPanics, install_silenceable_panic_hook, panics_silenced,
};
use crate::sync::scanner::{Diff, MAX_FILE_BYTES};
use crate::{
    EXTRACTOR_VERSION, Graph, SyncMode, SyncObserver, SyncPolicy, SyncProgress, resolve_db_path,
};

#[test]
fn panicking_extractor_skips_file_and_preserves_other_writes() {
    let worktree = TestWorktree::new("panic-skip");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    drop(graph);
    worktree.write("src/good.rs", "pub fn good() {}\n");
    worktree.write("src/bad.rs", "pub fn bad() {}\n");
    worktree.write("src/broken.rs", "pub fn broken() {}\n");
    let diff = Diff {
        new: vec![
            PathBuf::from("src/good.rs"),
            PathBuf::from("src/bad.rs"),
            PathBuf::from("src/broken.rs"),
        ],
        ..Diff::default()
    };

    let output = super::run_with_backend(
        graph_db_path(worktree.path()).as_path(),
        worktree.path(),
        SyncMode::Auto,
        &diff,
        &PanickingBackend,
        None,
        Pass1Limits::default(),
    )
    .expect("pass1 skips panicking file");

    let conn = open_test_connection(worktree.path());
    assert_eq!(output.files_written, 1);
    assert_eq!(file_count(&conn, "src/good.rs"), 1);
    assert_eq!(file_count(&conn, "src/bad.rs"), 0);
    assert_eq!(file_count(&conn, "src/broken.rs"), 0);
    assert_eq!(row_count(&conn, "symbols"), 1);
    // The panic and the failure are both counted, not only logged
    // (STD-02 §R32).
    let mut failed = output
        .failed
        .iter()
        .map(|failure| (failure.path.as_str(), failure.error_kind.as_str()))
        .collect::<Vec<_>>();
    failed.sort_unstable();
    assert_eq!(output.failed.len(), 2, "{:?}", output.failed);
    assert_eq!(
        failed,
        [("src/bad.rs", "panic"), ("src/broken.rs", "invalid_data")]
    );
    assert!(
        output
            .failed
            .iter()
            .any(|failure| failure.message.contains("intentional extractor panic")),
        "{:?}",
        output.failed
    );
}

#[test]
fn modified_file_replaces_prior_symbol_rows() {
    let worktree = TestWorktree::new("modified-replace");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write("src/lib.rs", "pub fn only_now() {}\n");
    {
        let conn = open_test_connection(worktree.path());
        insert_file_with_three_symbols(&conn, "src/lib.rs");
    }

    graph.sync(SyncMode::Auto).expect("sync modified file");

    let conn = open_test_connection(worktree.path());
    assert_eq!(
        row_count_for_file(&conn, "symbols", "file_path", "src/lib.rs"),
        1
    );
    assert_eq!(file_count(&conn, "src/lib.rs"), 1);
}

#[test]
fn deleted_file_row_cascades_all_pass1_tables() {
    let worktree = TestWorktree::new("deleted-cascade");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    {
        let conn = open_test_connection(worktree.path());
        insert_file_anchored_rows(&conn, "src/deleted.rs");
    }

    graph.sync(SyncMode::Auto).expect("sync deleted file");

    let conn = open_test_connection(worktree.path());
    for table in [
        "files",
        "symbols",
        "refs",
        "relations",
        "imports",
        "commands",
        "strings",
        "configs",
    ] {
        assert_eq!(row_count(&conn, table), 0, "{table} rows should be gone");
    }
}

#[test]
fn full_sync_rebuilds_cross_file_command_handlers() {
    let worktree = TestWorktree::new("cross-file-command-handler");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write(
        "src/add.rs",
        r#"
struct TaskAddArgs;

trait Execute {
    fn execute(self);
}

impl Execute for TaskAddArgs {
    fn execute(self) {
        helper();
    }
}

fn helper() {}
"#,
    );
    worktree.write(
        "src/command.rs",
        r#"
use clap::Subcommand;

#[derive(Subcommand)]
enum TaskSubcommand {
    Add(TaskAddArgs),
}

fn dispatch(command: TaskSubcommand) {
    match command {
        TaskSubcommand::Add(args) => args.execute(),
    }
}
"#,
    );

    graph.sync(SyncMode::Full).expect("initial full sync");
    graph
        .sync(SyncMode::Full)
        .expect("second full sync keeps cross-file command handlers valid");

    let conn = open_test_connection(worktree.path());
    let handler = conn
        .query_row(
            "SELECT s.qualified
             FROM commands c
             JOIN symbols s ON s.id = c.handler_symbol
             WHERE c.name = 'task add'",
            [],
            |row| row.get::<_, String>(0),
        )
        .expect("resolved task add handler");
    assert_eq!(handler, "<TaskAddArgs as Execute>::execute");
}

#[test]
fn pass1_writes_relations_but_not_refs() {
    let worktree = TestWorktree::new("relations-no-refs");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    drop(graph);
    worktree.write(
        "src/lib.rs",
        r#"
trait Service {
    fn run(&self);
}

struct Worker;

impl Service for Worker {
    fn run(&self) {
        helper();
    }
}

fn helper() {}
"#,
    );
    let diff = Diff {
        new: vec![PathBuf::from("src/lib.rs")],
        ..Diff::default()
    };

    super::run(
        graph_db_path(worktree.path()).as_path(),
        worktree.path(),
        SyncMode::Full,
        &diff,
        None,
    )
    .expect("run pass1");

    let conn = open_test_connection(worktree.path());
    assert_eq!(row_count(&conn, "relations"), 1);
    assert_eq!(row_count(&conn, "refs"), 0);
}

#[test]
fn pass1_returns_extracted_refs_for_pass2_handoff() {
    let worktree = TestWorktree::new("refs-handoff");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    drop(graph);
    worktree.write(
        "src/lib.rs",
        r#"
fn helper() {}

fn main() {
    helper();
}
"#,
    );
    let diff = Diff {
        new: vec![PathBuf::from("src/lib.rs")],
        ..Diff::default()
    };

    let output = super::run(
        graph_db_path(worktree.path()).as_path(),
        worktree.path(),
        SyncMode::Full,
        &diff,
        None,
    )
    .expect("run pass1");

    assert!(output.refs.iter().any(|file_refs| {
        file_refs.file_path == "src/lib.rs"
            && file_refs
                .refs
                .iter()
                .any(|raw_ref| raw_ref.target_name == "helper")
    }));
    assert_eq!(row_count(&open_test_connection(worktree.path()), "refs"), 0);
}

#[test]
fn sync_meta_timestamps_track_full_and_auto_modes() {
    let worktree = TestWorktree::new("meta-timestamps");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    worktree.write("src/lib.rs", "pub fn full_build() {}\n");

    graph.sync(SyncMode::Full).expect("full sync");

    let conn = open_test_connection(worktree.path());
    let full_after_full = meta_value(&conn, "last_full_build_at");
    let incremental_after_full = meta_value(&conn, "last_incremental_at");
    assert!(full_after_full > 0);
    assert_eq!(incremental_after_full, 0);
    drop(conn);

    worktree.write("src/next.rs", "pub fn incremental() {}\n");
    graph.sync(SyncMode::Auto).expect("auto sync");

    let conn = open_test_connection(worktree.path());
    assert!(meta_value(&conn, "last_full_build_at") >= full_after_full);
    assert!(meta_value(&conn, "last_incremental_at") > 0);
}

#[test]
fn cold_sync_100_rust_file_performance_smoke_prints_elapsed_ms() {
    let worktree = TestWorktree::new("perf-smoke");
    let graph = Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph");
    for index in 0..100 {
        worktree.write(
            &format!("src/file_{index}.rs"),
            &format!("pub fn marker_{index}() {{}}\n"),
        );
    }

    let started = Instant::now();
    let report = graph.sync(SyncMode::Full).expect("cold sync");
    let elapsed = started.elapsed();

    #[allow(clippy::print_stdout)]
    {
        println!("pass1_cold_sync_100_rust_files_ms={}", elapsed.as_millis());
    }
    assert_eq!(report.files_indexed, 100);
    assert_eq!(
        row_count(&open_test_connection(worktree.path()), "files"),
        100
    );
}

#[test]
fn pass1_holds_one_bounded_chunk_of_extracted_files_at_a_time() {
    let content = "pub fn chunked() {}\n";
    // By file count, and by source bytes: both limits make chunks of two.
    for limits in [
        Pass1Limits {
            chunk_files: 2,
            chunk_bytes: u64::MAX,
        },
        Pass1Limits {
            chunk_files: usize::MAX,
            chunk_bytes: 2 * content.len() as u64,
        },
    ] {
        let worktree = TestWorktree::new("bounded-chunks");
        drop(Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph"));
        let paths = (0..6)
            .map(|index| {
                let rel = format!("src/f{index}.rs");
                worktree.write(&rel, content);
                PathBuf::from(rel)
            })
            .collect::<Vec<_>>();
        let diff = Diff {
            new: paths.clone(),
            ..Diff::default()
        };
        let written = Arc::new(AtomicUsize::new(0));
        let observer = WrittenCounter(Arc::clone(&written));
        let backend = RecordingBackend {
            written,
            extracted: Mutex::new(Vec::new()),
        };

        let output = super::run_with_backend(
            graph_db_path(worktree.path()).as_path(),
            worktree.path(),
            SyncMode::Auto,
            &diff,
            &backend,
            Some(&observer),
            limits,
        )
        .expect("chunked pass1 succeeds");

        assert_eq!(output.files_written, 6);
        assert_eq!(output.total_files, 6);
        let mut extracted = backend
            .extracted
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        extracted.sort();
        // Each file is extracted only after every earlier chunk was written,
        // so at most one chunk of extracted rows is held: {limits:?}.
        let files_written_before_extraction = extracted
            .iter()
            .map(|(_, written)| *written)
            .collect::<Vec<_>>();
        assert_eq!(
            files_written_before_extraction,
            vec![0, 0, 2, 2, 4, 4],
            "{limits:?}"
        );
    }
}

#[test]
fn parse_past_its_deadline_is_an_extraction_failure() {
    let worktree = TestWorktree::new("parse-deadline");
    drop(Graph::open(worktree.path(), SyncPolicy::Manual).expect("open graph"));
    let python = (0..200)
        .map(|index| format!("def handler_{index}(value):\n    return value + {index}\n"))
        .collect::<String>();
    worktree.write("src/slow.py", &python);
    worktree.write("docs/notes.md", "# Notes\n\nNo tree-sitter parse here.\n");
    let diff = Diff {
        new: vec![PathBuf::from("docs/notes.md"), PathBuf::from("src/slow.py")],
        ..Diff::default()
    };

    let output = super::run_with_backend(
        graph_db_path(worktree.path()).as_path(),
        worktree.path(),
        SyncMode::Auto,
        &diff,
        &DefaultExtractorBackend {
            parse_timeout: Duration::ZERO,
        },
        None,
        Pass1Limits::default(),
    )
    .expect("pass1 skips the file whose parse expired");

    let conn = open_test_connection(worktree.path());
    assert_eq!(output.files_written, 1);
    assert_eq!(file_count(&conn, "src/slow.py"), 0, "no rows for it");
    assert_eq!(file_count(&conn, "docs/notes.md"), 1);
    drop(conn);

    // Control: the same file indexes within the default deadline.
    let output = super::run_with_backend(
        graph_db_path(worktree.path()).as_path(),
        worktree.path(),
        SyncMode::Auto,
        &Diff {
            new: vec![PathBuf::from("src/slow.py")],
            ..Diff::default()
        },
        &DefaultExtractorBackend::default(),
        None,
        Pass1Limits::default(),
    )
    .expect("pass1 indexes within the default deadline");
    assert_eq!(output.files_written, 1);
    let conn = open_test_connection(worktree.path());
    assert_eq!(
        row_count_for_file(&conn, "symbols", "file_path", "src/slow.py"),
        200
    );
}

#[test]
fn extraction_refuses_a_file_that_grew_past_the_byte_cap() {
    let worktree = TestWorktree::new("extract-byte-cap");
    let oversize = "a".repeat(usize::try_from(MAX_FILE_BYTES).expect("cap fits usize") + 1);
    worktree.write("big.md", &oversize);

    let error =
        match DefaultExtractorBackend::default().extract(worktree.path(), Path::new("big.md")) {
            Ok(_) => panic!("an oversize file must not be extracted"),
            Err(error) => error,
        };
    assert!(error.to_string().contains("byte cap"), "{error}");
}

#[test]
fn silenced_panics_are_scoped_to_the_extracting_thread() {
    install_silenceable_panic_hook();
    assert!(!panics_silenced());
    let silenced = SilencedPanics::enter();
    assert!(panics_silenced());
    let other_thread = thread::spawn(panics_silenced)
        .join()
        .expect("join other thread");
    assert!(
        !other_thread,
        "an unrelated thread's panics must still reach the previous hook"
    );
    drop(silenced);
    assert!(!panics_silenced());
}

/// Counts files pass 1 reports as touched.
struct WrittenCounter(Arc<AtomicUsize>);

impl SyncObserver for WrittenCounter {
    fn on_progress(&self, progress: &SyncProgress) {
        if progress.current_path.is_some() {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Records, for each extracted file, how many files were already written.
struct RecordingBackend {
    written: Arc<AtomicUsize>,
    extracted: Mutex<Vec<(PathBuf, usize)>>,
}

impl ExtractorBackend for RecordingBackend {
    fn extract(
        &self,
        worktree_root: &Path,
        rel_path: &Path,
    ) -> Result<ExtractedSourceFile, ExtractFileError> {
        self.extracted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((rel_path.to_path_buf(), self.written.load(Ordering::SeqCst)));
        DefaultExtractorBackend::default().extract(worktree_root, rel_path)
    }
}

struct PanickingBackend;

impl ExtractorBackend for PanickingBackend {
    fn extract(
        &self,
        worktree_root: &Path,
        rel_path: &Path,
    ) -> Result<ExtractedSourceFile, ExtractFileError> {
        if rel_path == Path::new("src/bad.rs") {
            panic!("intentional extractor panic");
        }
        if rel_path == Path::new("src/broken.rs") {
            return Err(ExtractFileError::new(
                "extract source file",
                "intentional extractor failure",
            ));
        }
        DefaultExtractorBackend::default().extract(worktree_root, rel_path)
    }
}

fn insert_file_with_three_symbols(conn: &Connection, rel: &str) {
    conn.execute(
        "INSERT INTO files (path, content_hash, mtime_ns, lang, byte_len, extracted_at)
         VALUES (?1, x'00', 1, 'rust', 12, 2)",
        params![rel],
    )
    .expect("insert file");
    for index in 0..3 {
        conn.execute(
            "INSERT INTO symbols (
                file_path, name, qualified, kind, span_start, span_end, signature, parent_symbol
             ) VALUES (?1, ?2, ?3, 'function', ?4, ?5, NULL, NULL)",
            params![
                rel,
                format!("old_{index}"),
                format!("crate::old_{index}"),
                index,
                index + 1
            ],
        )
        .expect("insert old symbol");
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO symbols_fts (rowid, name, qualified, signature)
             VALUES (?1, ?2, ?3, NULL)",
            params![id, format!("old_{index}"), format!("crate::old_{index}")],
        )
        .expect("insert old symbol fts");
    }
}

fn insert_file_anchored_rows(conn: &Connection, rel: &str) {
    conn.execute(
        "INSERT INTO files (path, content_hash, mtime_ns, lang, byte_len, extracted_at)
         VALUES (?1, x'00', 1, 'rust', 12, 2)",
        params![rel],
    )
    .expect("insert file");
    conn.execute(
        "INSERT INTO symbols (
            id, file_path, name, qualified, kind, span_start, span_end, signature, parent_symbol
         ) VALUES (1, ?1, 'run', 'crate::run', 'function', 0, 3, 'fn run()', NULL)",
        params![rel],
    )
    .expect("insert symbol");
    conn.execute(
        "INSERT INTO symbols_fts (rowid, name, qualified, signature)
         VALUES (1, 'run', 'crate::run', 'fn run()')",
        [],
    )
    .expect("insert symbol fts");
    conn.execute(
        "INSERT INTO refs (
            from_file, from_span_start, from_span_end, target_name, target_qualified,
            target_symbol_hint, kind, confidence
         ) VALUES (?1, 4, 7, 'run', 'crate::run', 1, 'call', 'exact')",
        params![rel],
    )
    .expect("insert ref");
    conn.execute(
        "INSERT INTO relations (
            from_qualified, to_qualified, kind, def_file, def_span_start, def_span_end, confidence
         ) VALUES ('crate::Type', 'crate::Trait', 'impl', ?1, 0, 10, 'exact')",
        params![rel],
    )
    .expect("insert relation");
    conn.execute(
        "INSERT INTO imports (from_file, target_path, target_symbol)
         VALUES (?1, 'crate::other', 'Other')",
        params![rel],
    )
    .expect("insert import");
    conn.execute(
        "INSERT INTO commands (name, file_path, span_start, handler_symbol)
         VALUES ('run', ?1, 0, 1)",
        params![rel],
    )
    .expect("insert command");
    conn.execute(
        "INSERT INTO strings (file_path, line, value, context_symbol)
         VALUES (?1, 1, 'hello world', 1)",
        params![rel],
    )
    .expect("insert string");
    let string_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO strings_fts (rowid, value) VALUES (?1, 'hello world')",
        params![string_id],
    )
    .expect("insert string fts");
    conn.execute(
        "INSERT INTO configs (file_path, line, key, kind)
         VALUES (?1, 1, 'app.name', 'toml')",
        params![rel],
    )
    .expect("insert config");
    let config_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO configs_fts (rowid, key) VALUES (?1, 'app.name')",
        params![config_id],
    )
    .expect("insert config fts");
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

fn row_count(conn: &Connection, table: &str) -> i64 {
    let sql = format!("SELECT count(*) FROM {table}");
    conn.query_row(&sql, [], |row| row.get(0))
        .expect("count rows")
}

fn row_count_for_file(conn: &Connection, table: &str, column: &str, rel: &str) -> i64 {
    let sql = format!("SELECT count(*) FROM {table} WHERE {column} = ?1");
    conn.query_row(&sql, [rel], |row| row.get(0))
        .expect("count rows for file")
}

fn file_count(conn: &Connection, rel: &str) -> i64 {
    row_count_for_file(conn, "files", "path", rel)
}

fn meta_value(conn: &Connection, key: &str) -> i64 {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
        row.get::<_, String>(0)
    })
    .expect("read meta value")
    .parse()
    .expect("meta value is integer")
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
            "orbit-graph-pass1-{name}-{}-{stamp}",
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

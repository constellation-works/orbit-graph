//! Smoke tests that exercise the packaged `orbit-graph` executable.

#![allow(clippy::expect_used)]

use std::fs;
#[cfg(unix)]
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::fd::{FromRawFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::net::UnixStream;

use fs2::FileExt;
use serde_json::Value;
use tempfile::TempDir;

use orbit_graph::{HistoryIndex, TaskTextAvailability, TemporalStatus};

#[test]
fn real_binary_indexes_and_queries_a_fixture() {
    let fixture = fixture_repository();

    let sync = run_json(fixture.path(), ["sync", "--full"]);
    assert!(
        sync["files_indexed"]
            .as_u64()
            .is_some_and(|count| count >= 1)
    );

    let search = run_json(
        fixture.path(),
        ["search", "helper", "--kind", "symbol", "--limit", "5"],
    );
    assert!(
        search["matches"]
            .as_array()
            .is_some_and(|matches| !matches.is_empty())
    );

    let show = run_json(
        fixture.path(),
        [
            "show",
            "symbol:src/lib.rs#entry:function",
            "--max-bytes",
            "256",
        ],
    );
    assert_eq!(show["metadata"]["file"], "src/lib.rs");
    assert!(
        show["source"]
            .as_str()
            .is_some_and(|source| source.contains("pub fn entry"))
    );

    let refs = run_json(
        fixture.path(),
        [
            "refs",
            "symbol:src/lib.rs#helper:function",
            "--confidence",
            "fuzzy",
            "--kind",
            "call",
        ],
    );
    assert!(refs["refs"].as_array().is_some_and(|refs| !refs.is_empty()));

    let callees = run_json(
        fixture.path(),
        ["callees", "symbol:src/lib.rs#entry:function"],
    );
    assert!(
        callees["callees"]
            .as_array()
            .is_some_and(|calls| !calls.is_empty())
    );

    let db_path = run_json(fixture.path(), ["db-path"]);
    assert!(
        db_path["path"]
            .as_str()
            .is_some_and(|path| path.contains("/.orbit-graph/"))
    );
}

#[cfg(unix)]
/// Agent-facing query fields (ORB-13099): every impacted node and every
/// reference carries enough to open or `show` it, `refs` states whether it
/// fell back, and `callees` hides unresolved calls with no indexed definition
/// by default while counting them. Existing fields are asserted alongside so
/// the additions stay additive.
#[test]
fn real_binary_query_output_carries_locations_context_and_callee_filtering() {
    let fixture = agent_query_fixture();
    let _ = run_json(fixture.path(), ["sync", "--full"]);

    // impact: each touched entry has a selector, file, and line, and the
    // selector round-trips through `show`.
    let impact = run_json(
        fixture.path(),
        [
            "impact",
            "symbol:src/lib.rs#helper:function",
            "--direction",
            "inbound",
        ],
    );
    assert_eq!(impact["fallback_used"], false);
    let touched = impact["touched"].as_array().expect("impact touched");
    let entry = touched
        .iter()
        .find(|node| node["qualified_name"] == "entry")
        .expect("entry is impacted");
    assert_eq!(entry["distance"], 1);
    assert_eq!(entry["edge_kind"], "call");
    assert_eq!(entry["selector"], "symbol:src/lib.rs#entry:function");
    assert_eq!(entry["file"], "src/lib.rs");
    assert_eq!(entry["line"], 5);
    let caller = touched
        .iter()
        .find(|node| node["qualified_name"] == "caller")
        .expect("caller is impacted");
    assert_eq!(caller["distance"], 2);
    assert_eq!(caller["line"], 10);
    let shown = run_json(
        fixture.path(),
        ["show", entry["selector"].as_str().expect("selector string")],
    );
    assert_eq!(shown["metadata"]["name"], "entry");
    assert_eq!(shown["metadata"]["file"], "src/lib.rs");
    let impact_table = run(
        fixture.path(),
        [
            "--format",
            "table",
            "impact",
            "symbol:src/lib.rs#helper:function",
        ],
    );
    let impact_table = String::from_utf8_lossy(&impact_table.stdout);
    assert!(impact_table.contains("LOCATION"), "{impact_table}");
    assert!(impact_table.contains("src/lib.rs:5"), "{impact_table}");

    // refs: each row names its enclosing symbol and carries the source line;
    // a top-level `use` has no enclosing symbol.
    let refs = run_json(
        fixture.path(),
        ["refs", "symbol:src/lib.rs#helper:function"],
    );
    assert_eq!(refs["fallback_used"], false);
    assert!(refs.get("fallback").is_none());
    let reference = &refs["refs"][0];
    assert_eq!(reference["file"], "src/lib.rs");
    assert_eq!(reference["line"], 6);
    assert_eq!(reference["kind"], "call");
    assert_eq!(reference["confidence"], "exact");
    assert_eq!(
        reference["from_selector"],
        "symbol:src/lib.rs#entry:function"
    );
    assert_eq!(reference["snippet"], "let value = helper();");
    let exported = run_json(
        fixture.path(),
        ["refs", "symbol:src/lib.rs#exported:function"],
    );
    let exported_refs = exported["refs"].as_array().expect("exported refs");
    let top_level_use = exported_refs
        .iter()
        .find(|row| row["kind"] == "use")
        .expect("top-level use row");
    assert!(top_level_use["from_selector"].is_null(), "{top_level_use}");
    assert_eq!(top_level_use["snippet"], "use fixture::exported;");
    let call = exported_refs
        .iter()
        .find(|row| row["kind"] == "call")
        .expect("call row");
    assert_eq!(call["from_selector"], "symbol:tools/run.rs#main:function");

    // refs fallback: the precise floor finds nothing, so the name-only rows
    // are under `fallback` and `fallback_used` says so explicitly.
    let method = run_json(
        fixture.path(),
        ["refs", "symbol:src/widget.rs#render:method"],
    );
    assert_eq!(method["fallback_used"], true);
    assert_eq!(method["refs"], serde_json::json!([]));
    let fallback_row = &method["fallback"]["refs"][0];
    assert_eq!(fallback_row["confidence"], "fuzzy_name");
    assert_eq!(
        fallback_row["from_selector"],
        "symbol:tools/draw.rs#draw:function"
    );
    assert_eq!(fallback_row["snippet"], "w.render()");

    // callees: unresolved calls with no indexed callable definition (`Some`,
    // `map`) are hidden by default and counted; the flag restores them.
    let selector = "symbol:src/lib.rs#entry:function";
    let filtered = run_json(fixture.path(), ["callees", selector]);
    let filtered_calls = filtered["callees"].as_array().expect("callees");
    assert_eq!(filtered["hidden_unresolved"], 2);
    assert_eq!(filtered_calls.len(), 1);
    assert_eq!(filtered_calls[0]["target_name"], "helper");
    assert_eq!(filtered_calls[0]["target_qualified"], "helper");
    assert_eq!(filtered_calls[0]["line"], 6);
    let everything = run_json(
        fixture.path(),
        ["callees", selector, "--include-unresolved"],
    );
    assert_eq!(everything["hidden_unresolved"], 0);
    let names = everything["callees"]
        .as_array()
        .expect("unfiltered callees")
        .iter()
        .filter_map(|edge| edge["target_name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["helper", "Some", "map"]);

    let plain = run(fixture.path(), ["callees", selector]);
    assert!(plain.status.success());
    assert_eq!(
        String::from_utf8_lossy(&plain.stdout).lines().count(),
        1,
        "only the resolved call is listed"
    );
    let notice = String::from_utf8_lossy(&plain.stderr);
    assert!(
        notice.contains("2 unresolved call(s)") && notice.contains("--include-unresolved"),
        "{notice}"
    );
    let ndjson = run(fixture.path(), ["--format", "ndjson", "callees", selector]);
    assert!(ndjson.status.success());
    assert_eq!(parse_ndjson(&ndjson.stdout), filtered_calls.clone());
    // The default filter is echoed on stderr in JSON mode too, while stdout
    // stays one parseable document.
    let json_mode = run_explicit_json(fixture.path(), ["callees", selector]);
    assert!(json_mode.status.success());
    assert!(String::from_utf8_lossy(&json_mode.stderr).contains("2 unresolved call(s)"));
    let document: Value = serde_json::from_slice(&json_mode.stdout).expect("one JSON document");
    assert_eq!(document["hidden_unresolved"], 2);
    let everything_stderr = run_explicit_json(
        fixture.path(),
        ["callees", selector, "--include-unresolved"],
    );
    assert!(
        everything_stderr.stderr.is_empty(),
        "nothing hidden, no notice"
    );

    // Headerless redirected output is documented in help.
    let help = run(fixture.path(), ["--help"]);
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("headerless tab-separated rows"), "{help}");
    let command_help = run(fixture.path(), ["refs", "--help"]);
    let command_help = String::from_utf8_lossy(&command_help.stdout);
    assert!(
        command_help.contains("headerless tab-separated rows"),
        "{command_help}"
    );
}

#[test]
fn real_binary_printed_selectors_round_trip_past_a_nested_same_name_decoy() {
    // A nested `inner::helper` is indexed before the top-level `helper`. A
    // selector printed for the top-level one must name it again as input to
    // every query command, not the decoy with the lower id (STD-01 §R32).
    let fixture = TempDir::new().expect("create round-trip fixture");
    run_git(fixture.path(), ["init", "-b", "main"]);
    run_git(
        fixture.path(),
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(fixture.path(), ["config", "user.name", "Graph Test"]);
    fs::create_dir_all(fixture.path().join("src")).expect("create src");
    fs::write(
        fixture.path().join("src/lib.rs"),
        "mod inner {\n    pub fn helper() -> i32 {\n        decoy_only()\n    }\n\n    fn decoy_only() -> i32 {\n        0\n    }\n}\n\npub fn helper() -> i32 {\n    caller2()\n}\n\npub fn caller2() -> i32 {\n    2\n}\n\npub fn entry() -> i32 {\n    helper()\n}\n",
    )
    .expect("write fixture source");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "fixture"]);
    let _ = run_json(fixture.path(), ["sync", "--full"]);

    // Selectors as printed by impact and by refs.
    let impact = run_json(
        fixture.path(),
        [
            "impact",
            "symbol:src/lib.rs#caller2:function",
            "--direction",
            "inbound",
        ],
    );
    let printed_by_impact = impact["touched"]
        .as_array()
        .expect("impact touched")
        .iter()
        .find(|node| node["distance"] == 1)
        .and_then(|node| node["selector"].as_str())
        .expect("helper is a direct caller of caller2")
        .to_string();
    assert_eq!(printed_by_impact, "symbol:src/lib.rs#helper:function");
    let refs = run_json(
        fixture.path(),
        ["refs", "symbol:src/lib.rs#caller2:function"],
    );
    let printed_by_refs = refs["refs"][0]["from_selector"]
        .as_str()
        .expect("the call names its enclosing symbol")
        .to_string();
    assert_eq!(printed_by_refs, printed_by_impact);

    let selector = printed_by_impact.as_str();
    let shown = run_json(fixture.path(), ["show", selector]);
    assert_eq!(shown["metadata"]["qualified"], "helper", "{shown}");
    assert!(
        shown["source"]
            .as_str()
            .is_some_and(|source| source.contains("caller2()")),
        "{shown}"
    );
    let callees = run_json(
        fixture.path(),
        ["callees", selector, "--include-unresolved"],
    );
    let names = callees["callees"]
        .as_array()
        .expect("callees")
        .iter()
        .filter_map(|edge| edge["target_name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["caller2"], "callees resolved the decoy: {callees}");
    let helper_refs = run_json(fixture.path(), ["refs", selector]);
    let from = helper_refs["refs"]
        .as_array()
        .expect("helper refs")
        .iter()
        .filter_map(|row| row["from_selector"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(from, ["symbol:src/lib.rs#entry:function"], "{helper_refs}");
    let helper_impact = run_json(
        fixture.path(),
        ["impact", selector, "--direction", "inbound"],
    );
    let impacted = helper_impact["touched"]
        .as_array()
        .expect("helper impact")
        .iter()
        .filter_map(|node| node["selector"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        impacted,
        ["symbol:src/lib.rs#entry:function"],
        "{helper_impact}"
    );
}

fn agent_query_fixture() -> TempDir {
    let fixture = TempDir::new().expect("create agent query fixture");
    run_git(fixture.path(), ["init", "-b", "main"]);
    run_git(
        fixture.path(),
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(fixture.path(), ["config", "user.name", "Graph Test"]);
    for (path, source) in [
        (
            "src/lib.rs",
            "pub fn helper() -> i32 {\n    1\n}\n\npub fn entry() -> Option<i32> {\n    let value = helper();\n    Some(value).map(|v| v + 1)\n}\n\npub fn caller() -> Option<i32> {\n    entry()\n}\n\npub fn exported() -> i32 {\n    2\n}\n",
        ),
        (
            "src/widget.rs",
            "pub struct Widget;\n\nimpl Widget {\n    pub fn render(&self) -> i32 {\n        3\n    }\n}\n",
        ),
        (
            "tools/run.rs",
            "use fixture::exported;\n\nfn main() {\n    let _ = exported();\n}\n",
        ),
        (
            "tools/draw.rs",
            "fn draw(w: &dyn Paint) -> i32 {\n    w.render()\n}\n",
        ),
    ] {
        let path = fixture.path().join(path);
        fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture dir");
        fs::write(path, source).expect("write fixture source");
    }
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "fixture"]);
    fixture
}

#[test]
fn real_binary_applies_git_ignore_rules_without_a_git_executable() {
    let fixture = fixture_repository();
    fs::write(fixture.path().join(".gitignore"), "secret.md\n").expect("write .gitignore");
    let marker = "SENSITIVE_MARKER_4821";
    let secret = fixture.path().join("secret.md");
    fs::write(&secret, format!("```text\n{marker}\n```\n")).expect("write secret");
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o600))
        .expect("restrict secret permissions");
    run_git(fixture.path(), ["check-ignore", "secret.md"]);

    let git_bin = TempDir::new().expect("create Git probe fixture");
    let missing_path = git_bin.path().join("no-such-directory");
    let failed_path = git_bin.path().join("bin");
    fs::create_dir(&failed_path).expect("create fake Git directory");
    let fake_git = failed_path.join("git");
    fs::write(&fake_git, "#!/bin/sh\nexit 2\n").expect("write failing Git executable");
    fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o700))
        .expect("make fake Git executable");

    // Ignore rules are matched in process, so a missing or broken `git` on
    // PATH neither aborts the sync nor lets ignored source in.
    for path in [&missing_path, &failed_path] {
        let output = run_with_env(
            fixture.path(),
            ["--format", "json", "sync", "--full"],
            &[("PATH", path.to_str().expect("UTF-8 fixture path"))],
        );
        assert!(
            output.status.success(),
            "sync must not depend on a git executable: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let matches = run_json(fixture.path(), ["search", marker]);
        assert_eq!(matches["matches"], serde_json::json!([]));
    }
}

#[cfg(unix)]
#[test]
fn real_binary_sync_finishes_with_thousands_of_gitignored_files() {
    // Regression: `git check-ignore --stdin` deadlocked once the ignored
    // paths it echoed filled its stdout pipe while sync was still writing
    // the path list, and hung every later sync behind the database lock.
    let fixture = fixture_repository();
    fs::write(fixture.path().join(".gitignore"), "env/\n").expect("write .gitignore");
    let site_packages = fixture.path().join("env/lib/python3/site-packages");
    fs::create_dir_all(&site_packages).expect("create ignored directory");
    for index in 0..4_000 {
        fs::write(
            site_packages.join(format!("vendored_module_with_a_long_name_{index:04}.py")),
            "VALUE = 1\n",
        )
        .expect("write ignored module");
    }

    let output = run_with_deadline(
        fixture.path(),
        &["--format", "json", "sync"],
        &[],
        Duration::from_secs(120),
    );
    assert!(
        output.status.success(),
        "sync failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sync: Value = serde_json::from_slice(&output.stdout).expect("JSON sync output");
    assert_eq!(sync["files_indexed"], 3, "only the tracked fixture sources");
}

#[cfg(unix)]
#[test]
fn real_binary_sync_never_runs_the_repository_fsmonitor() {
    let fixture = fixture_repository();
    let hook_dir = TempDir::new().expect("create hook directory");
    let marker = hook_dir.path().join("fsmonitor-ran");
    let hook = hook_dir.path().join("fsmonitor-hook");
    fs::write(
        &hook,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    )
    .expect("write fsmonitor hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).expect("make hook executable");
    run_git(
        fixture.path(),
        [
            "config",
            "core.fsmonitor",
            hook.to_str().expect("UTF-8 hook path"),
        ],
    );
    fs::write(fixture.path().join(".gitignore"), "ignored.rs\n").expect("write .gitignore");
    fs::write(fixture.path().join("ignored.rs"), "pub fn hidden() {}\n").expect("write source");
    fs::write(fixture.path().join("untracked.rs"), "pub fn fresh() {}\n").expect("write source");

    let sync = run_json(fixture.path(), ["sync", "--full"]);
    assert_eq!(sync["files_indexed"], 4, "{sync}");
    assert!(
        !marker.exists(),
        "sync executed the repository-configured core.fsmonitor command"
    );

    // Control: the fixture is armed, so Git itself does run the hook.
    let _ = Command::new("git")
        .current_dir(fixture.path())
        .args(["status", "--porcelain"])
        .output()
        .expect("run git status");
    assert!(marker.exists(), "git status should run the fsmonitor hook");
}

#[cfg(unix)]
#[test]
fn real_binary_sync_times_out_naming_the_lock_holder() {
    let fixture = fixture_repository();
    let _ = run_json(fixture.path(), ["sync"]);
    let db_path = run_json(fixture.path(), ["db-path"]);
    let lock_path = format!("{}.lock", db_path["path"].as_str().expect("database path"));

    let holder = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open graph lock file");
    holder.lock().expect("hold the graph database lock");
    let acquired_at = "2026-09-26T12:34:56.000000000Z";
    fs::write(
        &lock_path,
        serde_json::json!({
            "pid": std::process::id(),
            "acquired_at": acquired_at,
            "label": "fixture lock holder",
        })
        .to_string(),
    )
    .expect("write holder record");

    let started = Instant::now();
    let output = run_with_deadline(
        fixture.path(),
        &["--format", "json", "sync"],
        &[("ORBIT_GRAPH_LOCK_TIMEOUT_MS", "300")],
        Duration::from_secs(60),
    );
    let waited = started.elapsed();
    drop(holder);

    assert!(!output.status.success(), "a held lock must fail the sync");
    assert!(
        waited >= Duration::from_millis(300),
        "failed before the deadline"
    );
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON sync error");
    assert_eq!(error["error"]["code"], "graph_error");
    let message = error["error"]["message"].as_str().expect("error message");
    assert!(message.contains("timed out after 300 ms"), "{message}");
    assert!(
        message.contains(&format!("pid {}", std::process::id())),
        "{message}"
    );
    assert!(message.contains(acquired_at), "{message}");
    assert!(message.contains("fixture lock holder"), "{message}");

    // Released, the next sync proceeds.
    let _ = run_json(fixture.path(), ["sync"]);
}

#[cfg(unix)]
#[test]
fn real_binary_skips_a_file_above_the_byte_cap_with_a_warning() {
    let fixture = fixture_repository();
    fs::create_dir_all(fixture.path().join("data")).expect("create data directory");
    let filler = "a".repeat(4 * 1024 * 1024);
    fs::write(
        fixture.path().join("data/huge.json"),
        format!("{{\"HUGE_MARKER_KEY\": \"{filler}\"}}\n"),
    )
    .expect("write oversize config");

    let output = run_with_env(
        fixture.path(),
        ["--format", "json", "sync"],
        &[("RUST_LOG", "warn")],
    );
    assert!(
        output.status.success(),
        "sync failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("byte cap") && stderr.contains("data/huge.json"),
        "{stderr}"
    );
    let sync: Value = serde_json::from_slice(&output.stdout).expect("JSON sync output");
    assert_eq!(sync["files_indexed"], 3, "the oversize file gets no row");
    assert_eq!(
        sync["skipped"],
        serde_json::json!({
            "count": 1,
            "entries": [{"path": "data/huge.json", "reason": "oversize"}],
        })
    );
    assert_eq!(sync["failed"]["count"], 0, "{sync}");
    assert!(stderr.contains("1 file(s) larger than"), "{stderr}");
    let matches = run_json(fixture.path(), ["search", "HUGE_MARKER_KEY"]);
    assert_eq!(matches["matches"], serde_json::json!([]));
}

/// Restores a path's permissions on drop, so the fixture can be removed even
/// when an assertion fails.
#[cfg(unix)]
struct RestoreMode(std::path::PathBuf, u32);

#[cfg(unix)]
impl Drop for RestoreMode {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(self.1));
    }
}

#[cfg(unix)]
fn make_unreadable(path: &Path) -> RestoreMode {
    let mode = fs::metadata(path)
        .expect("fixture metadata")
        .permissions()
        .mode();
    fs::set_permissions(path, fs::Permissions::from_mode(0o000)).expect("chmod 000");
    RestoreMode(path.to_path_buf(), mode)
}

#[cfg(unix)]
#[test]
fn real_binary_sync_isolates_an_unreadable_file_and_directory() {
    if skip_as_root("real_binary_sync_isolates_an_unreadable_file_and_directory") {
        return;
    }
    let fixture = fixture_repository();
    fs::write(
        fixture.path().join("src/locked.rs"),
        "pub fn locked() -> i32 { 1 }\n",
    )
    .expect("write unreadable file");
    let sealed = fixture.path().join("src/sealed");
    fs::create_dir_all(&sealed).expect("create unreadable directory");
    fs::write(sealed.join("inner.rs"), "pub fn inner() -> i32 { 2 }\n")
        .expect("write file in unreadable directory");
    let _file = make_unreadable(&fixture.path().join("src/locked.rs"));
    let _dir = make_unreadable(&sealed);

    let output = run_explicit_json(fixture.path(), ["sync"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "sync failed: {stderr}");
    let sync: Value = serde_json::from_slice(&output.stdout).expect("JSON sync output");
    assert_eq!(
        sync["files_indexed"], 3,
        "every readable file is indexed: {sync}"
    );
    assert_eq!(sync["failed"]["count"], 2, "{sync}");
    let failed = sync["failed"]["entries"]
        .as_array()
        .expect("failed entries");
    let entry = |path: &str| {
        failed
            .iter()
            .find(|entry| entry["path"] == path)
            .unwrap_or_else(|| panic!("no failed entry for {path}: {sync}"))
    };
    assert_eq!(
        entry("src/locked.rs")["operation"],
        "read file for content hash"
    );
    assert_eq!(entry("src/locked.rs")["error_kind"], "permission_denied");
    assert_eq!(entry("src/sealed")["operation"], "scan directory");
    assert_eq!(entry("src/sealed")["error_kind"], "permission_denied");
    assert!(
        stderr.contains("warning: 2 path(s) could not be read"),
        "stderr names the count: {stderr}"
    );
    for name in ["helper", "unicode_helper"] {
        let matches = run_json(fixture.path(), ["search", name, "--kind", "symbol"]);
        assert!(
            matches["matches"]
                .as_array()
                .is_some_and(|matches| !matches.is_empty()),
            "{name} missing: {matches}"
        );
    }
}

/// What is already indexed under a path that became unreadable is unknown,
/// not deleted (STD-02 §R29).
#[cfg(unix)]
#[test]
fn real_binary_sync_keeps_what_is_indexed_under_an_unreadable_path() {
    if skip_as_root("real_binary_sync_keeps_what_is_indexed_under_an_unreadable_path") {
        return;
    }
    let fixture = fixture_repository();
    let sealed = fixture.path().join("src/sealed");
    fs::create_dir_all(&sealed).expect("create directory");
    fs::write(
        sealed.join("inner.rs"),
        "pub fn sealed_inner() -> i32 { 2 }\n",
    )
    .expect("write file in directory");
    let first = run_json(fixture.path(), ["sync"]);
    assert_eq!(first["files_indexed"], 4, "{first}");

    let dir = make_unreadable(&sealed);
    let second = run_json(fixture.path(), ["sync"]);
    assert_eq!(second["files_removed"], 0, "{second}");
    assert_eq!(second["files_indexed"], 4, "{second}");
    assert_eq!(second["failed"]["count"], 1, "{second}");
    assert_eq!(second["failed"]["entries"][0]["path"], "src/sealed");
    // Queries read source text, so look once the directory is readable
    // again; no sync has run since.
    drop(dir);
    let matches = run_json(
        fixture.path(),
        ["search", "sealed_inner", "--kind", "symbol"],
    );
    assert!(
        matches["matches"]
            .as_array()
            .is_some_and(|matches| !matches.is_empty()),
        "{matches}"
    );
}

#[cfg(unix)]
#[test]
fn real_binary_sync_fails_when_nothing_could_be_indexed() {
    if skip_as_root("real_binary_sync_fails_when_nothing_could_be_indexed") {
        return;
    }
    let fixture = TempDir::new().expect("create fixture");
    run_git(fixture.path(), ["init", "-b", "main"]);
    fs::write(fixture.path().join("only.rs"), "pub fn only() {}\n").expect("write source");
    let _file = make_unreadable(&fixture.path().join("only.rs"));

    let output = run_explicit_json(fixture.path(), ["sync"]);
    assert_eq!(output.status.code(), Some(1), "nothing indexed must fail");
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error");
    assert_eq!(error["error"]["code"], "graph_error", "{error}");
    let message = error["error"]["message"].as_str().expect("error message");
    assert!(
        message.contains("1 path(s) failed") && message.contains("only.rs"),
        "{message}"
    );
}

#[test]
fn real_binary_sync_names_the_database_and_branch_it_wrote() {
    let fixture = fixture_repository();
    let sync = run_json(fixture.path(), ["sync"]);
    let db_path = run_json(fixture.path(), ["db-path"]);
    assert_eq!(sync["database_path"], db_path["path"], "{sync}");
    assert_eq!(sync["branch"], "main", "{sync}");
    assert_eq!(
        sync["failed"],
        serde_json::json!({"count": 0, "entries": []})
    );
    assert_eq!(
        sync["skipped"],
        serde_json::json!({"count": 0, "entries": []})
    );

    let human = run(fixture.path(), ["--format", "table", "sync"]);
    assert!(human.status.success());
    let table = String::from_utf8_lossy(&human.stdout);
    let database = db_path["path"].as_str().expect("database path");
    assert!(
        table.contains("BRANCH") && table.contains("main") && table.contains("DATABASE"),
        "{table}"
    );
    assert!(table.contains(database), "{table}");
}

#[cfg(unix)]
#[test]
fn real_binary_indexes_every_file_across_bounded_pass1_chunks() {
    let fixture = fixture_repository();
    let generated = fixture.path().join("src/generated");
    fs::create_dir_all(&generated).expect("create generated directory");
    for index in 0..300 {
        fs::write(
            generated.join(format!("unit_{index:03}.rs")),
            format!("pub fn chunked_unit_{index:03}() {{}}\n"),
        )
        .expect("write generated source");
    }

    let sync = run_json(fixture.path(), ["sync"]);
    assert_eq!(sync["files_indexed"], 303);
    assert_eq!(sync["files_changed"], 303);
    for name in ["chunked_unit_000", "chunked_unit_299"] {
        let matches = run_json(fixture.path(), ["search", name, "--kind", "symbol"]);
        assert!(
            matches["matches"]
                .as_array()
                .is_some_and(|matches| !matches.is_empty()),
            "{name} missing: {matches}"
        );
    }
}

#[cfg(unix)]
#[test]
fn real_binary_creates_private_graph_database() {
    let fixture = fixture_repository();
    let _ = run_json(fixture.path(), ["sync", "--full"]);
    let db_path = run_json(fixture.path(), ["db-path"]);
    let db_path = db_path["path"].as_str().expect("database path");
    let mode = fs::metadata(db_path)
        .expect("graph database metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "graph database must be private: {mode:o}");
}

#[test]
fn real_binary_impact_direction_distinguishes_callers_from_callees() {
    let fixture = fixture_repository();
    let _ = run_json(fixture.path(), ["sync", "--full"]);
    let selector = "symbol:src/lib.rs#entry:function";

    let inbound = run_json(
        fixture.path(),
        ["impact", selector, "--direction", "inbound"],
    );
    let outbound = run_json(
        fixture.path(),
        ["impact", selector, "--direction", "outbound"],
    );

    assert_eq!(inbound["direction"], "inbound");
    assert_eq!(outbound["direction"], "outbound");
    let inbound_names = inbound["touched"]
        .as_array()
        .expect("inbound touched")
        .iter()
        .filter_map(|entry| entry["qualified_name"].as_str())
        .collect::<Vec<_>>();
    let outbound_names = outbound["touched"]
        .as_array()
        .expect("outbound touched")
        .iter()
        .filter_map(|entry| entry["qualified_name"].as_str())
        .collect::<Vec<_>>();
    assert!(inbound_names.iter().any(|name| name.ends_with("caller")));
    assert!(outbound_names.iter().any(|name| name.ends_with("helper")));
    assert_ne!(inbound_names, outbound_names);

    let default = run_json(fixture.path(), ["impact", selector]);
    assert!(default.get("direction").is_none());
    let both = run_json(fixture.path(), ["impact", selector, "--direction", "both"]);
    assert_eq!(both["direction"], "both");
    let mut both_without_direction = both;
    both_without_direction
        .as_object_mut()
        .expect("impact result object")
        .remove("direction");
    assert_eq!(default, both_without_direction);
}

#[test]
fn real_binary_rejects_malformed_selectors_with_json_error() {
    let fixture = fixture_repository();
    let human = run(fixture.path(), ["show", "not-a-selector"]);
    assert_eq!(human.status.code(), Some(1));
    assert!(human.stdout.is_empty());
    assert!(String::from_utf8_lossy(&human.stderr).contains("selectors must start with"));

    let output = run_explicit_json(fixture.path(), ["show", "not-a-selector"]);

    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error payload");
    assert_eq!(error["error"]["code"], "selector_parse_error");
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| { message.contains("selectors must start with") })
    );
}

#[test]
fn real_binary_show_rejects_database_path_outside_worktree() {
    let parent = TempDir::new().expect("create confinement fixture");
    let repo = parent.path().join("repo");
    fs::create_dir(&repo).expect("create repository directory");
    run_git(&repo, ["init", "-b", "main"]);
    run_git(&repo, ["config", "user.email", "graph@example.invalid"]);
    run_git(&repo, ["config", "user.name", "Graph Test"]);
    fs::create_dir(repo.join("src")).expect("create source directory");
    fs::write(repo.join("src/leak.rs"), "pub fn leak() -> i32 { 1 }\n").expect("write leak");
    fs::write(
        repo.join("src/abs.rs"),
        "pub fn absolute_leak() -> i32 { 1 }\n",
    )
    .expect("write abs");
    fs::write(repo.join("src/keep.rs"), "pub fn keep() -> i32 { 2 }\n").expect("write keep");
    run_git(&repo, ["add", "."]);
    run_git(&repo, ["commit", "-m", "seed"]);

    let seeded = run_json(&repo, ["sync", "--full"]);
    assert!(
        seeded["files_indexed"]
            .as_u64()
            .is_some_and(|count| count >= 3)
    );
    let inside = run_json(&repo, ["show", "symbol:src/leak.rs#leak:function"]);
    assert!(
        inside["source"]
            .as_str()
            .is_some_and(|source| source.contains("pub fn leak"))
    );

    let parent_marker = "PATH_TRAVERSAL_MARKER_12899";
    let absolute_marker = "ABSOLUTE_PATH_MARKER_12899";
    let parent_outside = parent.path().join("outside.rs");
    let absolute_outside = parent.path().join("absolute.rs");
    let parent_source = format!("{parent_marker}\n");
    let absolute_source = format!("{absolute_marker}\n");
    fs::write(&parent_outside, &parent_source).expect("write outside file");
    fs::write(&absolute_outside, &absolute_source).expect("write absolute file");
    let absolute_stored = absolute_outside
        .canonicalize()
        .expect("canonical absolute file")
        .to_string_lossy()
        .into_owned();

    let db_path = run_json(&repo, ["db-path"]);
    let db_path = db_path["path"].as_str().expect("database path");
    inject_outside_source_path(
        db_path,
        "src/leak.rs",
        "../outside.rs",
        "leak",
        parent_source.len(),
    );
    inject_outside_source_path(
        db_path,
        "src/abs.rs",
        &absolute_stored,
        "absolute_leak",
        absolute_source.len(),
    );

    let parent_selector = "symbol:../outside.rs#leak:function";
    let absolute_selector = format!("symbol:{absolute_stored}#absolute_leak:function");
    assert_show_hides_outside_source(&repo, parent_selector, parent_marker);
    assert_show_hides_outside_source(&repo, parent_selector, absolute_marker);
    assert_show_hides_outside_source(&repo, &absolute_selector, absolute_marker);
    assert_show_hides_outside_source(&repo, &absolute_selector, parent_marker);

    let kept = run_json(&repo, ["show", "symbol:src/keep.rs#keep:function"]);
    let kept_source = kept["source"].as_str().expect("kept source");
    assert!(kept_source.contains("pub fn keep"));
    assert!(!kept_source.contains(parent_marker));
    assert!(!kept_source.contains(absolute_marker));
}

fn assert_show_hides_outside_source(repo: &Path, selector: &str, marker: &str) {
    let output = run_explicit_json(repo, ["show", selector]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "show accepted an outside database path: {stderr}"
    );
    assert!(!stdout.contains(marker), "{stdout}");
    assert!(!stderr.contains(marker), "{stderr}");
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error payload");
    assert_eq!(error["error"]["code"], "graph_error");
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("worktree")),
        "{error}"
    );
}

fn inject_outside_source_path(db_path: &str, from: &str, to: &str, symbol: &str, span_end: usize) {
    let script = r#"
import sqlite3, sys
db, old, new, symbol, span_end = sys.argv[1:6]
span_end = int(span_end)
conn = sqlite3.connect(db)
conn.execute("PRAGMA busy_timeout=5000")
conn.execute("PRAGMA foreign_keys=OFF")
symbols = conn.execute(
    "UPDATE symbols SET file_path=?, span_start=0, span_end=? WHERE file_path=? AND name=? AND kind='function'",
    (new, span_end, old, symbol),
)
files = conn.execute(
    "UPDATE files SET path=?, byte_len=? WHERE path=?",
    (new, span_end, old),
)
if symbols.rowcount != 1 or files.rowcount != 1:
    raise SystemExit(
        f"injection missed symbols={symbols.rowcount} files={files.rowcount} for {old}"
    )
conn.commit()
"#;
    let output = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(db_path)
        .arg(from)
        .arg(to)
        .arg(symbol)
        .arg(span_end.to_string())
        .output()
        .expect("run python3 sqlite injection");
    assert!(
        output.status.success(),
        "sqlite injection failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn real_binary_help_succeeds() {
    let fixture = fixture_repository();
    let output = run(fixture.path(), ["--help"]);

    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("Usage: orbit-graph [OPTIONS] <COMMAND>"));
    for heading in [
        "Explore code:",
        "Follow relationships:",
        "Recommendations and history:",
        "Index and utilities:",
    ] {
        assert!(help.contains(heading), "missing help heading: {heading}");
    }
    for (command, description) in [
        ("overview", "Summarize indexed files and symbols"),
        (
            "search",
            "Search indexed symbols, strings, and configuration keys",
        ),
        ("show", "Show source and metadata for a graph selector"),
        ("refs", "List references to a symbol"),
        ("callees", "List outbound calls from a function or command"),
        ("implementors", "Find implementations of a trait"),
        ("deps", "List source-level imports for a file or directory"),
        (
            "trace",
            "Trace outbound calls from a discovered CLI command handler",
        ),
        ("impact", "Traverse the bounded graph around a selector"),
        (
            "recommend",
            "Recommend current change destinations from historical evidence",
        ),
        (
            "history",
            "Inspect and maintain historical delivery evidence",
        ),
        (
            "evaluate",
            "Run chronological or live leakage-safe recommendation evaluation",
        ),
        ("sync", "Update or rebuild the source graph index"),
        ("db-path", "Print the current graph database path"),
        (
            "clean",
            "Report obsolete graph databases; delete them with --confirm",
        ),
        (
            "version",
            "Print crate, extractor, and store schema versions",
        ),
    ] {
        assert!(
            help.lines().any(|line| {
                line.trim_start().starts_with(command) && line.contains(description)
            }),
            "missing command description: {command}"
        );
    }
    assert!(help.contains("orbit-graph <COMMAND> --help"));
    assert!(output.stderr.is_empty());
    assert!(!help.contains('\u{1b}'));

    let bare = run(fixture.path(), []);
    assert!(bare.status.success());
    assert_eq!(bare.stdout, output.stdout);
    assert!(bare.stderr.is_empty());

    let help_subcommand = run(fixture.path(), ["help"]);
    assert!(help_subcommand.status.success());
    assert_eq!(help_subcommand.stdout, output.stdout);
    assert!(help_subcommand.stderr.is_empty());

    for env in [[("NO_COLOR", "1")], [("TERM", "dumb")]] {
        let plain = run_with_env(fixture.path(), ["--help"], &env);
        assert!(plain.status.success());
        assert!(!String::from_utf8_lossy(&plain.stdout).contains('\u{1b}'));
        assert!(plain.stderr.is_empty());
    }

    for args in [["overview", "--help"], ["history", "--help"]] {
        let command_help = run(fixture.path(), args);
        assert!(command_help.status.success());
        assert!(String::from_utf8_lossy(&command_help.stdout).contains("Usage: orbit-graph"));
        assert!(command_help.stderr.is_empty());
    }

    let root_mode_help = run(fixture.path(), ["--format", "json", "--help"]);
    let nested_mode_help = run(fixture.path(), ["--format", "json", "refs", "--help"]);
    for explicit_mode_help in [root_mode_help, nested_mode_help] {
        assert!(explicit_mode_help.status.success());
        assert!(String::from_utf8_lossy(&explicit_mode_help.stdout).contains("Usage:"));
        assert!(explicit_mode_help.stderr.is_empty());
    }
}

#[test]
fn real_binary_unknown_command_keeps_json_error_protocol() {
    let fixture = fixture_repository();
    let output = run_explicit_json(fixture.path(), ["not-a-command"]);

    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error payload");
    assert_eq!(error["error"]["code"], "argument_error");
}

#[test]
fn real_binary_usage_errors_are_human_by_default_and_machine_readable_on_request() {
    let fixture = fixture_repository();

    for args in [["refs"], ["trace"]] {
        let human = run(fixture.path(), args);
        assert_eq!(human.status.code(), Some(2));
        assert!(human.stdout.is_empty());
        let diagnostic = String::from_utf8_lossy(&human.stderr);
        assert!(diagnostic.contains("required arguments"), "{diagnostic}");
        assert!(diagnostic.contains("Usage: orbit-graph"), "{diagnostic}");
        assert!(
            !diagnostic.contains("\\n"),
            "escaped diagnostic: {diagnostic}"
        );

        let json = run_explicit_json(fixture.path(), args);
        assert_eq!(json.status.code(), Some(2));
        assert!(json.stdout.is_empty());
        let envelope: Value = serde_json::from_slice(&json.stderr).expect("structured usage error");
        assert_eq!(envelope["error"]["code"], "argument_error");
        assert!(
            envelope["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("Usage: orbit-graph"))
        );
    }

    let ndjson = run(fixture.path(), ["--format", "ndjson", "refs"]);
    assert_eq!(ndjson.status.code(), Some(2));
    let envelope: Value = serde_json::from_slice(&ndjson.stderr).expect("NDJSON error envelope");
    assert_eq!(envelope["error"]["code"], "argument_error");

    let namespace = run(fixture.path(), ["history"]);
    assert_eq!(namespace.status.code(), Some(2));
    assert!(namespace.stdout.is_empty());
    let help = String::from_utf8_lossy(&namespace.stderr);
    assert!(help.contains("Usage: orbit-graph history"));
    for subcommand in ["import", "sync", "status", "rebuild"] {
        assert!(help.contains(subcommand), "missing {subcommand} in {help}");
    }
}

#[test]
fn real_binary_resolves_environment_modes_and_overview_format_without_ambiguity() {
    let fixture = fixture_repository();

    let environment_json = run_with_env(
        fixture.path(),
        ["version"],
        &[("ORBIT_GRAPH_FORMAT", "json")],
    );
    assert!(environment_json.status.success());
    assert_eq!(
        environment_json
            .stdout
            .iter()
            .filter(|byte| **byte == b'\n')
            .count(),
        1
    );
    let version: Value =
        serde_json::from_slice(&environment_json.stdout).expect("environment JSON mode");
    assert!(version["crate_version"].is_string());
    assert_eq!(
        version["store_schema_version"],
        orbit_graph::STORE_SCHEMA_VERSION
    );

    let explicit_table = run_with_env(
        fixture.path(),
        ["--format", "table", "version"],
        &[("ORBIT_GRAPH_FORMAT", "json")],
    );
    assert!(explicit_table.status.success());
    assert!(
        String::from_utf8_lossy(&explicit_table.stdout).starts_with("CRATE VERSION"),
        "explicit mode did not outrank environment: {}",
        String::from_utf8_lossy(&explicit_table.stdout)
    );

    let ndjson = run(fixture.path(), ["version", "--format", "ndjson"]);
    assert!(ndjson.status.success());
    assert_eq!(
        ndjson.stdout.iter().filter(|byte| **byte == b'\n').count(),
        1
    );
    serde_json::from_slice::<Value>(&ndjson.stdout).expect("one NDJSON detail record");

    let _ = run_json(fixture.path(), ["sync", "--full"]);
    let overview = run(
        fixture.path(),
        ["--format", "json", "overview", "--format", "full"],
    );
    assert!(
        overview.status.success(),
        "{}",
        String::from_utf8_lossy(&overview.stderr)
    );
    let overview: Value = serde_json::from_slice(&overview.stdout).expect("overview JSON");
    assert!(
        overview["files"]
            .as_array()
            .is_some_and(|files| !files.is_empty())
    );

    for environment in [[("NO_COLOR", "1")], [("TERM", "dumb")]] {
        let plain = run_with_env(
            fixture.path(),
            ["--format", "table", "version"],
            &environment,
        );
        assert!(plain.status.success());
        assert!(!String::from_utf8_lossy(&plain.stdout).contains('\u{1b}'));
    }
}

#[test]
fn real_binary_renders_exploration_views_and_lossless_record_units() {
    let fixture = fixture_repository();
    let _ = run_json(fixture.path(), ["sync", "--full"]);

    let commands: &[(&[&str], &[&str])] = &[
        (
            &["overview", "--format", "full"],
            &["FILES", "SYMBOLS", "src/lib.rs"],
        ),
        (&["search", "helper"], &["KIND", "MATCH", "PATH", "helper"]),
        (
            &["show", "symbol:src/lib.rs#entry:function"],
            &["KIND", "FILE", "Source:", "pub fn entry"],
        ),
        (
            &[
                "refs",
                "symbol:src/lib.rs#helper:function",
                "--confidence",
                "fuzzy",
            ],
            &["RECORD", "FILE", "CONFIDENCE", "src/lib.rs"],
        ),
        (
            &["callees", "symbol:src/lib.rs#entry:function"],
            &["TARGET", "CALL NAME", "helper"],
        ),
        (
            &["implementors", "symbol:src/lib.rs#Renderer:trait"],
            &["TYPE", "TRAIT", "Human", "Renderer"],
        ),
        (
            &["deps", "file:src/lib.rs"],
            &["FROM FILE", "TARGET PATH", "std::fmt"],
        ),
        (
            &["trace", "command:ship", "--depth", "2"],
            &["DEPTH", "TRAVERSAL", "ship", "helper"],
        ),
        (
            &[
                "impact",
                "symbol:src/lib.rs#helper:function",
                "--depth",
                "2",
                "--confidence",
                "fuzzy",
            ],
            &["SET", "DISTANCE", "SYMBOL", "entry"],
        ),
    ];
    for (args, expected) in commands {
        let mut table_args = vec!["--format", "table"];
        table_args.extend_from_slice(args);
        let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
            .current_dir(fixture.path())
            .args(&table_args)
            .output()
            .expect("run human exploration view");
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let human = String::from_utf8(output.stdout).expect("human view is UTF-8");
        assert!(!human.trim_start().starts_with('{'), "{args:?}: {human}");
        for needle in *expected {
            assert!(
                human.contains(needle),
                "{args:?} missing {needle:?}: {human}"
            );
        }

        let mut json_args = vec!["--format", "json"];
        json_args.extend_from_slice(args);
        let json = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
            .current_dir(fixture.path())
            .args(&json_args)
            .output()
            .expect("run exploration JSON");
        assert!(json.status.success(), "{args:?}");
        let document =
            serde_json::from_slice::<Value>(&json.stdout).expect("lossless JSON document");

        let mut ndjson_args = vec!["--format", "ndjson"];
        ndjson_args.extend_from_slice(args);
        let ndjson = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
            .current_dir(fixture.path())
            .args(&ndjson_args)
            .output()
            .expect("run exploration NDJSON");
        assert!(ndjson.status.success(), "{args:?}");
        let records = parse_ndjson(&ndjson.stdout);
        assert!(!records.is_empty(), "{args:?} has no NDJSON record unit");
        assert_exploration_record_units(args[0], &document, &records);
    }

    let plain = run(fixture.path(), ["search", "unicode_helper"]);
    assert!(plain.status.success());
    let plain = String::from_utf8(plain.stdout).expect("plain view is UTF-8");
    assert!(!plain.starts_with("KIND"));
    assert_eq!(plain.lines().count(), 1);
    assert_eq!(plain.trim_end().split('\t').count(), 4);
    assert!(plain.contains("界界"));
    assert!(plain.contains("e\u{301}_very_long_component_name.rs"));

    for args in [
        vec!["search", "definitely_absent"],
        vec!["callees", "symbol:src/lib.rs#helper:function"],
        vec!["implementors", "symbol:src/lib.rs#Missing:trait"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
            .current_dir(fixture.path())
            .args(args)
            .output()
            .expect("run empty exploration view");
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn real_binary_adapts_unicode_paths_to_narrow_terminals() {
    let fixture = fixture_repository();
    let _ = run_json(fixture.path(), ["sync", "--full"]);

    let (wide, wide_stderr) = run_in_pty(fixture.path(), &["search", "unicode_helper"], 64);
    assert!(wide.contains('…'), "{wide}");
    assert!(wide.contains("界"), "{wide}");
    assert_eq!(wide.lines().count(), 2, "{wide}");
    assert!(wide_stderr.is_empty(), "{wide_stderr}");

    let (narrow, narrow_stderr) = run_in_pty(fixture.path(), &["search", "unicode_helper"], 20);
    assert_eq!(narrow.lines().count(), 2, "{narrow}");
    assert!(narrow_stderr.contains("omitted columns"), "{narrow_stderr}");
}

#[cfg(unix)]
#[test]
fn real_binary_treats_a_closed_stdout_pipe_as_success() {
    let fixture = fixture_repository();
    let (reader, writer) = UnixStream::pair().expect("create stdout pipe");
    drop(reader);
    let writer: OwnedFd = writer.into();
    let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(fixture.path())
        .args(["--format", "json", "version"])
        .stdout(Stdio::from(writer))
        .output()
        .expect("run with closed stdout pipe");

    assert!(
        output.status.success(),
        "broken pipe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn real_binary_recommends_in_file_and_symbol_modes_and_validates_top_k() {
    let fixture = fixture_repository();
    let _ = run_json(fixture.path(), ["sync", "--full"]);
    init_empty_history(fixture.path());

    let files = run_json(
        fixture.path(),
        [
            "recommend",
            "--query",
            "helper",
            "--level",
            "file",
            "--limit",
            "1",
        ],
    );
    assert_eq!(
        files["resolved_target_revision"],
        git_stdout(fixture.path(), ["rev-parse", "HEAD"])
    );
    assert_eq!(files["recommendations"].as_array().map(Vec::len), Some(1));
    assert_eq!(files["recommendations"][0]["selector"], "file:src/lib.rs");
    assert!(files["source_freshness"]["status"].is_string());

    let symbols = run_json(
        fixture.path(),
        [
            "recommend",
            "--query",
            "helper",
            "--level",
            "symbol",
            "--limit",
            "2",
        ],
    );
    assert!(symbols["recommendations"].as_array().is_some_and(|values| {
        values.iter().any(|value| {
            value["selector"]
                .as_str()
                .is_some_and(|selector| selector.contains("#helper:function"))
        })
    }));

    let bad_limit = run_explicit_json(
        fixture.path(),
        ["recommend", "--query", "helper", "--limit", "0"],
    );
    assert!(!bad_limit.status.success());
    let error: Value = serde_json::from_slice(&bad_limit.stderr).expect("JSON error");
    assert_eq!(error["error"]["code"], "graph_error");

    let both = run_explicit_json(
        fixture.path(),
        ["recommend", "--query", "helper", "--task-id", "TASK-1"],
    );
    assert!(!both.status.success());
    let error: Value = serde_json::from_slice(&both.stderr).expect("JSON error");
    assert_eq!(error["error"]["code"], "argument_error");
}

#[test]
fn real_binary_renders_recommendation_and_index_views_with_complete_record_boundaries() {
    let fixture = fixture_repository();
    let sync = run(fixture.path(), ["sync", "--full"]);
    assert!(sync.status.success());
    init_empty_history(fixture.path());
    let sync_fields = String::from_utf8(sync.stdout)
        .expect("sync plain UTF-8")
        .trim_end()
        .split('\t')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    // Counts and duration first, then the branch and database written.
    assert_eq!(sync_fields.len(), 8, "{sync_fields:?}");
    assert!(
        sync_fields[..6]
            .iter()
            .all(|field| field.parse::<u128>().is_ok()),
        "{sync_fields:?}"
    );
    assert_eq!(sync_fields[6], "main");
    assert!(sync_fields[7].ends_with(".db"), "{sync_fields:?}");
    let sync_ndjson = run(fixture.path(), ["sync", "--format", "ndjson"]);
    let sync_ndjson = parse_ndjson(&sync_ndjson.stdout);
    assert_eq!(sync_ndjson.len(), 1);
    assert!(sync_ndjson[0]["files_indexed"].is_number());

    let plain = run(
        fixture.path(),
        [
            "recommend",
            "--query",
            "helper",
            "--level",
            "file",
            "--limit",
            "1",
        ],
    );
    assert!(plain.status.success());
    assert!(plain.stderr.is_empty());
    let fields = String::from_utf8(plain.stdout)
        .expect("recommendation plain UTF-8")
        .trim_end()
        .split('\t')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(fields.len(), 5);
    assert_eq!(fields[0], "1");
    assert_eq!(fields[1], "file:src/lib.rs");
    assert!(fields[2].parse::<f64>().is_ok());
    assert!(fields[3].contains("current destination text overlaps the query"));
    assert!(fields[3].contains("cold_start"));
    assert!(fields[4].contains("unavailable"));

    let empty = run(
        fixture.path(),
        ["recommend", "--query", "definitely_absent_destination"],
    );
    assert!(empty.status.success());
    assert!(empty.stdout.is_empty());
    let empty_diagnostic = String::from_utf8_lossy(&empty.stderr);
    assert!(empty_diagnostic.contains("no recommendations"));
    assert!(empty_diagnostic.contains("cold_start"));

    let table = run(
        fixture.path(),
        [
            "--format",
            "table",
            "recommend",
            "--query",
            "helper",
            "--limit",
            "1",
        ],
    );
    assert!(table.status.success());
    let table = String::from_utf8_lossy(&table.stdout);
    assert!(table.starts_with("RANK"));
    assert!(table.contains("SELECTOR"));
    assert!(table.contains("EVIDENCE"));
    assert!(!table.contains('\u{1b}'));

    let ndjson = run(
        fixture.path(),
        [
            "--format",
            "ndjson",
            "recommend",
            "--query",
            "helper",
            "--limit",
            "1",
        ],
    );
    assert!(ndjson.status.success());
    let records = parse_ndjson(&ndjson.stdout);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["record_type"], "recommendation_context");
    assert!(records[0]["context"]["source_freshness"].is_object());
    assert!(records[0]["context"]["fallbacks"].is_array());
    assert!(records[0]["context"].get("recommendations").is_none());
    assert_eq!(records[1]["record_type"], "recommendation");
    assert_eq!(records[1]["recommendation"]["rank"], 1);
    assert!(records[1]["recommendation"]["reasons"].is_array());

    let version = run(fixture.path(), ["version"]);
    assert!(version.status.success());
    let version = String::from_utf8(version.stdout).expect("version plain UTF-8");
    assert_eq!(version.trim_end().split('\t').count(), 5);

    let db_path = run(fixture.path(), ["db-path"]);
    assert!(db_path.status.success());
    let db_path = String::from_utf8(db_path.stdout).expect("db-path plain UTF-8");
    let db_path = db_path.trim_end().split('\t').collect::<Vec<_>>();
    assert_eq!(db_path.len(), 4, "{db_path:?}");
    assert_eq!(db_path[3], "true", "the synced database exists");
    let db_path_ndjson = run(fixture.path(), ["db-path", "--format", "ndjson"]);
    let db_path_ndjson = parse_ndjson(&db_path_ndjson.stdout);
    assert_eq!(db_path_ndjson.len(), 1);
    assert!(db_path_ndjson[0]["path"].is_string());

    let old_db = fixture.path().join(".orbit-graph/main.1.db");
    fs::write(&old_db, b"stale").expect("write obsolete database");
    let expected_old_db_path = old_db.canonicalize().expect("canonical obsolete database");
    let plan = run(fixture.path(), ["--format", "ndjson", "clean"]);
    assert!(plan.status.success());
    let plan = parse_ndjson(&plan.stdout);
    assert_eq!(plan[0]["record_type"], "clean_context");
    assert_eq!(plan[0]["context"]["applied"], false);
    assert_eq!(plan[1]["record_type"], "would_delete_database");
    assert_eq!(plan[1]["reason"], "older_extractor_version");
    assert!(old_db.exists(), "a plan deletes nothing");
    let clean = run(fixture.path(), ["--format", "ndjson", "clean", "--confirm"]);
    assert!(clean.status.success());
    let clean = parse_ndjson(&clean.stdout);
    assert_eq!(clean[0]["record_type"], "clean_context");
    assert_eq!(clean[0]["context"]["applied"], true);
    assert_eq!(clean[1]["record_type"], "deleted_database");
    assert_eq!(
        clean[1]["path"],
        expected_old_db_path.to_string_lossy().as_ref()
    );

    let other_old_db = fixture.path().join(".orbit-graph/main.2.db");
    fs::write(&other_old_db, b"stale").expect("write second obsolete database");
    let expected_other_old_db_path = other_old_db
        .canonicalize()
        .expect("canonical second obsolete database");
    let clean_human = run(fixture.path(), ["clean", "--confirm"]);
    assert!(clean_human.status.success());
    let clean_human = String::from_utf8_lossy(&clean_human.stdout);
    assert!(clean_human.contains("\ttrue\t0\t1\n"), "{clean_human}");
    assert!(
        clean_human.contains(
            format!(
                "deleted\tolder_extractor_version\t{}\n",
                expected_other_old_db_path.display()
            )
            .as_str()
        )
    );
}

#[test]
fn real_binary_clean_reports_by_default_and_deletes_exactly_the_report_with_confirm() {
    let fixture = fixture_repository();
    let [unreachable] = index_unreachable_detached_commits(fixture.path(), ["dropped"]);
    let graph_dir = fixture
        .path()
        .join(".orbit-graph")
        .canonicalize()
        .expect("canonical graph directory");
    fs::write(graph_dir.join("main.1.db"), b"stale").expect("write obsolete database");
    fs::write(graph_dir.join("main.1.db-wal"), b"stale").expect("write obsolete WAL");
    let newer = graph_dir.join(format!("main.{}.db", orbit_graph::EXTRACTOR_VERSION + 1));
    fs::write(&newer, b"newer").expect("write newer database");

    let before = digest_tree(fixture.path());
    let plan = run_explicit_json(fixture.path(), ["clean"]);
    assert!(plan.status.success(), "{plan:?}");
    assert_eq!(
        digest_tree(fixture.path()),
        before,
        "clean without --confirm changes nothing"
    );
    let stderr = String::from_utf8_lossy(&plan.stderr);
    assert!(
        stderr.contains("dry run") && stderr.contains("--confirm"),
        "{stderr}"
    );
    let plan: Value = serde_json::from_slice(&plan.stdout).expect("plan JSON");
    assert_eq!(plan["applied"], false, "{plan}");
    assert_eq!(plan["deleted"], serde_json::json!([]), "{plan}");
    let planned = listed_paths(&plan["would_delete"]);
    let detached = format!("detached-{}", &unreachable[..12]);
    assert!(
        planned.contains(&graph_dir.join("main.1.db").display().to_string())
            && planned.contains(&graph_dir.join("main.1.db-wal").display().to_string())
            && planned.iter().any(|path| path.contains(detached.as_str())),
        "{plan}"
    );
    assert!(
        plan["kept"]
            .as_array()
            .is_some_and(|kept| kept.iter().any(|item| {
                item["path"] == newer.display().to_string()
                    && item["reason"] == "newer_extractor_version"
            })),
        "{plan}"
    );

    let applied = run_json(fixture.path(), ["clean", "--confirm"]);
    assert_eq!(applied["applied"], true, "{applied}");
    assert_eq!(listed_paths(&applied["would_delete"]), planned, "{applied}");
    let deleted: Vec<String> = applied["deleted"]
        .as_array()
        .expect("deleted paths")
        .iter()
        .filter_map(|path| path.as_str().map(str::to_string))
        .collect();
    assert_eq!(deleted, planned, "{applied}");
    let after = digest_tree(fixture.path());
    let expected: std::collections::BTreeMap<_, _> = before
        .into_iter()
        .filter(|(path, _)| !planned.contains(&path.display().to_string()))
        .collect();
    assert_eq!(after, expected, "exactly the listed files are gone");
}

#[test]
fn real_binary_clean_keeps_a_detached_database_git_cannot_prove_stale() {
    let fixture = fixture_repository();
    let [ambiguous, corrupt, gone] =
        index_unreachable_detached_commits(fixture.path(), ["ambiguous", "corrupt", "gone"]);
    let objects = fixture.path().join(".git/objects");
    let loose = |sha: &str| objects.join(&sha[..2]).join(&sha[2..]);
    // A second loose object sharing the first 12 hex digits makes the prefix
    // ambiguous.
    let twin_tail = if ambiguous.ends_with('0') { "1" } else { "0" };
    fs::copy(
        loose(ambiguous.as_str()),
        objects
            .join(&ambiguous[..2])
            .join(format!("{}{twin_tail}", &ambiguous[2..39])),
    )
    .expect("write an ambiguous twin object");
    // An unreadable object is an object-database error, not proof of absence.
    let corrupt_object = loose(corrupt.as_str());
    fs::remove_file(&corrupt_object).expect("remove the loose commit object");
    fs::write(&corrupt_object, b"not a zlib stream").expect("corrupt the commit object");
    // A commit whose object is gone is the one case Git proves stale.
    fs::remove_file(loose(gone.as_str())).expect("remove the loose commit object");

    let before = digest_tree(fixture.path());
    let plan = run_json(fixture.path(), ["clean"]);
    assert_eq!(digest_tree(fixture.path()), before, "{plan}");
    let applied = run_json(fixture.path(), ["clean", "--confirm"]);
    for sha in [&ambiguous, &corrupt] {
        let prefix = format!("detached-{}", &sha[..12]);
        for report in [&plan, &applied] {
            assert!(
                report["kept"]
                    .as_array()
                    .is_some_and(|kept| kept.iter().any(|item| {
                        item["path"]
                            .as_str()
                            .is_some_and(|path| path.contains(prefix.as_str()))
                            && item["reason"] == "unverifiable"
                            && item["detail"]
                                .as_str()
                                .is_some_and(|detail| !detail.is_empty())
                    })),
                "{prefix} is kept and reported: {report}"
            );
        }
        let kept_file = fs::read_dir(fixture.path().join(".orbit-graph"))
            .expect("read graph directory")
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(prefix.as_str())
            });
        assert!(kept_file, "{prefix} survives clean --confirm: {applied}");
    }
    let gone_prefix = format!("detached-{}", &gone[..12]);
    assert!(
        listed_paths(&applied["deleted"])
            .iter()
            .any(|path| path.contains(gone_prefix.as_str())),
        "a commit Git reports not found is deleted: {applied}"
    );
}

/// Commit each name on its own throwaway branch, index each commit from a
/// detached HEAD and `main` normally, then delete the branches, so no ref
/// reaches the indexed commits. The branches outlive every sync, so no sync's
/// own cleanup removes an index made earlier.
fn index_unreachable_detached_commits<const N: usize>(
    root: &Path,
    names: [&str; N],
) -> [String; N] {
    let shas = names.map(|name| {
        run_git(root, ["checkout", "-q", "-b", name, "main"]);
        fs::write(
            root.join("src").join(format!("{name}.rs")),
            format!("pub fn {name}() {{}}\n"),
        )
        .expect("write branch source");
        run_git(root, ["add", "."]);
        run_git(root, ["commit", "-q", "-m", name]);
        let sha = git_stdout(root, ["rev-parse", "HEAD"]);
        run_git(root, ["checkout", "-q", "main"]);
        sha
    });
    for sha in &shas {
        run_git(root, ["checkout", "-q", "--detach", sha.as_str()]);
        run_json(root, ["sync"]);
    }
    run_git(root, ["checkout", "-q", "main"]);
    run_json(root, ["sync"]);
    for name in names {
        run_git(root, ["branch", "-q", "-D", name]);
    }
    shas
}

fn listed_paths(items: &Value) -> Vec<String> {
    items
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("path").unwrap_or(item).as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Every path under `root`, canonical, with a file's bytes (`None` for a
/// directory), `.git` included.
fn digest_tree(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>> {
    let mut digest = std::collections::BTreeMap::new();
    let mut pending = vec![root.canonicalize().expect("canonical root")];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir.as_path()).expect("read directory") {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                digest.insert(path.clone(), None);
                pending.push(path);
            } else {
                let bytes = fs::read(path.as_path()).expect("read file");
                digest.insert(path, Some(bytes));
            }
        }
    }
    digest
}

#[test]
fn real_binary_expands_corpus_cochanges_without_temporal_or_duplicate_leakage() {
    let fixture = TempDir::new().expect("create recommendation fixture");
    run_git(fixture.path(), ["init", "-b", "main"]);
    run_git(
        fixture.path(),
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(fixture.path(), ["config", "user.name", "Graph Test"]);
    fs::write(fixture.path().join("a.rs"), "pub fn alpha() -> i32 { 0 }\n").expect("write a");
    fs::write(fixture.path().join("b.rs"), "pub fn beta() -> i32 { 0 }\n").expect("write b");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "base"]);

    let before_one = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    fs::write(fixture.path().join("a.rs"), "pub fn alpha() -> i32 { 1 }\n").expect("edit a one");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "one"]);
    let after_one = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);

    fs::write(fixture.path().join("a.rs"), "pub fn alpha() -> i32 { 2 }\n").expect("edit a two");
    fs::write(fixture.path().join("b.rs"), "pub fn beta() -> i32 { 2 }\n").expect("edit b two");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "two"]);
    let after_two = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);

    fs::write(fixture.path().join("a.rs"), "pub fn alpha() -> i32 { 3 }\n").expect("edit a three");
    fs::write(fixture.path().join("b.rs"), "pub fn beta() -> i32 { 3 }\n").expect("edit b three");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "three"]);
    let after_three = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);

    fs::write(fixture.path().join("b.rs"), "pub fn beta() -> i32 { 4 }\n").expect("edit b future");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "future"]);
    let after_future = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);

    import_cli_delivery(
        fixture.path(),
        &before_one,
        &after_one,
        "D1",
        "TASK-1",
        "needle",
        "2000-01-01T00:00:01Z",
        "1999-12-31T00:00:00Z",
    );
    import_cli_delivery(
        fixture.path(),
        &after_one,
        &after_two,
        "D2",
        "TASK-2",
        "unrelated",
        "2000-01-01T00:00:02Z",
        "1999-12-31T00:00:00Z",
    );
    import_cli_delivery(
        fixture.path(),
        &after_one,
        &after_two,
        "D2-ALIAS",
        "TASK-2",
        "unrelated",
        "2000-01-01T00:00:02Z",
        "1999-12-31T00:00:00Z",
    );
    import_cli_delivery(
        fixture.path(),
        &after_two,
        &after_three,
        "D3",
        "TASK-3",
        "unrelated",
        "2000-01-01T00:00:03Z",
        "1999-12-31T00:00:00Z",
    );
    import_cli_delivery(
        fixture.path(),
        &after_three,
        &after_future,
        "D-FUTURE",
        "PENDING",
        "futuresecret",
        "2001-01-01T00:00:00.900Z",
        "2000-12-31T00:00:00Z",
    );

    let cutoff = "2001-01-01T00:00:00.100Z";
    let result = run_json(
        fixture.path(),
        [
            "recommend",
            "--query",
            "needle",
            "--level",
            "file",
            "--cutoff",
            cutoff,
        ],
    );
    let beta = result["recommendations"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["selector"] == "file:b.rs"))
        .expect("co-change destination b");
    assert_eq!(beta["association"]["support"], 2);
    assert_eq!(beta["association"]["source_count"], 3);
    assert_eq!(beta["association"]["destination_count"], 2);
    assert_eq!(beta["association"]["lift"], 1.0);
    assert_eq!(beta["counts"]["eligible_history_deliveries"], 3);
    assert!(beta["reasons"].as_array().is_some_and(|reasons| {
        reasons.iter().any(|reason| {
            reason["kind"] == "directional_cochange"
                && reason["explanation"]
                    .as_str()
                    .is_some_and(|text| text.contains("D2") && text.contains("D2-ALIAS"))
        })
    }));

    let future = run_json(
        fixture.path(),
        ["recommend", "--query", "futuresecret", "--cutoff", cutoff],
    );
    assert!(
        future["recommendations"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    let future_text = run_json(
        fixture.path(),
        ["recommend", "--query", "futuretext", "--cutoff", cutoff],
    );
    assert!(
        future_text["recommendations"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );

    let _ = run_json(
        fixture.path(),
        ["history", "sync", "--branch", "main", "--limit", "20"],
    );
    let snapshot_path = fixture.path().join("pending-snapshot.json");
    let snapshot = serde_json::json!({
        "task_id": "PENDING",
        "title": "needle",
        "description": "pending work before any delivery",
        "acceptance_criteria": ["recommend likely files"],
        "source": {"system": "task_service", "record_id": "PENDING@1"},
        "created_at": {
            "status": "known", "timestamp": "1999-12-30T00:00:00Z",
            "source": {"system": "task_service"}
        },
        "snapshot_available_at": {
            "status": "known", "timestamp": "1999-12-31T00:00:00Z",
            "source": {"system": "task_service", "record_id": "PENDING@1"}
        },
        "text_availability": "known_pre_execution",
        "captured_at": "1999-12-31T00:00:00Z"
    });
    fs::write(
        &snapshot_path,
        serde_json::to_vec(&snapshot).expect("encode snapshot"),
    )
    .expect("write snapshot");
    let snapshot_arg = snapshot_path.to_string_lossy();
    for level in ["file", "symbol"] {
        let pending = run_json(
            fixture.path(),
            [
                "recommend",
                "--task-id",
                "PENDING",
                "--task-snapshot",
                snapshot_arg.as_ref(),
                "--level",
                level,
            ],
        );
        assert_eq!(
            pending["recommendations"][0]["counts"]["eligible_history_deliveries"],
            3
        );
        assert!(pending["recommendations"].as_array().is_some_and(|items| {
            items.iter().all(|item| {
                item["supporting_delivery_ids"]
                    .as_array()
                    .is_some_and(|ids| ids.iter().all(|id| id != "D-FUTURE"))
            })
        }));
        assert!(pending["recommendations"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item["selector"]
                    .as_str()
                    .is_some_and(|selector| selector.contains("a.rs"))
            })
        }));
    }
}

#[test]
fn real_binary_live_default_cutoff_accepts_new_pending_snapshot() {
    let fixture = fixture_repository();
    let old_date = "2000-01-01T00:00:00Z";
    let amended = Command::new("git")
        .current_dir(fixture.path())
        .env("GIT_AUTHOR_DATE", old_date)
        .env("GIT_COMMITTER_DATE", old_date)
        .args(["commit", "--amend", "--no-edit"])
        .output()
        .expect("amend old target commit");
    assert!(
        amended.status.success(),
        "git amend failed: {}",
        String::from_utf8_lossy(&amended.stderr)
    );

    let captured_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time after Unix epoch")
        .as_secs();
    let snapshot_path = fixture.path().join("live-pending-snapshot.json");
    let snapshot = serde_json::json!({
        "task_id": "PENDING-LIVE",
        "title": "helper",
        "description": "new pending work captured after the old target commit",
        "acceptance_criteria": ["recommend the helper destination"],
        "source": {"system": "task_service", "record_id": "PENDING-LIVE@1"},
        "created_at": {
            "status": "known", "timestamp": "unix:0",
            "source": {"system": "task_service"}
        },
        "snapshot_available_at": {
            "status": "known", "timestamp": format!("unix:{captured_seconds}"),
            "source": {"system": "task_service", "record_id": "PENDING-LIVE@1"}
        },
        "text_availability": "known_pre_execution",
        "captured_at": format!("unix:{captured_seconds}")
    });
    fs::write(
        &snapshot_path,
        serde_json::to_vec(&snapshot).expect("encode live snapshot"),
    )
    .expect("write live snapshot");

    let before_future = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn helper() -> i32 { 2 }\n\npub fn entry() -> i32 { helper() }\n",
    )
    .expect("edit future delivery");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "pending delivery"]);
    let after_future = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    import_cli_delivery(
        fixture.path(),
        &before_future,
        &after_future,
        "D-PENDING-LIVE",
        "PENDING-LIVE",
        "helper",
        "2001-01-01T00:00:00Z",
        "2000-01-01T00:00:00Z",
    );

    let snapshot_arg = snapshot_path.to_string_lossy();
    for level in ["file", "symbol"] {
        let result = run_json(
            fixture.path(),
            [
                "recommend",
                "--task-id",
                "PENDING-LIVE",
                "--task-snapshot",
                snapshot_arg.as_ref(),
                "--level",
                level,
            ],
        );
        let effective_cutoff = result["effective_cutoff"]
            .as_str()
            .expect("effective cutoff");
        assert!(
            effective_cutoff.contains('T') && effective_cutoff.contains('.'),
            "live cutoff should preserve subsecond observation time: {effective_cutoff}"
        );
        assert!(
            result["recommendations"]
                .as_array()
                .is_some_and(|items| !items.is_empty()),
            "live recommendation should resolve in {level} mode: {result}"
        );
        assert!(result["recommendations"].as_array().is_some_and(|items| {
            items.iter().all(|item| {
                item["supporting_delivery_ids"]
                    .as_array()
                    .is_some_and(|ids| ids.iter().all(|id| id != "D-PENDING-LIVE"))
            })
        }));
    }
}

#[test]
fn real_binary_filters_deleted_destinations_from_stale_structure() {
    let fixture = TempDir::new().expect("create stale structure fixture");
    run_git(fixture.path(), ["init", "-b", "main"]);
    run_git(
        fixture.path(),
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(fixture.path(), ["config", "user.name", "Graph Test"]);
    fs::write(
        fixture.path().join("caller.rs"),
        "mod removed;\npub fn keeper() -> i32 { removed::gone() }\n",
    )
    .expect("write caller");
    fs::write(
        fixture.path().join("removed.rs"),
        "pub fn gone() -> i32 { 1 }\n",
    )
    .expect("write removed");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "base"]);
    let _ = run_json(fixture.path(), ["sync", "--full"]);
    init_empty_history(fixture.path());

    fs::write(
        fixture.path().join("caller.rs"),
        "pub fn keeper() -> i32 { 1 }\n",
    )
    .expect("remove call");
    fs::remove_file(fixture.path().join("removed.rs")).expect("delete callee");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "delete callee"]);

    for level in ["file", "symbol"] {
        let result = run_json(
            fixture.path(),
            ["recommend", "--query", "keeper", "--level", level],
        );
        assert!(result["recommendations"].as_array().is_some_and(|items| {
            items.iter().all(|item| {
                !item["selector"].as_str().is_some_and(|selector| {
                    selector.contains("removed.rs") || selector.contains("gone")
                })
            })
        }));
        assert!(result["fallbacks"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| item["kind"] == "stale_structure_excluded")
        }));
    }
}

#[test]
fn real_binary_imports_syncs_reports_and_rebuilds_history() {
    let fixture = fixture_repository();
    let before = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn helper() -> i32 { 2 }\n\npub fn entry() -> i32 { helper() }\n",
    )
    .expect("edit fixture source");
    run_git(fixture.path(), ["add", "."]);
    run_git(
        fixture.path(),
        ["commit", "-m", "deliver update\n\nTask-Id: ORB-CLI"],
    );
    let after = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    let repository = fixture
        .path()
        .canonicalize()
        .expect("canonical fixture")
        .to_string_lossy()
        .into_owned();
    let envelope = serde_json::json!({
        "schema_version": 2,
        "repository": repository,
        "landing_branch": "main",
        "before_revision": before,
        "after_revision": after,
        "delivery_id": "verified-cli-1",
        "evidence": "verified_delivery",
        "source": {"system": "cli_test", "record_id": "delivery-1"},
        "delivered_at": {
            "status": "known", "timestamp": "2026-09-07T00:00:00Z",
            "source": {"system": "delivery_service", "record_id": "landed-1"}
        },
        "captured_at": "2026-09-07T00:01:00Z",
        "tasks": [{
            "task_id": "ORB-CLI",
            "title": "Exercise history CLI",
            "description": "Verify the public import contract",
            "acceptance_criteria": ["CLI operations return JSON"],
            "source": {"system": "cli_test", "record_id": "ORB-CLI"},
            "created_at": {
                "status": "known", "timestamp": "2026-09-06T20:00:00Z",
                "source": {"system": "task_service", "record_id": "created-1"}
            },
            "snapshot_available_at": {
                "status": "known", "timestamp": "2026-09-06T21:00:00Z",
                "source": {"system": "task_service", "record_id": "snapshot-7"}
            },
            "text_availability": "known_pre_execution",
            "captured_at": "2026-09-07T00:00:30Z"
        }]
    });
    let envelope_path = fixture.path().join("delivery.json");
    fs::write(
        envelope_path.as_path(),
        serde_json::to_vec(&envelope).expect("encode envelope"),
    )
    .expect("write envelope");
    let envelope_arg = envelope_path.to_string_lossy();

    let imported = run_json(
        fixture.path(),
        ["history", "import", "--input", envelope_arg.as_ref()],
    );
    assert_eq!(imported["inserted"], true);
    assert_eq!(imported["landing_branch"], "main");
    assert!(imported["database_path"].as_str().is_some());
    let stored = HistoryIndex::open(fixture.path(), "main")
        .expect("open imported history")
        .deliveries()
        .expect("read imported delivery");
    let stored_delivery = &stored[0].delivery;
    assert_eq!(stored_delivery.captured_at, "2026-09-07T00:01:00Z");
    assert_eq!(stored_delivery.delivered_at.status, TemporalStatus::Known);
    assert_eq!(
        stored_delivery.delivered_at.timestamp.as_deref(),
        Some("2026-09-07T00:00:00Z")
    );
    assert_eq!(
        stored_delivery.tasks[0].text_availability,
        TaskTextAvailability::KnownPreExecution
    );
    assert_eq!(
        stored_delivery.tasks[0].created_at.timestamp.as_deref(),
        Some("2026-09-06T20:00:00Z")
    );
    assert_eq!(
        stored_delivery.tasks[0]
            .snapshot_available_at
            .source
            .record_id
            .as_deref(),
        Some("snapshot-7")
    );
    let duplicate = run_json(
        fixture.path(),
        ["history", "import", "--input", envelope_arg.as_ref()],
    );
    assert_eq!(duplicate["inserted"], false);
    let imported_human = run(
        fixture.path(),
        ["history", "import", "--input", envelope_arg.as_ref()],
    );
    assert!(imported_human.status.success());
    let imported_human = String::from_utf8_lossy(&imported_human.stdout);
    assert!(imported_human.contains("operation\timport"));
    assert!(imported_human.contains("delivery\tverified-cli-1"));
    assert!(imported_human.contains("inserted\tfalse"));
    let imported_ndjson = run(
        fixture.path(),
        [
            "history",
            "import",
            "--input",
            envelope_arg.as_ref(),
            "--format",
            "ndjson",
        ],
    );
    let imported_ndjson = parse_ndjson(&imported_ndjson.stdout);
    assert_eq!(imported_ndjson.len(), 1);
    assert_eq!(imported_ndjson[0]["delivery_id"], "verified-cli-1");

    let recommended = run_json(
        fixture.path(),
        [
            "recommend",
            "--query",
            "history CLI import contract",
            "--level",
            "symbol",
            "--limit",
            "3",
        ],
    );
    assert!(
        recommended["recommendations"]
            .as_array()
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item["selector"]
                        .as_str()
                        .is_some_and(|selector| selector.contains("src/lib.rs#helper:function"))
                        && item["supporting_delivery_ids"]
                            .as_array()
                            .is_some_and(|ids| ids.iter().any(|id| id == "verified-cli-1"))
                })
            })
    );
    let recommended_human = run(
        fixture.path(),
        [
            "recommend",
            "--query",
            "history CLI import contract",
            "--level",
            "symbol",
            "--limit",
            "3",
        ],
    );
    assert!(recommended_human.status.success());
    let recommended_human = String::from_utf8_lossy(&recommended_human.stdout);
    assert!(recommended_human.contains("src/lib.rs#helper:function"));
    assert!(recommended_human.contains("history"));

    let synced = run_json(
        fixture.path(),
        ["history", "sync", "--branch", "main", "--limit", "10"],
    );
    assert_eq!(synced["complete"], true);
    assert_eq!(synced["landing_branch"], "main");
    assert_eq!(synced["database_path"], imported["database_path"]);
    let synced_human = run(
        fixture.path(),
        ["history", "sync", "--branch", "main", "--limit", "10"],
    );
    assert!(synced_human.status.success());
    let synced_human = String::from_utf8_lossy(&synced_human.stdout);
    assert!(synced_human.contains("operation\tsync"));
    assert!(synced_human.contains("commits indexed\t"));
    let synced_ndjson = run(
        fixture.path(),
        ["history", "sync", "--branch", "main", "--format", "ndjson"],
    );
    let synced_ndjson = parse_ndjson(&synced_ndjson.stdout);
    assert_eq!(synced_ndjson.len(), 1);
    assert!(synced_ndjson[0]["complete"].is_boolean());
    let status = run_json(fixture.path(), ["history", "status", "--branch", "main"]);
    assert_eq!(status["schema_version"], 4);
    assert_eq!(status["extractor_version"], 2);
    assert_eq!(status["verified_deliveries"], 1);
    assert_eq!(status["git_only_deliveries"], 1);
    assert_eq!(status["task_associations"], 2);
    let status_human = run(fixture.path(), ["history", "status", "--branch", "main"]);
    assert!(status_human.status.success());
    let status_human = String::from_utf8_lossy(&status_human.stdout);
    assert!(status_human.contains("verified deliveries\t1"));
    assert!(status_human.contains("complete\ttrue"));
    let status_ndjson = run(
        fixture.path(),
        [
            "history", "status", "--branch", "main", "--format", "ndjson",
        ],
    );
    let status_ndjson = parse_ndjson(&status_ndjson.stdout);
    assert_eq!(status_ndjson.len(), 1);
    assert!(status_ndjson[0]["verified_deliveries"].is_number());

    let preview = run_json(
        fixture.path(),
        ["history", "rebuild", "--branch", "main", "--limit", "10"],
    );
    assert_eq!(preview["confirmed"], false);
    assert_eq!(preview["verified_deliveries"], 1);
    assert_eq!(preview["would_remove_deliveries"], 2);
    assert_eq!(preview["removed_deliveries"], 0);
    assert_eq!(
        run_json(fixture.path(), ["history", "status", "--branch", "main"]),
        status
    );
    let rebuilt = run_json(
        fixture.path(),
        [
            "history",
            "rebuild",
            "--branch",
            "main",
            "--limit",
            "10",
            "--confirm",
        ],
    );
    assert_eq!(rebuilt["confirmed"], true);
    assert_eq!(rebuilt["removed_deliveries"], 2);
    assert_eq!(rebuilt["removed_verified_deliveries"], 0);
    assert_eq!(rebuilt["database_path"], imported["database_path"]);
    assert_eq!(rebuilt["landing_branch"], "main");
    assert_eq!(rebuilt["sync"]["deliveries_inserted"], 1);
    assert_eq!(
        run_json(fixture.path(), ["history", "status", "--branch", "main"])["verified_deliveries"],
        1
    );
    let rebuilt_human = run(
        fixture.path(),
        [
            "history",
            "rebuild",
            "--branch",
            "main",
            "--limit",
            "10",
            "--confirm",
        ],
    );
    assert!(rebuilt_human.status.success());
    assert!(String::from_utf8_lossy(&rebuilt_human.stdout).contains("operation\trebuild"));
    let rebuilt_ndjson = run(
        fixture.path(),
        [
            "history",
            "rebuild",
            "--branch",
            "main",
            "--confirm",
            "--format",
            "ndjson",
        ],
    );
    let rebuilt_ndjson = parse_ndjson(&rebuilt_ndjson.stdout);
    assert_eq!(rebuilt_ndjson.len(), 1);
    assert!(rebuilt_ndjson[0]["sync"].is_object());
    let discarded = run_json(
        fixture.path(),
        [
            "history",
            "rebuild",
            "--branch",
            "main",
            "--confirm",
            "--discard-verified",
        ],
    );
    assert_eq!(discarded["removed_verified_deliveries"], 1);
    assert_eq!(
        run_json(fixture.path(), ["history", "status", "--branch", "main"])["verified_deliveries"],
        0
    );
}

#[test]
fn real_binary_recovers_each_history_contract_version_mismatch() {
    for key in ["extractor_version", "import_schema_version"] {
        let fixture = fixture_repository();
        let before = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn helper() -> i32 { 3 }\n",
        )
        .expect("edit history fixture");
        run_git(fixture.path(), ["add", "."]);
        run_git(fixture.path(), ["commit", "-m", "repair helper"]);
        let after = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
        import_cli_delivery(
            fixture.path(),
            before.as_str(),
            after.as_str(),
            "VERSION-VERIFIED",
            "VERSION-TASK",
            "Repair helper",
            "2002-01-01T00:00:00Z",
            "2001-01-01T00:00:00Z",
        );
        let synced = run_json(fixture.path(), ["history", "sync", "--branch", "main"]);
        let path = synced["database_path"].as_str().expect("history path");
        let conn = rusqlite::Connection::open(path).expect("open history fixture");
        conn.execute("UPDATE history_meta SET value='0' WHERE key=?1", [key])
            .expect("seed old version");
        let rejected = run_explicit_json(fixture.path(), ["history", "status", "--branch", "main"]);
        assert!(!rejected.status.success(), "{key}");
        let error: Value = serde_json::from_slice(&rejected.stderr).expect("JSON error");
        assert_eq!(error["error"]["code"], "version_mismatch", "{key}: {error}");
        let message = error["error"]["message"].as_str().expect("error message");
        for expected in [
            key,
            "=0",
            "expected 2",
            path,
            "orbit-graph history rebuild --branch main --confirm",
        ] {
            assert!(message.contains(expected), "{key}: {message}");
        }
        let preview = run_json(fixture.path(), ["history", "rebuild", "--branch", "main"]);
        assert_eq!(preview["confirmed"], false);
        assert_eq!(preview["verified_deliveries"], 1);
        let still_old: String = conn
            .query_row(
                "SELECT value FROM history_meta WHERE key=?1",
                [key],
                |row| row.get(0),
            )
            .expect("version after preview");
        assert_eq!(still_old, "0");
        let rebuilt = run_json(
            fixture.path(),
            ["history", "rebuild", "--branch", "main", "--confirm"],
        );
        assert_eq!(rebuilt["removed_verified_deliveries"], 0);
        let status = run_json(fixture.path(), ["history", "status", "--branch", "main"]);
        assert_eq!(status["verified_deliveries"], 1);
        let recommended = run_json(
            fixture.path(),
            ["recommend", "--query", "helper", "--branch", "main"],
        );
        assert!(recommended["resolved_target_revision"].is_string());
    }
}

#[test]
fn real_binary_refuses_history_rebuild_on_agent_main() {
    let fixture = fixture_repository();
    run_git(fixture.path(), ["branch", "agent-main"]);
    for confirm in [false, true] {
        let output = if confirm {
            run_explicit_json(
                fixture.path(),
                ["history", "rebuild", "--branch", "agent-main", "--confirm"],
            )
        } else {
            run_explicit_json(
                fixture.path(),
                ["history", "rebuild", "--branch", "agent-main"],
            )
        };
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("agent-main"));
    }
}

#[test]
fn real_binary_history_lock_timeout_names_holder() {
    let fixture = fixture_repository();
    let synced = run_json(fixture.path(), ["history", "sync", "--branch", "main"]);
    let db_path = Path::new(synced["database_path"].as_str().expect("history path"));
    let lock_path = db_path.with_extension("sqlite3.lock");
    let mut held = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&lock_path)
        .expect("open lock");
    let acquired_at = "2026-09-26T22:00:00Z";
    let label = "held history writer";
    write!(
        held,
        "{}",
        serde_json::json!({
            "pid": std::process::id(), "acquired_at": acquired_at, "label": label
        })
    )
    .expect("write holder record");
    held.flush().expect("flush holder record");
    held.lock_exclusive().expect("hold history lock");
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(fixture.path())
        .env("ORBIT_GRAPH_LOCK_TIMEOUT_MS", "100")
        .args(["--format", "json", "history", "sync", "--branch", "main"])
        .output()
        .expect("run blocked history writer");
    assert!(!output.status.success());
    assert!(started.elapsed() < Duration::from_secs(2));
    let message = String::from_utf8_lossy(&output.stderr);
    for expected in [
        std::process::id().to_string(),
        acquired_at.to_string(),
        label.to_string(),
    ] {
        assert!(message.contains(&expected), "{message}");
    }
}

#[test]
fn real_binary_recommend_reports_unknown_git_ancestry() {
    let fixture = fixture_repository();
    let synced = run_json(fixture.path(), ["history", "sync", "--branch", "main"]);
    let path = synced["database_path"].as_str().expect("history path");
    let conn = rusqlite::Connection::open(path).expect("open history fixture");
    conn.execute(
        "UPDATE history_scopes SET cursor=?1 WHERE landing_branch='main'",
        ["0000000000000000000000000000000000000000"],
    )
    .expect("seed missing Git cursor");
    let recommended = run_json(
        fixture.path(),
        ["recommend", "--query", "helper", "--branch", "main"],
    );
    let reason = recommended["source_freshness"]["reason"]
        .as_str()
        .expect("freshness reason");
    assert!(
        reason.contains("ancestry is unknown because Git failed"),
        "{reason}"
    );
}

#[cfg(unix)]
#[test]
fn real_binary_creates_and_restricts_history_database_without_losing_deliveries() {
    let fixture = fixture_repository();
    // The writing command creates the index; `history status` only reads it.
    run_history_with_umask_022(fixture.path(), "sync");
    let status = run_history_with_umask_022(fixture.path(), "status");
    let db_path = Path::new(
        status["database_path"]
            .as_str()
            .expect("history database path"),
    );
    let mode = fs::metadata(db_path)
        .expect("new history database metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "new history database mode: {mode:o}");

    let before = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    fs::write(fixture.path().join("src/added.rs"), "pub fn added() {}\n")
        .expect("write delivered source");
    run_git(fixture.path(), ["add", "src/added.rs"]);
    run_git(fixture.path(), ["commit", "-m", "add delivered source"]);
    let after = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    import_cli_delivery(
        fixture.path(),
        &before,
        &after,
        "private-history-delivery",
        "ORB-PRIVATE",
        "Private history task",
        "2000-01-01T00:00:00Z",
        "1999-01-01T00:00:00Z",
    );

    fs::set_permissions(db_path, fs::Permissions::from_mode(0o644))
        .expect("simulate existing permissive database");
    run_history_with_umask_022(fixture.path(), "sync");
    let reopened = run_history_with_umask_022(fixture.path(), "status");
    assert_eq!(reopened["verified_deliveries"], 1);
    assert_eq!(reopened["task_associations"], 1);
    let mode = fs::metadata(db_path)
        .expect("reopened history database metadata")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "reopened history database mode: {mode:o}"
    );
    let stored = HistoryIndex::open(fixture.path(), "main")
        .expect("open restricted history")
        .deliveries()
        .expect("read preserved delivery");
    // `history sync` also indexed the fixture's first commit as Git-only.
    let verified = stored
        .iter()
        .find(|change| change.delivery.delivery_id == "private-history-delivery")
        .expect("the verified delivery is preserved");
    assert_eq!(verified.delivery.tasks[0].task_id, "ORB-PRIVATE");
}

#[cfg(unix)]
fn run_history_with_umask_022(root: &Path, subcommand: &str) -> Value {
    let output = Command::new("sh")
        .current_dir(root)
        .args([
            "-c",
            "umask 022; exec \"$@\"",
            "sh",
            env!("CARGO_BIN_EXE_orbit-graph"),
            "--format",
            "json",
            "history",
            subcommand,
            "--branch",
            "main",
        ])
        .output()
        .expect("run history under umask 022");
    assert!(
        output.status.success(),
        "history {subcommand} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON history status")
}

#[test]
fn real_binary_rejects_history_repository_mismatch_with_json_error() {
    let fixture = fixture_repository();
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn helper() -> i32 { 2 }\n",
    )
    .expect("edit fixture source");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "second"]);
    let after = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    let before = git_stdout(fixture.path(), ["rev-parse", "HEAD~1"]);
    let envelope_path = fixture.path().join("bad-delivery.json");
    let envelope = serde_json::json!({
        "schema_version": 2, "repository": "wrong", "landing_branch": "main",
        "before_revision": before, "after_revision": after, "delivery_id": "bad",
        "evidence": "verified_delivery", "source": {"system": "test"},
        "delivered_at": {"status": "unavailable", "source": {"system": "test"}},
        "captured_at": "2026-09-07T00:00:00Z", "tasks": []
    });
    fs::write(
        &envelope_path,
        serde_json::to_vec(&envelope).expect("encode"),
    )
    .expect("write bad envelope");
    let envelope_arg = envelope_path.to_string_lossy();
    let output = run_explicit_json(
        fixture.path(),
        ["history", "import", "--input", envelope_arg.as_ref()],
    );
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error");
    assert_eq!(error["error"]["code"], "graph_error");
    assert!(
        error["details"]
            .as_str()
            .is_some_and(|details| details.contains("mismatch"))
    );
}

#[test]
fn real_binary_rejects_invalid_history_timestamps_without_partial_import() {
    let fixture = fixture_repository();
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn helper() -> i32 { 2 }\n",
    )
    .expect("edit fixture source");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "second"]);
    let after = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    let before = git_stdout(fixture.path(), ["rev-parse", "HEAD~1"]);
    let repository = fixture
        .path()
        .canonicalize()
        .expect("canonical fixture")
        .to_string_lossy()
        .into_owned();
    let envelope = serde_json::json!({
        "schema_version": 2, "repository": repository, "landing_branch": "main",
        "before_revision": before, "after_revision": after, "delivery_id": "bad-time",
        "evidence": "verified_delivery", "source": {"system": "test"},
        "delivered_at": {
            "status": "known", "timestamp": "not-a-time",
            "source": {"system": "test_clock"}
        },
        "captured_at": "2026-09-07T00:00:00Z", "tasks": []
    });
    let envelope_path = fixture.path().join("bad-time.json");
    fs::write(
        &envelope_path,
        serde_json::to_vec(&envelope).expect("encode invalid envelope"),
    )
    .expect("write invalid envelope");
    let envelope_arg = envelope_path.to_string_lossy();
    let output = run_explicit_json(
        fixture.path(),
        ["history", "import", "--input", envelope_arg.as_ref()],
    );
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error");
    assert!(error.to_string().contains("timestamp"), "{error}");
    let status = run_json(fixture.path(), ["history", "status", "--branch", "main"]);
    assert_eq!(status["deliveries"], 0);
}

#[test]
fn real_binary_rejects_signed_rfc3339_components_without_changing_history_state() {
    let fixture = fixture_repository();
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn helper() -> i32 { 2 }\n",
    )
    .expect("edit fixture source");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "second"]);
    let after = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    let before = git_stdout(fixture.path(), ["rev-parse", "HEAD~1"]);
    let repository = fixture
        .path()
        .canonicalize()
        .expect("canonical fixture")
        .to_string_lossy()
        .into_owned();
    let valid = serde_json::json!({
        "schema_version": 2, "repository": repository, "landing_branch": "main",
        "before_revision": before, "after_revision": after,
        "delivery_id": "valid-time", "evidence": "verified_delivery",
        "source": {"system": "test"},
        "delivered_at": {
            "status": "known", "timestamp": "unix:1788739200",
            "source": {"system": "test_clock"}
        },
        "captured_at": "2026-09-07T04:00:00.123+02:30", "tasks": []
    });
    let envelope_path = fixture.path().join("time.json");
    fs::write(
        &envelope_path,
        serde_json::to_vec(&valid).expect("encode valid envelope"),
    )
    .expect("write valid envelope");
    let envelope_arg = envelope_path.to_string_lossy();
    let imported = run_json(
        fixture.path(),
        ["history", "import", "--input", envelope_arg.as_ref()],
    );
    assert_eq!(imported["inserted"], true);

    for (index, captured_at) in [
        "2026-+9-07T04:00:00Z",
        "2026-09-+7T04:00:00Z",
        "2026-09-07T+4:00:00Z",
        "2026-09-07T04:+0:00Z",
        "2026-09-07T04:00:+0Z",
        "2026-09-07T04:00:00+9:00",
        "2026-09-07T04:00:00+00:+9",
    ]
    .into_iter()
    .enumerate()
    {
        let mut invalid = valid.clone();
        invalid["delivery_id"] = serde_json::json!(format!("invalid-time-{index}"));
        invalid["captured_at"] = serde_json::json!(captured_at);
        fs::write(
            &envelope_path,
            serde_json::to_vec(&invalid).expect("encode invalid envelope"),
        )
        .expect("write invalid envelope");
        let output = run(
            fixture.path(),
            ["history", "import", "--input", envelope_arg.as_ref()],
        );
        assert!(!output.status.success(), "accepted {captured_at}");
    }

    let status = run_json(fixture.path(), ["history", "status", "--branch", "main"]);
    assert_eq!(status["deliveries"], 1);
    assert_eq!(status["cursor"], Value::Null);
}

/// Creates the history index with no deliveries, as `history sync` would
/// before indexing any commit, for tests of cold-start recommendations:
/// `recommend` only reads and reports a missing index.
fn init_empty_history(root: &Path) {
    HistoryIndex::open(root, "main").expect("initialize an empty history index");
}

fn run_json<const N: usize>(cwd: &Path, args: [&str; N]) -> Value {
    let output = run_explicit_json(cwd, args);
    assert!(
        output.status.success(),
        "orbit-graph failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON command output")
}

fn parse_ndjson(bytes: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(|line| serde_json::from_str(line).expect("NDJSON record"))
        .collect()
}

fn assert_exploration_record_units(command: &str, document: &Value, records: &[Value]) {
    let array = |field: &str| document[field].as_array().cloned().unwrap_or_default();
    match command {
        "search" => assert_eq!(records, array("matches")),
        "show" => assert_eq!(records, std::slice::from_ref(document)),
        "callees" => assert_eq!(records, array("callees")),
        "overview" => {
            let files = array("files");
            assert_eq!(records.len(), files.len() + 1);
            assert_eq!(records[0]["record_type"], "overview_context");
            assert_eq!(
                records[0]["context"]["total_files"],
                document["total_files"]
            );
            for (record, file) in records[1..].iter().zip(files) {
                assert_eq!(record["record_type"], "overview_file");
                assert_eq!(record["file"], file);
            }
        }
        "refs" => {
            let refs = array("refs");
            let relations = array("relations");
            assert_eq!(records[0]["record_type"], "refs_context");
            assert_eq!(records[0]["context"]["target"], document["target"]);
            assert_eq!(
                records
                    .iter()
                    .filter(|record| record["record_type"] == "reference")
                    .count(),
                refs.len()
            );
            assert_eq!(
                records
                    .iter()
                    .filter(|record| record["record_type"] == "relation")
                    .count(),
                relations.len()
            );
        }
        "implementors" => {
            let implementors = array("implementors");
            assert_eq!(records.len(), implementors.len() + 1);
            assert_eq!(records[0]["context"]["trait_name"], document["trait_name"]);
            for (record, implementor) in records[1..].iter().zip(implementors) {
                assert_eq!(record["implementor"], implementor);
            }
        }
        "deps" => {
            let imports = array("imports");
            assert_eq!(records.len(), imports.len() + 1);
            assert_eq!(records[0]["context"]["scope"], document["scope"]);
            for (record, import) in records[1..].iter().zip(imports) {
                assert_eq!(record["import"], import);
            }
        }
        "trace" => {
            assert_eq!(
                records[0]["context"]["visited_nodes"],
                document["visited_nodes"]
            );
            assert_eq!(records[1]["root"], document["root"]);
        }
        "impact" => {
            let touched = array("touched");
            assert_eq!(
                records[0]["context"]["visited_nodes"],
                document["visited_nodes"]
            );
            assert_eq!(
                records
                    .iter()
                    .filter(|record| record["record_type"] == "impact")
                    .count(),
                touched.len()
            );
        }
        _ => panic!("unexpected exploration command {command}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn import_cli_delivery(
    root: &Path,
    before: &str,
    after: &str,
    delivery_id: &str,
    task_id: &str,
    title: &str,
    delivered_at: &str,
    snapshot_available_at: &str,
) {
    let repository = root
        .canonicalize()
        .expect("canonical fixture")
        .to_string_lossy()
        .into_owned();
    let mut tasks = vec![serde_json::json!({
        "task_id": task_id,
        "title": title,
        "description": "neutral delivery description",
        "acceptance_criteria": [],
        "source": {"system": "task_service", "record_id": task_id},
        "created_at": {
            "status": "known", "timestamp": "1999-01-01T00:00:00Z",
            "source": {"system": "task_service", "record_id": task_id}
        },
        "snapshot_available_at": {
            "status": "known", "timestamp": snapshot_available_at,
            "source": {"system": "task_service", "record_id": task_id}
        },
        "text_availability": "known_pre_execution",
        "captured_at": "2002-01-01T00:00:00Z"
    })];
    if delivery_id == "D1" {
        tasks.push(serde_json::json!({
            "task_id": "FUTURE-TEXT",
            "title": "futuretext",
            "description": "must not be visible before its snapshot cutoff",
            "acceptance_criteria": [],
            "source": {"system": "task_service", "record_id": "FUTURE-TEXT"},
            "created_at": {
                "status": "known", "timestamp": "1999-01-01T00:00:00Z",
                "source": {"system": "task_service"}
            },
            "snapshot_available_at": {
                "status": "known", "timestamp": "2001-01-01T00:00:00.900Z",
                "source": {"system": "task_service"}
            },
            "text_availability": "known_pre_execution",
            "captured_at": "2002-01-01T00:00:00Z"
        }));
    }
    let envelope = serde_json::json!({
        "schema_version": 2,
        "repository": repository,
        "landing_branch": "main",
        "before_revision": before,
        "after_revision": after,
        "delivery_id": delivery_id,
        "evidence": "verified_delivery",
        "source": {"system": "cli_test", "record_id": delivery_id},
        "delivered_at": {
            "status": "known", "timestamp": delivered_at,
            "source": {"system": "delivery_service", "record_id": delivery_id}
        },
        "captured_at": "2002-01-01T00:00:00Z",
        "tasks": tasks
    });
    let path = root.join(format!("{delivery_id}.json"));
    fs::write(
        &path,
        serde_json::to_vec(&envelope).expect("encode delivery"),
    )
    .expect("write delivery");
    let path_arg = path.to_string_lossy();
    let imported = run_json(root, ["history", "import", "--input", path_arg.as_ref()]);
    assert_eq!(imported["inserted"], true);
}

fn run<const N: usize>(cwd: &Path, args: [&str; N]) -> Output {
    run_with_env(cwd, args, &[])
}

fn run_explicit_json<const N: usize>(cwd: &Path, args: [&str; N]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(cwd)
        .args(["--format", "json"])
        .args(args)
        .output()
        .expect("run orbit-graph in JSON mode")
}

fn run_with_env<const N: usize>(cwd: &Path, args: [&str; N], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit-graph"));
    command.current_dir(cwd).args(args);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().expect("run orbit-graph")
}

/// Runs the binary with its output in files (no pipe to fill) and waits for
/// it in process until `deadline`; the guard kills and reaps it on timeout
/// or panic (`STD-03 §R17`, `§R18`).
#[cfg(unix)]
fn run_with_deadline(
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    deadline: Duration,
) -> Output {
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let capture = TempDir::new().expect("create capture directory");
    let stdout_path = capture.path().join("stdout");
    let stderr_path = capture.path().join("stderr");
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit-graph"));
    command
        .current_dir(cwd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(fs::File::create(&stdout_path).expect("create stdout capture"))
        .stderr(fs::File::create(&stderr_path).expect("create stderr capture"));
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = ChildGuard(command.spawn().expect("spawn orbit-graph"));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.0.try_wait().expect("poll orbit-graph") {
            break status;
        }
        assert!(
            started.elapsed() < deadline,
            "orbit-graph {args:?} did not finish within {deadline:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    Output {
        status,
        stdout: fs::read(&stdout_path).expect("read stdout capture"),
        stderr: fs::read(&stderr_path).expect("read stderr capture"),
    }
}

fn fixture_repository() -> TempDir {
    let fixture = TempDir::new().expect("create fixture repository");
    run_git(fixture.path(), ["init", "-b", "main"]);
    run_git(
        fixture.path(),
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(fixture.path(), ["config", "user.name", "Graph Test"]);
    fs::create_dir_all(fixture.path().join("src")).expect("create source directory");
    fs::write(
        fixture.path().join("src/lib.rs"),
        "use std::fmt::Debug;\n\npub trait Renderer {}\npub struct Human;\nimpl Renderer for Human {}\n\npub fn helper() -> i32 { 1 }\n\npub fn entry() -> i32 { helper() }\n\npub fn caller() -> i32 { entry() }\n",
    )
    .expect("write fixture source");
    let unicode_dir = fixture.path().join("src/界界");
    fs::create_dir_all(&unicode_dir).expect("create Unicode source directory");
    fs::write(
        unicode_dir.join("e\u{301}_very_long_component_name.rs"),
        "pub fn unicode_helper() -> &'static str { \"café\" }\n",
    )
    .expect("write Unicode fixture source");
    fs::write(
        fixture.path().join("src/cli.py"),
        "import click\n\n@click.command()\ndef ship():\n    helper()\n\ndef helper():\n    return 'ok'\n",
    )
    .expect("write command fixture source");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "fixture"]);
    fixture
}

#[cfg(unix)]
fn run_in_pty(cwd: &Path, args: &[&str], columns: u16) -> (String, String) {
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: 24,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: openpty initializes both descriptors on success; no termios is supplied.
    let result = unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut size,
        )
    };
    assert_eq!(result, 0, "openpty failed");
    // SAFETY: successful openpty returned newly owned file descriptors.
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    // SAFETY: successful openpty returned newly owned file descriptors.
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    let child = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(cwd)
        .args(args)
        .stdout(Stdio::from(slave))
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn orbit-graph in PTY");

    // Drop the parent's slave before reading so EOF/EIO reflects the child only.
    // Draining while the child runs avoids losing buffered macOS PTY output when
    // the final slave descriptor closes.
    let mut reader = std::fs::File::from(master);
    let mut stdout = Vec::new();
    loop {
        let mut buffer = [0_u8; 4096];
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => stdout.extend_from_slice(&buffer[..count]),
            Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
            Err(error) => panic!("read PTY output: {error}"),
        }
    }
    let output = child
        .wait_with_output()
        .expect("wait for orbit-graph in PTY");
    assert!(
        output.status.success(),
        "PTY command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8(stdout)
            .expect("PTY output is UTF-8")
            .replace("\r\n", "\n"),
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    )
}

#[test]
fn real_binary_live_evaluation_excludes_held_out_and_later_deliveries() {
    let fixture = TempDir::new().expect("live evaluation fixture");
    let root = fixture.path();
    run_git(root, ["init", "-b", "main"]);
    run_git(root, ["config", "user.email", "graph@example.invalid"]);
    run_git(root, ["config", "user.name", "Graph Test"]);
    let commit = |rel: &str, body: &str, message: &str| {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, body).expect("write");
        run_git(root, ["add", rel]);
        run_git(root, ["commit", "-m", message]);
    };
    commit(
        "src/keep.rs",
        "pub fn keep() {}\n",
        "initialize keep module",
    );
    commit(
        "src/noise.rs",
        "pub fn noise() {}\n",
        "chore: adjust unrelated noise module",
    );
    commit(
        "src/widget.rs",
        "pub fn widget_cache() -> u32 { 1 }\n",
        "repair widget cache invalidation for durable defaults",
    );
    commit(
        "src/widget.rs",
        "pub fn widget_cache() -> u32 { 2 }\n",
        "repair widget cache invalidation for durable defaults",
    );
    commit(
        "src/widget.rs",
        "pub fn widget_cache() -> u32 { 3 }\n",
        "repair widget cache invalidation for durable defaults [ORB-8888]",
    );
    let head = git_stdout(root, ["rev-parse", "HEAD"]);
    let held_out_sha = git_stdout(root, ["rev-parse", "HEAD^"]);
    let parent_sha = git_stdout(root, ["rev-parse", "HEAD^^"]);
    let later = head.trim();
    let held_out = held_out_sha.trim();
    let parent = parent_sha.trim();

    let unsynced = TempDir::new().expect("unsynced");
    run_git(unsynced.path(), ["init", "-b", "main"]);
    run_git(
        unsynced.path(),
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(unsynced.path(), ["config", "user.name", "Graph Test"]);
    fs::write(unsynced.path().join("README"), "unsynced\n").expect("readme");
    run_git(unsynced.path(), ["add", "README"]);
    run_git(unsynced.path(), ["commit", "-m", "unsynced root"]);
    fs::write(unsynced.path().join("README"), "changed\n").expect("readme");
    run_git(unsynced.path(), ["add", "README"]);
    run_git(unsynced.path(), ["commit", "-m", "unsynced change"]);
    let denied = run_explicit_json(
        unsynced.path(),
        ["evaluate", "--live", "--branch", "main", "--limit", "1"],
    );
    assert!(
        !denied.status.success(),
        "live evaluation must fail closed without a history index"
    );
    let denied_err = String::from_utf8_lossy(&denied.stderr);
    assert!(
        denied_err.contains("history") || denied_err.contains("index"),
        "{denied_err}"
    );

    let conflict = run(
        root,
        [
            "evaluate",
            "--live",
            "--input",
            "corpus.json",
            "--limit",
            "1",
        ],
    );
    assert!(!conflict.status.success(), " --live conflicts with --input");

    run_json(
        root,
        ["history", "sync", "--branch", "main", "--limit", "20"],
    );
    let report = run_json(
        root,
        [
            "evaluate", "--live", "--branch", "main", "--limit", "2", "--k", "5",
        ],
    );
    assert_eq!(report["mode"], "live_git");
    assert_eq!(report["leakage_violations"], 0);
    assert_eq!(report["cases_evaluated"], 2);
    assert_eq!(report["held_out_indexed_cases"], 2);
    assert_eq!(report["production_weight"], 0.5);
    assert_eq!(report["production_exponent"], 2.0);
    let variant_names = report["variants"]
        .as_array()
        .expect("variants")
        .iter()
        .map(|variant| variant["name"].as_str().expect("name"))
        .collect::<Vec<_>>();
    assert_eq!(
        variant_names,
        [
            "no_commit_text",
            "linear_0.5",
            "squared_0.5",
            "squared_0.25",
            "linear_1.0"
        ]
    );
    for name in ["all", "title_restating", "without_title_restating"] {
        assert!(
            report["cohorts"]
                .as_array()
                .expect("cohorts")
                .iter()
                .any(|cohort| cohort["name"] == name),
            "missing cohort {name}"
        );
    }
    let cohort_cases = |name: &str| {
        report["cohorts"]
            .as_array()
            .expect("cohorts")
            .iter()
            .find(|cohort| cohort["name"] == name)
            .expect("cohort")["cases"]
            .as_u64()
            .expect("cases")
    };
    assert_eq!(cohort_cases("all"), 2);
    assert_eq!(cohort_cases("title_restating"), 1);
    assert_eq!(cohort_cases("without_title_restating"), 1);

    let cases = report["cases"].as_array().expect("cases");
    let case_for = |commit: &str| {
        cases
            .iter()
            .find(|case| case["commit"] == commit)
            .unwrap_or_else(|| panic!("missing case {commit}"))
    };
    let later_case = case_for(later);
    let held_case = case_for(held_out);
    assert_eq!(later_case["parent"].as_str(), Some(held_out));
    assert_eq!(held_case["parent"].as_str(), Some(parent));
    assert_eq!(later_case["title_restating"], true);
    assert_eq!(held_case["title_restating"], false);
    assert_eq!(
        held_case["subject"].as_str(),
        Some("repair widget cache invalidation for durable defaults")
    );
    assert!(
        held_case["truth"]
            .as_array()
            .expect("truth")
            .iter()
            .any(|selector| selector == "file:src/widget.rs")
    );
    let prohibited = |case: &Value, id: &str| {
        case["prohibited_delivery_ids"]
            .as_array()
            .expect("prohibited")
            .iter()
            .any(|value| value == id)
    };
    let held_id = format!("git:{held_out}");
    let later_id = format!("git:{later}");
    assert!(prohibited(held_case, &held_id));
    assert!(prohibited(held_case, &later_id));
    assert!(prohibited(later_case, &later_id));
    assert!(!prohibited(later_case, &held_id));
    let evidence_excludes = |case: &Value, forbidden: &[&str]| {
        assert_eq!(case["held_out_indexed"], true);
        assert_eq!(case["leakage"].as_array().expect("leakage").len(), 0);
        for variant in case["variants"].as_array().expect("variants") {
            let evidence = variant["evidence_delivery_ids"]
                .as_array()
                .expect("evidence");
            assert!(
                evidence
                    .iter()
                    .all(|id| id.as_str().is_none_or(|id| !forbidden.contains(&id))),
                "prohibited delivery leaked into {}: {evidence:?}",
                variant["name"]
            );
        }
    };
    // C's target is its parent, so both C and the later commit are prohibited.
    // D's target is C, so C's own delivery is eligible and only D is prohibited.
    evidence_excludes(held_case, &[&held_id, &later_id]);
    evidence_excludes(later_case, &[&later_id]);
    let squared = held_case["variants"]
        .as_array()
        .expect("variants")
        .iter()
        .find(|variant| variant["name"] == "squared_0.5")
        .expect("squared");
    assert!(
        squared["ranked"]
            .as_array()
            .expect("ranked")
            .iter()
            .any(|selector| selector == "file:src/widget.rs"),
        "ancestor commit text should surface the held-out file: {squared}"
    );

    let human = run(
        root,
        [
            "evaluate", "--live", "--branch", "main", "--limit", "2", "--k", "5",
        ],
    );
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    assert!(String::from_utf8_lossy(&human.stdout).contains("squared_0.5"));
}

fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout<const N: usize>(cwd: &Path, args: [&str; N]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git output is UTF-8")
        .trim()
        .to_string()
}

/// Whether the test process runs as root, for whom `chmod 000` restricts
/// nothing. A permission test then skips, naming the missing capability on
/// stderr (STD-04 §R8); CI runs it as a regular user.
#[cfg(unix)]
fn skip_as_root(test: &str) -> bool {
    use std::io::Write as _;
    // SAFETY: geteuid has no preconditions and cannot fail.
    if unsafe { libc::geteuid() } != 0 {
        return false;
    }
    let _ = writeln!(
        std::io::stderr(),
        "skipping {test}: chmod 000 does not restrict root"
    );
    true
}

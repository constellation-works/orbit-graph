//! Real-executable coverage for Orbit plugin and chronological evaluation paths.

#![allow(clippy::expect_used)]
#![allow(
    clippy::disallowed_methods,
    reason = "fixtures are written with fs::write; clippy.toml bans it only from shipped code"
)]

use std::fs;
#[cfg(unix)]
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::fd::{FromRawFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use serde_json::{Value, json};
use tempfile::TempDir;

use orbit_graph::EXTRACTOR_VERSION;

mod common;

// The plugin contract's tool names and envelope version. The protocol lives in
// the `orbit-graph` binary, which has no library target to import them from;
// `plugin_contract` ties the envelope version to the executable's report.
const RECOMMEND_TOOL_NAME: &str = "orbit.graph.recommend";
const STATUS_TOOL_NAME: &str = "orbit.graph.status";
const MAINTAIN_TOOL_NAME: &str = "orbit.graph.maintain";
const VERSION_TOOL_NAME: &str = "orbit.graph.version";
const CHANGES_TOOL_NAME: &str = "orbit.graph.changes";
const PLUGIN_SCHEMA_VERSION: u32 = 1;

#[cfg(unix)]
#[derive(serde::Serialize)]
struct LauncherProbeEnvelope {
    ok: bool,
    output: LauncherProbeOutput,
}

#[cfg(unix)]
#[derive(serde::Serialize)]
struct LauncherProbeOutput {
    crate_version: &'static str,
    extractor_version: u32,
    history_schema_version: u32,
    plugin_schema_version: u32,
    store_schema_version: u32,
}

/// Tools of the root manifest that are not read-only code-graph queries.
const NON_QUERY_TOOLS: [&str; 5] = ["version", "status", "recommend", "maintain", "changes"];
/// `GRAPH_ORBIT_TIMEOUT_SECONDS` for the adapter tests that bound time.
const ADAPTER_TIMEOUT_SECONDS: u64 = 1;
/// What a bounded adapter call may take beyond its timeout: process start-up,
/// history reads and scheduling under a loaded CI host. Generous, because a
/// stuck subprocess is killed at the timeout, so a passing run never waits
/// for it, and a failing one waits [`STUCK_PAST_CEILING`] longer.
const LATENCY_MARGIN: Duration = Duration::from_secs(10);
/// How long the fake Orbit's stuck subprocesses outlive the ceiling, so a call
/// that waited for one instead of timing out cannot pass.
const STUCK_PAST_CEILING: Duration = Duration::from_secs(10);

#[test]
fn maintain_import_redacts_task_text_in_history_database() {
    let fixture = evaluation_fixture();
    let token = "glpat-12345678901234567890";
    let mut delivery = fixture_delivery(fixture.path(), "training");
    delivery["tasks"][0]["title"] = json!(format!("release {token}"));
    delivery["tasks"][0]["description"] =
        json!("https://deploy:very-private-password@gitlab.example/repo");
    delivery["tasks"][0]["acceptance_criteria"] = json!(["Authorization: Bearer abc.def.ghi"]);
    plugin_json(
        fixture.path(),
        MAINTAIN_TOOL_NAME,
        json!({
            "schema_version": 1,
            "operation": "import",
            "repository": fixture.path().canonicalize().expect("canonical fixture"),
            "branch": "main",
            "delivery": delivery,
        }),
    );
    let index =
        orbit_graph::HistoryIndex::open_read_only(fixture.path(), "main").expect("history index");
    let stored = index
        .delivery("fixture:training")
        .expect("read delivery")
        .expect("delivery");
    assert!(stored.delivery.tasks[0].title.contains("[REDACTED_SECRET]"));
    let bytes = fs::read(index.database_path()).expect("read history database");
    for secret in [token, "very-private-password", "abc.def.ghi"] {
        assert!(
            !bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes())
        );
    }
}

/// The longest a call bounded by [`ADAPTER_TIMEOUT_SECONDS`] may take.
fn adapter_latency_ceiling() -> Duration {
    Duration::from_secs(ADAPTER_TIMEOUT_SECONDS) + LATENCY_MARGIN
}

/// How long a stuck fake Orbit subprocess sleeps.
fn stuck_seconds() -> u64 {
    (adapter_latency_ceiling() + STUCK_PAST_CEILING).as_secs()
}

#[test]
fn no_argv_plugin_supports_status_and_query_and_task_id_both_levels() {
    let fixture = evaluation_fixture();
    let repository = fixture.path().canonicalize().expect("canonical fixture");
    let _ = plugin_json(
        fixture.path(),
        MAINTAIN_TOOL_NAME,
        json!({
            "schema_version": 1,
            "operation": "import",
            "repository": repository,
            "branch": "main",
            "delivery": fixture_delivery(fixture.path(), "training")
        }),
    );

    let status = plugin_json(
        fixture.path(),
        STATUS_TOOL_NAME,
        json!({"schema_version": 1, "repository": repository, "branch": "main"}),
    );
    assert_eq!(status["operation"], "status");
    assert_eq!(status["status"]["verified_deliveries"], 0);
    let status_with_mode_environment = plugin_output_with_env(
        fixture.path(),
        STATUS_TOOL_NAME,
        json!({"schema_version": 1, "repository": repository, "branch": "main"}),
        &[
            ("ORBIT_GRAPH_FORMAT", std::ffi::OsStr::new("table")),
            ("CLICOLOR_FORCE", std::ffi::OsStr::new("1")),
        ],
    );
    assert!(status_with_mode_environment.status.success());
    let status_with_mode_environment = plugin_success(&status_with_mode_environment);
    assert_eq!(status_with_mode_environment, status);
    #[cfg(unix)]
    {
        let status_with_tty = plugin_json_with_tty_stdout(
            fixture.path(),
            STATUS_TOOL_NAME,
            json!({"schema_version": 1, "repository": repository, "branch": "main"}),
        );
        assert_eq!(status_with_tty, status);
    }

    for level in ["file", "symbol"] {
        let query = plugin_json(
            fixture.path(),
            RECOMMEND_TOOL_NAME,
            json!({
                "schema_version": 1,
                "repository": repository,
                "query": "parser validation",
                "level": level,
                "branch": "main",
                "cutoff": "unix:20"
            }),
        );
        assert_eq!(query["adapter"]["task_text"], "request_query");
        assert!(
            query["result"]["recommendations"]
                .as_array()
                .is_some_and(|items| !items.is_empty()),
            "query recommendations missing in {level}: {query}"
        );

        let task = plugin_json(
            fixture.path(),
            RECOMMEND_TOOL_NAME,
            json!({
                "schema_version": 1,
                "repository": repository,
                "task_id": "TASK-TARGET",
                "task_snapshot": task_snapshot("TASK-TARGET", "parser validation", 15),
                "level": level,
                "branch": "main",
                "revision": git_stdout(fixture.path(), ["rev-parse", "HEAD~2"]),
                "cutoff": "unix:20"
            }),
        );
        assert_eq!(task["adapter"]["task_text"], "supplied_snapshot");
        assert_eq!(task["result"]["input"]["task_id"], "TASK-TARGET");
        assert!(
            task["result"]["recommendations"]
                .as_array()
                .is_some_and(|items| !items.is_empty()),
            "task recommendations missing in {level}: {task}"
        );
    }
}

#[test]
fn plugin_import_claim_is_stored_as_caller_attested_and_weighted_below_verified() {
    let fixture = evaluation_fixture();
    let repository = fixture.path().canonicalize().expect("canonical fixture");
    let imported = plugin_json(
        fixture.path(),
        MAINTAIN_TOOL_NAME,
        json!({
            "schema_version": 1,
            "operation": "import",
            "repository": repository,
            "branch": "main",
            "delivery": fixture_delivery(fixture.path(), "training")
        }),
    );
    assert_eq!(imported["evidence"]["requested"], "verified_delivery");
    assert_eq!(imported["evidence"]["stored"], "caller_attested");
    assert_eq!(imported["evidence"]["downgraded"], true);
    let index = orbit_graph::HistoryIndex::open_read_only(&repository, "main")
        .expect("read imported delivery");
    let delivery = index
        .delivery("fixture:training")
        .expect("read delivery")
        .expect("imported delivery");
    assert_eq!(
        serde_json::to_value(delivery.delivery.evidence).expect("encode evidence"),
        "caller_attested"
    );
    let conn = rusqlite::Connection::open(index.database_path()).expect("open history rows");
    let evidence: String = conn
        .query_row(
            "SELECT evidence FROM history_deliveries WHERE delivery_id=?1",
            ["fixture:training"],
            |row| row.get(0),
        )
        .expect("stored evidence");
    assert_eq!(evidence, "caller_attested");
    let recommend = |repo: &Path| {
        plugin_json(
            repo,
            RECOMMEND_TOOL_NAME,
            json!({
                "schema_version": 1,
                "repository": repo,
                "branch": "main",
                "query": "parser validation",
                "level": "file",
                "cutoff": "unix:20"
            }),
        )
    };
    let attested = recommend(&repository);
    let reasons = &attested["result"]["recommendations"][0]["reasons"];
    assert!(
        reasons.to_string().contains("actual-change evidence 0.55"),
        "{attested}"
    );

    // The standalone import is the trusted producer path. It still keeps
    // verified evidence and its full weight on the same agent-main shape.
    let trusted = evaluation_fixture();
    let trusted_repository = trusted.path().canonicalize().expect("trusted repository");
    let delivery_path = trusted.path().join("trusted-delivery.json");
    fs::write(
        &delivery_path,
        serde_json::to_vec(&fixture_delivery(trusted.path(), "training")).expect("encode delivery"),
    )
    .expect("write delivery");
    let output = run(
        trusted.path(),
        [
            "history",
            "import",
            "--input",
            delivery_path.to_str().expect("delivery path"),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let trusted_index = orbit_graph::HistoryIndex::open_read_only(&trusted_repository, "main")
        .expect("read trusted index");
    assert_eq!(
        trusted_index
            .delivery("fixture:training")
            .expect("read trusted delivery")
            .expect("trusted delivery")
            .delivery
            .evidence,
        orbit_graph::DeliveryEvidence::VerifiedDelivery
    );
    let verified = recommend(&trusted_repository);
    let reasons = &verified["result"]["recommendations"][0]["reasons"];
    assert!(
        reasons.to_string().contains("actual-change evidence 1.00"),
        "{verified}"
    );
}

#[test]
fn plugin_v2_envelopes_wrap_all_tools_and_errors_while_v1_is_deprecated() {
    let fixture = evaluation_fixture();
    let repository = fixture.path().canonicalize().expect("canonical fixture");

    for (tool, input, operation) in [
        (
            MAINTAIN_TOOL_NAME,
            json!({
                "operation": "history_sync",
                "repository": repository,
                "branch": "main",
                "limit": 100
            }),
            "history_sync",
        ),
        (
            STATUS_TOOL_NAME,
            json!({"repository": repository, "branch": "main"}),
            "status",
        ),
        (
            RECOMMEND_TOOL_NAME,
            json!({
                "repository": repository,
                "branch": "main",
                "query": "parser validation"
            }),
            "recommend",
        ),
    ] {
        let output = plugin_output_with_env(fixture.path(), tool, input, &[]);
        let value = plugin_success(&output);
        assert_eq!(value["operation"], operation);
        assert!(output.stderr.is_empty(), "v2 requests are not deprecated");
    }

    let invalid = plugin_output_with_env(
        fixture.path(),
        STATUS_TOOL_NAME,
        json!({"schema_version": 2}),
        &[],
    );
    assert_plugin_error(&invalid, "invalid v2 input");

    let bare = plugin_raw_output(
        fixture.path(),
        Some(STATUS_TOOL_NAME),
        json!({"repository": repository, "branch": "main"}),
        &[],
    );
    let bare_value = plugin_success(&bare);
    assert_eq!(bare_value["operation"], "status");
    assert!(String::from_utf8_lossy(&bare.stderr).contains("deprecated"));

    let request = json!({
        "schema_version": 1,
        "tool": STATUS_TOOL_NAME,
        "input": {"repository": repository, "branch": "main"},
        "context": {"workspace_root": repository, "agent": "test", "model": "test"}
    });
    // Only `ORBIT_TOOL_NAME` selects the plugin protocol; without it stdin is
    // never read and a bare invocation prints help (STD-04 §R13).
    let without_environment_tool = plugin_raw_output(fixture.path(), None, request.clone(), &[]);
    assert!(without_environment_tool.status.success());
    assert!(
        String::from_utf8_lossy(&without_environment_tool.stdout)
            .contains("Usage: orbit-graph [OPTIONS] <COMMAND>"),
        "{}",
        String::from_utf8_lossy(&without_environment_tool.stdout)
    );

    let mismatch = plugin_raw_output(fixture.path(), Some(RECOMMEND_TOOL_NAME), request, &[]);
    let mismatch = assert_plugin_error(&mismatch, "tool selectors must agree");
    assert!(
        mismatch["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("does not match"))
    );
}

#[test]
fn plugin_version_is_repository_independent_and_deterministic() {
    let fixture = TempDir::new().expect("non-repository workspace");
    let first = plugin_json(fixture.path(), VERSION_TOOL_NAME, json!({}));
    let second = plugin_json(fixture.path(), VERSION_TOOL_NAME, json!({}));
    assert_eq!(first, second);
    assert_eq!(
        first,
        json!({
            "crate_version": env!("CARGO_PKG_VERSION"),
            "extractor_version": orbit_graph::EXTRACTOR_VERSION,
            "store_schema_version": orbit_graph::STORE_SCHEMA_VERSION,
            "history_schema_version": orbit_graph::HISTORY_INDEX_SCHEMA_VERSION,
            "plugin_schema_version": PLUGIN_SCHEMA_VERSION,
        })
    );
}

#[test]
fn plugin_errors_carry_stable_codes_and_validate_before_routing() {
    let not_a_repository = TempDir::new().expect("non-repository workspace");
    let fixture = evaluation_fixture();
    let code = |repository: &Path, tool: &str, input: Value| {
        let output = plugin_output_with_env(repository, tool, input, &[]);
        assert_plugin_error(&output, tool)["error"]["code"]
            .as_str()
            .expect("error code")
            .to_string()
    };

    // Request validation precedes repository routing, so a malformed request
    // is reported as such even when the repository is also unusable.
    for (tool, input) in [
        (STATUS_TOOL_NAME, json!({"schema_version": 2})),
        (STATUS_TOOL_NAME, json!({"unknown": true})),
        (
            RECOMMEND_TOOL_NAME,
            json!({"query": "parser", "task_id": "ORB-1"}),
        ),
        (RECOMMEND_TOOL_NAME, json!({"query": "  "})),
        (
            RECOMMEND_TOOL_NAME,
            json!({"query": "parser", "hybrid": true, "hybrid_limit": 0}),
        ),
        (
            MAINTAIN_TOOL_NAME,
            json!({"operation": "history_sync", "limit": 1001}),
        ),
        (
            MAINTAIN_TOOL_NAME,
            json!({"operation": "orbit_sync", "limit": 101}),
        ),
        (MAINTAIN_TOOL_NAME, json!({"operation": "import"})),
        (MAINTAIN_TOOL_NAME, json!({"operation": "rebuild"})),
        (
            MAINTAIN_TOOL_NAME,
            json!({"operation": "graph_sync", "budget_ms": 999}),
        ),
        (
            MAINTAIN_TOOL_NAME,
            json!({"operation": "graph_sync", "budget_ms": 110_001}),
        ),
        // graph_sync maintains the plugin state and refuses to run without it.
        (MAINTAIN_TOOL_NAME, json!({"operation": "graph_sync"})),
        (
            MAINTAIN_TOOL_NAME,
            json!({"operation": "history_sync", "full": true}),
        ),
        (
            MAINTAIN_TOOL_NAME,
            json!({"operation": "graph_sync", "branch": "main"}),
        ),
        (
            MAINTAIN_TOOL_NAME,
            json!({"operation": "graph_sync", "limit": 10}),
        ),
        (
            MAINTAIN_TOOL_NAME,
            json!({"operation": "orbit_sync", "budget_ms": 5000}),
        ),
        ("graph.unknown", json!({})),
    ] {
        assert_eq!(
            code(not_a_repository.path(), tool, input.clone()),
            "invalid_request",
            "{tool} {input}"
        );
    }
    for (tool, input) in [
        (STATUS_TOOL_NAME, json!({})),
        (RECOMMEND_TOOL_NAME, json!({"query": "parser"})),
        (
            MAINTAIN_TOOL_NAME,
            json!({"operation": "history_sync", "limit": 10}),
        ),
    ] {
        assert_eq!(
            code(not_a_repository.path(), tool, input.clone()),
            "repository_unavailable",
            "{tool} {input}"
        );
    }
    // A read-only tool against a repository whose history was never synced
    // fails with `index_missing` naming the maintenance call, in the caller's
    // own tool spelling, and creates nothing (STD-01 §R31).
    for (tool, maintain, input) in [
        (STATUS_TOOL_NAME, "orbit.graph.maintain", json!({})),
        ("graph.status", "graph.maintain", json!({})),
        (
            RECOMMEND_TOOL_NAME,
            "orbit.graph.maintain",
            json!({"query": "parser"}),
        ),
        (
            "graph.recommend",
            "graph.maintain",
            json!({"query": "parser"}),
        ),
    ] {
        let output = plugin_output_with_env(fixture.path(), tool, input, &[]);
        let response: Value = serde_json::from_slice(&output.stdout).expect("plugin response");
        assert_eq!(
            response["error"]["code"], "index_missing",
            "{tool}: {response}"
        );
        let message = response["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(maintain) && message.contains("history_sync"),
            "{tool}: {message}"
        );
    }
    assert!(!fixture.path().join(".orbit-graph").exists());
    plugin_json(
        fixture.path(),
        MAINTAIN_TOOL_NAME,
        json!({"operation": "history_sync", "limit": 10}),
    );
    // A well-formed request naming an unknown revision of a real repository
    // is `not_found`, distinct from a graph failure.
    assert_eq!(
        code(
            fixture.path(),
            RECOMMEND_TOOL_NAME,
            json!({"query": "parser", "revision": "no-such-revision"})
        ),
        "not_found"
    );
}

#[cfg(unix)]
#[test]
fn launchers_reject_a_stale_path_binary_and_ignore_the_environment() {
    let fixture = TempDir::new().expect("launcher fixture");
    let stale_dir = fixture.path().join("stale");
    fs::create_dir(&stale_dir).expect("stale directory");
    let stale = stale_dir.join("orbit-graph");
    fs::write(
        &stale,
        "#!/bin/sh\necho 'Usage: orbit-graph <COMMAND>' >&2\nexit 1\n",
    )
    .expect("stale binary");
    fs::set_permissions(&stale, fs::Permissions::from_mode(0o755))
        .expect("executable stale binary");
    // macOS dirname treats `--` as a pathname and accepts multiple operands,
    // unlike GNU dirname. Keep that behavior reproducible on CI hosts with
    // GNU coreutils too.
    let dirname = stale_dir.join("dirname");
    fs::write(
        &dirname,
        "#!/bin/sh\nfor path do\n    case \"$path\" in\n        */*) directory=${path%/*}; [ -n \"$directory\" ] || directory=/ ;;\n        *) directory=. ;;\n    esac\n    printf '%s\\n' \"$directory\"\ndone\n",
    )
    .expect("BSD-compatible dirname shim");
    fs::set_permissions(&dirname, fs::Permissions::from_mode(0o755))
        .expect("executable dirname shim");
    let stale_path = format!("{}:/usr/bin:/bin", stale_dir.display());
    let real = Path::new(env!("CARGO_BIN_EXE_orbit-graph"));
    let current_path = format!(
        "{}:{}",
        real.parent().expect("binary directory").display(),
        stale_path
    );

    for launcher in ["bin/orbit-graph", "plugin/bin/orbit-graph"] {
        let launcher = repository_root().join(launcher);
        // Orbit clears a backend's environment, so an override variable can
        // never select the executable: only the bundled binary and PATH do.
        let real_str = real.to_str().expect("UTF-8 binary path");
        let rejected = launcher_version(&launcher, &[UNBOUND], &stale_path, Some(real_str));
        assert_eq!(rejected.status.code(), Some(0), "{launcher:?}");
        let response: Value = serde_json::from_slice(&rejected.stdout).expect("structured error");
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "incompatible_binary");
        assert_eq!(
            response["error"]["path"],
            stale.to_str().expect("UTF-8 path")
        );
        assert!(
            response["error"]["message"]
                .as_str()
                .expect("message")
                .contains(&format!(
                    "extractor_version={}",
                    orbit_graph::EXTRACTOR_VERSION
                ))
        );
        assert!(
            rejected.stderr.is_empty(),
            "{launcher:?}: {}",
            String::from_utf8_lossy(&rejected.stderr)
        );

        let selected = launcher_version(&launcher, &[UNBOUND], &current_path, None);
        assert_eq!(
            selected.status.code(),
            Some(0),
            "{launcher:?}: {}",
            String::from_utf8_lossy(&selected.stderr)
        );
        let response: Value = serde_json::from_slice(&selected.stdout).expect("version envelope");
        assert_eq!(response["ok"], true, "{launcher:?}: {response}");
        // The committed manifests run under the named override, which every
        // call reports.
        assert_override_reported(&selected, real);
        assert_eq!(
            response["output"]["plugin_schema_version"],
            PLUGIN_SCHEMA_VERSION
        );
        assert_eq!(
            response["output"]["extractor_version"],
            orbit_graph::EXTRACTOR_VERSION
        );
    }
}

#[cfg(unix)]
#[test]
fn bundled_binary_precedes_path() {
    let fixture = TempDir::new().expect("bundled launcher fixture");
    let launcher_dir = fixture.path().join("bin");
    fs::create_dir(&launcher_dir).expect("launcher directory");
    let launcher = launcher_dir.join("orbit-graph");
    fs::copy(repository_root().join("bin/orbit-graph"), &launcher).expect("copy launcher");
    fs::copy(
        repository_root().join("plugin.yaml"),
        fixture.path().join("plugin.yaml"),
    )
    .expect("copy manifest");
    let stale_dir = fixture.path().join("stale");
    fs::create_dir(&stale_dir).expect("stale directory");
    fs::write(stale_dir.join("orbit-graph"), "#!/bin/sh\nexit 1\n").expect("stale PATH binary");
    fs::set_permissions(
        stale_dir.join("orbit-graph"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("executable stale binary");
    let path = format!("{}:/usr/bin:/bin", stale_dir.display());

    // scripts/bundle-plugin-binary.sh refuses an incompatible candidate and
    // copies a compatible one beside the launcher.
    let bundle = repository_root().join("scripts/bundle-plugin-binary.sh");
    let refused = Command::new("sh")
        .env("PATH", &path)
        .arg(&bundle)
        .arg("--binary")
        .arg(stale_dir.join("orbit-graph"))
        .arg(fixture.path())
        .output()
        .expect("run bundler with a stale binary");
    assert_eq!(refused.status.code(), Some(1));
    assert!(!launcher_dir.join("orbit-graph.bin").exists());
    let bundled = Command::new("sh")
        .env("PATH", &path)
        .arg(&bundle)
        .args(["--binary", env!("CARGO_BIN_EXE_orbit-graph")])
        .arg(fixture.path())
        .output()
        .expect("run bundler");
    assert!(
        bundled.status.success(),
        "{}",
        String::from_utf8_lossy(&bundled.stderr)
    );
    let bundled_binary = launcher_dir.join("orbit-graph.bin");
    assert!(
        !fs::symlink_metadata(&bundled_binary)
            .expect("bundled binary")
            .file_type()
            .is_symlink(),
        "Orbit refuses plugin trees containing symbolic links"
    );

    assert!(
        String::from_utf8_lossy(&bundled.stderr).contains("orbit plugin add"),
        "the bundler names the re-consent step: {}",
        String::from_utf8_lossy(&bundled.stderr)
    );
    let digest = sha256_hex(&bundled_binary);
    let manifest = fs::read_to_string(fixture.path().join("plugin.yaml")).expect("manifest");
    assert!(
        manifest.contains(&format!("    args: [--backend-sha256, {digest}]\n")),
        "the bundler binds the manifest to the bundled executable:\n{manifest}"
    );
    assert!(
        !manifest.contains(&format!("args: [{UNBOUND}]")),
        "{manifest}"
    );

    let selected = launcher_version(&launcher, &["--backend-sha256", &digest], &path, None);
    assert_eq!(selected.status.code(), Some(0));
    let response: Value = serde_json::from_slice(&selected.stdout).expect("version envelope");
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(
        response["output"]["plugin_schema_version"],
        PLUGIN_SCHEMA_VERSION
    );
    assert!(response.get("backend_override").is_none(), "{response}");
    assert!(
        selected.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );

    // --unbound needs the named override and leaves a bound manifest alone.
    let unbound = Command::new("sh")
        .env("PATH", &path)
        .arg(&bundle)
        .args(["--unbound", "--binary", env!("CARGO_BIN_EXE_orbit-graph")])
        .arg(fixture.path())
        .output()
        .expect("run bundler unbound against a bound manifest");
    assert_eq!(unbound.status.code(), Some(1));
    assert_eq!(
        fs::read_to_string(fixture.path().join("plugin.yaml")).expect("manifest"),
        manifest
    );
    fs::copy(
        repository_root().join("plugin.yaml"),
        fixture.path().join("plugin.yaml"),
    )
    .expect("restore the committed manifest");
    let unbound = Command::new("sh")
        .env("PATH", &path)
        .arg(&bundle)
        .args(["--unbound", "--binary", env!("CARGO_BIN_EXE_orbit-graph")])
        .arg(fixture.path())
        .output()
        .expect("run bundler unbound");
    assert!(
        unbound.status.success(),
        "{}",
        String::from_utf8_lossy(&unbound.stderr)
    );
    assert_eq!(
        fs::read_to_string(fixture.path().join("plugin.yaml")).expect("manifest"),
        fs::read_to_string(repository_root().join("plugin.yaml")).expect("committed manifest")
    );
    let selected = launcher_version(&launcher, &[UNBOUND], &path, None);
    assert_eq!(selected.status.code(), Some(0));
    // The launcher reports its own directory resolved (`pwd -P`), and macOS
    // temp dirs sit behind the /var -> /private/var symlink.
    let bundled_binary = bundled_binary
        .canonicalize()
        .expect("resolve the bundled binary");
    assert_override_reported(&selected, &bundled_binary);
}

/// The named override in `spec.backend.args`: run a compatible executable
/// whatever its digest, and say so.
#[cfg(unix)]
const UNBOUND: &str = "--allow-unbound-backend";

#[cfg(unix)]
#[test]
fn launcher_runs_only_the_executable_its_manifest_binds() {
    let fixture = TempDir::new().expect("binding fixture");
    let fake_dir = fixture.path().join("fake");
    fs::create_dir(&fake_dir).expect("fake directory");
    let fake = fake_dir.join("orbit-graph");
    let ran = fixture.path().join("fake-ran");
    // A PATH impostor that answers the version probe exactly as the real
    // executable does, so only the digest can tell them apart.
    executable(
        &fake,
        format!(
            "#!/bin/sh\ncat >/dev/null\n: > '{}'\nprintf '%s\\n' '{}'\n",
            ran.display(),
            serde_json::to_string(&LauncherProbeEnvelope {
                ok: true,
                output: LauncherProbeOutput {
                    crate_version: "fake",
                    extractor_version: EXTRACTOR_VERSION,
                    history_schema_version: orbit_graph::HISTORY_INDEX_SCHEMA_VERSION,
                    plugin_schema_version: PLUGIN_SCHEMA_VERSION,
                    store_schema_version: orbit_graph::STORE_SCHEMA_VERSION,
                },
            })
            .expect("serialize version envelope")
        ),
    );
    let fake_path = format!("{}:/usr/bin:/bin", fake_dir.display());
    let real = Path::new(env!("CARGO_BIN_EXE_orbit-graph"));
    let real_path = format!(
        "{}:/usr/bin:/bin",
        real.parent().expect("binary directory").display()
    );
    let real_digest = sha256_hex(real);
    let fake_digest = sha256_hex(&fake);

    for launcher in ["bin/orbit-graph", "plugin/bin/orbit-graph"] {
        let launcher = repository_root().join(launcher);
        let bound = ["--backend-sha256", real_digest.as_str()];

        let refused = launcher_version(&launcher, &bound, &fake_path, None);
        assert_eq!(refused.status.code(), Some(0), "{launcher:?}");
        let response: Value = serde_json::from_slice(&refused.stdout).expect("structured error");
        assert_eq!(response["ok"], false, "{launcher:?}: {response}");
        assert_eq!(response["error"]["code"], "incompatible_binary");
        assert_eq!(response["error"]["path"], fake.to_str().expect("UTF-8"));
        assert_eq!(response["error"]["detail"]["expected_sha256"], real_digest);
        assert_eq!(response["error"]["detail"]["actual_sha256"], fake_digest);
        assert!(
            !ran.exists(),
            "{launcher:?}: the digest is checked before the executable runs"
        );

        let selected = launcher_version(&launcher, &bound, &real_path, None);
        let response: Value = serde_json::from_slice(&selected.stdout).expect("version envelope");
        assert_eq!(response["ok"], true, "{launcher:?}: {response}");
        assert!(response.get("backend_override").is_none(), "{response}");
        assert!(selected.stderr.is_empty(), "{launcher:?}");

        // A manifest that neither binds nor names the override, or both, or
        // carries anything else, runs nothing.
        for args in [
            &[][..],
            &[UNBOUND, "--backend-sha256", real_digest.as_str()][..],
            &["--backend-sha256", "not-a-digest"][..],
            &["--verbose"][..],
        ] {
            let refused = launcher_version(&launcher, args, &real_path, None);
            let response: Value =
                serde_json::from_slice(&refused.stdout).expect("structured error");
            assert_eq!(
                response["error"]["code"], "incompatible_binary",
                "{launcher:?} {args:?}: {response}"
            );
        }

        // The operator override runs the impostor, and says so on stderr.
        let overridden = launcher_version(&launcher, &[UNBOUND], &fake_path, None);
        let response: Value = serde_json::from_slice(&overridden.stdout).expect("version envelope");
        assert_eq!(response["ok"], true, "{launcher:?}: {response}");
        let notice = String::from_utf8_lossy(&overridden.stderr);
        assert!(
            notice.contains("backend override") && notice.contains(fake.to_str().expect("UTF-8")),
            "{launcher:?}: {notice}"
        );
        fs::remove_file(&ran).expect("reset the impostor marker");
    }
}

#[cfg(unix)]
#[test]
fn launchers_reject_version_number_prefixes_before_the_real_request() {
    let fixture = TempDir::new().expect("version probe fixture");
    let fake_dir = fixture.path().join("fake");
    fs::create_dir(&fake_dir).expect("fake directory");
    let fake = fake_dir.join("orbit-graph");
    let fake_path = format!("{}:/usr/bin:/bin", fake_dir.display());
    let cases = [
        ("a stale extractor version 21", 21, 1, true, false),
        (
            "current 23/1 envelope",
            EXTRACTOR_VERSION,
            PLUGIN_SCHEMA_VERSION,
            true,
            true,
        ),
        ("extractor version 210", 210, 1, true, false),
        (
            "an extractor version with the current numeric prefix",
            EXTRACTOR_VERSION * 10,
            1,
            true,
            false,
        ),
        (
            "plugin schema version 10",
            EXTRACTOR_VERSION,
            10,
            true,
            false,
        ),
        (
            "versions outside the supported envelope",
            EXTRACTOR_VERSION,
            PLUGIN_SCHEMA_VERSION,
            false,
            false,
        ),
    ];

    for launcher_name in ["bin/orbit-graph", "plugin/bin/orbit-graph"] {
        let launcher = repository_root().join(launcher_name);
        for (
            index,
            (description, extractor_version, plugin_schema_version, supported_shape, compatible),
        ) in cases.iter().enumerate()
        {
            let received_request = fixture.path().join(format!("real-request-{index}"));
            let response = if *supported_shape {
                serde_json::to_string(&LauncherProbeEnvelope {
                    ok: true,
                    output: LauncherProbeOutput {
                        crate_version: "fake",
                        extractor_version: *extractor_version,
                        history_schema_version: 1,
                        plugin_schema_version: *plugin_schema_version,
                        store_schema_version: 1,
                    },
                })
                .expect("serialize version envelope")
            } else {
                json!({
                    "ok": true,
                    "metadata": {
                        "output": {
                            "crate_version": "fake",
                            "extractor_version": extractor_version,
                            "store_schema_version": 1,
                            "history_schema_version": 1,
                            "plugin_schema_version": plugin_schema_version,
                        }
                    }
                })
                .to_string()
            };
            executable(
                &fake,
                format!(
                    "#!/bin/sh\nif [ \"${{ORBIT_GRAPH_LAUNCHER_PROBING:-}}\" = 1 ]; then\n    cat >/dev/null\nelse\n    cat > '{}'\nfi\nprintf '%s\\n' '{}'\n",
                    received_request.display(),
                    response
                ),
            );

            let refused = launcher_version(&launcher, &[UNBOUND], &fake_path, None);
            let result: Value = serde_json::from_slice(&refused.stdout).expect("launcher response");
            if *compatible {
                assert_eq!(
                    result["ok"], true,
                    "{launcher_name}: {description}: {result}"
                );
                assert!(
                    received_request.exists(),
                    "{launcher_name}: {description}: launcher did not send the real request"
                );
            } else {
                assert_eq!(
                    result["error"]["code"], "incompatible_binary",
                    "{launcher_name}: {description}: {result}"
                );
                assert!(
                    !received_request.exists(),
                    "{launcher_name}: {description}: launcher sent the real request"
                );
            }
        }
    }
}

#[cfg(unix)]
fn assert_override_reported(output: &Output, binary: &Path) {
    let response: Value = serde_json::from_slice(&output.stdout).expect("response envelope");
    let binary = binary.to_str().expect("UTF-8 path");
    assert_eq!(response["backend_override"], binary, "{response}");
    let notice = String::from_utf8_lossy(&output.stderr);
    assert!(
        notice.contains("backend override") && notice.contains(binary),
        "stderr names the override: {notice}"
    );
}

#[cfg(unix)]
fn sha256_hex(path: &Path) -> String {
    for (program, args) in [("sha256sum", &[][..]), ("shasum", &["-a", "256"][..])] {
        if let Ok(output) = Command::new(program).args(args).arg(path).output()
            && output.status.success()
        {
            let stdout = String::from_utf8(output.stdout).expect("digest output");
            return stdout
                .split_whitespace()
                .next()
                .expect("digest")
                .to_string();
        }
    }
    panic!("neither sha256sum nor shasum is available");
}

#[cfg(unix)]
fn launcher_version(
    launcher: &Path,
    args: &[&str],
    path: &str,
    environment_binary: Option<&str>,
) -> Output {
    let mut command = Command::new(launcher);
    command
        .args(args)
        .env("PATH", path)
        .env_remove("ORBIT_GRAPH_BACKEND_OVERRIDE")
        .env("ORBIT_TOOL_NAME", VERSION_TOOL_NAME)
        .env_remove("ORBIT_GRAPH_BIN")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(binary) = environment_binary {
        command.env("ORBIT_GRAPH_BIN", binary);
    }
    let mut child = command.spawn().expect("spawn launcher");
    child
        .stdin
        .as_mut()
        .expect("launcher stdin")
        .write_all(b"{\"tool\":\"orbit.graph.version\",\"input\":{}}")
        .expect("write version envelope");
    child.wait_with_output().expect("launcher result")
}

#[test]
fn v2_context_defaults_repository_and_explicit_input_wins() {
    let fixture = evaluation_fixture();
    let repository = fixture.path().canonicalize().expect("canonical fixture");

    let maintained = plugin_json(
        fixture.path(),
        MAINTAIN_TOOL_NAME,
        json!({"operation": "history_sync", "branch": "main", "limit": 100}),
    );
    assert_eq!(
        maintained["repository"],
        repository.to_string_lossy().as_ref()
    );
    let status = plugin_json(fixture.path(), STATUS_TOOL_NAME, json!({"branch": "main"}));
    assert_eq!(status["repository"], repository.to_string_lossy().as_ref());
    let recommendation = plugin_json(
        fixture.path(),
        RECOMMEND_TOOL_NAME,
        json!({"branch": "main", "query": "parser validation"}),
    );
    assert_eq!(
        recommendation["repository"],
        repository.to_string_lossy().as_ref()
    );

    let other = TempDir::new().expect("other context");
    let request = json!({
        "schema_version": 1,
        "tool": STATUS_TOOL_NAME,
        "input": {"repository": repository, "branch": "main"},
        "context": {"workspace_root": other.path(), "agent": "test", "model": "test"}
    });
    let explicit = plugin_success(&plugin_raw_output(
        fixture.path(),
        Some(STATUS_TOOL_NAME),
        request,
        &[],
    ));
    assert_eq!(
        explicit["repository"],
        repository.to_string_lossy().as_ref()
    );
}

/// A plugin envelope as Orbit sends it, with the effective
/// `[plugins.graph]` section as `context.config`.
fn configured_output(repository: &Path, tool: &str, input: Value, config: Value) -> Output {
    let request = json!({
        "schema_version": 1,
        "tool": tool,
        "input": input,
        "context": {
            "workspace_root": repository,
            "agent": "plugin-integration-test",
            "model": "test",
            "config": config
        }
    });
    plugin_raw_output(repository, Some(tool), request, &[])
}

#[test]
fn configured_branch_is_what_calls_naming_no_branch_work_on() {
    let fixture = evaluation_fixture();
    run_git(fixture.path(), ["branch", "agent-main", "HEAD~2"]);
    let agent_main = git_stdout(fixture.path(), ["rev-parse", "agent-main"]);
    let config = json!({"branch": "agent-main"});

    // The seeded activity's input, exactly as Orbit hands it to the tool.
    let activity: Value = serde_norway::from_str(
        &fs::read_to_string(repository_root().join("definitions/activities/history-sync.yaml"))
            .expect("history-sync activity"),
    )
    .expect("activity YAML");
    assert_eq!(activity["spec"]["config"]["tool"], MAINTAIN_TOOL_NAME);
    let activity_input = activity["spec"]["config"]["input"].clone();
    assert!(activity_input.get("branch").is_none(), "{activity_input}");
    let synced = plugin_success(&configured_output(
        fixture.path(),
        MAINTAIN_TOOL_NAME,
        activity_input,
        config.clone(),
    ));
    assert_eq!(synced["branch"], "agent-main", "{synced}");
    assert_eq!(synced["result"]["snapshot_tip"], agent_main.as_str());

    let status = plugin_success(&configured_output(
        fixture.path(),
        STATUS_TOOL_NAME,
        json!({}),
        config.clone(),
    ));
    assert_eq!(status["status"]["landing_branch"], "agent-main");
    assert_eq!(status["status"]["cursor"], agent_main.as_str());

    // A call's own branch wins over the configuration.
    let explicit = plugin_success(&configured_output(
        fixture.path(),
        MAINTAIN_TOOL_NAME,
        json!({"operation": "history_sync", "branch": "main"}),
        config.clone(),
    ));
    assert_eq!(explicit["branch"], "main");
    let status = plugin_success(&configured_output(
        fixture.path(),
        STATUS_TOOL_NAME,
        json!({"branch": "main"}),
        config,
    ));
    assert_eq!(status["status"]["landing_branch"], "main");

    // Without configuration the default is main.
    let default = plugin_json(
        fixture.path(),
        MAINTAIN_TOOL_NAME,
        json!({"operation": "history_sync"}),
    );
    assert_eq!(default["branch"], "main");

    // A key this backend does not read is named on stderr and otherwise
    // ignored; a malformed branch is refused.
    let ignored = configured_output(
        fixture.path(),
        STATUS_TOOL_NAME,
        json!({}),
        json!({"branch": "agent-main", "index_dir": ".orbit-graph"}),
    );
    plugin_success(&ignored);
    assert!(
        String::from_utf8_lossy(&ignored.stderr)
            .contains("ignoring plugin config key \"index_dir\""),
        "{}",
        String::from_utf8_lossy(&ignored.stderr)
    );
    for config in [json!({"branch": ""}), json!({"branch": 7}), json!("main")] {
        let refused = configured_output(fixture.path(), STATUS_TOOL_NAME, json!({}), config);
        let response = assert_plugin_error(&refused, "malformed config");
        assert_eq!(response["error"]["code"], "invalid_request", "{response}");
    }
}

#[test]
fn inapplicable_fields_are_refused_by_name_before_any_index_is_opened() {
    let fixture = evaluation_fixture();
    let state = TempDir::new().expect("plugin state");
    let refusal = |tool: &str, input: Value| -> String {
        let output = plugin_output_with_env(
            fixture.path(),
            tool,
            input.clone(),
            &[("ORBIT_PLUGIN_STATE", state.path().as_os_str())],
        );
        let response = assert_plugin_error(&output, tool);
        assert_eq!(response["error"]["code"], "invalid_request", "{input}");
        response["error"]["message"]
            .as_str()
            .expect("message")
            .to_string()
    };
    let delivery = fixture_delivery(fixture.path(), "training");
    let snapshot = task_snapshot("TASK-1", "Parser", 1);
    for (operation, fields) in [
        (
            "history_sync",
            json!({"delivery": delivery, "workspace": "ws", "task_ids": ["T-1"],
                   "run_ids": ["R-1"], "task_snapshots": [snapshot], "full": true,
                   "budget_ms": 5000}),
        ),
        (
            "import",
            json!({"delivery": delivery, "limit": 10, "workspace": "ws", "task_ids": ["T-1"],
                   "run_ids": ["R-1"], "task_snapshots": [snapshot], "full": true,
                   "budget_ms": 5000}),
        ),
        (
            "orbit_sync",
            json!({"workspace": "ws", "delivery": delivery, "full": true, "budget_ms": 5000}),
        ),
        (
            "graph_sync",
            json!({"branch": "main", "limit": 10, "delivery": delivery, "workspace": "ws",
                   "task_ids": ["T-1"], "run_ids": ["R-1"], "task_snapshots": [snapshot]}),
        ),
    ] {
        let reads: &[&str] = match operation {
            "history_sync" => &["branch", "limit"],
            "import" => &["branch", "delivery"],
            "orbit_sync" => &[
                "branch",
                "limit",
                "workspace",
                "task_ids",
                "run_ids",
                "task_snapshots",
            ],
            _ => &["full", "budget_ms"],
        };
        let mut input = fields.clone();
        input["operation"] = json!(operation);
        let message = refusal(MAINTAIN_TOOL_NAME, input);
        let named = message
            .split_once(" does not use ")
            .and_then(|(_, rest)| rest.split_once("; it reads only "))
            .map(|(named, _)| named.split(", ").collect::<Vec<_>>())
            .unwrap_or_else(|| panic!("{operation}: {message}"));
        for field in fields.as_object().expect("fields").keys() {
            assert_eq!(
                named.contains(&field.as_str()),
                !reads.contains(&field.as_str()),
                "{operation}: {field} in {message}"
            );
        }
    }
    let message = refusal(
        RECOMMEND_TOOL_NAME,
        json!({"query": "parser", "hybrid_limit": 5}),
    );
    assert!(message.contains("hybrid_limit"), "{message}");

    assert!(
        !fixture.path().join(".orbit-graph").exists(),
        "no history index was opened"
    );
    assert_eq!(
        fs::read_dir(state.path()).expect("plugin state").count(),
        0,
        "no plugin state was written"
    );
}

#[test]
fn plugin_state_contains_every_index_file() {
    let fixture = evaluation_fixture();
    let repository = fixture.path().canonicalize().expect("canonical fixture");
    let state = TempDir::new().expect("plugin state");
    let state_value = state.path().as_os_str();

    for input in [
        json!({"operation": "history_sync", "branch": "main", "limit": 100}),
        json!({"operation": "graph_sync"}),
    ] {
        let maintain = plugin_output_with_env(
            fixture.path(),
            MAINTAIN_TOOL_NAME,
            input,
            &[("ORBIT_PLUGIN_STATE", state_value)],
        );
        let _ = plugin_success(&maintain);
    }
    let output = plugin_output_with_env(
        fixture.path(),
        RECOMMEND_TOOL_NAME,
        json!({"branch": "main", "query": "parser validation"}),
        &[("ORBIT_PLUGIN_STATE", state_value)],
    );
    let recommendation = plugin_success(&output);
    assert_eq!(
        recommendation["repository"],
        repository.to_string_lossy().as_ref()
    );
    assert!(
        !fixture.path().join(".orbit-graph").exists(),
        "plugin state routing must not create repository-local indexes"
    );
    let repository_states = fs::read_dir(state.path())
        .expect("read plugin state")
        .collect::<Result<Vec<_>, _>>()
        .expect("plugin state entries");
    assert_eq!(repository_states.len(), 1);
    let names = fs::read_dir(repository_states[0].path())
        .expect("read repository state")
        .map(|entry| {
            entry
                .expect("state file")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        names.iter().any(|name| name.starts_with("change-history.")),
        "history index missing from plugin state: {names:?}"
    );
    for published in [
        "graph.current.json".to_string(),
        format!("graph.{EXTRACTOR_VERSION}.1.db"),
    ] {
        assert!(
            names.contains(&published),
            "code-graph index file {published} missing from plugin state: {names:?}"
        );
    }
}

/// Plugin state is created owner-only whatever the umask (STD-05 §R8): the
/// state root and the repository's `<hash>/` directory are `0700`, and every
/// index file, lock and pointer in it is `0600`. The umask is set on the
/// child process only (STD-04 §R6).
#[cfg(unix)]
#[test]
fn plugin_state_is_owner_only_under_a_permissive_umask() {
    let fixture = evaluation_fixture();
    let parent = TempDir::new().expect("plugin state parent");
    let state = parent.path().join("state");
    for input in [
        json!({"operation": "history_sync", "branch": "main", "limit": 100}),
        json!({"operation": "graph_sync"}),
    ] {
        let request = json!({
            "schema_version": 1,
            "tool": MAINTAIN_TOOL_NAME,
            "input": input,
            "context": {
                "workspace_root": fixture.path(),
                "agent": "plugin-integration-test",
                "model": "test"
            }
        });
        let mut child = Command::new("sh")
            .current_dir(fixture.path())
            .args(["-c", "umask 0002; exec \"$@\"", "sh"])
            .arg(env!("CARGO_BIN_EXE_orbit-graph"))
            .env("ORBIT_TOOL_NAME", MAINTAIN_TOOL_NAME)
            .env("ORBIT_PLUGIN_STATE", &state)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn plugin under umask 0002");
        child
            .stdin
            .take()
            .expect("plugin stdin")
            .write_all(request.to_string().as_bytes())
            .expect("write plugin request");
        let output = child.wait_with_output().expect("run plugin");
        let _ = plugin_success(&output);
    }

    let mode = |path: &Path| {
        fs::symlink_metadata(path)
            .expect("state metadata")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&state), 0o700, "plugin state root");
    let repositories = fs::read_dir(&state)
        .expect("read plugin state")
        .map(|entry| entry.expect("state entry").path())
        .collect::<Vec<_>>();
    assert_eq!(repositories.len(), 1, "{repositories:?}");
    let repository_state = &repositories[0];
    assert_eq!(
        mode(repository_state),
        0o700,
        "{}",
        repository_state.display()
    );
    let mut files = 0;
    for entry in fs::read_dir(repository_state).expect("read repository state") {
        let path = entry.expect("state file").path();
        let expected = if path.is_dir() { 0o700 } else { 0o600 };
        assert_eq!(mode(&path), expected, "{}", path.display());
        files += 1;
    }
    assert!(
        files >= 4,
        "graph, history, lock and pointer files expected"
    );
}

/// The evaluation workspace is a fresh `0700` directory with a random name
/// (STD-05 §R8), so a directory another process planted at a predictable
/// `<tmp>/orbit-graph-evaluation-<pid>-<n>` is neither used nor removed.
#[cfg(unix)]
#[test]
fn evaluation_workspace_never_reuses_or_removes_a_predictable_temp_path() {
    let fixture = evaluation_fixture();
    let repository = fixture.path().canonicalize().expect("canonical fixture");
    let target_revision = git_stdout(fixture.path(), ["rev-parse", "HEAD~2"]);
    let temp = TempDir::new().expect("evaluation temp root");
    let corpus_path = temp.path().join("evaluation.json");
    let corpus = json!({
        "schema_version": 1,
        "repository": repository,
        "landing_branch": "main",
        "source": {"system": "test-public-export", "record_id": "fixture-v1"},
        "complete": true,
        "coverage_note": "complete two-delivery synthetic fixture",
        "k": 3,
        "training_deliveries": [
            fixture_delivery(fixture.path(), "training"),
            fixture_delivery(fixture.path(), "future")
        ],
        "cases": [{
            "id": "prospective-target",
            "target_revision": target_revision,
            "cutoff": "unix:20",
            "task_snapshot": task_snapshot("TASK-TARGET", "parser validation", 15),
            "held_out_delivery": fixture_delivery(fixture.path(), "held-out"),
            "source": {"system": "test-public-observation", "record_id": "target@15"}
        }]
    });
    fs::write(
        &corpus_path,
        serde_json::to_vec_pretty(&corpus).expect("encode corpus"),
    )
    .expect("write corpus");
    let tmpdir = temp.path().join("tmp");
    fs::create_dir(&tmpdir).expect("temp dir");

    // `$$` is the shell's pid, which `exec` hands to orbit-graph: the planted
    // directory is the path the first evaluation workspace used to take.
    let output = Command::new("sh")
        .current_dir(fixture.path())
        .env("TMPDIR", &tmpdir)
        .args([
            "-c",
            "planted=\"$TMPDIR/orbit-graph-evaluation-$$-0\"; \
             mkdir \"$planted\" && echo planted > \"$planted/sentinel\" && exec \"$@\"",
            "sh",
        ])
        .arg(env!("CARGO_BIN_EXE_orbit-graph"))
        .args(["--format", "json", "evaluate", "--input"])
        .arg(&corpus_path)
        .output()
        .expect("run evaluation");
    assert!(
        output.status.success(),
        "evaluation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("evaluation JSON");
    assert_eq!(report["coverage"]["cases_evaluated"], 1, "{report}");

    let remaining = fs::read_dir(&tmpdir)
        .expect("read temp dir")
        .map(|entry| entry.expect("temp entry").path())
        .collect::<Vec<_>>();
    assert_eq!(
        remaining.len(),
        1,
        "only the planted directory remains: {remaining:?}"
    );
    let sentinel = remaining[0].join("sentinel");
    assert_eq!(
        fs::read_to_string(&sentinel).expect("the planted directory is untouched"),
        "planted\n"
    );
    assert_eq!(
        fs::read_dir(&remaining[0]).expect("planted dir").count(),
        1,
        "nothing was cloned into the planted directory"
    );
}

/// A repository with a call chain and an import for the query tools.
fn query_fixture() -> TempDir {
    let fixture = TempDir::new().expect("create query fixture");
    run_git(fixture.path(), ["init", "-b", "main"]);
    run_git(
        fixture.path(),
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(fixture.path(), ["config", "user.name", "Graph Test"]);
    fs::create_dir_all(fixture.path().join("src")).expect("create src");
    fs::write(
        fixture.path().join("src/parser.rs"),
        "pub fn parse() -> bool { drop(0); helper() }\npub fn helper() -> bool { true }\n",
    )
    .expect("write parser");
    fs::write(
        fixture.path().join("src/lib.rs"),
        "mod parser;\nuse crate::parser::parse;\npub fn parser_test() -> bool { parse() }\n",
    )
    .expect("write lib");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "base"]);
    fixture
}

#[test]
fn query_tools_answer_from_the_published_index_and_name_missing_and_stale_indexes() {
    let fixture = query_fixture();
    let state = TempDir::new().expect("plugin state");
    let environment = [("ORBIT_PLUGIN_STATE", state.path().as_os_str())];
    let output = |tool: &str, input: Value| {
        plugin_output_with_env(fixture.path(), tool, input, &environment)
    };
    let call = |verb: &str, input: Value| {
        let response = plugin_success(&output(&format!("orbit.graph.{verb}"), input));
        assert_matches_schema(&response, &format!("schemas/{verb}.response.json"));
        assert_eq!(response["operation"], verb, "{response}");
        response
    };
    let parse = "symbol:src/parser.rs#parse:function";
    let helper = "symbol:src/parser.rs#helper:function";
    let requests = [
        ("search", json!({"query": "parse"})),
        ("show", json!({"selector": parse})),
        ("refs", json!({"selector": helper})),
        ("callees", json!({"selector": parse})),
        (
            "impact",
            json!({"selector": helper, "direction": "inbound"}),
        ),
        ("trace", json!({"command": "parse"})),
        ("deps", json!({"selector": "file:src/lib.rs"})),
        ("overview", json!({})),
    ];
    assert_eq!(
        requests.clone().map(|(verb, _)| verb).to_vec(),
        manifest_query_verbs()
    );

    // No index yet: every tool fails with a code and names the call that
    // builds one, in the spelling the caller used.
    for (verb, input) in &requests {
        for (prefix, maintain) in [
            ("orbit.graph.", "orbit.graph.maintain"),
            ("graph.", "graph.maintain"),
        ] {
            let tool = format!("{prefix}{verb}");
            let response = assert_plugin_error(&output(&tool, input.clone()), &tool);
            assert_eq!(response["error"]["code"], "index_missing", "{tool}");
            let message = response["error"]["message"].as_str().expect("message");
            assert!(
                message.contains(maintain) && message.contains("graph_sync"),
                "{tool}: {message}"
            );
        }
    }

    let synced = plugin_success(&output(
        MAINTAIN_TOOL_NAME,
        json!({"operation": "graph_sync"}),
    ));
    assert_matches_schema(&synced, "schemas/maintain.response.json");
    let index_dir = PathBuf::from(
        synced["code_index"]["directory"]
            .as_str()
            .expect("index directory"),
    );
    let state_files = || {
        fs::read_dir(&index_dir)
            .expect("list index directory")
            .map(|entry| {
                entry
                    .expect("index entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    let published_files = state_files();
    let head = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);

    for (verb, input) in &requests {
        let response = call(verb, input.clone());
        assert_eq!(response["index"]["fresh"], true, "{response}");
        assert_eq!(response["index"]["revision"], head.as_str());
        assert!(response["index"].get("stale").is_none(), "{response}");
        assert_eq!(response["truncated"], false, "{response}");
    }
    let names = |items: &Value, field: &str| {
        items
            .as_array()
            .expect("result array")
            .iter()
            .filter_map(|item| item[field].as_str().map(str::to_string))
            .collect::<Vec<_>>()
    };
    let search = call("search", json!({"query": "parse", "kind": "symbol"}));
    assert!(names(&search["result"]["matches"], "name").contains(&"parse".to_string()));
    let show = call("show", json!({"selector": parse, "max_bytes": 1024}));
    assert_eq!(show["result"]["metadata"]["name"], "parse");
    assert!(
        show["result"]["source"]
            .as_str()
            .is_some_and(|source| source.contains("helper()")),
        "{show}"
    );
    let missing = call(
        "show",
        json!({"selector": "symbol:src/parser.rs#nope:function"}),
    );
    assert!(missing["result"].is_null(), "{missing}");
    let refs = call("refs", json!({"selector": helper, "confidence": "exact"}));
    assert!(!refs["result"]["refs"].as_array().expect("refs").is_empty());
    // Unresolved calls with no indexed definition (`drop`) are hidden and
    // counted by default, as the CLI does, and listed on request.
    let callees = call("callees", json!({"selector": parse}));
    assert_eq!(
        names(&callees["result"]["callees"], "target_name"),
        ["helper"]
    );
    assert_eq!(callees["result"]["hidden_unresolved"], 1, "{callees}");
    let all_callees = call(
        "callees",
        json!({"selector": parse, "include_unresolved": true}),
    );
    assert_eq!(
        names(&all_callees["result"]["callees"], "target_name"),
        ["drop", "helper"]
    );
    assert_eq!(all_callees["result"]["hidden_unresolved"], 0);
    let impact = call(
        "impact",
        json!({"selector": helper, "direction": "inbound", "depth": 2}),
    );
    let touched = names(&impact["result"]["touched"], "qualified_name");
    assert!(touched.contains(&"parse".to_string()), "{impact}");
    let trace = call("trace", json!({"command": "command:parse", "depth": 2}));
    assert!(trace["result"]["visited_nodes"].is_number(), "{trace}");
    let deps = call("deps", json!({"selector": "file:src/lib.rs"}));
    assert_eq!(deps["result"]["scope"], "file:src/lib.rs");
    let overview = call("overview", json!({"format": "full"}));
    assert_eq!(overview["result"]["total_files"], 2);
    // Filters reach the library.
    let python = call("search", json!({"query": "parse", "lang": "python"}));
    assert_eq!(python["result"]["matches"], json!([]), "{python}");
    let type_refs = call("refs", json!({"selector": helper, "kind": "type"}));
    assert_eq!(type_refs["result"]["refs"], json!([]), "{type_refs}");
    let scoped = call("overview", json!({"selector": "dir:src"}));
    assert_eq!(scoped["result"]["scope"], "src", "{scoped}");

    // Outputs are bounded, and every cut is reported.
    let bounded = call("overview", json!({"format": "full", "limit": 1}));
    assert_eq!(bounded["truncated"], true);
    // Nested arrays are capped too: the returned file lists one of its two
    // symbols.
    assert_eq!(
        bounded["truncation"],
        json!({
            "files": {"returned": 1, "total": 2},
            "files[].symbols": {"returned": 1, "total": 2},
        })
    );
    assert_eq!(bounded["result"]["files"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        bounded["result"]["files"][0]["symbols"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    // Search fetches one match past its limit, so a cut is reported with a
    // lower bound, and a limit that holds every match reports none.
    let cut = call("search", json!({"query": "parser", "limit": 1}));
    assert_eq!(cut["truncated"], true, "{cut}");
    assert_eq!(
        cut["truncation"],
        json!({"matches": {"returned": 1, "total_at_least": 2}})
    );
    let whole = call("search", json!({"query": "parser", "limit": 2}));
    assert_eq!(whole["truncated"], false, "{whole}");
    assert_eq!(
        names(&whole["result"]["matches"], "name"),
        ["parser", "parser_test"]
    );

    // Queries only read the published index (STD-01 R31).
    assert_eq!(state_files(), published_files);

    // A new commit leaves the index stale: results still come back, marked
    // with the revision they describe and the call that refreshes them.
    fs::write(
        fixture.path().join("src/extra.rs"),
        "pub fn extra() -> bool { true }\n",
    )
    .expect("write extra");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "extra"]);
    let stale = call("search", json!({"query": "parse"}));
    assert_eq!(stale["index"]["fresh"], false, "{stale}");
    assert_eq!(stale["index"]["revision"], head.as_str());
    assert_eq!(
        stale["index"]["stale"]["fix"],
        json!({"tool": "orbit.graph.maintain", "input": {"operation": "graph_sync"}})
    );
    let stale_bare = plugin_success(&output("graph.overview", json!({})));
    assert_eq!(
        stale_bare["index"]["stale"]["fix"]["tool"], "graph.maintain",
        "{stale_bare}"
    );

    // An index from another extractor is refused with the full-rebuild fix.
    let pointer_path = index_dir.join("graph.current.json");
    let mut pointer: Value =
        serde_json::from_str(&fs::read_to_string(&pointer_path).expect("read pointer"))
            .expect("parse pointer");
    pointer["extractor_version"] = json!(1);
    fs::write(&pointer_path, pointer.to_string()).expect("write pointer");
    let incompatible = assert_plugin_error(
        &output("orbit.graph.refs", json!({"selector": helper})),
        "incompatible index",
    );
    assert_eq!(incompatible["error"]["code"], "index_incompatible");
    assert!(
        incompatible["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("\"full\":true")),
        "{incompatible}"
    );
}

/// Check `value` against the subset of JSON Schema the plugin's schemas use
/// (type, const, enum, required, properties, additionalProperties as false
/// or a schema, items), as Orbit checks tool output against `output_schema`.
fn assert_matches_schema(value: &Value, schema_path: &str) {
    let schema: Value = serde_json::from_str(
        &fs::read_to_string(repository_root().join(schema_path)).expect("read schema"),
    )
    .expect("parse schema");
    let mut errors = Vec::new();
    check_schema(value, &schema, "$", &mut errors);
    assert!(errors.is_empty(), "{schema_path}: {errors:?}\n{value}");
}

fn check_schema(value: &Value, schema: &Value, at: &str, errors: &mut Vec<String>) {
    let type_matches = |name: &str| match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    };
    let allowed = match &schema["type"] {
        Value::String(name) => vec![name.as_str()],
        Value::Array(names) => names.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !allowed.is_empty() && !allowed.iter().any(|name| type_matches(name)) {
        errors.push(format!("{at}: expected {allowed:?}, got {value}"));
        return;
    }
    if let Some(expected) = schema.get("const")
        && value != expected
    {
        errors.push(format!("{at}: expected const {expected}, got {value}"));
    }
    if let Some(options) = schema["enum"].as_array()
        && !options.contains(value)
    {
        errors.push(format!("{at}: {value} is not one of {options:?}"));
    }
    if let Some(object) = value.as_object() {
        for required in schema["required"].as_array().into_iter().flatten() {
            let name = required.as_str().unwrap_or_default();
            if !object.contains_key(name) {
                errors.push(format!("{at}: missing required {name}"));
            }
        }
        let properties = schema["properties"].as_object();
        for (name, field) in object {
            match properties.and_then(|properties| properties.get(name)) {
                Some(property) => check_schema(field, property, &format!("{at}.{name}"), errors),
                None if schema["additionalProperties"] == false => {
                    errors.push(format!("{at}: unexpected property {name}"));
                }
                None if schema["additionalProperties"].is_object() => check_schema(
                    field,
                    &schema["additionalProperties"],
                    &format!("{at}.{name}"),
                    errors,
                ),
                None => {}
            }
        }
    }
    if let (Some(items), Some(schema_items)) = (value.as_array(), schema.get("items")) {
        for (index, item) in items.iter().enumerate() {
            check_schema(item, schema_items, &format!("{at}[{index}]"), errors);
        }
    }
}

/// `graph_sync` reports the paths its build could not read and the files it
/// skipped, in the shape of the CLI `sync` output (STD-02 §R32).
#[cfg(unix)]
#[test]
fn graph_sync_reports_failed_and_skipped_paths() {
    if skip_as_root("graph_sync_reports_failed_and_skipped_paths") {
        return;
    }
    let fixture = evaluation_fixture();
    let state = TempDir::new().expect("plugin state");
    let locked = fixture.path().join("src/locked.rs");
    fs::write(&locked, "pub fn locked() {}\n").expect("write unreadable file");
    fs::write(
        fixture.path().join("src/huge.json"),
        format!("{{\"k\": \"{}\"}}\n", "a".repeat(4 * 1024 * 1024)),
    )
    .expect("write oversize file");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod 000");

    let output = plugin_output_with_env(
        fixture.path(),
        MAINTAIN_TOOL_NAME,
        json!({"operation": "graph_sync"}),
        &[("ORBIT_PLUGIN_STATE", state.path().as_os_str())],
    );
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).expect("restore mode");
    let synced = plugin_success(&output);

    assert_eq!(synced["coverage"]["state"], "published", "{synced}");
    assert_eq!(synced["result"]["files_indexed"], 2, "{synced}");
    assert_eq!(synced["result"]["failed"]["count"], 1, "{synced}");
    let failure = &synced["result"]["failed"]["entries"][0];
    assert_eq!(failure["path"], "src/locked.rs");
    assert_eq!(failure["operation"], "read file for content hash");
    assert_eq!(failure["error_kind"], "permission_denied");
    assert_eq!(
        synced["result"]["skipped"],
        json!({"count": 1, "entries": [{"path": "src/huge.json", "reason": "oversize"}]})
    );
    assert_matches_schema(&synced, "schemas/maintain.response.json");
}

#[test]
fn graph_sync_publishes_structure_that_recommend_applies_at_its_revision() {
    let fixture = evaluation_fixture();
    let state = TempDir::new().expect("plugin state");
    let environment = [("ORBIT_PLUGIN_STATE", state.path().as_os_str())];
    let call = |tool: &str, input: Value| {
        plugin_success(&plugin_output_with_env(
            fixture.path(),
            tool,
            input,
            &environment,
        ))
    };
    let recommend = || {
        call(
            RECOMMEND_TOOL_NAME,
            json!({"branch": "main", "query": "parser future secret"}),
        )
    };
    let structure_fallbacks = |recommendation: &Value| {
        recommendation["result"]["fallbacks"]
            .as_array()
            .expect("fallbacks")
            .iter()
            .filter_map(|fallback| fallback["kind"].as_str())
            .filter(|kind| kind.starts_with("structure_"))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    // Give the checkout a call edge for structure to expand along.
    fs::write(
        fixture.path().join("tests/parser.rs"),
        "pub fn parser_test() -> bool { parse() && true }\n",
    )
    .expect("write test with an edge");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "edge"]);
    let _ = call(
        MAINTAIN_TOOL_NAME,
        json!({"operation": "history_sync", "branch": "main", "limit": 100}),
    );

    // Before any build: no structure, and the fallback names the fix.
    let before = recommend();
    assert_eq!(before["result"]["structure_applied"], false, "{before}");
    assert_eq!(structure_fallbacks(&before), ["structure_index_missing"]);
    let status = call(STATUS_TOOL_NAME, json!({}));
    assert_eq!(status["code_index"]["state"], "missing", "{status}");
    assert_eq!(status["code_index"]["fresh"], false);

    let head = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    let synced = call(MAINTAIN_TOOL_NAME, json!({"operation": "graph_sync"}));
    assert_eq!(synced["operation"], "graph_sync");
    assert_eq!(synced["coverage"]["complete"], true, "{synced}");
    assert_eq!(synced["coverage"]["state"], "published");
    assert_eq!(synced["result"]["seeded_from_published"], false);
    assert_eq!(synced["result"]["budget_ms"], 90_000);
    assert_eq!(synced["result"]["files_indexed"], 2);
    assert_eq!(
        synced["result"]["failed"],
        json!({"count": 0, "entries": []})
    );
    assert_eq!(
        synced["result"]["skipped"],
        json!({"count": 0, "entries": []})
    );
    assert_eq!(synced["code_index"]["state"], "ready");
    assert_eq!(synced["code_index"]["fresh"], true);
    assert_eq!(synced["code_index"]["published"]["revision"], head.as_str());
    assert_eq!(synced["code_index"]["published"]["files"], 2);
    assert_eq!(synced["code_index"]["published"]["mode"], "full");
    // The response names the directory it wrote (STD-01 R30).
    let index_dir = PathBuf::from(
        synced["code_index"]["directory"]
            .as_str()
            .expect("index directory"),
    );
    assert!(index_dir.starts_with(state.path()), "{synced}");
    let state_files = || {
        fs::read_dir(&index_dir)
            .expect("list index directory")
            .map(|entry| {
                entry
                    .expect("index entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    let published_files = state_files();

    let after = recommend();
    assert_eq!(after["result"]["structure_applied"], true, "{after}");
    assert!(
        structure_fallbacks(&after).is_empty(),
        "structure fallbacks after graph_sync: {after}"
    );
    let status = call(STATUS_TOOL_NAME, json!({}));
    assert_eq!(status["code_index"]["fresh"], true, "{status}");
    assert_matches_schema(&status, "schemas/status.response.json");
    assert_matches_schema(&synced, "schemas/maintain.response.json");
    assert_matches_schema(&after, "schemas/recommend.response.json");
    // Recommend and status only read the published index (STD-01 R31).
    assert_eq!(state_files(), published_files);
    assert_eq!(status["code_index"]["checkout_revision"], head.as_str());

    // A new commit makes the published index stale; recommend says so and
    // skips structure rather than reusing it for another revision.
    fs::write(
        fixture.path().join("src/lexer.rs"),
        "pub fn lex() -> bool { crate::parse() }\n",
    )
    .expect("write lexer");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "lexer"]);
    let stale = recommend();
    assert_eq!(stale["result"]["structure_applied"], false, "{stale}");
    assert_eq!(structure_fallbacks(&stale), ["structure_index_stale"]);
    assert_eq!(
        call(STATUS_TOOL_NAME, json!({}))["code_index"]["fresh"],
        false
    );

    // An incremental build starts from the published index and refreshes it.
    let resynced = call(
        MAINTAIN_TOOL_NAME,
        json!({"operation": "graph_sync", "budget_ms": 60_000}),
    );
    assert_eq!(resynced["coverage"]["complete"], true, "{resynced}");
    assert_eq!(resynced["result"]["seeded_from_published"], true);
    assert_eq!(resynced["result"]["files_changed"], 1);
    assert_eq!(resynced["code_index"]["published"]["mode"], "incremental");
    assert_eq!(resynced["code_index"]["published"]["files"], 3);
    let refreshed = recommend();
    assert_eq!(
        refreshed["result"]["structure_applied"], true,
        "{refreshed}"
    );
    assert!(structure_fallbacks(&refreshed).is_empty(), "{refreshed}");

    // A full rebuild ignores the published index.
    let rebuilt = call(
        MAINTAIN_TOOL_NAME,
        json!({"operation": "graph_sync", "full": true}),
    );
    assert_eq!(rebuilt["result"]["seeded_from_published"], false);
    assert_eq!(rebuilt["code_index"]["published"]["mode"], "full");
    assert_eq!(rebuilt["code_index"]["published"]["files"], 3);
}

#[test]
fn chronological_evaluation_reports_four_variants_both_levels_and_no_stale_results() {
    let fixture = evaluation_fixture();
    let repository = fixture.path().canonicalize().expect("canonical fixture");
    let target_revision = git_stdout(fixture.path(), ["rev-parse", "HEAD~2"]);
    let corpus_path = fixture.path().join("evaluation.json");
    let corpus = json!({
        "schema_version": 1,
        "repository": repository,
        "landing_branch": "main",
        "source": {"system": "test-public-export", "record_id": "fixture-v1"},
        "complete": true,
        "coverage_note": "complete two-delivery synthetic fixture",
        "k": 3,
        "training_deliveries": [
            fixture_delivery(fixture.path(), "training"),
            fixture_delivery(fixture.path(), "future")
        ],
        "cases": [{
            "id": "prospective-target",
            "target_revision": target_revision.clone(),
            "cutoff": "unix:20",
            "task_snapshot": task_snapshot("TASK-TARGET", "parser validation", 15),
            "held_out_delivery": fixture_delivery(fixture.path(), "held-out"),
            "source": {"system": "test-public-observation", "record_id": "target@15"}
        }]
    });
    fs::write(
        &corpus_path,
        serde_json::to_vec_pretty(&corpus).expect("encode corpus"),
    )
    .expect("write corpus");
    let _ = plugin_json(
        fixture.path(),
        MAINTAIN_TOOL_NAME,
        json!({
            "schema_version": 1,
            "operation": "import",
            "repository": repository,
            "branch": "main",
            "delivery": fixture_delivery(fixture.path(), "held-out")
        }),
    );

    let output = run(
        fixture.path(),
        [
            "evaluate",
            "--input",
            corpus_path.to_string_lossy().as_ref(),
        ],
    );
    assert!(
        output.status.success(),
        "evaluation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("evaluation JSON");
    let repeated = run(
        fixture.path(),
        [
            "evaluate",
            "--input",
            corpus_path.to_string_lossy().as_ref(),
        ],
    );
    let repeated: Value = serde_json::from_slice(&repeated.stdout).expect("repeat evaluation JSON");
    assert_eq!(
        metric_projection(&report),
        metric_projection(&repeated),
        "non-latency metrics must be deterministic and independent of operational indexes"
    );
    assert_eq!(report["coverage"]["cases_evaluated"], 1);
    assert_eq!(report["coverage"]["cases_excluded"], 0);
    assert_eq!(report["metrics"].as_array().map(Vec::len), Some(8));
    let variants = report["metrics"]
        .as_array()
        .expect("metrics")
        .iter()
        .filter_map(|metric| metric["variant"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        variants,
        std::collections::BTreeSet::from([
            "combined",
            "frequency",
            "graph_only",
            "task_search_only"
        ])
    );
    assert!(
        report["metrics"].as_array().is_some_and(|metrics| {
            metrics.iter().all(|metric| {
                // No stale results: 0.0 over returned results, or null when the
                // variant returned nothing to be stale (STD-02 §R29).
                let stale_ok = if metric["returned"].as_u64().unwrap_or_default() > 0 {
                    metric["stale_result_rate"] == 0.0
                } else {
                    metric["stale_result_rate"].is_null()
                };
                stale_ok
                    && metric["mean_latency_ms"].is_number()
                    && metric["recall_at_k"].is_number()
                    && metric["precision_at_k"].is_number()
            })
        }),
        "{:#}",
        report["metrics"]
    );
    let status = run(fixture.path(), ["history", "status", "--branch", "main"]);
    assert!(status.status.success());
    let status: Value = serde_json::from_slice(&status.stdout).expect("history status JSON");
    assert_eq!(
        status["deliveries"], 1,
        "evaluation must preserve the pre-existing operational index exactly"
    );
    assert_eq!(report["coverage"]["isolated_indexes"], true);
    assert_eq!(
        report["cases"][0]["graph_snapshot"]["record_id"],
        target_revision
    );
    assert_eq!(
        report["cases"][0]["graph_structure_applied"]["graph_only:file"], true,
        "graph-only must use the frozen target graph rather than lexical-only fallback"
    );

    let human = run_cli(
        fixture.path(),
        [
            "--format",
            "table",
            "evaluate",
            "--input",
            corpus_path.to_string_lossy().as_ref(),
        ],
    );
    assert!(human.status.success());
    let human = String::from_utf8_lossy(&human.stdout);
    assert!(human.contains("COVERAGE NOTE"));
    assert!(human.contains("VARIANT"));
    assert!(human.contains("EXCLUSIONS"));
    assert!(human.contains("prospective-target"));

    let ndjson = run_cli(
        fixture.path(),
        [
            "--format",
            "ndjson",
            "evaluate",
            "--input",
            corpus_path.to_string_lossy().as_ref(),
        ],
    );
    assert!(ndjson.status.success());
    let records = String::from_utf8_lossy(&ndjson.stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("evaluation NDJSON record"))
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 10);
    assert_eq!(records[0]["record_type"], "evaluation_context");
    assert_eq!(records[0]["context"]["coverage"]["cases_evaluated"], 1);
    assert!(records[0]["context"].get("metrics").is_none());
    assert!(records[0]["context"].get("cases").is_none());
    assert!(
        records[1..9]
            .iter()
            .all(|record| record["record_type"] == "evaluation_metric")
    );
    assert_eq!(records[9]["record_type"], "evaluation_case");
    assert!(records[9]["case"]["truth_coverage"].is_object());
    let future = run(
        fixture.path(),
        [
            "recommend",
            "--query",
            "future_secret",
            "--variant",
            "task-search-only",
            "--revision",
            target_revision.as_str(),
            "--cutoff",
            "unix:20",
            "--branch",
            "main",
        ],
    );
    assert!(future.status.success());
    let future: Value = serde_json::from_slice(&future.stdout).expect("future query JSON");
    assert!(
        future["recommendations"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "future task and delivery evidence leaked across the cutoff: {future}"
    );
}

#[test]
fn bounded_history_bootstrap_advances_across_cli_and_plugin_batches() {
    for via_plugin in [false, true] {
        let fixture = many_commit_fixture();
        let repository = fixture.path().canonicalize().expect("canonical fixture");
        let mut reports = Vec::new();
        for _ in 0..4 {
            let report = if via_plugin {
                plugin_json(
                    fixture.path(),
                    MAINTAIN_TOOL_NAME,
                    json!({
                        "schema_version": 1,
                        "operation": "history_sync",
                        "repository": repository,
                        "branch": "main",
                        "limit": 2
                    }),
                )["result"]
                    .clone()
            } else {
                let output = run(
                    fixture.path(),
                    ["history", "sync", "--branch", "main", "--limit", "2"],
                );
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                serde_json::from_slice(&output.stdout).expect("history sync JSON")
            };
            reports.push(report);
        }
        assert_eq!(reports[0]["complete"], false);
        assert_eq!(reports[1]["complete"], false);
        assert_eq!(reports[2]["complete"], true);
        assert_eq!(reports[3]["complete"], true);
        assert!(reports[0]["resume_from"].is_string());
        assert_eq!(reports[3]["commits_indexed"], 0);
        let status = run(fixture.path(), ["history", "status", "--branch", "main"]);
        let status: Value = serde_json::from_slice(&status.stdout).expect("history status JSON");
        assert_eq!(status["deliveries"], 5);
        assert_eq!(status["complete"], true);
        assert_eq!(
            status["cursor"],
            git_stdout(fixture.path(), ["rev-parse", "HEAD"])
        );
    }
}

#[test]
fn evaluation_rejects_unverified_pre_cutoff_and_unattested_hybrid_truth() {
    let fixture = evaluation_fixture();
    let repository = fixture.path().canonicalize().expect("canonical fixture");
    let target = git_stdout(fixture.path(), ["rev-parse", "HEAD~2"]);
    let mut pre_cutoff = fixture_delivery(fixture.path(), "held-out");
    pre_cutoff["delivered_at"]["timestamp"] = json!("unix:10");
    let mut contradictory = fixture_delivery(fixture.path(), "held-out");
    contradictory["delivered_at"]["timestamp"] = json!("unix:10");
    let mut git_only = fixture_delivery(fixture.path(), "held-out");
    git_only["delivery_id"] = json!("fixture:git-only-held-out");
    git_only["evidence"] = json!("git_only");
    let corpus = json!({
        "schema_version": 1,
        "repository": repository,
        "landing_branch": "main",
        "source": {"system": "test-public-export"},
        "complete": true,
        "coverage_note": "admission regression cases",
        "k": 3,
        "training_deliveries": [],
        "cases": [
            {
                "id": "pre-cutoff",
                "target_revision": target.clone(),
                "cutoff": "unix:20",
                "task_snapshot": task_snapshot("TASK-TARGET", "parser", 15),
                "held_out_delivery": pre_cutoff,
                "source": {"system": "test"}
            },
            {
                "id": "contradictory-chronology",
                "target_revision": target.clone(),
                "cutoff": "unix:20",
                "task_snapshot": task_snapshot("TASK-TARGET", "parser", 15),
                "held_out_delivery": contradictory,
                "prospective_delivery_lower_bound": {
                    "status": "known",
                    "timestamp": "unix:25",
                    "source": {"system": "test", "record_id": "run-start"}
                },
                "source": {"system": "test"}
            },
            {
                "id": "unverified",
                "target_revision": target.clone(),
                "cutoff": "unix:20",
                "task_snapshot": task_snapshot("TASK-TARGET", "parser", 15),
                "held_out_delivery": git_only,
                "source": {"system": "test"}
            },
            {
                "id": "hybrid-without-observation",
                "target_revision": target,
                "cutoff": "unix:20",
                "task_snapshot": task_snapshot("TASK-TARGET", "parser", 15),
                "held_out_delivery": fixture_delivery(fixture.path(), "held-out"),
                "hybrid_hits": [{"task_id": "TASK-TRAIN", "score": 1.0}],
                "source": {"system": "test"}
            }
        ]
    });
    let path = fixture.path().join("invalid-chronology.json");
    fs::write(&path, serde_json::to_vec(&corpus).expect("encode corpus")).expect("write corpus");
    let output = run(
        fixture.path(),
        ["evaluate", "--input", path.to_string_lossy().as_ref()],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("evaluation JSON");
    assert_eq!(report["coverage"]["cases_evaluated"], 0);
    // With every case excluded there is no data behind any metric: each one
    // is null, not a measured 0.0 (STD-02 §R29; schema version 2).
    assert_eq!(report["schema_version"], 2);
    let metrics = report["metrics"].as_array().expect("metrics");
    assert_eq!(metrics.len(), 8);
    for metric in metrics {
        assert_eq!(metric["cases"], 0, "{metric}");
        assert_eq!(metric["returned"], 0, "{metric}");
        for field in [
            "recall_at_k",
            "precision_at_k",
            "stale_result_rate",
            "mean_latency_ms",
            "max_latency_ms",
        ] {
            assert!(
                metric[field].is_null(),
                "{field} must be null with no data: {metric}"
            );
        }
    }
    assert!(case_exclusions(&report, 0).contains(&"held_out_delivery_not_proven_after_cutoff"));
    assert!(case_exclusions(&report, 1).contains(&"contradictory_delivery_chronology"));
    assert!(case_exclusions(&report, 2).contains(&"held_out_delivery_not_verified"));
    assert!(
        case_exclusions(&report, 3).contains(&"hybrid_hits_not_attested_strictly_before_cutoff")
    );
    let human = run_cli(
        fixture.path(),
        [
            "--format",
            "table",
            "evaluate",
            "--input",
            path.to_string_lossy().as_ref(),
        ],
    );
    assert!(human.status.success());
    let human = String::from_utf8_lossy(&human.stdout);
    assert!(human.contains("admission regression cases"));
    let metric_rows = human
        .lines()
        .filter(|line| line.contains("task_search_only") || line.contains("frequency"))
        .collect::<Vec<_>>();
    assert_eq!(metric_rows.len(), 4, "{human}");
    for row in metric_rows {
        assert_eq!(
            row.matches("n/a").count(),
            4,
            "recall, precision, stale rate and latency print n/a, not 0.000: {row}"
        );
        assert!(!row.contains("0.000"), "{row}");
    }
    assert!(human.contains("held_out_delivery_not_verified"));
    assert!(human.contains("hybrid_hits_not_attested_strictly_before_cutoff"));
}

#[test]
fn public_adapter_is_idempotent_and_supports_honest_live_task_observations() {
    let fixture = adapter_fixture();
    let repository = fixture
        .path()
        .join("repo")
        .canonicalize()
        .expect("repository");
    let callback_path = executable_path_with(fixture.path());
    let unwritable_tmp = fixture.path().join("unwritable-tmp");
    fs::create_dir(&unwritable_tmp).expect("create unwritable TMPDIR");
    #[cfg(unix)]
    fs::set_permissions(&unwritable_tmp, fs::Permissions::from_mode(0o555))
        .expect("make TMPDIR unwritable");
    let sync = || {
        plugin_output_with_env(
            repository.as_path(),
            MAINTAIN_TOOL_NAME,
            json!({
                "schema_version": 1,
                "operation": "orbit_sync",
                "repository": repository,
                "branch": "main",
                "workspace": "ws-test",
                "run_ids": ["RUN-1"],
                "task_snapshots": [task_snapshot("TASK-PRIOR", "original parser observation", 5)]
            }),
            &[
                ("PATH", callback_path.as_os_str()),
                ("TMPDIR", unwritable_tmp.as_os_str()),
            ],
        )
    };
    let first = sync();
    let first = plugin_success(&first);
    assert_eq!(first["outcomes"][0]["status"], "inserted");
    let index = orbit_graph::HistoryIndex::open_read_only(repository.as_path(), "main")
        .expect("read run-observed index");
    let observed = index
        .delivery("orbit-run:RUN-1:TASK-PRIOR")
        .expect("read delivery")
        .expect("run delivery");
    assert_eq!(
        observed.delivery.evidence,
        orbit_graph::DeliveryEvidence::VerifiedDelivery
    );
    assert_eq!(observed.delivery.tasks[0].title, "parser validation");
    assert_eq!(observed.delivery.tasks[0].source.system, "orbit.task.show");
    let conn = rusqlite::Connection::open(index.database_path()).expect("open history rows");
    let (title, source): (String, String) = conn
        .query_row(
            "SELECT title,source_system FROM history_tasks WHERE delivery_id=?1",
            ["orbit-run:RUN-1:TASK-PRIOR"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("run-observed task row");
    assert_eq!(title, "parser validation");
    assert_eq!(source, "orbit.task.show");
    let (provenance, supplied): (String, String) = conn
        .query_row(
            "SELECT provenance,payload_json FROM history_supplied_task_snapshots WHERE delivery_id=?1",
            ["orbit-run:RUN-1:TASK-PRIOR"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("supplied snapshot row");
    assert_eq!(provenance, "caller_supplied");
    let supplied: Value = serde_json::from_str(&supplied).expect("snapshot payload");
    assert_eq!(supplied["title"], "original parser observation");
    let second = sync();
    let second = plugin_success(&second);
    assert_eq!(second["outcomes"][0]["status"], "already_indexed");
    assert_eq!(second["status"]["deliveries"], 1);
    assert_eq!(
        index
            .delivery("orbit-run:RUN-1:TASK-PRIOR")
            .expect("read replay"),
        Some(observed)
    );

    let supplied_recommendation = plugin_output_with_env(
        repository.as_path(),
        RECOMMEND_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": repository,
            "branch": "main",
            "workspace": "ws-test",
            "task_id": "TASK-TARGET",
            "task_snapshot": task_snapshot("TASK-TARGET", "caller text", 15),
            "level": "file"
        }),
        &[("PATH", callback_path.as_os_str())],
    );
    assert_eq!(
        plugin_success(&supplied_recommendation)["adapter"]["task_text"],
        "supplied_snapshot+verified_task_id"
    );

    for input in [
        json!({
            "schema_version": 1,
            "repository": repository,
            "branch": "main",
            "query": "parser validation",
            "level": "file"
        }),
        json!({
            "schema_version": 1,
            "repository": repository,
            "branch": "main",
            "workspace": "ws-test",
            "task_id": "TASK-TARGET",
            "hybrid": true,
            "level": "file"
        }),
    ] {
        let output = plugin_output_with_env(
            repository.as_path(),
            RECOMMEND_TOOL_NAME,
            input,
            &[("PATH", callback_path.as_os_str())],
        );
        let value = plugin_success(&output);
        assert!(
            value["result"]["recommendations"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );
    }

    let replay = plugin_output_with_env(
        repository.as_path(),
        RECOMMEND_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": repository,
            "branch": "main",
            "workspace": "ws-test",
            "task_id": "TASK-TARGET",
            "cutoff": "unix:9999999999"
        }),
        &[("PATH", callback_path.as_os_str())],
    );
    assert_plugin_error(&replay, "post-execution text must fail strict replay");

    let wrong_workspace = plugin_output_with_env(
        repository.as_path(),
        RECOMMEND_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": repository,
            "workspace": "ws-wrong",
            "task_id": "TASK-TARGET"
        }),
        &[("PATH", callback_path.as_os_str())],
    );
    assert_plugin_error(&wrong_workspace, "wrong workspace must fail");

    let obsolete_root = plugin_output_with_env(
        repository.as_path(),
        RECOMMEND_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": repository,
            "workspace": "ws-test",
            "orbit_root": fixture.path(),
            "task_id": "TASK-TARGET"
        }),
        &[("PATH", callback_path.as_os_str())],
    );
    assert_plugin_error(
        &obsolete_root,
        "orbit_root must no longer be accepted by the request contract",
    );

    let other_repository = fixture.path().join("other-repo");
    run_git(
        fixture.path(),
        ["clone", repository.to_string_lossy().as_ref(), "other-repo"],
    );
    let wrong_repository = plugin_output_with_env(
        other_repository.as_path(),
        RECOMMEND_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": other_repository,
            "workspace": "ws-test",
            "task_id": "TASK-TARGET"
        }),
        &[("PATH", callback_path.as_os_str())],
    );
    assert_plugin_error(&wrong_repository, "wrong repository must fail");

    let supplied_snapshot_bypass = plugin_output_with_env(
        repository.as_path(),
        MAINTAIN_TOOL_NAME,
        json!({
            "schema_version": 1,
            "operation": "orbit_sync",
            "repository": repository,
            "branch": "main",
            "workspace": "ws-wrong",
            "run_ids": ["RUN-1"],
            "task_snapshots": [task_snapshot("TASK-PRIOR", "parser", 5)]
        }),
        &[("PATH", callback_path.as_os_str())],
    );
    let supplied_snapshot_bypass = plugin_success(&supplied_snapshot_bypass);
    assert_eq!(
        supplied_snapshot_bypass["outcomes"][0]["status"], "excluded",
        "supplied snapshots must not bypass public workspace verification"
    );
}

#[test]
fn orbit_sync_can_store_a_later_supplied_snapshot_without_replacing_observed_text() {
    let fixture = adapter_fixture();
    let repository = fixture
        .path()
        .join("repo")
        .canonicalize()
        .expect("repository");
    let callback_path = executable_path_with(fixture.path());
    let sync = |snapshot: Option<Value>| {
        let mut input = json!({
            "schema_version": 1,
            "operation": "orbit_sync",
            "repository": repository,
            "branch": "main",
            "workspace": "ws-test",
            "run_ids": ["RUN-1"]
        });
        if let Some(snapshot) = snapshot {
            input["task_snapshots"] = json!([snapshot]);
        }
        plugin_success(&plugin_output_with_env(
            &repository,
            MAINTAIN_TOOL_NAME,
            input,
            &[("PATH", callback_path.as_os_str())],
        ))
    };
    assert_eq!(sync(None)["outcomes"][0]["status"], "inserted");
    let index = orbit_graph::HistoryIndex::open_read_only(&repository, "main")
        .expect("read observed delivery");
    let original = index
        .delivery("orbit-run:RUN-1:TASK-PRIOR")
        .expect("read delivery")
        .expect("observed delivery");
    let snapshot = task_snapshot(
        "TASK-PRIOR",
        "earlier caller text ghp_12345678901234567890",
        5,
    );
    let backfill = sync(Some(snapshot.clone()));
    assert_eq!(backfill["outcomes"][0]["status"], "already_indexed");
    assert_eq!(backfill["outcomes"][0]["supplied_snapshot"], "inserted");
    let stored = index
        .delivery("orbit-run:RUN-1:TASK-PRIOR")
        .expect("read updated delivery")
        .expect("updated delivery");
    assert!(
        stored.supplied_snapshots[0]
            .title
            .contains("[REDACTED_SECRET]")
    );
    let db_bytes = fs::read(index.database_path()).expect("read history database");
    let secret = b"ghp_12345678901234567890";
    assert!(
        !db_bytes
            .windows(secret.len())
            .any(|window| window == secret)
    );
    assert_eq!(stored.delivery, original.delivery);
    assert_eq!(stored.files, original.files);
    let conn = rusqlite::Connection::open(index.database_path()).expect("open history rows");
    let provenance: String = conn
        .query_row(
            "SELECT provenance FROM history_supplied_task_snapshots WHERE delivery_id=?1",
            ["orbit-run:RUN-1:TASK-PRIOR"],
            |row| row.get(0),
        )
        .expect("supplied provenance");
    assert_eq!(provenance, "caller_supplied");
    assert_eq!(
        sync(Some(snapshot))["outcomes"][0]["supplied_snapshot"],
        "already_indexed"
    );
}

#[test]
fn public_adapter_bounds_time_and_pipe_output() {
    let adapter_timeout = ADAPTER_TIMEOUT_SECONDS.to_string();
    for (body, expected, code, retryable) in [
        (
            format!("sleep {}", stuck_seconds()),
            "timed out",
            "timeout",
            true,
        ),
        (
            "head -c 2097152 /dev/zero".to_string(),
            "exceeded",
            "graph_error",
            false,
        ),
    ] {
        let fixture = adapter_fixture();
        let repository = fixture
            .path()
            .join("repo")
            .canonicalize()
            .expect("repository");
        let shim = fixture.path().join("orbit");
        executable(&shim, format!("#!/bin/sh\n{body}\n"));
        let callback_path = executable_path_with(fixture.path());
        let started = Instant::now();
        let output = plugin_output_with_env(
            repository.as_path(),
            RECOMMEND_TOOL_NAME,
            json!({
                "schema_version": 1,
                "repository": repository,
                "workspace": "ws-test",
                "task_id": "TASK-TARGET"
            }),
            &[
                ("PATH", callback_path.as_os_str()),
                (
                    "GRAPH_ORBIT_TIMEOUT_SECONDS",
                    std::ffi::OsStr::new(&adapter_timeout),
                ),
            ],
        );
        let error = assert_plugin_error(&output, "bounded adapter failure");
        let elapsed = started.elapsed();
        assert!(
            elapsed < adapter_latency_ceiling(),
            "{body}: took {elapsed:?}, over the {:?} ceiling",
            adapter_latency_ceiling()
        );
        assert!(
            error["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(expected)),
            "{body}: {error}"
        );
        assert_eq!(error["error"]["code"], code, "{body}: {error}");
        assert_eq!(error["error"]["retryable"], retryable, "{body}: {error}");
    }
}

/// The plugin's environment is validated once, before any tool runs: a bad
/// value is refused naming its variable, even for a tool that would not
/// read it (STD-02 §R28).
#[test]
fn a_malformed_plugin_environment_is_refused_before_any_tool_runs() {
    let fixture = adapter_fixture();
    let repository = fixture
        .path()
        .join("repo")
        .canonicalize()
        .expect("repository");
    for (variable, value) in [
        ("GRAPH_ORBIT_TIMEOUT_SECONDS", "0"),
        ("GRAPH_ORBIT_TIMEOUT_SECONDS", "61"),
        ("GRAPH_ORBIT_TIMEOUT_SECONDS", "soon"),
        ("ORBIT_PLUGIN_STATE", ""),
        ("ORBIT_GRAPH_LOCK_TIMEOUT_MS", "-1"),
    ] {
        let output = plugin_output_with_env(
            repository.as_path(),
            "orbit.graph.version",
            json!({}),
            &[(variable, std::ffi::OsStr::new(value))],
        );
        let error = assert_plugin_error(&output, variable);
        assert!(
            error["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(variable)),
            "{variable}={value:?}: {error}"
        );
    }
}

#[test]
fn public_adapter_discovers_workspaces_over_mcp_and_syncs_requested_tasks() {
    let fixture = adapter_fixture();
    let repository = fixture
        .path()
        .join("repo")
        .canonicalize()
        .expect("repository");
    let callback_path = executable_path_with(fixture.path());
    let sync = plugin_output_with_env(
        repository.as_path(),
        MAINTAIN_TOOL_NAME,
        json!({
            "schema_version": 1,
            "operation": "orbit_sync",
            "repository": repository,
            "branch": "main",
            "workspace": "ws-test",
            "task_ids": ["TASK-PRIOR"]
        }),
        &[("PATH", callback_path.as_os_str())],
    );
    let sync = plugin_success(&sync);
    assert_eq!(sync["coverage"]["task_ids_examined"], 1, "{sync}");
    assert_eq!(sync["outcomes"][0]["run_id"], "RUN-1", "{sync}");
    assert_eq!(sync["outcomes"][0]["status"], "inserted", "{sync}");
    assert_eq!(sync["status"]["deliveries"], 1);

    let by_name = plugin_output_with_env(
        repository.as_path(),
        RECOMMEND_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": repository,
            "branch": "main",
            "workspace": "test",
            "task_id": "TASK-TARGET",
            "level": "file"
        }),
        &[("PATH", callback_path.as_os_str())],
    );
    plugin_success(&by_name);

    let invocations =
        fs::read_to_string(fixture.path().join("orbit-invocations.log")).expect("invocation log");
    assert!(
        invocations.lines().any(|line| line == "mcp serve"),
        "{invocations}"
    );
    assert!(
        !invocations.contains("tool run orbit.workspace.list"),
        "workspace discovery is MCP-only: {invocations}"
    );
    assert!(!invocations.contains("--operator"), "{invocations}");

    let remoteless = plugin_output_with_env(
        repository.as_path(),
        RECOMMEND_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": repository,
            "workspace": "ws-remoteless",
            "task_id": "TASK-TARGET"
        }),
        &[("PATH", callback_path.as_os_str())],
    );
    let remoteless = assert_plugin_error(&remoteless, "a workspace without git_remote");
    assert!(
        remoteless["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("git_remote")),
        "{remoteless}"
    );

    run_git(
        repository.as_path(),
        [
            "remote",
            "set-url",
            "origin",
            "https://example.invalid/constellation/other.git",
        ],
    );
    let foreign_origin = plugin_output_with_env(
        repository.as_path(),
        RECOMMEND_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": repository,
            "workspace": "ws-test",
            "task_id": "TASK-TARGET"
        }),
        &[("PATH", callback_path.as_os_str())],
    );
    let foreign_origin = assert_plugin_error(&foreign_origin, "a foreign origin");
    assert!(
        foreign_origin["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("does not match")
                && !message.contains("example.invalid")),
        "{foreign_origin}"
    );
}

#[test]
fn public_adapter_enforces_callback_denials() {
    let fixture = adapter_fixture();
    let repository = fixture
        .path()
        .join("repo")
        .canonicalize()
        .expect("repository");
    let callback_path = executable_path_with(fixture.path());
    let recommend = json!({
        "schema_version": 1,
        "repository": repository,
        "workspace": "ws-test",
        "task_id": "TASK-TARGET"
    });
    for (mode, expected) in [
        ("denied", "policy_denied"),
        ("refused", "may not start this command"),
    ] {
        let output = plugin_output_with_env(
            repository.as_path(),
            RECOMMEND_TOOL_NAME,
            recommend.clone(),
            &[
                ("PATH", callback_path.as_os_str()),
                ("GRAPH_TEST_DISCOVERY", std::ffi::OsStr::new(mode)),
            ],
        );
        let error = assert_plugin_error(&output, mode);
        assert!(
            error["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(expected)),
            "{mode}: {error}"
        );
        assert_eq!(error["error"]["retryable"], false, "{mode}: {error}");
    }
    // An MCP refusal keeps Orbit's own code beside the plugin's.
    let denied = plugin_output_with_env(
        repository.as_path(),
        RECOMMEND_TOOL_NAME,
        recommend.clone(),
        &[
            ("PATH", callback_path.as_os_str()),
            ("GRAPH_TEST_DISCOVERY", std::ffi::OsStr::new("denied")),
        ],
    );
    let denied = assert_plugin_error(&denied, "an MCP refusal");
    assert_eq!(denied["error"]["code"], "orbit_refused", "{denied}");
    assert_eq!(
        denied["error"]["orbit"]["code"], "policy_denied",
        "{denied}"
    );
    assert_eq!(
        denied["error"]["orbit"]["tool"], "orbit.workspace.list",
        "{denied}"
    );

    let sync = plugin_output_with_env(
        repository.as_path(),
        MAINTAIN_TOOL_NAME,
        json!({
            "schema_version": 1,
            "operation": "orbit_sync",
            "repository": repository,
            "branch": "main",
            "workspace": "ws-test",
            "task_ids": ["TASK-PRIOR"]
        }),
        &[
            ("PATH", callback_path.as_os_str()),
            ("GRAPH_TEST_RUN_SHOW", std::ffi::OsStr::new("denied")),
        ],
    );
    let sync = plugin_success(&sync);
    // A refused run read is infrastructure, not a verdict on the run: it is
    // `failed`, with Orbit's refusal code preserved.
    assert_eq!(sync["outcomes"][0]["run_id"], "RUN-1", "{sync}");
    assert_eq!(sync["outcomes"][0]["status"], "failed", "{sync}");
    assert!(
        sync["outcomes"][0]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("operator")),
        "{sync}"
    );
    let error = &sync["outcomes"][0]["error"];
    assert_eq!(error["code"], "orbit_refused", "{sync}");
    assert_eq!(error["orbit"]["code"], "policy_denied", "{sync}");
    assert_eq!(error["orbit"]["tool"], "orbit.workflow.run.show", "{sync}");
    assert_eq!(error["retryable"], false, "{sync}");
    assert_eq!(sync["coverage"]["failed"], 1, "{sync}");
    assert_eq!(sync["coverage"]["excluded"], 0, "{sync}");
    assert_eq!(sync["status"]["deliveries"], 0);
}

#[test]
fn orbit_sync_reports_an_unreadable_task_as_failed_and_imports_the_rest() {
    let fixture = adapter_fixture();
    let repository = fixture
        .path()
        .join("repo")
        .canonicalize()
        .expect("repository");
    let callback_path = executable_path_with(fixture.path());
    let sync = plugin_output_with_env(
        repository.as_path(),
        MAINTAIN_TOOL_NAME,
        json!({
            "schema_version": 1,
            "operation": "orbit_sync",
            "repository": repository,
            "branch": "main",
            "workspace": "ws-test",
            "task_ids": ["TASK-MISSING", "TASK-PRIOR"]
        }),
        &[("PATH", callback_path.as_os_str())],
    );
    let sync = plugin_success(&sync);
    let outcomes = sync["outcomes"].as_array().expect("outcomes");
    let missing = outcomes
        .iter()
        .find(|outcome| outcome["task_id"] == "TASK-MISSING")
        .unwrap_or_else(|| panic!("the unreadable task is reported by identity: {sync}"));
    assert_eq!(missing["status"], "failed", "{sync}");
    assert_eq!(missing["error"]["code"], "orbit_refused", "{sync}");
    assert_eq!(
        missing["error"]["orbit"]["code"], "task_not_found",
        "{sync}"
    );
    let delivered = outcomes
        .iter()
        .find(|outcome| outcome["run_id"] == "RUN-1")
        .unwrap_or_else(|| panic!("the readable task's run is still imported: {sync}"));
    assert_eq!(delivered["status"], "inserted", "{sync}");
    assert_eq!(sync["coverage"]["task_ids_examined"], 2, "{sync}");
    assert_eq!(sync["coverage"]["failed"], 1, "{sync}");
    assert_eq!(sync["coverage"]["excluded"], 0, "{sync}");
    assert_eq!(sync["status"]["deliveries"], 1, "{sync}");

    // A run that is examined and judged ineligible is `excluded`, counted
    // apart from failures.
    let wrong_workspace = plugin_success(&plugin_output_with_env(
        repository.as_path(),
        MAINTAIN_TOOL_NAME,
        json!({
            "schema_version": 1,
            "operation": "orbit_sync",
            "repository": repository,
            "branch": "main",
            "workspace": "ws-wrong",
            "run_ids": ["RUN-1"]
        }),
        &[("PATH", callback_path.as_os_str())],
    ));
    assert_eq!(
        wrong_workspace["outcomes"][0]["status"], "excluded",
        "{wrong_workspace}"
    );
    assert_eq!(
        wrong_workspace["coverage"]["excluded"], 1,
        "{wrong_workspace}"
    );
    assert_eq!(
        wrong_workspace["coverage"]["failed"], 0,
        "{wrong_workspace}"
    );
}

/// Run a hybrid `recommend` against the adapter fixture with the fake
/// `orbit.search` in `mode`, returning the response and how long it took.
#[cfg(unix)]
fn hybrid_recommend(fixture: &TempDir, mode: &str) -> (Value, Duration) {
    let repository = fixture
        .path()
        .join("repo")
        .canonicalize()
        .expect("repository");
    let callback_path = executable_path_with(fixture.path());
    plugin_json(
        repository.as_path(),
        MAINTAIN_TOOL_NAME,
        json!({"operation": "history_sync", "limit": 10}),
    );
    let adapter_timeout = ADAPTER_TIMEOUT_SECONDS.to_string();
    let started = Instant::now();
    let output = plugin_output_with_env(
        repository.as_path(),
        RECOMMEND_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": repository,
            "branch": "main",
            "workspace": "ws-test",
            "query": "parser validation",
            "hybrid": true,
            "level": "file"
        }),
        &[
            ("PATH", callback_path.as_os_str()),
            ("GRAPH_TEST_SEARCH", std::ffi::OsStr::new(mode)),
            (
                "GRAPH_ORBIT_TIMEOUT_SECONDS",
                std::ffi::OsStr::new(&adapter_timeout),
            ),
        ],
    );
    (plugin_success(&output), started.elapsed())
}

#[cfg(unix)]
fn hybrid_warnings(response: &Value) -> String {
    response["adapter"]["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(target_os = "linux")]
#[test]
fn public_adapter_stops_a_grandchild_that_holds_orbit_stdout() {
    let fixture = adapter_fixture();
    let (response, elapsed) = hybrid_recommend(&fixture, "fork");
    // The grandchild sleeps for stuck_seconds(), past the ceiling, holding
    // the pipe: finishing inside the ceiling proves the group was swept.
    assert!(
        elapsed < adapter_latency_ceiling(),
        "took {elapsed:?}, over the {:?} ceiling",
        adapter_latency_ceiling()
    );
    assert_eq!(
        response["adapter"]["hybrid_search"], "orbit.search_hybrid",
        "{response}"
    );
    let pid = fs::read_to_string(fixture.path().join("grandchild.pid")).expect("grandchild pid");
    let proc_entry = Path::new("/proc").join(pid.trim());
    let deadline = Instant::now() + Duration::from_secs(5);
    while proc_entry.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !proc_entry.exists(),
        "grandchild {pid} outlived the adapter call"
    );
}

#[cfg(unix)]
#[test]
fn public_adapter_reports_signals_marks_truncated_stderr_and_counts_dropped_hits() {
    let fixture = adapter_fixture();
    let (killed, _) = hybrid_recommend(&fixture, "signal");
    assert_eq!(
        killed["adapter"]["hybrid_search"], "local_lexical_fallback",
        "{killed}"
    );
    assert!(
        hybrid_warnings(&killed).contains("signal 9"),
        "a signal-killed child is reported as a signal: {killed}"
    );

    let (noisy, _) = hybrid_recommend(&fixture, "noisy");
    let warnings = hybrid_warnings(&noisy);
    assert!(
        warnings.contains("exit 3") && warnings.contains("…[truncated 1904 bytes]"),
        "truncated stderr is marked with what was cut: {noisy}"
    );

    let (malformed, _) = hybrid_recommend(&fixture, "malformed");
    assert_eq!(
        malformed["adapter"]["hybrid_hits_dropped"], 1,
        "{malformed}"
    );
    assert!(
        hybrid_warnings(&malformed).contains("dropped 1 malformed"),
        "{malformed}"
    );
    let (clean, _) = hybrid_recommend(&fixture, "");
    assert_eq!(clean["adapter"]["hybrid_hits_dropped"], 0, "{clean}");
}

#[cfg(target_os = "linux")]
#[test]
fn public_adapter_reaps_an_mcp_server_that_outlives_its_session() {
    let fixture = adapter_fixture();
    let repository = fixture
        .path()
        .join("repo")
        .canonicalize()
        .expect("repository");
    let callback_path = executable_path_with(fixture.path());
    plugin_json(
        repository.as_path(),
        MAINTAIN_TOOL_NAME,
        json!({"operation": "history_sync", "limit": 10}),
    );
    let adapter_timeout = ADAPTER_TIMEOUT_SECONDS.to_string();
    let started = Instant::now();
    let output = plugin_output_with_env(
        repository.as_path(),
        RECOMMEND_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": repository,
            "branch": "main",
            "workspace": "ws-test",
            "task_id": "TASK-TARGET",
            "level": "file"
        }),
        &[
            ("PATH", callback_path.as_os_str()),
            ("GRAPH_TEST_DISCOVERY", std::ffi::OsStr::new("linger")),
            (
                "GRAPH_ORBIT_TIMEOUT_SECONDS",
                std::ffi::OsStr::new(&adapter_timeout),
            ),
        ],
    );
    plugin_success(&output);
    // The fake server lingers for stuck_seconds() after EOF, past the
    // ceiling, so finishing inside the ceiling proves it was reaped.
    let elapsed = started.elapsed();
    assert!(
        elapsed < adapter_latency_ceiling(),
        "took {elapsed:?}, over the {:?} ceiling",
        adapter_latency_ceiling()
    );
    let pid = fs::read_to_string(fixture.path().join("linger.pid")).expect("lingering pid");
    assert!(
        !Path::new("/proc").join(pid.trim()).exists(),
        "lingering MCP server {pid} was not reaped"
    );
}

#[test]
fn evaluation_reports_added_deleted_renamed_and_unsupported_truth_coverage() {
    let fixture = mixed_truth_fixture();
    let repository = fixture.path().canonicalize().expect("repository");
    let before = git_stdout(fixture.path(), ["rev-parse", "HEAD~1"]);
    let after = git_stdout(fixture.path(), ["rev-parse", "HEAD"]);
    let corpus = json!({
        "schema_version": 1,
        "repository": repository,
        "landing_branch": "main",
        "source": {"system": "test"},
        "complete": true,
        "coverage_note": "mixed truth fixture",
        "k": 10,
        "training_deliveries": [],
        "cases": [{
            "id": "mixed",
            "target_revision": before.clone(),
            "cutoff": "unix:20",
            "task_snapshot": task_snapshot("TASK-MIXED", "rename delete add", 15),
            "held_out_delivery": {
                "schema_version": 2,
                "repository": repository,
                "landing_branch": "main",
                "before_revision": before,
                "after_revision": after,
                "delivery_id": "mixed-heldout",
                "evidence": "verified_delivery",
                "source": {"system": "test"},
                "delivered_at": {"status": "known", "timestamp": "unix:30", "source": {"system": "test"}},
                "captured_at": "unix:31",
                "tasks": [task_snapshot("TASK-MIXED", "rename delete add", 15)]
            },
            "source": {"system": "test"}
        }]
    });
    let path = fixture.path().join("mixed-corpus.json");
    fs::write(&path, serde_json::to_vec(&corpus).expect("encode corpus")).expect("write corpus");
    let output = run(
        fixture.path(),
        ["evaluate", "--input", path.to_string_lossy().as_ref()],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("evaluation JSON");
    let coverage = &report["cases"][0]["truth_coverage"];
    assert_eq!(coverage["file_changes_total"], 4);
    assert_eq!(coverage["file_truth_eligible"], 3);
    assert_eq!(
        coverage["file_truth_omitted"]["added_file_absent_at_target"],
        1
    );
    assert!(
        coverage["symbol_truth_eligible"]
            .as_u64()
            .is_some_and(|count| count >= 2)
    );
    assert_eq!(
        coverage["symbol_truth_omitted"]["added_symbol_absent_at_target"],
        1
    );
    assert_eq!(
        coverage["symbol_truth_omitted"]["symbol_truth_unavailable_unsupported_language"],
        1
    );
}

#[test]
fn manifests_are_versioned_and_describe_all_registered_tools() {
    let root = repository_root();
    for (file, name) in [
        ("orbit-graph-recommend.orbit-tool.yaml", RECOMMEND_TOOL_NAME),
        ("orbit-graph-status.orbit-tool.yaml", STATUS_TOOL_NAME),
        ("orbit-graph-maintain.orbit-tool.yaml", MAINTAIN_TOOL_NAME),
    ] {
        let manifest: Value = serde_norway::from_slice(
            &fs::read(root.join("plugin").join(file)).expect("read manifest"),
        )
        .expect("parse manifest");
        assert_eq!(manifest["schemaVersion"], 1);
        assert_eq!(manifest["name"], name);
        assert!(
            manifest["parameters"]
                .as_array()
                .is_some_and(|p| !p.is_empty())
        );
        let schema_version = manifest["parameters"]
            .as_array()
            .and_then(|parameters| {
                parameters
                    .iter()
                    .find(|parameter| parameter["name"] == "schema_version")
            })
            .expect("published schema_version parameter");
        assert_eq!(schema_version["required"], false);
    }
}

#[test]
#[ignore = "requires an Orbit binary in ORBIT_GRAPH_TEST_ORBIT_BIN"]
fn installed_orbit_registration_and_invocation_when_authority_binary_is_requested() {
    let orbit_bin = test_orbit_bin();
    let fixture = evaluation_fixture();
    let isolated = TempDir::new().expect("isolated Orbit root");
    let isolated_root = isolated.path().join(".orbit");
    let graph_bin = env!("CARGO_BIN_EXE_orbit-graph");
    let plugin = repository_root().join("plugin");
    let initialized = orbit_command(&orbit_bin)
        .current_dir(fixture.path())
        .args([
            "workspace",
            "init",
            "--name",
            "orbit-graph-plugin-test",
            "--ship-mode",
            "local",
            "--root",
        ])
        .arg(&isolated_root)
        .output()
        .expect("initialize isolated Orbit config");
    assert!(
        initialized.status.success(),
        "isolated Orbit init failed: {}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    for manifest in [
        "orbit-graph-recommend.orbit-tool.yaml",
        "orbit-graph-status.orbit-tool.yaml",
        "orbit-graph-maintain.orbit-tool.yaml",
    ] {
        let output = orbit_command(&orbit_bin)
            .args(["tool", "add", graph_bin, "--manifest"])
            .arg(plugin.join(manifest))
            .args(["--root"])
            .arg(&isolated_root)
            .output()
            .expect("register external tool");
        assert!(
            output.status.success(),
            "registration failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let maintenance = isolated_tool_run(
        &orbit_bin,
        &isolated_root,
        fixture.path(),
        MAINTAIN_TOOL_NAME,
        json!({
            "schema_version": 1,
            "operation": "history_sync",
            "repository": fixture.path().canonicalize().expect("canonical fixture"),
            "branch": "main",
            "limit": 100
        }),
    );
    assert!(
        maintenance.status.success(),
        "installed maintenance invocation failed: {}",
        String::from_utf8_lossy(&maintenance.stderr)
    );
    let value = installed_output(&maintenance.stdout);
    assert_eq!(value["operation"], "history_sync", "{value}");

    // Status reads the index history_sync created.
    let output = isolated_tool_run(
        &orbit_bin,
        &isolated_root,
        fixture.path(),
        STATUS_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": fixture.path().canonicalize().expect("canonical fixture"),
            "branch": "main"
        }),
    );
    assert!(
        output.status.success(),
        "installed invocation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = installed_output(&output.stdout);
    assert_eq!(value["operation"], "status", "{value}");

    for level in ["file", "symbol"] {
        for request in [
            json!({
                "schema_version": 1,
                "repository": fixture.path().canonicalize().expect("canonical fixture"),
                "branch": "main",
                "query": "parser",
                "level": level
            }),
            json!({
                "schema_version": 1,
                "repository": fixture.path().canonicalize().expect("canonical fixture"),
                "branch": "main",
                "task_id": "TASK-PENDING",
                "task_snapshot": task_snapshot("TASK-PENDING", "parser", 15),
                "cutoff": "unix:20",
                "level": level
            }),
        ] {
            let output = isolated_tool_run(
                &orbit_bin,
                &isolated_root,
                fixture.path(),
                RECOMMEND_TOOL_NAME,
                request,
            );
            assert!(
                output.status.success(),
                "installed {level} recommendation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let value = installed_output(&output.stdout);
            assert!(
                value["result"]["recommendations"]
                    .as_array()
                    .is_some_and(|items| !items.is_empty()),
                "installed {level} recommendation empty: {value}"
            );
        }
    }
}

#[test]
#[ignore = "requires an Orbit binary in ORBIT_GRAPH_TEST_ORBIT_BIN"]
fn install_and_uninstall_scripts_work_and_reject_malformed_arguments_first() {
    let orbit_bin = test_orbit_bin();
    let fixture = evaluation_fixture();
    let isolated = TempDir::new().expect("isolated Orbit root");
    let isolated_root = isolated.path().join(".orbit");
    let initialized = orbit_command(&orbit_bin)
        .current_dir(fixture.path())
        .args([
            "workspace",
            "init",
            "--name",
            "orbit-graph-script-test",
            "--ship-mode",
            "local",
            "--root",
        ])
        .arg(&isolated_root)
        .output()
        .expect("initialize isolated Orbit config");
    assert!(initialized.status.success());
    let root = repository_root();
    let install = root.join("scripts/install-orbit-plugin.sh");
    let uninstall = root.join("scripts/uninstall-orbit-plugin.sh");
    let graph_bin = env!("CARGO_BIN_EXE_orbit-graph");
    let path = executable_path(&orbit_bin);

    let malformed = orbit_command("sh")
        .current_dir(fixture.path())
        .env("PATH", &path)
        .arg(&install)
        .args(["--orbit-root", "", "--binary", graph_bin])
        .output()
        .expect("run malformed installer");
    assert_eq!(malformed.status.code(), Some(2));

    let installed = orbit_command("sh")
        .current_dir(fixture.path())
        .env("PATH", &path)
        .arg(&install)
        .arg("--orbit-root")
        .arg(&isolated_root)
        .args(["--binary", graph_bin])
        .output()
        .expect("run installer");
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    // plugin/plugin.yaml carries the named override, which the installer
    // reports rather than applies silently.
    assert!(
        String::from_utf8_lossy(&installed.stderr).contains("backend override"),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let status = isolated_tool_run(
        &orbit_bin,
        &isolated_root,
        fixture.path(),
        STATUS_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": fixture.path().canonicalize().expect("repository"),
            "branch": "main"
        }),
    );
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );

    let removed = orbit_command("sh")
        .current_dir(fixture.path())
        .env("PATH", &path)
        .arg(&uninstall)
        .arg("--orbit-root")
        .arg(&isolated_root)
        .output()
        .expect("run uninstaller");
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    let missing = isolated_tool_run(
        &orbit_bin,
        &isolated_root,
        fixture.path(),
        STATUS_TOOL_NAME,
        json!({
            "schema_version": 1,
            "repository": fixture.path().canonicalize().expect("repository")
        }),
    );
    assert!(!missing.status.success());
}

/// The legacy installer applies the launcher's binding rule and refuses the
/// retired `ORBIT_GRAPH_BIN`, all before registering anything. A recording
/// `orbit` stands in for Orbit, so this runs without one.
#[cfg(unix)]
#[test]
fn installer_follows_the_backend_binding_and_refuses_orbit_graph_bin() {
    let fixture = TempDir::new().expect("installer fixture");
    let root = repository_root();
    fs::create_dir(fixture.path().join("scripts")).expect("scripts directory");
    fs::create_dir(fixture.path().join("plugin")).expect("plugin directory");
    fs::create_dir(fixture.path().join("fake-bin")).expect("fake bin directory");
    let install = fixture.path().join("scripts/install-orbit-plugin.sh");
    fs::copy(root.join("scripts/install-orbit-plugin.sh"), &install).expect("copy installer");
    for manifest in [
        "orbit-graph-recommend.orbit-tool.yaml",
        "orbit-graph-status.orbit-tool.yaml",
        "orbit-graph-maintain.orbit-tool.yaml",
    ] {
        fs::copy(
            root.join("plugin").join(manifest),
            fixture.path().join("plugin").join(manifest),
        )
        .expect("copy sidecar manifest");
    }
    let registrations = fixture.path().join("registrations");
    executable(
        &fixture.path().join("fake-bin/orbit"),
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
            registrations.display()
        ),
    );
    let path = format!(
        "{}:/usr/bin:/bin",
        fixture.path().join("fake-bin").display()
    );
    let graph_bin = env!("CARGO_BIN_EXE_orbit-graph");
    let committed = fs::read_to_string(root.join("plugin/plugin.yaml")).expect("plugin manifest");
    let with_args = |args: &str| {
        fs::write(
            fixture.path().join("plugin/plugin.yaml"),
            committed.replace(
                &format!("    args: [{UNBOUND}]\n"),
                &format!("    args: [{args}]\n"),
            ),
        )
        .expect("write plugin manifest");
    };
    let run = |environment_binary: Option<&str>| {
        let mut command = orbit_command("sh");
        command
            .env("PATH", &path)
            .arg(&install)
            .args(["--binary", graph_bin]);
        if let Some(binary) = environment_binary {
            command.env("ORBIT_GRAPH_BIN", binary);
        }
        command.output().expect("run installer")
    };
    let registered = || fs::read_to_string(&registrations).unwrap_or_default();

    with_args(UNBOUND);
    let refused = run(Some(graph_bin));
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("ORBIT_GRAPH_BIN"));
    assert!(registered().is_empty());

    with_args(&format!("--backend-sha256, {}", "0".repeat(64)));
    let refused = run(None);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("incompatible_binary"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(registered().is_empty());

    with_args(&format!(
        "--backend-sha256, {}",
        sha256_hex(Path::new(graph_bin))
    ));
    let bound = run(None);
    assert!(
        bound.status.success(),
        "{}",
        String::from_utf8_lossy(&bound.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&bound.stderr).contains("backend override"),
        "{}",
        String::from_utf8_lossy(&bound.stderr)
    );
    assert_eq!(registered().lines().count(), 3, "{}", registered());

    with_args(UNBOUND);
    let overridden = run(None);
    assert!(overridden.status.success());
    let notice = String::from_utf8_lossy(&overridden.stderr);
    assert!(
        notice.contains("backend override") && notice.contains(graph_bin),
        "{notice}"
    );
    assert_eq!(registered().lines().count(), 6, "{}", registered());
}

/// A tool's output from `orbit tool run --full`: the plugin envelope, whose
/// `output` is unwrapped, or the bare output. A plugin error, which exits
/// zero, fails the test.
fn installed_output(stdout: &[u8]) -> Value {
    let mut value: Value = serde_json::from_slice(stdout).expect("Orbit tool JSON");
    assert_ne!(value["ok"], false, "{value}");
    if value["ok"] == true {
        value["output"].take()
    } else {
        value
    }
}

/// The Orbit binary the installed-Orbit tests run against. They are
/// `#[ignore]`d, so reaching this without it is a misconfigured run, not a
/// skip.
fn test_orbit_bin() -> String {
    std::env::var("ORBIT_GRAPH_TEST_ORBIT_BIN")
        .ok()
        .filter(|orbit_bin| !orbit_bin.is_empty())
        .expect(
            "set ORBIT_GRAPH_TEST_ORBIT_BIN to an Orbit binary to run the installed-Orbit tests \
             (see CONTRIBUTING.md)",
        )
}

/// A command with no inherited Orbit environment: a test run inside an Orbit
/// agent session would otherwise carry its task, run and policy variables
/// (such as `ORBIT_PROC_ALLOWED_PROGRAMS`) into the isolated Orbit root.
fn orbit_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("ORBIT_") {
            command.env_remove(key);
        }
    }
    command
}

fn isolated_tool_run(
    orbit_bin: &str,
    orbit_root: &Path,
    repository: &Path,
    tool: &str,
    input: Value,
) -> Output {
    orbit_command(orbit_bin)
        .current_dir(repository)
        .args(["tool", "run", tool, "--input"])
        .arg(input.to_string())
        .args(["--full", "--root"])
        .arg(orbit_root)
        .output()
        .expect("invoke isolated Orbit tool")
}

/// The read-only code-graph query verbs the root `plugin.yaml` registers, in
/// manifest order.
fn manifest_query_verbs() -> Vec<String> {
    let manifest: Value = serde_norway::from_slice(
        &fs::read(repository_root().join("plugin.yaml")).expect("read plugin.yaml"),
    )
    .expect("parse plugin.yaml");
    manifest["spec"]["tools"]
        .as_array()
        .expect("plugin.yaml tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .filter(|name| !NON_QUERY_TOOLS.contains(name))
        .map(str::to_string)
        .collect()
}

/// The repository root, which owns `plugin/` and `scripts/` while this crate
/// lives in `crates/orbit-graph-cli`.
fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate manifest directory has a repository root")
        .to_path_buf()
}

fn executable_path(orbit_bin: &str) -> std::ffi::OsString {
    let mut paths = vec![
        Path::new(orbit_bin)
            .parent()
            .expect("Orbit binary parent")
            .to_path_buf(),
    ];
    if let Some(current) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&current));
    }
    std::env::join_paths(paths).expect("join executable path")
}

fn evaluation_fixture() -> TempDir {
    let fixture = TempDir::new().expect("create fixture");
    run_git(fixture.path(), ["init", "-b", "main"]);
    run_git(
        fixture.path(),
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(fixture.path(), ["config", "user.name", "Graph Test"]);
    fs::create_dir_all(fixture.path().join("src")).expect("create src");
    fs::create_dir_all(fixture.path().join("tests")).expect("create tests");
    fs::write(
        fixture.path().join("src/parser.rs"),
        "pub fn parse() -> bool { false }\n",
    )
    .expect("write parser");
    fs::write(
        fixture.path().join("tests/parser.rs"),
        "pub fn parser_test() -> bool { parse() && false }\n",
    )
    .expect("write test");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "base"]);
    fs::write(
        fixture.path().join("src/parser.rs"),
        "pub fn parse() -> bool { true }\n",
    )
    .expect("write training parser");
    fs::write(
        fixture.path().join("tests/parser.rs"),
        "pub fn parser_test() -> bool { parse() }\n",
    )
    .expect("write training test");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "training"]);
    fs::write(
        fixture.path().join("src/parser.rs"),
        "pub fn parse() -> bool { 1 + 1 == 2 }\n",
    )
    .expect("write held-out parser");
    fs::write(
        fixture.path().join("tests/parser.rs"),
        "pub fn parser_test() -> bool { parse() && true }\n",
    )
    .expect("write held-out test");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "held out"]);
    fs::write(
        fixture.path().join("src/parser.rs"),
        "pub fn parse() -> bool { future_secret() }\npub fn future_secret() -> bool { true }\n",
    )
    .expect("write future parser");
    fs::write(
        fixture.path().join("tests/parser.rs"),
        "pub fn parser_test() -> bool { true }\n",
    )
    .expect("remove future graph edge");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "future"]);
    fixture
}

fn many_commit_fixture() -> TempDir {
    let fixture = TempDir::new().expect("create fixture");
    run_git(fixture.path(), ["init", "-b", "main"]);
    run_git(
        fixture.path(),
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(fixture.path(), ["config", "user.name", "Graph Test"]);
    for value in 0..=5 {
        fs::write(fixture.path().join("value.txt"), value.to_string()).expect("write value");
        run_git(fixture.path(), ["add", "."]);
        run_git(fixture.path(), ["commit", "-m", &format!("commit {value}")]);
    }
    fixture
}

/// The `origin` of the adapter fixture, published as its workspace's `git_remote`.
const FIXTURE_REMOTE: &str = "https://example.invalid/constellation/fixture.git";

fn adapter_fixture() -> TempDir {
    let fixture = TempDir::new().expect("create adapter fixture");
    let repository = fixture.path().join("repo");
    fs::create_dir_all(&repository).expect("create repository");
    run_git(&repository, ["init", "-b", "main"]);
    run_git(&repository, ["remote", "add", "origin", FIXTURE_REMOTE]);
    run_git(
        &repository,
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(&repository, ["config", "user.name", "Graph Test"]);
    fs::write(
        repository.join("parser.rs"),
        "pub fn parse() -> bool { false }\n",
    )
    .expect("write base");
    run_git(&repository, ["add", "."]);
    run_git(&repository, ["commit", "-m", "base"]);
    let before = git_stdout(&repository, ["rev-parse", "HEAD"]);
    fs::write(
        repository.join("parser.rs"),
        "pub fn parse() -> bool { true }\n",
    )
    .expect("write delivery");
    run_git(&repository, ["add", "."]);
    run_git(&repository, ["commit", "-m", "delivery"]);
    let after = git_stdout(&repository, ["rev-parse", "HEAD"]);
    let canonical = repository.canonicalize().expect("canonical repository");
    // The public row shape `orbit mcp serve` returns for `orbit.workspace.list`
    // (Orbit 0.23): workspace identity and `git_remote`, never a checkout path.
    let discovery = json!({
        "machine_id": "hm_fixture",
        "workspaces": [
            {
                "base_branch": "main",
                "created_at": "2026-09-07T00:00:00Z",
                "git_remote": FIXTURE_REMOTE,
                "id": "ws-test",
                "name": "test",
                "owner_machine_id": "hm_fixture",
                "ship_mode": "pr",
                "status": "active",
                "updated_at": "2026-09-07T00:00:00Z"
            },
            {
                "base_branch": "main",
                "created_at": "2026-09-07T00:00:00Z",
                "id": "ws-remoteless",
                "name": "remoteless",
                "owner_machine_id": "hm_fixture",
                "status": "active",
                "updated_at": "2026-09-07T00:00:00Z"
            }
        ]
    });
    let initialized = json!({"jsonrpc": "2.0", "id": 1, "result": {
        "protocolVersion": "2025-06-18",
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "orbit-mcp", "version": "0.23.0"}
    }});
    let notification = json!({"jsonrpc": "2.0", "method": "notifications/message",
        "params": {"level": "info", "data": "fixture notification"}});
    let discovered = json!({"jsonrpc": "2.0", "id": 2, "result": {
        "content": [{"type": "text", "text": discovery.to_string()}],
        "structuredContent": discovery,
        "isError": false
    }});
    let refusal = json!({
        "code": "policy_denied",
        "message": "plugin graph may not call orbit.workspace.list: not in permissions.orbit_tools"
    });
    let denied = json!({"jsonrpc": "2.0", "id": 2, "result": {
        "content": [{"type": "text", "text": refusal.to_string()}],
        "structuredContent": refusal,
        "isError": true
    }});
    let unknown = json!({"jsonrpc": "2.0", "id": 2,
        "error": {"code": -32602, "message": "fixture serves only orbit.workspace.list"}});
    let run_show = json!({
        "run": {"state": "success", "finished_at": "2026-09-07T00:00:30Z"},
        "pipeline_state": {"step_outputs": {
            "0": {"workspace_path": canonical},
            "2": {
                "phase": "commit", "committed": true, "task_id": "TASK-PRIOR",
                "base_sha": before, "commit_sha": after
            }
        }}
    });
    let mut prior = public_task("TASK-PRIOR", "done", "parser validation");
    prior["job_run_id"] = json!("RUN-1");
    let target = public_task("TASK-TARGET", "in-progress", "parser validation");
    let search = json!({"results": [{"id": "TASK-PRIOR", "score": 1.0}]});
    let log = fixture.path().join("orbit-invocations.log");
    let linger = fixture.path().join("linger.pid");
    // GRAPH_TEST_DISCOVERY selects the MCP server's behaviour (granted, denied
    // by the callback allowlist, refused before serving, or lingering after
    // EOF); GRAPH_TEST_RUN_SHOW=denied models the operator-only run read, as
    // Orbit's structured stderr refusal. GRAPH_TEST_SEARCH selects how
    // `orbit.search` misbehaves: a grandchild holding stdout (fork), death by
    // SIGKILL (signal), oversized stderr (noisy) or a malformed hit.
    // TASK-MISSING is always refused as `task_not_found`.
    let run_show_denied = json!({
        "code": "policy_denied",
        "error": "orbit.workflow.run.show requires the operator capability"
    });
    let task_missing =
        json!({"code": "task_not_found", "error": "task TASK-MISSING was not found"});
    let malformed_search =
        json!({"results": [{"id": "TASK-PRIOR", "score": 1.0}, {"id": "TASK-TARGET"}]});
    let grandchild = fixture.path().join("grandchild.pid");
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$*" in *"--root"*|*"--operator"*) exit 8 ;; esac
case "$*" in
  "mcp serve")
    if [ "$GRAPH_TEST_DISCOVERY" = refused ]; then
      printf '%s\n' "policy_denied: plugin graph may not start this command" >&2
      exit 1
    fi
    while IFS= read -r line; do
      case "$line" in
        *'"method":"initialize"'*) printf '%s\n' '{initialized}' ;;
        *'"method":"tools/call"'*'"name":"orbit.workspace.list"'*)
          printf '%s\n' '{notification}'
          if [ "$GRAPH_TEST_DISCOVERY" = denied ]; then
            printf '%s\n' '{denied}'
          else
            printf '%s\n' '{discovered}'
          fi ;;
        *'"method":"tools/call"'*) printf '%s\n' '{unknown}' ;;
      esac
    done
    if [ "$GRAPH_TEST_DISCOVERY" = linger ]; then
      printf '%s' "$$" > "{linger}"
      exec sleep {stuck}
    fi ;;
  *"tool run orbit.workflow.run.show"*"RUN-1"*)
    if [ "$GRAPH_TEST_RUN_SHOW" = denied ]; then
      printf '%s\n' '{run_show_denied}' >&2
      exit 1
    fi
    printf '%s\n' '{run_show}' ;;
  *"tool run orbit.task.show"*"TASK-MISSING"*)
    printf '%s\n' '{task_missing}' >&2
    exit 1 ;;
  *"tool run orbit.task.show"*"TASK-PRIOR"*) printf '%s\n' '{prior}' ;;
  *"tool run orbit.task.show"*"TASK-TARGET"*) printf '%s\n' '{target}' ;;
  *"tool run orbit.search"*)
    case "$GRAPH_TEST_SEARCH" in
      fork)
        sleep {stuck} &
        printf '%s' "$!" > "{grandchild}" ;;
      signal) kill -9 $$ ;;
      noisy)
        head -c 6000 /dev/zero | tr '\0' x >&2
        exit 3 ;;
      malformed)
        printf '%s\n' '{malformed_search}'
        exit 0 ;;
    esac
    printf '%s\n' '{search}' ;;
  *) exit 9 ;;
esac
"#,
        log = log.display(),
        linger = linger.display(),
        grandchild = grandchild.display(),
        stuck = stuck_seconds(),
    );
    executable(&fixture.path().join("orbit"), script);
    fixture
}

fn mixed_truth_fixture() -> TempDir {
    let fixture = TempDir::new().expect("create mixed truth fixture");
    run_git(fixture.path(), ["init", "-b", "main"]);
    run_git(
        fixture.path(),
        ["config", "user.email", "graph@example.invalid"],
    );
    run_git(fixture.path(), ["config", "user.name", "Graph Test"]);
    fs::write(
        fixture.path().join("old.rs"),
        "pub fn retained() -> bool { false }\n",
    )
    .expect("write old");
    fs::write(fixture.path().join("delete.rs"), "pub fn doomed() {}\n").expect("write delete");
    fs::write(fixture.path().join("data.txt"), "before\n").expect("write data");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "base"]);
    run_git(fixture.path(), ["mv", "old.rs", "renamed.rs"]);
    fs::write(
        fixture.path().join("renamed.rs"),
        "pub fn retained() -> bool { false }\n",
    )
    .expect("edit renamed");
    fs::remove_file(fixture.path().join("delete.rs")).expect("remove deleted fixture file");
    fs::write(fixture.path().join("data.txt"), "after\n").expect("edit data");
    fs::write(fixture.path().join("added.rs"), "pub fn added() {}\n").expect("write added");
    run_git(fixture.path(), ["add", "."]);
    run_git(fixture.path(), ["commit", "-m", "mixed changes"]);
    fixture
}

fn public_task(id: &str, status: &str, title: &str) -> Value {
    json!({
        "id": id,
        "title": title,
        "description": "update parser validation",
        "acceptance_criteria": ["parser changes"],
        "status": status,
        "created_at": "2026-09-06T00:00:00Z",
        "history": [{"event": "started", "to_status": "in-progress"}]
    })
}

fn executable(path: &Path, contents: String) {
    fs::write(path, contents).expect("write executable shim");
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(path).expect("shim metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("chmod shim");
    }
}

fn executable_path_with(first: &Path) -> std::ffi::OsString {
    let mut paths = vec![first.to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).expect("join callback PATH")
}

fn fixture_delivery(repository: &Path, kind: &str) -> Value {
    let (before, after, task, delivered, snapshot) = match kind {
        "training" => ("HEAD~3", "HEAD~2", "TASK-TRAIN", 10, 5),
        "held-out" => ("HEAD~2", "HEAD~1", "TASK-TARGET", 30, 15),
        "future" => ("HEAD~1", "HEAD", "TASK-FUTURE", 40, 35),
        _ => panic!("unsupported delivery fixture {kind}"),
    };
    let title = if kind == "future" {
        "future_secret"
    } else {
        "parser validation"
    };
    json!({
        "schema_version": 2,
        "repository": repository.canonicalize().expect("canonical repository"),
        "landing_branch": "main",
        "before_revision": git_stdout(repository, ["rev-parse", before]),
        "after_revision": git_stdout(repository, ["rev-parse", after]),
        "delivery_id": format!("fixture:{kind}"),
        "evidence": "verified_delivery",
        "source": {"system": "test-delivery-feed", "record_id": kind},
        "delivered_at": {
            "status": "known", "timestamp": format!("unix:{delivered}"),
            "source": {"system": "test-delivery-feed", "record_id": format!("{kind}:landed")}
        },
        "captured_at": format!("unix:{}", delivered + 1),
        "tasks": [task_snapshot(task, title, snapshot)]
    })
}

fn task_snapshot(task_id: &str, title: &str, captured: usize) -> Value {
    json!({
        "task_id": task_id,
        "title": title,
        "description": "update parser and its validation test",
        "acceptance_criteria": ["parser behavior and tests change together"],
        "source": {"system": "test-task-api", "record_id": format!("{task_id}@{captured}")},
        "created_at": {
            "status": "known", "timestamp": "unix:1",
            "source": {"system": "test-task-api", "record_id": task_id}
        },
        "snapshot_available_at": {
            "status": "known", "timestamp": format!("unix:{captured}"),
            "source": {"system": "test-task-api", "record_id": format!("{task_id}@{captured}")}
        },
        "text_availability": "known_pre_execution",
        "captured_at": format!("unix:{captured}")
    })
}

fn plugin_json(repository: &Path, tool: &str, input: Value) -> Value {
    let output = plugin_output_with_env(repository, tool, input, &[]);
    plugin_success(&output)
}

fn plugin_success(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "plugin process failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut response: Value = serde_json::from_slice(&output.stdout).expect("plugin response JSON");
    assert_eq!(response["ok"], true, "plugin returned an error: {response}");
    response
        .get_mut("output")
        .expect("successful plugin output")
        .take()
}

fn assert_plugin_error(output: &Output, context: &str) -> Value {
    assert!(
        output.status.success(),
        "{context}: structured plugin errors must exit zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value =
        serde_json::from_slice(&output.stdout).expect("structured plugin error JSON");
    assert_eq!(response["ok"], false, "{context}: {response}");
    assert!(
        response["error"]["code"].is_string(),
        "{context}: {response}"
    );
    assert!(
        response["error"]["message"].is_string(),
        "{context}: {response}"
    );
    assert!(
        response["error"]["retryable"].is_boolean(),
        "{context}: {response}"
    );
    response
}

fn plugin_output_with_env(
    repository: &Path,
    tool: &str,
    input: Value,
    environment: &[(&str, &std::ffi::OsStr)],
) -> Output {
    let request = json!({
        "schema_version": 1,
        "tool": tool,
        "input": input,
        "context": {
            "workspace_root": repository,
            "agent": "plugin-integration-test",
            "model": "test"
        }
    });
    plugin_raw_output(repository, Some(tool), request, environment)
}

fn plugin_raw_output(
    repository: &Path,
    environment_tool: Option<&str>,
    request: Value,
    environment: &[(&str, &std::ffi::OsStr)],
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit-graph"));
    command
        .current_dir(repository)
        .env_remove("ORBIT_PLUGIN_STATE")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(tool) = environment_tool {
        command.env("ORBIT_TOOL_NAME", tool);
    } else {
        command.env_remove("ORBIT_TOOL_NAME");
    }
    for (key, value) in environment {
        command.env(key, value);
    }
    command
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            let written = child
                .stdin
                .as_mut()
                .expect("plugin stdin")
                .write_all(request.to_string().as_bytes());
            // A process that never reads stdin may exit before the write.
            match written {
                Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => {
                    return Err(error);
                }
                _ => {}
            }
            child.wait_with_output()
        })
        .expect("run plugin")
}

#[cfg(unix)]
fn plugin_json_with_tty_stdout(repository: &Path, tool: &str, input: Value) -> Value {
    let request = json!({
        "schema_version": 1,
        "tool": tool,
        "input": input,
        "context": {
            "workspace_root": repository,
            "agent": "plugin-integration-test",
            "model": "test"
        }
    });
    let mut master_fd = -1;
    let mut slave_fd = -1;
    // SAFETY: `openpty` initializes both descriptors on success; ownership is
    // transferred immediately to standard library descriptor wrappers.
    let opened = unsafe {
        libc::openpty(
            &raw mut master_fd,
            &raw mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(opened, 0, "open plugin stdout pseudo-terminal");
    // SAFETY: successful `openpty` returned distinct, owned descriptors.
    let master = unsafe { fs::File::from_raw_fd(master_fd) };
    // SAFETY: successful `openpty` returned distinct, owned descriptors.
    let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };
    let mut child = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(repository)
        .env("ORBIT_TOOL_NAME", tool)
        .env("ORBIT_GRAPH_FORMAT", "table")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::from(slave))
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn plugin with TTY stdout");
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("plugin stdin")
            .write_all(request.to_string().as_bytes())
            .expect("write plugin input");
    }
    drop(child.stdin.take());
    // A PTY has a finite kernel buffer. Drain it while the plugin is still
    // running so a larger JSON response cannot block the child before it
    // exits. Keeping the master in the reader also keeps the PTY alive until
    // the slave closes after the child has finished writing.
    let stdout_reader = std::thread::spawn(move || {
        let mut master = master;
        let mut stdout = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            match master.read(&mut buffer) {
                Ok(0) => return Ok(stdout),
                Ok(count) => stdout.extend_from_slice(&buffer[..count]),
                Err(error) if error.raw_os_error() == Some(libc::EIO) => return Ok(stdout),
                Err(error) => return Err(error),
            }
        }
    });
    let output = child.wait_with_output().expect("wait for TTY plugin");
    assert!(
        output.status.success(),
        "TTY plugin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = stdout_reader
        .join()
        .expect("TTY stdout reader panicked")
        .expect("read plugin pseudo-terminal");
    let mut response: Value = serde_json::from_slice(&stdout).expect("TTY plugin JSON");
    assert_eq!(response["ok"], true, "TTY plugin error: {response}");
    response["output"].take()
}

fn run<const N: usize>(repository: &Path, args: [&str; N]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(repository)
        .args(["--format", "json"])
        .args(args)
        .output()
        .expect("run orbit-graph")
}

fn run_cli<const N: usize>(repository: &Path, args: [&str; N]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(repository)
        .args(args)
        .output()
        .expect("run orbit-graph CLI")
}

/// Fixture git runs hermetically (`common::git_command`): no host or user
/// configuration, and no inherited `GIT_*` variable, shapes the fixture.
fn run_git<const N: usize>(repository: &Path, args: [&str; N]) {
    let output = common::git_command(repository)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout<const N: usize>(repository: &Path, args: [&str; N]) -> String {
    let output = common::git_command(repository)
        .args(args)
        .output()
        .expect("run git");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("git UTF-8")
        .trim()
        .to_string()
}

fn metric_projection(report: &Value) -> Vec<Value> {
    report["metrics"]
        .as_array()
        .expect("metrics")
        .iter()
        .map(|metric| {
            json!({
                "variant": metric["variant"],
                "level": metric["level"],
                "k": metric["k"],
                "cases": metric["cases"],
                "relevant": metric["relevant"],
                "true_positives": metric["true_positives"],
                "recall_at_k": metric["recall_at_k"],
                "precision_at_k": metric["precision_at_k"],
                "stale_result_rate": metric["stale_result_rate"],
            })
        })
        .collect()
}

fn case_exclusions(report: &Value, index: usize) -> Vec<&str> {
    report["cases"][index]["exclusions"]
        .as_array()
        .expect("case exclusions")
        .iter()
        .filter_map(Value::as_str)
        .collect()
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

/// A repository whose second commit changes `helper`, which `entry` calls and
/// `tests/helper.rs` tests. Returns the fixture and its two commit ids.
fn changes_fixture() -> (TempDir, String, String) {
    let fixture = TempDir::new().expect("create changes fixture");
    let git = |args: &[&str]| {
        let output = common::git_command(fixture.path())
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git UTF-8")
            .trim()
            .to_string()
    };
    git(&["init", "-b", "main"]);
    fs::create_dir_all(fixture.path().join("src")).expect("create src");
    fs::create_dir_all(fixture.path().join("tests")).expect("create tests");
    fs::write(
        fixture.path().join("Cargo.toml"),
        "[package]\nname = \"tool\"\nversion = \"0.1.0\"\n",
    )
    .expect("write manifest");
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn helper() -> i32 {\n    1\n}\n\npub fn entry() -> i32 {\n    helper()\n}\n",
    )
    .expect("write lib");
    fs::write(
        fixture.path().join("tests/helper.rs"),
        "use tool::helper;\n\n#[test]\nfn helper_is_positive() {\n    assert!(helper() > 0);\n}\n",
    )
    .expect("write test");
    git(&["add", "."]);
    git(&["commit", "-m", "base"]);
    let base = git(&["rev-parse", "HEAD"]);
    fs::write(
        fixture.path().join("src/lib.rs"),
        "pub fn helper() -> i32 {\n    2\n}\n\npub fn entry() -> i32 {\n    helper() + 1\n}\n",
    )
    .expect("change lib");
    git(&["commit", "-am", "head"]);
    let head = git(&["rev-parse", "HEAD"]);
    (fixture, base, head)
}

/// `orbit.graph.changes` answers a revision range with labelled callers and
/// tests, caches snapshots under plugin state only, and a warm call reuses
/// them (STD-01 §R31: nothing is written to the repository).
#[test]
fn changes_tool_labels_callers_and_tests_and_caches_under_plugin_state() {
    let (fixture, base, head) = changes_fixture();
    let repository = fixture.path().canonicalize().expect("canonical fixture");
    let state = TempDir::new().expect("plugin state");
    let environment = [("ORBIT_PLUGIN_STATE", state.path().as_os_str())];
    let input = json!({
        "repository": repository,
        "base": base,
        "head": head,
        "confidence": "fuzzy_name"
    });

    let output = plugin_output_with_env(
        fixture.path(),
        CHANGES_TOOL_NAME,
        input.clone(),
        &environment,
    );
    let value = plugin_success(&output);
    assert_matches_schema(&value, "schemas/changes.response.json");
    assert_eq!(value["operation"], "changes");
    assert_eq!(value["complete"], true);
    let result = &value["result"];
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["comparison"]["base"]["commit_sha"], base.as_str());
    assert_eq!(result["timings"]["base_cache"], "miss");
    let helper = result["symbols"]
        .as_array()
        .expect("symbols")
        .iter()
        .find(|symbol| {
            symbol["selector"]
                .as_str()
                .is_some_and(|s| s.contains("#helper:"))
        })
        .expect("helper is a changed symbol");
    assert_eq!(helper["status"], "modified");
    let callers = helper["callers"].as_array().expect("callers");
    assert!(
        callers.iter().any(|caller| caller["caller"]["selector"]
            .as_str()
            .is_some_and(|s| s.contains("#entry:"))),
        "entry calls helper: {helper}"
    );
    for labelled in callers
        .iter()
        .chain(helper["candidate_tests"].as_array().expect("tests"))
    {
        assert!(labelled["source"].is_string(), "{labelled}");
        assert!(labelled["confidence"].is_string(), "{labelled}");
    }
    assert!(
        helper["candidate_tests"]
            .as_array()
            .expect("tests")
            .iter()
            .any(|test| test["test"]["label"]
                .as_str()
                .is_some_and(|l| l.starts_with("tests/helper.rs"))),
        "the test that calls helper is a candidate: {helper}"
    );

    // Snapshots live under the repository's plugin state directory; the
    // repository gains no graph scratch state and no temporary tree is left.
    let state_dirs: Vec<PathBuf> = fs::read_dir(state.path())
        .expect("read plugin state")
        .map(|entry| entry.expect("state entry").path())
        .collect();
    assert_eq!(
        state_dirs.len(),
        1,
        "one repository state dir: {state_dirs:?}"
    );
    assert!(state_dirs[0].join("changes-snapshots").is_dir());
    assert!(!repository.join(".orbit-graph").exists());

    let warm = plugin_success(&plugin_output_with_env(
        fixture.path(),
        "graph.changes",
        input,
        &environment,
    ));
    assert_matches_schema(&warm, "schemas/changes.response.json");
    assert_eq!(warm["result"]["timings"]["base_cache"], "hit");
    assert_eq!(warm["result"]["timings"]["head_cache"], "hit");
    assert_eq!(warm["result"]["symbols"], result["symbols"]);
}

/// Without `head` the working tree, untracked files included, is compared;
/// its temporary tree is built under plugin state and removed afterwards.
#[test]
fn changes_tool_compares_the_working_tree_without_writing_to_the_repository() {
    let (fixture, _base, head) = changes_fixture();
    let repository = fixture.path().canonicalize().expect("canonical fixture");
    fs::write(
        repository.join("src/extra.rs"),
        "pub fn extra() -> i32 {\n    3\n}\n",
    )
    .expect("write untracked file");
    let status_before = common::git_command(&repository)
        .args(["status", "--porcelain"])
        .output()
        .expect("git status")
        .stdout;
    let state = TempDir::new().expect("plugin state");

    let value = plugin_success(&plugin_output_with_env(
        fixture.path(),
        CHANGES_TOOL_NAME,
        json!({"repository": repository, "base": head}),
        &[("ORBIT_PLUGIN_STATE", state.path().as_os_str())],
    ));
    assert_matches_schema(&value, "schemas/changes.response.json");
    let result = &value["result"];
    assert_eq!(result["comparison"]["mode"], "working_tree");
    assert!(
        result["symbols"]
            .as_array()
            .expect("symbols")
            .iter()
            .any(|symbol| symbol["selector"]
                .as_str()
                .is_some_and(|s| s.contains("#extra:"))),
        "the untracked file's symbol is a change: {result}"
    );
    assert_eq!(result["timings"]["head_cache"], "disabled");

    let status_after = common::git_command(&repository)
        .args(["status", "--porcelain"])
        .output()
        .expect("git status")
        .stdout;
    assert_eq!(
        status_before, status_after,
        "the working tree and index are untouched"
    );
    assert!(!repository.join(".orbit-graph").exists());
    for entry in fs::read_dir(state.path()).expect("read plugin state") {
        let scratch = entry.expect("state entry").path().join("changes-scratch");
        if scratch.exists() {
            assert_eq!(
                fs::read_dir(&scratch).expect("read scratch").count(),
                0,
                "no temporary tree outlives the call"
            );
        }
    }
}

/// Without plugin state the tool caches nothing and says so, rather than
/// falling back to the repository.
#[test]
fn changes_tool_without_plugin_state_caches_nothing_and_says_so() {
    let (fixture, base, head) = changes_fixture();
    let repository = fixture.path().canonicalize().expect("canonical fixture");
    let value = plugin_success(&plugin_output_with_env(
        fixture.path(),
        CHANGES_TOOL_NAME,
        json!({"repository": repository, "base": base, "head": head}),
        &[],
    ));
    assert_eq!(value["complete"], true);
    assert!(
        value["result"]["notices"]
            .as_array()
            .expect("notices")
            .iter()
            .any(|notice| notice
                .as_str()
                .is_some_and(|n| n.contains("ORBIT_PLUGIN_STATE"))),
        "{value}"
    );
    assert!(!repository.join(".orbit-graph").exists());
}

/// Caps are applied and signalled, never silent.
#[test]
fn changes_tool_signals_every_cap_it_applies() {
    let (fixture, base, head) = changes_fixture();
    let repository = fixture.path().canonicalize().expect("canonical fixture");
    let value = plugin_success(&plugin_output_with_env(
        fixture.path(),
        CHANGES_TOOL_NAME,
        json!({
            "repository": repository,
            "base": base,
            "head": head,
            "max_symbols": 1,
            "confidence": "fuzzy_name"
        }),
        &[],
    ));
    assert_matches_schema(&value, "schemas/changes.response.json");
    assert_eq!(value["truncated"], true);
    let result = &value["result"];
    assert_eq!(result["symbols"].as_array().map(Vec::len), Some(1));
    assert!(
        !result["not_analysed"]
            .as_array()
            .expect("not analysed")
            .is_empty(),
        "{result}"
    );
    assert!(
        result["truncation"]
            .as_array()
            .expect("truncation")
            .iter()
            .any(|flag| flag["bound"] == "max_symbols"),
        "{result}"
    );
}

/// Bad input fails with `invalid_request` before anything is indexed; a
/// missing repository is `repository_unavailable`.
#[test]
fn changes_tool_rejects_bad_input_before_indexing() {
    let (fixture, _base, head) = changes_fixture();
    let repository = fixture.path().canonicalize().expect("canonical fixture");
    let state = TempDir::new().expect("plugin state");
    let environment = [("ORBIT_PLUGIN_STATE", state.path().as_os_str())];
    for (input, code) in [
        (
            json!({"repository": repository, "base": "no-such-ref", "head": head}),
            "invalid_request",
        ),
        (
            json!({"repository": repository, "head": head}),
            "invalid_request",
        ),
        (
            json!({"repository": repository, "budget_ms": 120_000}),
            "invalid_request",
        ),
        (
            json!({"repository": repository, "symbols": ["file:src/lib.rs"]}),
            "invalid_request",
        ),
        (
            json!({"repository": repository, "unknown": true}),
            "invalid_request",
        ),
        (
            json!({"repository": repository.join("missing")}),
            "repository_unavailable",
        ),
    ] {
        let output = plugin_output_with_env(
            fixture.path(),
            CHANGES_TOOL_NAME,
            input.clone(),
            &environment,
        );
        let response = assert_plugin_error(&output, &input.to_string());
        assert_eq!(response["error"]["code"], code, "{input}: {response}");
    }
    assert_eq!(
        fs::read_dir(state.path())
            .expect("read plugin state")
            .count(),
        0,
        "nothing is indexed or cached for rejected input"
    );
}

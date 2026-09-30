//! Canonical v2 plugin installation and the real Orbit CLI/MCP surfaces.

#![allow(clippy::expect_used)]
#![allow(
    clippy::disallowed_methods,
    reason = "fixture files are written with fs::write; the production restriction does not apply"
)]

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use serde_json::{Value, json};
use tempfile::TempDir;

mod common;

#[test]
#[ignore = "requires an Orbit binary in ORBIT_GRAPH_TEST_ORBIT_BIN and its native sandbox"]
fn installed_v2_plugin_serves_every_tool_over_cli_and_mcp() {
    let fixture = Fixture::new();
    fixture.install();
    let requests = requests();
    let advertised = fixture.manifest["spec"]["tools"]
        .as_array()
        .expect("manifest tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect::<BTreeSet<_>>();
    let exercised = requests.iter().map(|(verb, _)| *verb).collect();
    assert_eq!(advertised, exercised, "every advertised tool is exercised");

    // Grants enable the backend, but do not grant an ordinary caller the
    // capability to mutate state. Test the actual CLI authority boundary.
    let refused = fixture.cli_tool("maintain", &json!({"operation": "graph_sync"}), false);
    assert!(!refused.status.success(), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("capability_denied"),
        "{refused:?}"
    );
    for (verb, input) in &requests {
        // A fully cleared environment carries no caller identity. Exercise
        // the CLI's explicit, audited operator path rather
        // than inventing a worker credential or depending on the test host.
        let output = fixture.cli_tool(verb, input, true);
        let value = json_output(output, verb);
        assert_tool_result(verb, input, &value);
    }
    let task_request = fixture.task_recommendation();
    let recommendation = json_output(
        fixture.cli_tool("recommend", &task_request, true),
        "authoritative task recommendation",
    );
    assert_tool_result("recommend", &task_request, &recommendation);
    assert_eq!(
        recommendation["adapter"]["task_text"], "orbit.task.show_public_observation",
        "the recommendation observes the task through the live host callback"
    );
    fixture.assert_public_search_hit(&task_request["task_id"]);
    let hybrid_request = json!({"query":"parser", "hybrid":true, "workspace":"graph-v2-test"});
    let hybrid = json_output(
        fixture.cli_tool("recommend", &hybrid_request, true),
        "public task search recommendation",
    );
    assert_hybrid_search(&hybrid);
    let task_sync = json!({"operation":"orbit_sync", "workspace":"graph-v2-test", "task_ids":[task_request["task_id"]]});
    let sync = json_output(
        fixture.cli_tool("maintain", &task_sync, true),
        "authoritative task sync",
    );
    assert_task_sync(&sync);

    let mut agent = Mcp::start(&fixture, false);
    let version = agent.call("version", json!({}));
    assert_ne!(version["isError"], true, "{version}");
    assert_tool_result("version", &json!({}), &version["structuredContent"]);
    let refused = agent.call("maintain", json!({"operation": "graph_sync"}));
    assert_eq!(refused["isError"], true, "{refused}");
    assert_eq!(refused["structuredContent"]["code"], "capability_denied");
    drop(agent);

    let mut mcp = Mcp::start(&fixture, true);
    let listed = mcp.request("tools/list", json!({}));
    let names = listed["tools"]
        .as_array()
        .expect("MCP tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .filter(|name| name.starts_with("graph_"))
        .map(|name| name.trim_start_matches("graph_"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names, advertised,
        "the assembled MCP surface lists every tool"
    );
    for (verb, input) in requests {
        let result = mcp.call(verb, input.clone());
        assert_ne!(result["isError"], true, "{verb}: {result}");
        assert_tool_result(verb, &input, &result["structuredContent"]);
    }
    let recommendation = mcp.call("recommend", task_request.clone());
    assert_ne!(recommendation["isError"], true, "{recommendation}");
    assert_tool_result(
        "recommend",
        &task_request,
        &recommendation["structuredContent"],
    );
    assert_eq!(
        recommendation["structuredContent"]["adapter"]["task_text"],
        "orbit.task.show_public_observation"
    );
    let hybrid = mcp.call("recommend", hybrid_request);
    assert_ne!(hybrid["isError"], true, "{hybrid}");
    assert_hybrid_search(&hybrid["structuredContent"]);
    let sync = mcp.call("maintain", task_sync);
    assert_ne!(sync["isError"], true, "{sync}");
    assert_task_sync(&sync["structuredContent"]);
    let malformed = mcp.call("search", json!({"query": "parse", "unknown_field": true}));
    assert_eq!(malformed["isError"], true, "{malformed}");
    assert!(
        malformed["structuredContent"]
            .to_string()
            .contains("unknown_field"),
        "the malformed-field error names its input: {malformed}"
    );
}

fn assert_hybrid_search(value: &Value) {
    assert_eq!(
        value["adapter"]["hybrid_search"], "orbit.search_lexical_rank",
        "available public search must avoid fallback: {value}"
    );
    assert_eq!(value["adapter"]["hybrid_hits_dropped"], 0, "{value}");
    assert_eq!(value["adapter"]["warnings"], json!([]), "{value}");
}

fn assert_task_sync(value: &Value) {
    assert_eq!(value["coverage"]["task_ids_examined"], 1, "{value}");
    assert_eq!(value["coverage"]["failed"], 0, "{value}");
    assert_eq!(value["coverage"]["discovered_unique_runs"], 0, "{value}");
    assert_eq!(value["outcomes"], json!([]), "{value}");
    assert_eq!(value["status"]["verified_deliveries"], 0, "{value}");
}

fn requests() -> Vec<(&'static str, Value)> {
    vec![
        ("version", json!({})),
        ("maintain", json!({"operation": "history_sync"})),
        ("maintain", json!({"operation": "graph_sync"})),
        ("status", json!({})),
        ("recommend", json!({"query": "parser"})),
        ("search", json!({"query": "parse"})),
        ("show", json!({"selector": "file:src/parser.rs"})),
        (
            "refs",
            json!({"selector": "symbol:src/parser.rs#helper:function"}),
        ),
        (
            "callees",
            json!({"selector": "symbol:src/parser.rs#parse:function"}),
        ),
        (
            "impact",
            json!({"selector": "symbol:src/parser.rs#helper:function", "direction": "inbound"}),
        ),
        ("trace", json!({"command": "parser parse"})),
        ("deps", json!({"selector": "file:src/lib.rs"})),
        ("overview", json!({"selector": "dir:src", "format": "full"})),
        ("changes", json!({"base": "HEAD~1", "head": "HEAD"})),
    ]
}

fn assert_tool_result(verb: &str, input: &Value, value: &Value) {
    if verb == "version" {
        assert_eq!(value["crate_version"], env!("CARGO_PKG_VERSION"), "{value}");
        return;
    }
    let operation = if verb == "maintain" {
        &input["operation"]
    } else {
        &json!(verb)
    };
    assert_eq!(&value["operation"], operation, "{verb}: {value}");
    assert!(value["repository"].as_str().is_some(), "{verb}: {value}");
    let array = match verb {
        "recommend" => Some("/result/recommendations"),
        "search" => Some("/result/matches"),
        "refs" => Some("/result/refs"),
        "callees" => Some("/result/callees"),
        "impact" => Some("/result/touched"),
        "changes" => Some("/result/symbols"),
        _ => None,
    };
    if let Some(array) = array {
        assert!(
            value
                .pointer(array)
                .and_then(Value::as_array)
                .is_some_and(|items| !items.is_empty()),
            "{verb} answers the fixture with actual records: {value}"
        );
    }
    if verb == "trace" {
        assert!(value["result"]["root"].is_object(), "{value}");
    }
    if value.get("index").is_some() {
        assert_eq!(value["index"]["fresh"], true, "{value}");
    }
}

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
    repository: PathBuf,
    source: PathBuf,
    orbit: String,
    manifest: Value,
}

impl Fixture {
    fn new() -> Self {
        let orbit = std::env::var("ORBIT_GRAPH_TEST_ORBIT_BIN")
            .ok()
            .filter(|value| !value.is_empty())
            .expect("set ORBIT_GRAPH_TEST_ORBIT_BIN to run the real installed-v2-plugin test");
        let orbit = Path::new(&orbit)
            .canonicalize()
            .expect("ORBIT_GRAPH_TEST_ORBIT_BIN executable path")
            .to_str()
            .expect("UTF-8 Orbit executable path")
            .to_string();
        let temp = TempDir::new().expect("private Orbit fixture");
        // Mac /tmp and /var are symlinks; Orbit's seeded definitions require
        // physical roots, so resolve the fixture before passing any path.
        let root = temp.path().canonicalize().expect("physical fixture root");
        let home = root.join("home");
        let repository = root.join("repository");
        let source = root.join("source");
        for path in [&home, &repository, &source] {
            fs::create_dir(path).expect("fixture directory");
        }
        let original = repository_root().join(".orbit-plugin");
        copy_tree(&original, &source.join(".orbit-plugin"));
        let manifest_path = source.join(".orbit-plugin/plugin.yaml");
        let manifest_text = fs::read_to_string(&manifest_path).expect("manifest");
        let mut manifest: Value = serde_norway::from_str(&manifest_text).expect("manifest YAML");
        // A local export has no verified first-party source. Use the public
        // graph.* alias under the same real v2 contract as a local install.
        manifest["metadata"]
            .as_object_mut()
            .expect("metadata")
            .remove("origin");
        // Preserve the inline backend.args binding that the real bundler
        // updates; serializing all YAML would rewrite it as a block sequence.
        fs::write(
            &manifest_path,
            manifest_text.replace("  origin: orbit\n", ""),
        )
        .expect("write local manifest");
        let fixture = Self {
            _temp: temp,
            root,
            home,
            repository,
            source,
            orbit,
            manifest,
        };
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&[
            "remote",
            "add",
            "origin",
            "https://example.invalid/graph-plugin-fixture.git",
        ]);
        fs::create_dir(fixture.repository.join("src")).expect("fixture source directory");
        fixture.write_parser(false);
        fs::write(fixture.repository.join("src/lib.rs"),
            "mod parser;\nuse crate::parser::{parse, ParseArgs};\npub fn parser_test() -> bool { parse(ParseArgs) }\n")
            .expect("fixture lib source");
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-m", "base"]);
        fixture.write_parser(true);
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-m", "change helper"]);
        fixture
    }

    fn write_parser(&self, result: bool) {
        fs::write(self.repository.join("src/parser.rs"), format!(
            "use clap::Subcommand;\n#[derive(Subcommand)]\npub enum ParserSubcommand {{ Parse(ParseArgs) }}\npub struct ParseArgs;\npub fn dispatch(command: ParserSubcommand) -> bool {{ match command {{ ParserSubcommand::Parse(args) => parse(args) }} }}\npub fn parse(_args: ParseArgs) -> bool {{ helper() }}\npub fn helper() -> bool {{ {result} }}\n"
        )).expect("fixture parser source");
    }

    fn git(&self, args: &[&str]) {
        let output = common::git_command(&self.repository)
            .args(args)
            .output()
            .expect("fixture git");
        assert!(output.status.success(), "git {args:?}: {output:?}");
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.orbit);
        command
            .env_clear()
            .current_dir(&self.repository)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.home)
            .env("TMPDIR", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env("GIT_AUTHOR_NAME", "Orbit Graph Test")
            .env("GIT_AUTHOR_EMAIL", "orbit-graph-test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Orbit Graph Test")
            .env("GIT_COMMITTER_EMAIL", "orbit-graph-test@example.invalid")
            .env("PATH", self.executable_path());
        command
    }

    fn executable_path(&self) -> std::ffi::OsString {
        let mut paths = vec![
            Path::new(&self.orbit)
                .parent()
                .expect("Orbit binary directory")
                .to_path_buf(),
        ];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        std::env::join_paths(paths).expect("isolated command PATH")
    }

    fn install(&self) {
        let plugin_root = self.source.join(".orbit-plugin");
        let bundle = Command::new("sh")
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", self.executable_path())
            .arg(repository_root().join("scripts/bundle-plugin-binary.sh"))
            .args(["--binary", env!("CARGO_BIN_EXE_orbit-graph")])
            .arg(&plugin_root)
            .output()
            .expect("bundle plugin executable");
        assert!(bundle.status.success(), "{bundle:?}");
        for args in [
            vec![
                "workspace",
                "init",
                "--name",
                "graph-v2-test",
                "--ship-mode",
                "local",
            ],
            vec![
                "plugin",
                "add",
                plugin_root.to_str().expect("plugin root path"),
            ],
            vec!["plugin", "enable", "graph", "--grant", "fs,orbit_tools"],
        ] {
            let output = self
                .command()
                .args(&args)
                .output()
                .expect("install isolated plugin");
            assert!(output.status.success(), "{args:?}: {output:?}");
        }
    }

    fn cli_tool(&self, verb: &str, input: &Value, operator: bool) -> Output {
        let mut command = self.command();
        if operator {
            command.env("ORBIT_OPERATOR", "1");
        }
        command
            .args([
                "tool",
                "run",
                &format!("graph.{verb}"),
                "--input",
                &input.to_string(),
                "--full",
            ])
            .output()
            .expect("invoke installed plugin tool")
    }

    fn task_recommendation(&self) -> Value {
        let task = json_output(
            self.command()
                .env("ORBIT_OPERATOR", "1")
                .args([
                    "task",
                    "add",
                    "--title",
                    "Improve parser parse",
                    "--description",
                    "Improve parser parsing in src/parser.rs",
                    "--acceptance-criteria",
                    "Parser parsing remains correct",
                    "--complexity",
                    "low",
                    "--json",
                ])
                .output()
                .expect("create private recommendation task"),
            "fixture task",
        );
        json!({"task_id":task["id"].as_str().expect("created task id"), "workspace":"graph-v2-test"})
    }

    fn assert_public_search_hit(&self, task_id: &Value) {
        let search = json_output(
            self.command()
                .env("ORBIT_OPERATOR", "1")
                .args(["tool", "run", "orbit.search", "--input"])
                .arg(json!({"query":"parser", "kind":"task", "workspace":"graph-v2-test", "limit":20, "model":"codex"}).to_string())
                .arg("--full")
                .output()
                .expect("search the private task corpus"),
            "public lexical task search",
        );
        assert!(
            search["results"]
                .as_array()
                .expect("public search results")
                .iter()
                .any(|hit| hit["id"] == *task_id),
            "the hybrid request has an actual public task hit: {search}"
        );
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates")
        .parent()
        .expect("repository")
        .to_path_buf()
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir(destination).expect("create plugin tree");
    for entry in fs::read_dir(source).expect("plugin source") {
        let entry = entry.expect("plugin entry");
        let kind = entry.file_type().expect("plugin entry type");
        assert!(
            !kind.is_symlink(),
            "the canonical plugin tree contains no symlinks"
        );
        if entry.file_name() == "orbit-graph.bin" {
            continue;
        }
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy plugin file");
        }
    }
}

fn json_output(output: Output, context: &str) -> Value {
    assert!(output.status.success(), "{context}: {output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).expect("Orbit JSON response");
    assert_ne!(value["ok"], false, "{context}: {value}");
    if value["ok"] == true {
        value["output"].clone()
    } else {
        value
    }
}

/// Own the MCP process group until it is killed and reaped, including on
/// a failed assertion. Drain both pipes concurrently, with a byte ceiling.
struct Mcp {
    child: Child,
    input: ChildStdin,
    responses: Receiver<Value>,
    next_id: u64,
}

impl Mcp {
    fn start(fixture: &Fixture, operator: bool) -> Self {
        let mut command = fixture.command();
        command.args(["mcp", "serve", "--workspace", "graph-v2-test"]);
        if operator {
            command.arg("--operator");
        }
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start real Orbit MCP server");
        let input = child.stdin.take().expect("MCP stdin");
        let stdout = child.stdout.take().expect("MCP stdout");
        let stderr = child.stderr.take().expect("MCP stderr");
        let (sender, responses) = mpsc::sync_channel(4);
        thread::spawn(move || {
            for line in BufReader::new(stdout.take(4 * 1024 * 1024)).lines() {
                let line = line.expect("MCP stdout line");
                let response = serde_json::from_str(&line).expect("MCP JSON line");
                if sender.send(response).is_err() {
                    break;
                }
            }
        });
        thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr
                .take(1024 * 1024)
                .read_to_end(&mut bytes)
                .expect("drain MCP stderr");
        });
        let mut session = Self {
            child,
            input,
            responses,
            next_id: 1,
        };
        let initialized = session.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18",
            "capabilities": {}, "clientInfo": {"name": "graph-v2-test", "version": "1"}}),
        );
        assert!(
            initialized["capabilities"]["tools"].is_object(),
            "{initialized}"
        );
        session.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        session
    }

    fn send(&mut self, value: Value) {
        writeln!(self.input, "{value}").expect("write MCP request");
        self.input.flush().expect("flush MCP request");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let value = self
                .responses
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("MCP response before deadline");
            if value["id"] == id {
                assert!(value.get("error").is_none(), "{method}: {value}");
                return value["result"].clone();
            }
        }
    }

    fn call(&mut self, verb: &str, input: Value) -> Value {
        self.request(
            "tools/call",
            json!({"name": format!("graph_{verb}"), "arguments": input}),
        )
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Ok(group) = libc::pid_t::try_from(self.child.id()) {
            // SAFETY: this child has not been reaped, so the process-group
            // identity created at spawn cannot have been reused.
            unsafe {
                libc::killpg(group, libc::SIGKILL);
            }
        }
        #[cfg(not(unix))]
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

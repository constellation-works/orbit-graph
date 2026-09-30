//! Consistency of the Orbit plugin's published contract with the crate.
//!
//! The plugin launcher, the legacy installer, and the conformance goldens
//! restate versions the crate defines. These checks keep them
//! from drifting, and keep a released plugin version from silently changing
//! the contract it pins (`tests/plugin-releases.json`).

#![allow(clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use orbit_graph::{EXTRACTOR_VERSION, HISTORY_INDEX_SCHEMA_VERSION, STORE_SCHEMA_VERSION};

const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Files that pin the executable contract the plugin launches.
const PINNING_SCRIPTS: [&str; 2] = [
    ".orbit-plugin/bin/orbit-graph",
    "scripts/install-orbit-plugin.sh",
];

#[test]
fn manifests_declare_the_crate_version_and_first_party_origin() {
    {
        let path = ".orbit-plugin/plugin.yaml";
        let manifest = read_yaml(path);
        assert_eq!(
            manifest["metadata"]["version"], CRATE_VERSION,
            "{path}: metadata.version must equal the crate version reported by graph.version"
        );
        // A verified `git+` install from constellation-works registers
        // `orbit.graph.*`, the names the skill, docs/plugin.md, and bundled activity use.
        assert_eq!(manifest["metadata"]["publisher"], "constellation-works");
        assert_eq!(manifest["metadata"]["origin"], "orbit", "{path}");
    }
}

#[test]
fn launchers_and_installer_pin_the_crate_contract() {
    for path in PINNING_SCRIPTS {
        let text = read(path);
        for (field, expected) in [
            ("extractor_version", EXTRACTOR_VERSION),
            ("plugin_schema_version", plugin_schema_version()),
        ] {
            let pins = pinned_numbers(&text, field);
            assert!(!pins.is_empty(), "{path} does not pin {field}");
            assert!(
                pins.iter().all(|pin| *pin == expected),
                "{path} pins {field} {pins:?}; the crate defines {expected}"
            );
        }
    }
}

#[test]
fn version_goldens_match_the_crate_contract() {
    {
        let path = ".orbit-plugin/tests/conformance/version.yaml";
        let golden = read_yaml(path);
        let case = golden["tests"]
            .as_array()
            .and_then(|cases| cases.iter().find(|case| case["tool"] == "version"))
            .expect("version golden");
        assert_eq!(case["expect"]["output"], current_contract(), "{path}");
    }
}

#[test]
fn a_released_version_never_changes_its_contract() {
    let ledger: Value =
        serde_json::from_str(&read("tests/plugin-releases.json")).expect("parse release ledger");
    let releases = ledger["releases"].as_array().expect("releases array");
    assert!(!releases.is_empty());
    let mut previous: Option<(u64, u64, u64)> = None;
    let current = semver(CRATE_VERSION);
    let mut released_current = None;
    for release in releases {
        let version = release["version"].as_str().expect("release version");
        let parsed = semver(version);
        assert!(
            previous.is_none_or(|previous| previous < parsed),
            "tests/plugin-releases.json must list versions in increasing order: {version}"
        );
        previous = Some(parsed);
        assert!(
            parsed <= current,
            "released version {version} is newer than the crate version {CRATE_VERSION}"
        );
        if parsed == current {
            released_current = Some(release);
        }
    }
    if let Some(release) = released_current {
        let mut recorded = release.clone();
        recorded
            .as_object_mut()
            .expect("release entry object")
            .remove("version");
        let mut contract = current_contract();
        contract
            .as_object_mut()
            .expect("contract object")
            .remove("crate_version");
        assert_eq!(
            recorded, contract,
            "version {CRATE_VERSION} was released with a different contract; bump the \
             workspace version and the manifest's metadata.version instead of reusing it"
        );
    }
}

#[test]
fn every_tool_schema_property_is_described() {
    let manifest = read_yaml(".orbit-plugin/plugin.yaml");
    let mut schemas = vec![
        manifest["spec"]["config"]["schema"]
            .as_str()
            .expect("config schema path")
            .to_string(),
    ];
    for tool in manifest["spec"]["tools"].as_array().expect("tools") {
        for key in ["input_schema", "output_schema"] {
            let reference = tool[key]["$ref"]
                .as_str()
                .unwrap_or_else(|| panic!("{} {key} must reference a schema file", tool["name"]));
            schemas.push(reference.to_string());
        }
    }
    let mut undescribed = Vec::new();
    for path in schemas {
        let schema: Value = serde_json::from_str(&read_plugin(&path)).expect("parse schema");
        collect_undescribed(&schema, &path, &mut undescribed);
    }
    assert!(
        undescribed.is_empty(),
        "schema properties without a description: {undescribed:?}"
    );
}

/// `changes` is an additive, read-only tool: declared with its schemas, and
/// every conformance golden for it replays against the executable. The
/// goldens run in an empty non-Git workspace, as Orbit's conformance runner
/// does, so they pin validation and routing codes.
#[test]
fn changes_tool_is_declared_and_its_conformance_goldens_replay() {
    let manifest = read_yaml(".orbit-plugin/plugin.yaml");
    let tool = manifest["spec"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == "changes")
        .expect("plugin.yaml declares changes");
    assert_eq!(tool["execution_kind"], "read_only");
    assert_eq!(tool["mcp_scope"], "workspace");
    for (key, path) in [
        ("input_schema", "schemas/changes.request.json"),
        ("output_schema", "schemas/changes.response.json"),
    ] {
        assert_eq!(tool[key]["$ref"], path);
        let schema: Value = serde_json::from_str(&read_plugin(path)).expect("parse schema");
        assert_eq!(schema["type"], "object", "{path}");
        assert_eq!(schema["additionalProperties"], false, "{path}");
    }
    let description = tool["description"].as_str().expect("description");
    for when in ["after implementing", "before review", "pick tests"] {
        assert!(
            description.contains(when),
            "the description says when to call it: {when}"
        );
    }

    let goldens = read_yaml(".orbit-plugin/tests/conformance/changes.yaml");
    let cases = goldens["tests"].as_array().expect("conformance tests");
    assert!(!cases.is_empty());
    let workspace = tempfile::TempDir::new().expect("conformance workspace");
    let workspace_path = workspace.path().to_str().expect("UTF-8 workspace");
    for case in cases {
        let name = case["name"].as_str().expect("case name");
        assert_eq!(case["tool"], "changes", "{name}");
        let input: Value = serde_json::from_str(
            &case["input"]
                .to_string()
                .replace("{{workspace}}", workspace_path),
        )
        .expect("substituted input");
        let request = json!({
            "schema_version": 1,
            "tool": "orbit.graph.changes",
            "input": input,
            "context": {"workspace_root": workspace_path, "agent": "contract", "model": "test"}
        });
        let output = run_plugin(workspace.path(), "orbit.graph.changes", &request);
        let response: Value = serde_json::from_slice(&output).expect("plugin response JSON");
        assert_eq!(response["ok"], false, "{name}: {response}");
        assert_eq!(
            response["error"]["code"], case["expect"]["error"]["code"],
            "{name}: {response}"
        );
    }
    assert_eq!(
        fs::read_dir(workspace.path())
            .expect("read workspace")
            .count(),
        0,
        "a rejected call writes nothing into the workspace"
    );
}

fn run_plugin(workspace: &Path, tool: &str, request: &Value) -> Vec<u8> {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(workspace)
        .env_remove("ORBIT_PLUGIN_STATE")
        .env("ORBIT_TOOL_NAME", tool)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn plugin");
    child
        .stdin
        .take()
        .expect("plugin stdin")
        .write_all(request.to_string().as_bytes())
        .expect("write request");
    let output = child.wait_with_output().expect("run plugin");
    assert!(
        output.status.success(),
        "structured plugin errors exit zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn collect_undescribed(schema: &Value, location: &str, undescribed: &mut Vec<String>) {
    if let Some(properties) = schema["properties"].as_object() {
        for (name, property) in properties {
            let location = format!("{location}#{name}");
            if property["description"]
                .as_str()
                .is_none_or(|text| text.trim().is_empty())
            {
                undescribed.push(location.clone());
            }
            collect_undescribed(property, &location, undescribed);
        }
    }
    if schema["items"].is_object() {
        collect_undescribed(&schema["items"], &format!("{location}[]"), undescribed);
    }
}

fn current_contract() -> Value {
    json!({
        "crate_version": CRATE_VERSION,
        "extractor_version": EXTRACTOR_VERSION,
        "store_schema_version": STORE_SCHEMA_VERSION,
        "history_schema_version": HISTORY_INDEX_SCHEMA_VERSION,
        "plugin_schema_version": plugin_schema_version(),
    })
}

/// The plugin envelope version the executable reports. The protocol lives in
/// the `orbit-graph` binary, which has no library target to import it from.
fn plugin_schema_version() -> u32 {
    let output = Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .args(["version", "--format", "json"])
        .output()
        .expect("run orbit-graph version");
    assert!(output.status.success(), "orbit-graph version: {output:?}");
    let document: Value = serde_json::from_slice(&output.stdout).expect("parse version output");
    document["plugin_schema_version"]
        .as_u64()
        .and_then(|version| u32::try_from(version).ok())
        .unwrap_or_else(|| panic!("no plugin_schema_version in {document}"))
}

/// Every number written after `field` as `field=N` or `"field":N`.
fn pinned_numbers(text: &str, field: &str) -> Vec<u32> {
    let mut pins = Vec::new();
    for (index, _) in text.match_indices(field) {
        let rest = &text[index + field.len()..];
        let rest = rest.strip_prefix('"').unwrap_or(rest);
        let Some(rest) = rest.strip_prefix('=').or_else(|| rest.strip_prefix(':')) else {
            continue;
        };
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(number) = digits.parse() {
            pins.push(number);
        }
    }
    pins
}

fn semver(version: &str) -> (u64, u64, u64) {
    let mut parts = version.split('.').map(|part| {
        part.parse::<u64>()
            .unwrap_or_else(|_| panic!("{version} is not a plain MAJOR.MINOR.PATCH version"))
    });
    let parsed = (
        parts.next().expect("major"),
        parts.next().expect("minor"),
        parts.next().expect("patch"),
    );
    assert!(parts.next().is_none(), "{version} has extra components");
    parsed
}

fn read_yaml(path: &str) -> Value {
    serde_norway::from_str(&read(path)).unwrap_or_else(|error| panic!("parse {path}: {error}"))
}

fn read(path: &str) -> String {
    fs::read_to_string(repository_root().join(path))
        .unwrap_or_else(|error| panic!("read {path}: {error}"))
}

/// The repository root, which owns the plugin files while this crate lives in
/// `crates/orbit-graph-cli`.
fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate manifest directory has a repository root")
        .to_path_buf()
}

/// The source plugin is a single self-contained tree. Paths in the manifest
/// stay relative to that tree, and Orbit refuses symlinks in it.
#[test]
fn plugin_tree_is_self_contained() {
    let root = repository_root();
    let plugin = root.join(".orbit-plugin");
    assert!(!root.join("plugin.yaml").exists());
    assert!(!root.join("orbit_plugin.yaml").exists());
    assert!(!root.join("plugin").exists());
    let manifest = read_yaml(".orbit-plugin/plugin.yaml");
    assert_eq!(manifest["spec"]["backend"]["command"], "bin/orbit-graph");
    let mut paths = vec!["bin/orbit-graph".to_string()];
    for tool in manifest["spec"]["tools"].as_array().expect("tools") {
        for key in ["input_schema", "output_schema"] {
            paths.push(tool[key]["$ref"].as_str().expect("schema ref").to_string());
        }
    }
    paths.push(
        manifest["spec"]["config"]["schema"]
            .as_str()
            .expect("config schema")
            .to_string(),
    );
    for field in ["activities", "jobs", "routines"] {
        for pattern in manifest["spec"]["definitions"][field]
            .as_array()
            .expect("definitions")
        {
            let pattern = pattern.as_str().expect("definition glob");
            let directory = pattern.strip_suffix("*.yaml").expect("yaml glob");
            let mut files = Vec::new();
            collect_files(&plugin.join(directory), "", &mut files);
            assert!(!files.is_empty(), "{pattern} has no files");
        }
    }
    for field in ["skills", "tests"] {
        for value in manifest["spec"][field].as_array().expect("paths") {
            let path = value.as_str().expect("path");
            let directory = path.strip_suffix("*.yaml").unwrap_or(path);
            assert!(plugin.join(directory).exists(), "{path} is missing");
        }
    }
    for path in paths {
        assert!(plugin.join(&path).is_file(), "{path} is missing");
    }
    fn assert_no_symlinks(path: &Path) {
        for entry in fs::read_dir(path).expect("read plugin directory") {
            let entry = entry.expect("plugin entry");
            let kind = entry.file_type().expect("file type");
            assert!(!kind.is_symlink(), "symlink in plugin: {:?}", entry.path());
            if kind.is_dir() {
                assert_no_symlinks(&entry.path());
            }
        }
    }
    assert_no_symlinks(&plugin);
}

/// A release ships no executable, so the committed manifest cannot bind one:
/// it carries the named override, and only a bundling step records a digest.
#[test]
fn committed_manifests_carry_the_named_unbound_override() {
    {
        let path = ".orbit-plugin/plugin.yaml";
        let manifest = read_yaml(path);
        assert_eq!(
            manifest["spec"]["backend"]["args"],
            json!(["--allow-unbound-backend"]),
            "{path}: a committed manifest must not bind a locally built executable; \
             scripts/bundle-plugin-binary.sh records one in an installed tree"
        );
    }
}

/// STD-01 §R36: every `schemas/config.json` key is consumed, by a
/// `{{config.<key>}}` template in the manifest or a definition, or by the
/// backend, which warns about any `context.config` key it does not read.
#[test]
fn every_config_key_is_consumed() {
    let manifest = read_yaml(".orbit-plugin/plugin.yaml");
    let schema: Value = serde_json::from_str(&read_plugin(
        manifest["spec"]["config"]["schema"]
            .as_str()
            .expect("config schema path"),
    ))
    .expect("parse config schema");
    let keys = schema["properties"]
        .as_object()
        .expect("config properties")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let defaults = manifest["spec"]["config"]["defaults"]
        .as_object()
        .expect("config defaults");
    for key in defaults.keys() {
        assert!(
            keys.contains(key),
            "plugin.yaml defaults {key}, which config.json does not declare"
        );
    }
    let mut templates = read(".orbit-plugin/plugin.yaml");
    let mut definitions = Vec::new();
    collect_files(
        &repository_root().join(".orbit-plugin/definitions"),
        "",
        &mut definitions,
    );
    for file in definitions {
        templates.push_str(&read(&format!(".orbit-plugin/definitions/{file}")));
    }
    let workspace = tempfile::TempDir::new().expect("workspace");
    for key in keys {
        if templates.contains(&format!("{{{{config.{key}}}}}")) {
            continue;
        }
        let value = schema["properties"][&key]["default"].clone();
        let request = json!({
            "schema_version": 1,
            "tool": "orbit.graph.version",
            "input": {},
            "context": {"workspace_root": workspace.path(), "config": {key.as_str(): value}}
        });
        let output = run_plugin_output(workspace.path(), "orbit.graph.version", &request, &[]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("ignoring plugin config key"),
            "config.json declares {key}, which no template and no backend path reads: {stderr}"
        );
        let response: Value = serde_json::from_slice(&output.stdout).expect("response");
        assert_eq!(response["ok"], true, "{key}: {response}");
    }
    // The warning itself is what the check above relies on.
    let request = json!({
        "tool": "orbit.graph.version",
        "input": {},
        "context": {"workspace_root": workspace.path(), "config": {"index_dir": ".orbit-graph"}}
    });
    let output = run_plugin_output(workspace.path(), "orbit.graph.version", &request, &[]);
    assert!(String::from_utf8_lossy(&output.stderr).contains("ignoring plugin config key"));
}

/// STD-01 §R25, §R36: each tool's request schema and the executable agree.
/// Properties equal the fields the serde struct accepts (its
/// `deny_unknown_fields` list), each enum equals the variants it accepts,
/// and each `const`, `minimum`, `maximum`, `minLength` and `required` is
/// what the runtime refuses, with `invalid_request`, before it routes the
/// repository. Every probe names a repository that does not exist, so a
/// request that passes validation stops at `repository_unavailable`.
#[test]
fn request_schemas_equal_the_serde_structs_and_runtime_validation() {
    let workspace = tempfile::TempDir::new().expect("workspace");
    let state = tempfile::TempDir::new().expect("plugin state");
    let missing = workspace.path().join("missing-repository");
    let missing = missing.to_str().expect("UTF-8 path").to_string();
    let manifest = read_yaml(".orbit-plugin/plugin.yaml");
    for tool in resolved_tools(&manifest) {
        let verb = tool["name"].as_str().expect("tool name");
        let name = format!("orbit.graph.{verb}");
        let schema = &tool["input_schema"];
        let properties = schema["properties"].as_object().expect("properties");
        let call = |input: Value| -> Value {
            let request = json!({"tool": name, "input": input});
            let output = run_plugin_output(
                workspace.path(),
                &name,
                &request,
                &[("ORBIT_PLUGIN_STATE", state.path().as_os_str())],
            );
            serde_json::from_slice(&output.stdout).expect("plugin response JSON")
        };
        let refused = |response: &Value, field: &str, context: &str| {
            assert_eq!(response["ok"], false, "{verb} {context}: {response}");
            assert_eq!(
                response["error"]["code"], "invalid_request",
                "{verb} {context}: {response}"
            );
            let message = response["error"]["message"].as_str().expect("message");
            assert!(
                message.contains(field),
                "{verb} {context}: the refusal names {field}: {message}"
            );
        };
        let passes = |response: &Value, field: &str, context: &str| {
            let refused_here = response["error"]["code"] == "invalid_request"
                && response["error"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains(field));
            assert!(!refused_here, "{verb} {context}: {response}");
        };
        let base = |field: &str| probe_base(verb, field, &missing);

        // Properties: the serde struct's accepted fields.
        let mut input = base("");
        input["unadvertised_parity_probe"] = json!(1);
        let response = call(input);
        refused(&response, "unadvertised_parity_probe", "unknown field");
        let message = response["error"]["message"].as_str().expect("message");
        let mut accepted = backticked_after(message, "expected");
        accepted.sort();
        let mut advertised = properties.keys().cloned().collect::<Vec<_>>();
        advertised.sort();
        assert_eq!(
            advertised, accepted,
            "{verb}: schema properties differ from the fields the executable accepts"
        );

        for (field, property) in properties {
            // A field can be omitted, but every advertised property has a
            // non-null type. `Option<T>` must not silently reinterpret null
            // as omission, including fields an operation does not use.
            let mut input = base(field);
            input[field] = Value::Null;
            refused(&call(input), field, &format!("null {field}"));

            // Enums: exactly the variants the executable accepts.
            if let Some(values) = property["enum"].as_array() {
                let mut input = base(field);
                input[field] = json!("not_a_parity_variant");
                let response = call(input);
                refused(
                    &response,
                    "not_a_parity_variant",
                    &format!("{field} variant"),
                );
                let message = response["error"]["message"].as_str().expect("message");
                let mut variants = backticked_after(message, "expected");
                variants.sort();
                let mut declared = values
                    .iter()
                    .map(|value| value.as_str().expect("string enum").to_string())
                    .collect::<Vec<_>>();
                declared.sort();
                assert_eq!(declared, variants, "{verb} {field}: enum differs");
            }
            if let Some(value) = property.get("const") {
                let mut input = base(field);
                input[field] = value.clone();
                passes(&call(input), field, &format!("{field}={value}"));
                let mut input = base(field);
                input[field] = json!(value.as_u64().expect("integer const") + 1);
                refused(&call(input), field, &format!("{field} other than {value}"));
            }
            // Bounds, checked before routing.
            for (keyword, inside, outside) in [("minimum", 0, -1), ("maximum", 0, 1)] {
                if let Some(bound) = property[keyword].as_i64() {
                    let mut input = base(field);
                    input[field] = json!(bound + inside);
                    passes(&call(input), field, &format!("{field}={bound}"));
                    let mut input = base(field);
                    input[field] = json!(bound + outside);
                    refused(&call(input), field, &format!("{field} past its {keyword}"));
                }
            }
            if property["minLength"].as_u64() == Some(1) {
                let mut input = base(field);
                input[field] = json!("");
                refused(&call(input), field, &format!("empty {field}"));
            }
            if property["items"]["minLength"].as_u64() == Some(1) {
                let mut input = base(field);
                input[field] = json!([""]);
                refused(&call(input), field, &format!("empty {field} item"));
            }
        }
        for field in schema["required"].as_array().into_iter().flatten() {
            let field = field.as_str().expect("required field");
            let mut input = base(field);
            input.as_object_mut().expect("object").remove(field);
            refused(&call(input), field, &format!("without {field}"));
        }
        if let Some(exclusive) = schema["not"]["required"].as_array() {
            let mut input = base("");
            for field in exclusive {
                let field = field.as_str().expect("field");
                input[field] = json!("parity");
            }
            let response = call(input);
            assert_eq!(response["error"]["code"], "invalid_request", "{verb}");
        }
    }
}

/// A request that passes validation for `field` of tool `verb`: its required
/// fields, and the mode that reads `field` (the `maintain` operation, or
/// `recommend`'s `hybrid`).
fn probe_base(verb: &str, field: &str, repository: &str) -> Value {
    let mut input = if verb == "version" {
        json!({})
    } else {
        json!({"repository": repository})
    };
    match verb {
        "search" => input["query"] = json!("parser"),
        "show" | "refs" | "callees" | "impact" | "deps" => {
            input["selector"] = json!("file:src/lib.rs");
        }
        "trace" => input["command"] = json!("sync"),
        "recommend" => {
            input["query"] = json!("parser");
            if field == "hybrid_limit" {
                input["hybrid"] = json!(true);
            }
        }
        "maintain" => {
            input["operation"] = json!(match field {
                "delivery" => "import",
                "workspace" | "task_ids" | "run_ids" | "task_snapshots" => "orbit_sync",
                "full" | "budget_ms" => "graph_sync",
                _ => "history_sync",
            });
        }
        _ => {}
    }
    input
}

/// The backtick-quoted names after `marker` in a serde error message, such
/// as the field list of `unknown field `x`, expected one of `a`, `b``.
fn backticked_after(message: &str, marker: &str) -> Vec<String> {
    let rest = message.split_once(marker).map_or("", |(_, rest)| rest);
    rest.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// The plugin manifest's tools with every schema `$ref` resolved.
fn resolved_tools(manifest: &Value) -> Vec<Value> {
    manifest["spec"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| {
            let mut tool = tool.clone();
            for key in ["input_schema", "output_schema"] {
                if let Some(path) = tool[key]["$ref"].as_str() {
                    tool[key] = serde_json::from_str(&read_plugin(path))
                        .unwrap_or_else(|error| panic!("parse {path}: {error}"));
                }
            }
            tool
        })
        .collect()
}

fn read_plugin(path: &str) -> String {
    read(&format!(".orbit-plugin/{path}"))
}

/// Relative paths of every file under `directory`.
fn collect_files(directory: &Path, prefix: &str, files: &mut Vec<String>) {
    for entry in fs::read_dir(directory).expect("read directory") {
        let entry = entry.expect("directory entry");
        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        if entry.file_type().expect("file type").is_dir() {
            collect_files(&entry.path(), &format!("{name}/"), files);
        } else {
            files.push(name);
        }
    }
}

fn run_plugin_output(
    workspace: &Path,
    tool: &str,
    request: &Value,
    environment: &[(&str, &std::ffi::OsStr)],
) -> std::process::Output {
    use std::io::Write;
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit-graph"));
    command
        .current_dir(workspace)
        .env_remove("ORBIT_PLUGIN_STATE")
        .env_remove("ORBIT_GRAPH_BACKEND_OVERRIDE")
        .env("ORBIT_TOOL_NAME", tool)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, value) in environment {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn plugin");
    child
        .stdin
        .take()
        .expect("plugin stdin")
        .write_all(request.to_string().as_bytes())
        .expect("write request");
    let output = child.wait_with_output().expect("run plugin");
    assert!(
        output.status.success(),
        "structured plugin errors exit zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

//! Consistency of the Orbit plugin's published contract with the crate.
//!
//! The plugin launcher, the legacy installer, the conformance goldens, and the
//! manifests each restate versions the crate defines. These checks keep them
//! from drifting, and keep a released plugin version from silently changing
//! the contract it pins (`tests/plugin-releases.json`).

#![allow(clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use orbit_graph::plugin::PLUGIN_SCHEMA_VERSION;
use orbit_graph::{EXTRACTOR_VERSION, HISTORY_INDEX_SCHEMA_VERSION, STORE_SCHEMA_VERSION};

const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Files that pin the executable contract the plugin launches.
const PINNING_SCRIPTS: [&str; 3] = [
    "bin/orbit-graph",
    "plugin/bin/orbit-graph",
    "scripts/install-orbit-plugin.sh",
];

#[test]
fn manifests_declare_the_crate_version_and_first_party_origin() {
    for path in ["plugin.yaml", "plugin/plugin.yaml"] {
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
            ("plugin_schema_version", PLUGIN_SCHEMA_VERSION),
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
    for path in [
        "tests/conformance/version.yaml",
        "plugin/tests/conformance/version.yaml",
    ] {
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
             workspace version and both manifests' metadata.version instead of reusing it"
        );
    }
}

#[test]
fn every_tool_schema_property_is_described() {
    let manifest = read_yaml("plugin.yaml");
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
        let schema: Value = serde_json::from_str(&read(&path)).expect("parse schema");
        collect_undescribed(&schema, &path, &mut undescribed);
    }
    assert!(
        undescribed.is_empty(),
        "schema properties without a description: {undescribed:?}"
    );
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
        "plugin_schema_version": PLUGIN_SCHEMA_VERSION,
    })
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

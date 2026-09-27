//! The plugin's tool table is the one place a tool is named (STD-02 §R24);
//! it must serve exactly the tools `orbit_plugin.yaml` declares.

use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::plugin::TOOLS;

fn repository_root() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

#[test]
fn the_tool_table_serves_exactly_the_tools_plugin_yaml_declares() {
    let manifest: serde_norway::Value = serde_norway::from_str(
        &fs::read_to_string(repository_root().join("orbit_plugin.yaml"))
            .expect("read orbit_plugin.yaml"),
    )
    .expect("parse orbit_plugin.yaml");
    let declared = manifest["spec"]["tools"]
        .as_sequence()
        .expect("spec.tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name").to_string())
        .collect::<Vec<_>>();
    let served = TOOLS
        .iter()
        .map(|tool| {
            let verb = tool
                .name
                .strip_prefix("orbit.graph.")
                .expect("first-party spelling");
            assert_eq!(tool.v2_alias, format!("graph.{verb}"), "{}", tool.name);
            verb.to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        served, declared,
        "tool table and orbit_plugin.yaml disagree"
    );

    for (tool, entry) in TOOLS
        .iter()
        .zip(manifest["spec"]["tools"].as_sequence().expect("tools"))
    {
        let schema_path = entry["input_schema"]["$ref"]
            .as_str()
            .expect("input_schema $ref");
        let schema: Value = serde_json::from_str(
            &fs::read_to_string(repository_root().join(schema_path)).expect("read schema"),
        )
        .expect("parse schema");
        assert_eq!(
            tool.needs_repository,
            schema["properties"].get("repository").is_some(),
            "{}: needs_repository must match whether {schema_path} takes a repository",
            tool.name
        );
    }
}

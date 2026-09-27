#![allow(missing_docs)]

use std::collections::BTreeMap;
use std::path::Path;

use crate::Extractor;
use crate::languages::ConfigExtractor;

fn extract_yaml(source: &str) -> crate::ExtractedFile {
    ConfigExtractor.extract(Path::new("app.yaml"), source.as_bytes())
}
fn extract_toml(source: &str) -> crate::ExtractedFile {
    ConfigExtractor.extract(Path::new("Cargo.toml"), source.as_bytes())
}
fn extract_json(source: &str) -> crate::ExtractedFile {
    ConfigExtractor.extract(Path::new("settings.json"), source.as_bytes())
}

fn key_lines(file: crate::ExtractedFile) -> BTreeMap<String, usize> {
    file.configs
        .into_iter()
        .map(|config| (config.key, config.line))
        .collect()
}

#[test]
fn parsed_toml_keys_use_their_source_lines() {
    let source = "# settings\nport = 8080\n\n[server]\nport = 9000\n[server.tls]\nenabled = true\n[client]\nport = 3000\ninline = { one = 1, nested = { two = 2 } }\n\"dotted.key\" = 4\n";
    assert_eq!(
        key_lines(extract_toml(source)),
        BTreeMap::from([
            ("port".into(), 2),
            ("server".into(), 4),
            ("server.port".into(), 5),
            ("server.tls".into(), 6),
            ("server.tls.enabled".into(), 7),
            ("client".into(), 8),
            ("client.port".into(), 9),
            ("client.inline".into(), 10),
            ("client.inline.one".into(), 10),
            ("client.inline.nested".into(), 10),
            ("client.inline.nested.two".into(), 10),
            ("client.dotted.key".into(), 11),
        ])
    );
}

#[test]
fn parsed_yaml_keys_use_their_source_lines() {
    let source = "# settings\nserver:\n  port: 8080\n  tls:\n    enabled: true\nclient:\n  port: 3000\n  inline: { one: 1, nested: { two: 2 } }\n'quoted.key': yes\n";
    assert_eq!(
        key_lines(extract_yaml(source)),
        BTreeMap::from([
            ("server".into(), 2),
            ("server.port".into(), 3),
            ("server.tls".into(), 4),
            ("server.tls.enabled".into(), 5),
            ("client".into(), 6),
            ("client.port".into(), 7),
            ("client.inline".into(), 8),
            ("client.inline.one".into(), 8),
            ("client.inline.nested".into(), 8),
            ("client.inline.nested.two".into(), 8),
            ("quoted.key".into(), 9),
        ])
    );
}

#[test]
fn parsed_json_keys_use_their_source_lines() {
    let source = "{\n  \"server\": {\n    \"port\": 8080,\n    \"tls\": {\"enabled\": true}\n  },\n  \"client\": {\n    \"port\": 3000,\n    \"escaped\\u002ekey\": false\n  },\n  \"items\": [{\"ignored\": 1}]\n}\n";
    assert_eq!(
        key_lines(extract_json(source)),
        BTreeMap::from([
            ("server".into(), 2),
            ("server.port".into(), 3),
            ("server.tls".into(), 4),
            ("server.tls.enabled".into(), 4),
            ("client".into(), 6),
            ("client.port".into(), 7),
            ("client.escaped.key".into(), 8),
            ("items".into(), 10),
        ])
    );
}

#[test]
fn parsed_key_locations_ignore_scalar_text_and_blank_lines() {
    let toml = "note = \"\"\"\n[pretend]\nport = 1\n\"\"\"\n[real]\nport = 2\n";
    assert_eq!(
        key_lines(extract_toml(toml)),
        BTreeMap::from([
            ("note".into(), 1),
            ("real".into(), 5),
            ("real.port".into(), 6),
        ])
    );

    let yaml = "server:\n\n  port: 8080\n  note: |\n    port: pretend\nclient:\n  port: 3000\n";
    assert_eq!(
        key_lines(extract_yaml(yaml)),
        BTreeMap::from([
            ("server".into(), 1),
            ("server.port".into(), 3),
            ("server.note".into(), 4),
            ("client".into(), 6),
            ("client.port".into(), 7),
        ])
    );
}

#[test]
fn parsed_yaml_aliases_and_flow_mappings_have_source_lines() {
    let alias = "defaults: &defaults\n  port: 8080\nserver: *defaults\n";
    assert_eq!(
        key_lines(extract_yaml(alias)),
        BTreeMap::from([
            ("defaults".into(), 1),
            ("defaults.port".into(), 2),
            ("server".into(), 3),
            ("server.port".into(), 3),
        ])
    );

    let flow = "{server: {\n  port: 8080,\n  tls: {enabled: true}},\n client: {port: 3000}}\n";
    assert_eq!(
        key_lines(extract_yaml(flow)),
        BTreeMap::from([
            ("server".into(), 1),
            ("server.port".into(), 2),
            ("server.tls".into(), 3),
            ("server.tls.enabled".into(), 3),
            ("client".into(), 4),
            ("client.port".into(), 4),
        ])
    );

    let document = "---\n{server: {\n  port: 8080},\n client: {port: 3000}}\n";
    assert_eq!(
        key_lines(extract_yaml(document)),
        BTreeMap::from([
            ("server".into(), 2),
            ("server.port".into(), 3),
            ("client".into(), 4),
            ("client.port".into(), 4),
        ])
    );
}

#[test]
fn multiline_inline_mappings_keep_nested_positions() {
    let toml = "inline = {\n  port = 8080,\n  nested = { port = 9000 },\n}\nport = 3000\n";
    assert_eq!(
        key_lines(extract_toml(toml)),
        BTreeMap::from([
            ("inline".into(), 1),
            ("inline.port".into(), 2),
            ("inline.nested".into(), 3),
            ("inline.nested.port".into(), 3),
            ("port".into(), 5),
        ])
    );

    let yaml = "inline: {\n  port: 8080,\n  nested: { port: 9000 }}\nport: 3000\n";
    assert_eq!(
        key_lines(extract_yaml(yaml)),
        BTreeMap::from([
            ("inline".into(), 1),
            ("inline.port".into(), 2),
            ("inline.nested".into(), 3),
            ("inline.nested.port".into(), 3),
            ("port".into(), 4),
        ])
    );
}

#[test]
fn config_yaml_toml_json_each_populate_configs_with_correct_kind() {
    let yml = "server:\n  port: 8080\n  host: localhost\nfeatures:\n  - alpha\n";
    let file = extract_yaml(yml);
    assert!(!file.configs.is_empty());
    assert!(
        file.configs
            .iter()
            .any(|c| c.kind == "yaml" && c.key.contains("server"))
    );
    assert!(file.relations.is_empty() && file.refs.is_empty()); // config never emits these

    let toml_src = "[package]\nname = \"test\"\n\n[dependencies]\nfoo = \"1\"\n";
    let file = extract_toml(toml_src);
    assert!(
        file.configs
            .iter()
            .any(|c| c.kind == "toml" && c.key.contains("package"))
    );
    assert!(file.refs.is_empty() && file.relations.is_empty());

    let json = r#"{"db": {"url": "postgres://"}, "debug": true}"#;
    let file = extract_json(json);
    assert!(
        file.configs
            .iter()
            .any(|c| c.kind == "json" && c.key.contains("db"))
    );
    assert!(file.relations.is_empty());
}

#[test]
fn yaml_extracts_nested_string_keys_and_skips_non_string_keys() {
    let yaml = "server:\n  port: 8080\n  host: localhost\n42: ignored\n'quoted key': yes\n";
    let file = extract_yaml(yaml);
    let keys: Vec<_> = file
        .configs
        .iter()
        .map(|config| config.key.as_str())
        .collect();
    assert_eq!(keys, ["server", "server.port", "server.host", "quoted key"]);
    assert!(file.configs.iter().all(|config| config.kind == "yaml"));
}

#[test]
fn malformed_yaml_uses_line_scan_fallback_for_both_extensions() {
    let malformed = "server:\n  port: [unclosed\n";
    for extension in ["yaml", "yml"] {
        let path = format!("app.{extension}");
        let file = ConfigExtractor.extract(Path::new(&path), malformed.as_bytes());
        let keys: Vec<_> = file
            .configs
            .iter()
            .map(|config| config.key.as_str())
            .collect();
        assert_eq!(keys, ["server", "port"]);
        assert!(file.configs.iter().all(|config| config.kind == "yaml"));
    }
}

#[test]
fn yaml_aliases_preserve_nested_keys() {
    let yaml = "defaults: &defaults\n  host: localhost\nserver: *defaults\n";
    let file = extract_yaml(yaml);
    let keys: Vec<_> = file
        .configs
        .iter()
        .map(|config| config.key.as_str())
        .collect();
    assert_eq!(keys, ["defaults", "defaults.host", "server", "server.host"]);
}

#[test]
fn duplicate_yaml_keys_use_line_scan_fallback() {
    let yaml = "server:\n  port: 8080\nserver:\n  host: localhost\n";
    let file = extract_yaml(yaml);
    let keys: Vec<_> = file
        .configs
        .iter()
        .map(|config| config.key.as_str())
        .collect();
    assert_eq!(keys, ["server", "port", "server", "host"]);
}

#[test]
fn config_env_parses_keys() {
    let env = "DB_HOST=localhost\nPORT=3000\n# comment\n";
    let file = ConfigExtractor.extract(Path::new(".env"), env.as_bytes());
    assert!(
        file.configs
            .iter()
            .any(|c| c.kind == "env" && c.key == "DB_HOST")
    );
    assert!(file.configs.iter().any(|c| c.key == "PORT"));
}

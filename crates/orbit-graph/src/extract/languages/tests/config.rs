#![allow(missing_docs)]

use std::path::Path;

use crate::extract::Extractor;
use crate::extract::languages::ConfigExtractor;

fn extract_yaml(source: &str) -> crate::extract::ExtractedFile {
    ConfigExtractor.extract(Path::new("app.yaml"), source.as_bytes())
}
fn extract_toml(source: &str) -> crate::extract::ExtractedFile {
    ConfigExtractor.extract(Path::new("Cargo.toml"), source.as_bytes())
}
fn extract_json(source: &str) -> crate::extract::ExtractedFile {
    ConfigExtractor.extract(Path::new("settings.json"), source.as_bytes())
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

//! Environment variable contract: every variable `orbit-graph` reads is one
//! documented constant, production sources read the environment only through
//! those constants, and `docs/usage.md` matches the generated table (STD-01 §R25).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::environment::ENV_VARS;

fn repository_root() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

#[derive(Debug, PartialEq, Eq)]
struct DeclaredEnvVar {
    ident: String,
    value: String,
    default: String,
    overridden_flag: String,
    meaning: String,
}

fn parse_declared_env_vars(source: &str) -> Vec<DeclaredEnvVar> {
    let mut vars = Vec::new();
    let mut meaning = Vec::new();
    let mut default = None;
    let mut overridden_flag = None;

    for raw in source.lines() {
        let line = raw.trim();
        if let Some(doc) = line.strip_prefix("///") {
            let text = doc.strip_prefix(' ').unwrap_or(doc).trim();
            if text.is_empty() {
                continue;
            }
            if let Some(value) = text.strip_prefix("Default:") {
                default = Some(value.trim().to_owned());
            } else if let Some(value) = text.strip_prefix("Overridden flag:") {
                overridden_flag = Some(value.trim().to_owned());
            } else {
                meaning.push(text.to_owned());
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("pub const ")
            && let Some((ident, rhs)) = rest.split_once(':')
            && rhs.trim_start().starts_with("&str")
            && let Some((_, quoted)) = rhs.split_once('"')
            && let Some((value, _)) = quoted.split_once('"')
        {
            vars.push(DeclaredEnvVar {
                ident: ident.trim().to_owned(),
                value: value.to_owned(),
                default: default
                    .take()
                    .unwrap_or_else(|| panic!("{ident} is missing Default: doc line")),
                overridden_flag: overridden_flag
                    .take()
                    .unwrap_or_else(|| panic!("{ident} is missing Overridden flag: doc line")),
                meaning: {
                    let joined = meaning.join(" ");
                    meaning.clear();
                    assert!(!joined.is_empty(), "{ident} is missing meaning doc text");
                    joined
                },
            });
            continue;
        }
        if !line.is_empty() && !line.starts_with("#[") {
            meaning.clear();
            default = None;
            overridden_flag = None;
        }
    }
    vars
}

fn render_markdown_table(vars: &[DeclaredEnvVar]) -> String {
    let mut lines = vec![
        "<!-- env-table:begin -->".to_owned(),
        "| Variable | Default | Overridden flag | Meaning |".to_owned(),
        "| --- | --- | --- | --- |".to_owned(),
    ];
    for var in vars {
        lines.push(format!(
            "| `{}` | {} | {} | {} |",
            var.value,
            var.default.replace('|', "\\|"),
            var.overridden_flag.replace('|', "\\|"),
            var.meaning.replace('|', "\\|"),
        ));
    }
    lines.push("<!-- env-table:end -->".to_owned());
    lines.join("\n")
}

#[test]
fn env_var_constants_match_usage_table() {
    let declared = parse_declared_env_vars(include_str!("../environment.rs"));
    assert!(!declared.is_empty());

    let declared_values = declared
        .iter()
        .map(|var| var.value.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        declared_values, ENV_VARS,
        "ENV_VARS must list every documented environment variable in declaration order"
    );

    let unique = declared_values.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(unique.len(), declared_values.len(), "duplicate env var");

    let usage = fs::read_to_string(repository_root().join("docs/usage.md")).expect("read usage.md");
    let begin = usage
        .find("<!-- env-table:begin -->")
        .expect("docs/usage.md has env-table:begin");
    let end = usage
        .find("<!-- env-table:end -->")
        .expect("docs/usage.md has env-table:end")
        + "<!-- env-table:end -->".len();
    let actual = &usage[begin..end];
    let expected = render_markdown_table(&declared);
    assert_eq!(
        actual, expected,
        "docs/usage.md environment table is out of date; run scripts/check-env-vars.sh --update"
    );
}

#[test]
fn production_env_reads_name_documented_constants() {
    let declared = parse_declared_env_vars(include_str!("../environment.rs"));
    let valid_idents = declared
        .iter()
        .map(|var| var.ident.as_str())
        .collect::<BTreeSet<_>>();

    let crates_dir = repository_root().join("crates");
    let mut files = Vec::new();
    collect_production_sources(&crates_dir, &mut files);
    assert!(!files.is_empty());

    let mut offenders = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).expect("read production source");
        for (index, raw) in text.lines().enumerate() {
            let trimmed = raw.trim();
            if trimmed.starts_with("//") {
                continue;
            }
            let code = raw.split(" //").next().unwrap_or(raw);
            if code.contains("try_from_default_env") {
                offenders.push(format!("{}:{}: {}", file.display(), index + 1, trimmed));
                continue;
            }
            for pattern in ["env::var_os(", "env::var(", "read_var(", "try_from_env("] {
                let mut rest = code;
                while let Some(pos) = rest.find(pattern) {
                    let after = &rest[pos + pattern.len()..];
                    let arg = after.split(')').next().unwrap_or(after).trim();
                    if !valid_idents.contains(arg) {
                        offenders.push(format!("{}:{}: {}", file.display(), index + 1, trimmed));
                    }
                    rest = after;
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "production env reads must name a documented constant:\n{}",
        offenders.join("\n")
    );
}

fn collect_production_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read crates dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name != "tests") {
                collect_production_sources(&path, out);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs")
            && path
                .components()
                .any(|component| component.as_os_str() == "src")
        {
            out.push(path);
        }
    }
}

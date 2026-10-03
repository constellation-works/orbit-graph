//! Public help and representative output payloads, captured from the real
//! executable. Update explicitly with UPDATE_GOLDENS=1 and review the bytes.

#![allow(clippy::expect_used)]
#![allow(
    clippy::disallowed_methods,
    reason = "the explicit golden-update mode writes checked-in fixtures"
)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

const COMMANDS: &[&[&str]] = &[
    &[],
    &["sync"],
    &["search"],
    &["show"],
    &["refs"],
    &["callees"],
    &["impact"],
    &["changes"],
    &["recommend"],
    &["history"],
    &["history", "import"],
    &["history", "sync"],
    &["history", "status"],
    &["history", "rebuild"],
    &["evaluate"],
    &["trace"],
    &["overview"],
    &["implementors"],
    &["deps"],
    &["db-path"],
    &["clean"],
    &["version"],
];

#[test]
fn every_command_help_matches_the_public_golden() {
    let cwd = TempDir::new().expect("create isolated working directory");
    for command in COMMANDS {
        let output = run(cwd.path(), command, &["--help"]);
        assert!(output.status.success(), "{command:?}");
        assert!(output.stderr.is_empty(), "{command:?}");
        assert!(!output.stdout.contains(&0x1b), "help has no ANSI styling");
        let name = if command.is_empty() {
            "root".to_owned()
        } else {
            command.join("-")
        };
        check_golden(&format!("{name}.txt"), &output.stdout);
    }
    assert!(
        !cwd.path().join(".orbit-graph").exists(),
        "help creates no graph state"
    );
}

#[test]
fn version_json_matches_the_public_golden() {
    let cwd = TempDir::new().expect("create isolated working directory");
    git2::Repository::init(cwd.path()).expect("anchor repository discovery");
    let output = run(cwd.path(), &["version"], &["--json"]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    check_golden("version.json", &output.stdout);
    assert!(!cwd.path().join(".orbit-graph").exists());
}

#[test]
fn representative_outputs_match_the_public_goldens() {
    const CASES: &[(&str, &[&str], i32)] = &[
        ("sync", &["sync", "--full"], 0),
        ("search", &["search", "helper", "--kind", "symbol"], 0),
        ("search-empty", &["search", "absent_query"], 0),
        ("search-limited", &["search", "helper", "--limit", "1"], 0),
        ("show", &["show", "symbol:src/lib.rs#entry:function"], 0),
        (
            "show-metadata",
            &["show", "file:src/lib.rs", "--max-bytes", "0"],
            0,
        ),
        ("refs", &["refs", "symbol:src/lib.rs#helper:function"], 0),
        ("overview", &["overview", "--detail", "full"], 0),
        ("clean", &["clean"], 0),
        ("changes", &["changes", "HEAD~1..HEAD", "--no-cache"], 0),
        ("changes-empty", &["changes", "HEAD..HEAD", "--no-cache"], 0),
        ("error", &["show", "not-a-selector"], 2),
    ];
    for mode in ["json", "ndjson", "auto"] {
        let fixture = output_fixture();
        for (name, args, exit_code) in CASES {
            let output = run(fixture.path(), args, &["--format", mode]);
            assert_eq!(
                output.status.code(),
                Some(*exit_code),
                "{name} {mode}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = normalize_stdout(name, mode, &output.stdout, fixture.path());
            let stderr = normalize_paths(&output.stderr, fixture.path());
            let captured = format!("exit: {exit_code}\nstdout:\n{stdout}\nstderr:\n{stderr}");
            let mode = if mode == "auto" { "plain" } else { mode };
            check_golden_in("output", &format!("{name}-{mode}.txt"), captured.as_bytes());
            if *name == "sync" {
                // Exercise clean's dry-run paths without deleting any file.
                fs::write(fixture.path().join(".orbit-graph/main.1.db"), [])
                    .expect("obsolete database fixture");
            }
        }
    }
}

fn output_fixture() -> TempDir {
    let fixture = TempDir::new().expect("create output fixture");
    let root = fixture.path();
    let repo = git2::Repository::init_opts(
        root,
        git2::RepositoryInitOptions::new().initial_head("main"),
    )
    .expect("anchor fixture repository");
    fs::create_dir(root.join("src")).expect("source directory");
    fs::create_dir(root.join("tests")).expect("test directory");
    fs::write(
        root.join("tests/test_lib.rs"),
        "fn test_helper() { helper(); }\n",
    )
    .expect("candidate test source");
    let source = "pub fn helper() -> i32 { 1 }\npub fn entry() -> i32 { helper() }\n#[test]\nfn helper_test() { let value = helper(); assert_eq!(value, 1); }\n";
    for (index, content) in [source.to_owned(), source.replacen("{ 1 }", "{ 2 }", 1)]
        .iter()
        .enumerate()
    {
        fs::write(root.join("src/lib.rs"), content).expect("write source");
        let mut git_index = repo.index().expect("fixture index");
        git_index
            .add_path(Path::new("src/lib.rs"))
            .expect("stage source");
        git_index
            .add_path(Path::new("tests/test_lib.rs"))
            .expect("stage test source");
        git_index.write().expect("write index");
        let tree_id = git_index.write_tree().expect("write tree");
        let tree = repo.find_tree(tree_id).expect("fixture tree");
        let signature = git2::Signature::new(
            "Golden Fixture",
            "golden@example.invalid",
            &git2::Time::new(1_700_000_000, 0),
        )
        .expect("fixed signature");
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        let parents = parent.iter().collect::<Vec<_>>();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            &format!("fixture {index}"),
            &tree,
            &parents,
        )
        .expect("commit fixture");
    }
    // A second sync table with a deterministic reason; changes compares only
    // committed trees, so this untracked oversize file stays out of symbol evidence.
    fs::write(root.join("oversize.rs"), vec![b' '; 4 * 1024 * 1024 + 1]).expect("oversize fixture");
    fixture
}

/// Preserve shipping serialization, ordering and whitespace. Only fixture
/// roots, sync duration, changes generation time and its three wall-clock
/// timings vary; commit IDs, fields, nulls, counts and records stay pinned.
fn normalize_stdout(name: &str, mode: &str, bytes: &[u8], root: &Path) -> String {
    let mut text = normalize_paths(bytes, root);
    if mode == "auto" {
        if name == "sync" {
            text = text
                .split_inclusive('\n')
                .map(|line| {
                    if line.starts_with("sync_summary\t") {
                        let mut cells = line
                            .trim_end_matches('\n')
                            .split('\t')
                            .map(str::to_owned)
                            .collect::<Vec<_>>();
                        assert_eq!(cells.len(), 9, "sync plain column contract");
                        cells[4]
                            .parse::<u64>()
                            .expect("sync duration is an integer");
                        cells[4] = "0".to_owned();
                        format!("{}\n", cells.join("\t"))
                    } else {
                        line.to_owned()
                    }
                })
                .collect();
        }
        return text;
    }
    if name == "sync" || name.starts_with("changes") {
        let document: Value = serde_json::from_str(text.lines().next().expect("machine document"))
            .expect("JSON record");
        if name == "sync" {
            document["duration_ms"]
                .as_u64()
                .expect("duration remains an integer");
            normalize_field(&mut text, "duration_ms", &document["duration_ms"], "0");
        } else {
            let document = if mode == "ndjson" {
                &document["context"]
            } else {
                &document
            };
            assert!(
                document["generated_at"].is_string(),
                "generation time remains text"
            );
            normalize_field(
                &mut text,
                "generated_at",
                &document["generated_at"],
                "\"2000-01-01T00:00:00Z\"",
            );
            for field in ["prepare_ms", "analysis_ms", "total_ms"] {
                document["timings"][field]
                    .as_u64()
                    .expect("timing remains an integer");
                normalize_field(&mut text, field, &document["timings"][field], "0");
            }
        }
    }
    text
}

fn normalize_field(text: &mut String, field: &str, value: &Value, replacement: &str) {
    assert!(
        !value.is_null(),
        "normalization must not hide a missing {field}"
    );
    let original = format!("\"{field}\":{value}");
    assert_eq!(
        text.matches(&original).count(),
        1,
        "normalize exactly one {field}"
    );
    *text = text.replacen(&original, &format!("\"{field}\":{replacement}"), 1);
}

fn normalize_paths(bytes: &[u8], root: &Path) -> String {
    let root = root
        .canonicalize()
        .expect("physical fixture root")
        .to_string_lossy()
        .into_owned();
    let json_root = serde_json::to_string(&root).expect("escaped root");
    String::from_utf8(bytes.to_vec())
        .expect("UTF-8 output")
        .replace(&json_root[1..json_root.len() - 1], "<ROOT>")
        .replace(&root, "<ROOT>")
}

fn run(cwd: &Path, command: &[&str], args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", cwd.join(".git/hermetic-home"))
        .env("XDG_CONFIG_HOME", cwd.join(".git/hermetic-home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("RUST_LOG", "off")
        .env("TERM", "dumb")
        .env("COLUMNS", "1")
        .env("NO_COLOR", "1")
        .args(command)
        .args(args)
        .output()
        .expect("run public CLI surface")
}

fn check_golden(name: &str, actual: &[u8]) {
    check_golden_in("help", name, actual);
}

fn check_golden_in(directory: &str, name: &str, actual: &[u8]) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(directory)
        .join(name);
    if std::env::var("UPDATE_GOLDENS").as_deref() == Ok("1") {
        fs::create_dir_all(path.parent().expect("golden directory"))
            .expect("create golden directory");
        fs::write(&path, actual).expect("write explicit golden update");
    }
    let expected = fs::read(&path).expect("read public golden; regenerate with UPDATE_GOLDENS=1");
    assert_eq!(
        actual,
        expected,
        "public CLI surface changed: {}",
        path.display()
    );
}

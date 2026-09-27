use std::fs;
use std::path::Path;

use crate::{
    Confidence, Graph, ProgramNameSource, RefOpts, RuntimeInvocationSymbol, Selector, SyncMode,
    SyncPolicy,
};

fn write(root: &Path, path: &str, contents: &str) {
    let target = root.join(path);
    fs::create_dir_all(target.parent().expect("parent directory")).expect("create directory");
    fs::write(target, contents).expect("write file");
}

fn fixture_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    crate::tests::support::set_discovery_boundary(dir.path());
    dir
}

fn synced_graph(root: &Path) -> Graph {
    let graph = Graph::open(root, SyncPolicy::Manual).expect("open graph");
    graph.sync(SyncMode::Full).expect("sync graph");
    graph
}

#[test]
fn runtime_invocations_report_program_line_and_enclosing_symbol() {
    let dir = fixture_dir();
    write(
        dir.path(),
        "tests/test_cli.py",
        "import subprocess\n\n\ndef git(*args):\n    return args\n\n\ndef test_status():\n    \
         subprocess.run([\"git\", \"status\"])\n",
    );
    write(
        dir.path(),
        "tests/cli.rs",
        "#[test]\nfn runs() {\n    Command::new(env!(\"CARGO_BIN_EXE_tool\"));\n}\n",
    );
    let graph = synced_graph(dir.path());

    let invocations = graph.runtime_invocations().expect("runtime invocations");
    let rows: Vec<_> = invocations
        .iter()
        .map(|invocation| {
            (
                invocation.file.as_str(),
                invocation.line,
                invocation.program.as_str(),
                invocation.symbol.clone(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "tests/cli.rs",
                3,
                "tool",
                Some(RuntimeInvocationSymbol {
                    name: "runs".to_string(),
                    kind: "test".to_string(),
                    qualified: "runs".to_string(),
                }),
            ),
            (
                "tests/test_cli.py",
                9,
                "git",
                Some(RuntimeInvocationSymbol {
                    name: "test_status".to_string(),
                    kind: "function".to_string(),
                    qualified: "test_status".to_string(),
                }),
            ),
        ]
    );

    // A program named like a function is not a reference to that function,
    // even at the name-only floor.
    let selector: Selector = "symbol:tests/test_cli.py#git:function"
        .parse()
        .expect("selector");
    let refs = graph
        .refs(
            &selector,
            &RefOpts {
                confidence: Confidence::FuzzyName,
                kind: None,
            },
        )
        .expect("refs at the fuzzy floor");
    assert!(refs.refs.is_empty(), "{refs:?}");
    let impact = graph
        .impact(&selector, 1, Confidence::FuzzyName)
        .expect("impact at the fuzzy floor");
    assert!(
        impact
            .touched
            .iter()
            .all(|entry| !entry.qualified_name.contains("test_status")),
        "{impact:?}"
    );
}

#[test]
fn program_names_come_from_the_nearest_manifests() {
    let dir = fixture_dir();
    write(
        dir.path(),
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/*\"]\n",
    );
    write(
        dir.path(),
        "crates/tool/Cargo.toml",
        "[package]\nname = \"tool\"\n\n[[bin]]\nname = \"tool-cli\"\npath = \"src/main.rs\"\n",
    );
    write(dir.path(), "crates/tool/src/lib.rs", "pub fn run() {}\n");
    write(dir.path(), "crates/tool/src/bin/extra.rs", "fn main() {}\n");
    write(
        dir.path(),
        "crates/tool/src/bin/nested/main.rs",
        "fn main() {}\n",
    );
    write(
        dir.path(),
        "pyproject.toml",
        "[project]\nname = \"pkg\"\n\n[project.scripts]\npkg-cli = \"pkg.cli:main\"\n\n\
         [tool.poetry.scripts]\npkg-legacy = \"pkg.cli:legacy\"\n",
    );
    write(dir.path(), "pkg/cli.py", "def main():\n    pass\n");
    let graph = Graph::open(dir.path(), SyncPolicy::Manual).expect("open graph");

    let names: Vec<_> = graph
        .program_names("crates/tool/src/lib.rs")
        .expect("cargo program names")
        .into_iter()
        .map(|name| (name.name, name.source, name.manifest))
        .collect();
    let cargo = "crates/tool/Cargo.toml".to_string();
    let pyproject = "pyproject.toml".to_string();
    assert_eq!(
        names,
        vec![
            (
                "extra".to_string(),
                ProgramNameSource::CargoBin,
                cargo.clone()
            ),
            (
                "nested".to_string(),
                ProgramNameSource::CargoBin,
                cargo.clone()
            ),
            (
                "pkg-cli".to_string(),
                ProgramNameSource::PyprojectScript,
                pyproject.clone()
            ),
            (
                "pkg-legacy".to_string(),
                ProgramNameSource::PyprojectScript,
                pyproject.clone()
            ),
            (
                "tool".to_string(),
                ProgramNameSource::CargoPackage,
                cargo.clone()
            ),
            ("tool-cli".to_string(), ProgramNameSource::CargoBin, cargo),
        ]
    );

    // The virtual workspace manifest has no `[package]`, so a root-level file
    // gets only the pyproject scripts.
    let root_names: Vec<_> = graph
        .program_names("pkg/cli.py")
        .expect("pyproject program names")
        .into_iter()
        .map(|name| name.name)
        .collect();
    assert_eq!(root_names, vec!["pkg-cli", "pkg-legacy"]);

    let error = graph
        .program_names("../outside.py")
        .expect_err("a path leaving the worktree is refused");
    assert!(error.to_string().contains("worktree"), "{error}");
}

#[cfg(unix)]
#[test]
fn program_names_skip_external_manifests_and_find_contained_ancestors() {
    use std::os::unix::fs::symlink;

    let dir = fixture_dir();
    let outside = tempfile::tempdir().expect("outside");
    write(dir.path(), "Cargo.toml", "[package]\nname = \"root\"\n");
    write(
        dir.path(),
        "pyproject.toml",
        "[project.scripts]\nroot-cli = \"root:main\"\n",
    );
    write(dir.path(), "nested/src/lib.rs", "pub fn run() {}\n");
    write(
        outside.path(),
        "Cargo.toml",
        "[package]\nname = \"outside\"\n",
    );
    write(
        outside.path(),
        "pyproject.toml",
        "[project.scripts]\noutside-cli = \"outside:main\"\n",
    );
    symlink(
        outside.path().join("Cargo.toml"),
        dir.path().join("nested/Cargo.toml"),
    )
    .expect("external Cargo manifest link");
    symlink(
        outside.path().join("pyproject.toml"),
        dir.path().join("nested/pyproject.toml"),
    )
    .expect("external pyproject manifest link");

    let graph = Graph::open(dir.path(), SyncPolicy::Manual).expect("open graph");
    let names: Vec<_> = graph
        .program_names("nested/src/lib.rs")
        .expect("contained ancestor names")
        .into_iter()
        .map(|name| (name.name, name.manifest))
        .collect();
    assert_eq!(
        names,
        vec![
            ("root".to_string(), "Cargo.toml".to_string()),
            ("root-cli".to_string(), "pyproject.toml".to_string()),
        ]
    );
}

#[cfg(unix)]
#[test]
fn program_names_skip_external_bin_directory_and_targets() {
    use std::os::unix::fs::symlink;

    let dir = fixture_dir();
    let outside = tempfile::tempdir().expect("outside");
    write(dir.path(), "Cargo.toml", "[package]\nname = \"safe\"\n");
    write(dir.path(), "src/lib.rs", "pub fn run() {}\n");
    write(outside.path(), "external.rs", "fn main() {}\n");
    symlink(outside.path(), dir.path().join("src/bin")).expect("external bin directory link");
    let graph = Graph::open(dir.path(), SyncPolicy::Manual).expect("open graph");
    let names: Vec<_> = graph
        .program_names("src/lib.rs")
        .expect("program names")
        .into_iter()
        .map(|name| name.name)
        .collect();
    assert_eq!(names, vec!["safe"]);

    let nested = fixture_dir();
    write(nested.path(), "Cargo.toml", "[package]\nname = \"safe\"\n");
    write(nested.path(), "src/lib.rs", "pub fn run() {}\n");
    write(nested.path(), "src/bin/local.rs", "fn main() {}\n");
    write(outside.path(), "external-dir/main.rs", "fn main() {}\n");
    symlink(
        outside.path().join("external.rs"),
        nested.path().join("src/bin/external.rs"),
    )
    .expect("external bin file link");
    symlink(
        outside.path().join("external-dir"),
        nested.path().join("src/bin/external-dir"),
    )
    .expect("external bin target directory link");
    let graph = Graph::open(nested.path(), SyncPolicy::Manual).expect("open graph");
    let names: Vec<_> = graph
        .program_names("src/lib.rs")
        .expect("program names")
        .into_iter()
        .map(|name| name.name)
        .collect();
    assert_eq!(names, vec!["local", "safe"]);
}

#[cfg(unix)]
#[test]
fn program_names_follow_internal_manifest_and_bin_symlinks() {
    use std::os::unix::fs::symlink;

    let dir = fixture_dir();
    write(dir.path(), "nested/src/lib.rs", "pub fn run() {}\n");
    write(
        dir.path(),
        "shared/Cargo.toml",
        "[package]\nname = \"linked\"\n",
    );
    write(
        dir.path(),
        "shared/pyproject.toml",
        "[project.scripts]\nlinked-cli = \"linked:main\"\n",
    );
    write(dir.path(), "shared/bins/direct.rs", "fn main() {}\n");
    write(dir.path(), "shared/bins/nested/main.rs", "fn main() {}\n");
    symlink(
        dir.path().join("shared/Cargo.toml"),
        dir.path().join("nested/Cargo.toml"),
    )
    .expect("internal Cargo manifest link");
    symlink(
        dir.path().join("shared/pyproject.toml"),
        dir.path().join("nested/pyproject.toml"),
    )
    .expect("internal pyproject manifest link");
    symlink(
        dir.path().join("shared/bins"),
        dir.path().join("nested/src/bin"),
    )
    .expect("internal bin directory link");

    let graph = Graph::open(dir.path(), SyncPolicy::Manual).expect("open graph");
    let names: Vec<_> = graph
        .program_names("nested/src/lib.rs")
        .expect("program names")
        .into_iter()
        .map(|name| (name.name, name.manifest))
        .collect();
    assert_eq!(
        names,
        vec![
            ("direct".to_string(), "nested/Cargo.toml".to_string()),
            ("linked".to_string(), "nested/Cargo.toml".to_string()),
            (
                "linked-cli".to_string(),
                "nested/pyproject.toml".to_string()
            ),
            ("nested".to_string(), "nested/Cargo.toml".to_string()),
        ]
    );
}

//! Verification harness for the change-explorer fixture corpus under
//! `tests/fixtures/change-explorer/`. Each case is materialized into a real,
//! temporary Git repository with reproducible commit SHAs, indexed snapshot
//! by snapshot with the real built `orbit-graph` binary, and checked against
//! the evidence recorded in that case's `expected.json`. See
//! `tests/fixtures/change-explorer/README.md` for the manifest schema.

#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

const CASES: &[&str] = &[
    "direct-call",
    "ambiguous-same-name",
    "changed-signature",
    "removed-symbol",
    "renamed-file",
    "changed-test",
    "generated-unsupported",
    "cycle",
    "branch-divergence",
];

const FIXTURE_AUTHOR_NAME: &str = "Orbit Graph Fixture";
const FIXTURE_AUTHOR_EMAIL: &str = "fixture@orbit-graph.invalid";
const ROOT_COMMIT_DATE: &str = "2023-12-31T00:00:00+00:00";
const BASE_COMMIT_DATE: &str = "2024-01-01T00:00:00+00:00";
const HEAD_COMMIT_DATE: &str = "2024-01-02T00:00:00+00:00";

#[test]
fn fixture_corpus_layout_matches_manifest_expectations() {
    let fixtures_root = fixtures_root();
    assert!(fixtures_root.join("README.md").is_file());
    for case_id in CASES {
        let case_dir = fixtures_root.join(case_id);
        assert!(
            case_dir.join("base").is_dir(),
            "case `{case_id}` missing base/"
        );
        assert!(
            case_dir.join("head").is_dir(),
            "case `{case_id}` missing head/"
        );
        assert!(
            case_dir.join("expected.json").is_file(),
            "case `{case_id}` missing expected.json"
        );
        assert_no_nested_git(&case_dir);
    }
}

#[test]
fn direct_call_case_produces_expected_evidence() {
    verify_case("direct-call");
}

#[test]
fn ambiguous_same_name_case_produces_expected_evidence() {
    verify_case("ambiguous-same-name");
}

#[test]
fn changed_signature_case_produces_expected_evidence() {
    verify_case("changed-signature");
}

#[test]
fn removed_symbol_case_produces_expected_evidence() {
    verify_case("removed-symbol");
}

#[test]
fn renamed_file_case_produces_expected_evidence() {
    verify_case("renamed-file");
}

#[test]
fn changed_test_case_produces_expected_evidence() {
    verify_case("changed-test");
}

#[test]
fn generated_unsupported_case_produces_expected_evidence() {
    verify_case("generated-unsupported");
}

#[test]
fn cycle_case_produces_expected_evidence() {
    verify_case("cycle");
}

#[test]
fn branch_divergence_case_produces_expected_evidence() {
    verify_case("branch-divergence");
}

/// Build a case, verify commit SHAs are reproducible, index every snapshot,
/// and check every claim in the case's `expected.json` against real query
/// output from the built `orbit-graph` binary.
fn verify_case(case_id: &str) {
    let case_dir = fixtures_root().join(case_id);
    let manifest = load_manifest(&case_dir);

    let first_build = build_case(&case_dir, &manifest);
    let second_build = build_case(&case_dir, &manifest);
    assert_eq!(
        first_build.snapshots, second_build.snapshots,
        "case `{case_id}` must produce identical commit SHAs across independent builds"
    );

    let worktrees_root = TempDir::new().expect("create worktree root");
    let mut worktrees: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (snapshot, sha) in &first_build.snapshots {
        let destination = worktrees_root.path().join(snapshot);
        checkout_worktree(&first_build.repository_path, sha, &destination);
        let sync = run_graph_json(&destination, &["sync", "--full"]);
        assert!(
            sync["files_indexed"]
                .as_u64()
                .is_some_and(|count| count >= 1),
            "case `{case_id}` snapshot `{snapshot}` indexed no files"
        );
        worktrees.insert(snapshot.clone(), destination);
    }

    verify_changed_symbols(case_id, &manifest, &worktrees);
    verify_unchanged_symbols(case_id, &manifest, &worktrees);
    verify_expected_references(case_id, &manifest, &worktrees);
    verify_expected_impact(case_id, &manifest, &worktrees);
    verify_candidate_tests(case_id, &manifest, &worktrees);
    verify_known_gaps(case_id, &manifest, &worktrees);
}

struct BuiltCase {
    _repository: TempDir,
    repository_path: PathBuf,
    snapshots: BTreeMap<String, String>,
}

/// Materialize a case's manifest topology into a fresh temporary Git
/// repository with fixed author/committer identity and timestamps, and
/// return the commit SHA for every named snapshot.
fn build_case(case_dir: &Path, manifest: &Value) -> BuiltCase {
    let topology = manifest["topology"].as_str().expect("case has a topology");
    let repository = TempDir::new().expect("create case repository");
    let repository_path = repository.path().to_path_buf();
    let mut snapshots = BTreeMap::new();

    match topology {
        "linear" => {
            run_git(&repository_path, &["init", "-q", "-b", "trunk"]);
            configure_identity(&repository_path);
            let base_sha = commit_tree(
                &repository_path,
                &case_dir.join("base"),
                "base",
                BASE_COMMIT_DATE,
            );
            snapshots.insert("base".to_string(), base_sha);
            let head_sha = commit_tree(
                &repository_path,
                &case_dir.join("head"),
                "head",
                HEAD_COMMIT_DATE,
            );
            snapshots.insert("head".to_string(), head_sha);
        }
        "branch_divergence" => {
            let branch_topology = manifest["branch_topology"]
                .as_object()
                .expect("branch_divergence case has branch_topology");
            let root_branch = branch_topology["root_branch"]
                .as_str()
                .expect("branch_topology.root_branch");
            let base_branch = branch_topology["base_branch"]
                .as_str()
                .expect("branch_topology.base_branch");
            let head_branch = branch_topology["head_branch"]
                .as_str()
                .expect("branch_topology.head_branch");

            run_git(&repository_path, &["init", "-q", "-b", root_branch]);
            configure_identity(&repository_path);
            let root_sha = commit_tree(
                &repository_path,
                &case_dir.join("root"),
                "root",
                ROOT_COMMIT_DATE,
            );
            snapshots.insert("root".to_string(), root_sha.clone());

            run_git(
                &repository_path,
                &["checkout", "-q", "-b", base_branch, root_branch],
            );
            let base_sha = commit_tree(
                &repository_path,
                &case_dir.join("base"),
                "base",
                BASE_COMMIT_DATE,
            );
            snapshots.insert("base".to_string(), base_sha);

            run_git(
                &repository_path,
                &["checkout", "-q", "-b", head_branch, root_branch],
            );
            let head_sha = commit_tree(
                &repository_path,
                &case_dir.join("head"),
                "head",
                HEAD_COMMIT_DATE,
            );
            snapshots.insert("head".to_string(), head_sha);

            let merge_base =
                git_stdout(&repository_path, &["merge-base", base_branch, head_branch]);
            assert_eq!(
                merge_base, root_sha,
                "branch-divergence merge-base must equal the root commit"
            );
        }
        other => panic!("unknown topology `{other}`"),
    }

    BuiltCase {
        _repository: repository,
        repository_path,
        snapshots,
    }
}

/// Replace the working tree of `repository_path` with the contents of
/// `source_dir` and commit it with a fixed identity and timestamp.
fn commit_tree(repository_path: &Path, source_dir: &Path, message: &str, date: &str) -> String {
    clear_worktree(repository_path);
    copy_tree(source_dir, repository_path);
    run_git(repository_path, &["add", "-A"]);
    run_git_with_env(
        repository_path,
        &["commit", "-q", "--allow-empty", "-m", message],
        &[
            ("GIT_AUTHOR_NAME", FIXTURE_AUTHOR_NAME),
            ("GIT_AUTHOR_EMAIL", FIXTURE_AUTHOR_EMAIL),
            ("GIT_AUTHOR_DATE", date),
            ("GIT_COMMITTER_NAME", FIXTURE_AUTHOR_NAME),
            ("GIT_COMMITTER_EMAIL", FIXTURE_AUTHOR_EMAIL),
            ("GIT_COMMITTER_DATE", date),
        ],
    );
    git_stdout(repository_path, &["rev-parse", "HEAD"])
}

fn clear_worktree(repository_path: &Path) {
    for entry in fs::read_dir(repository_path).expect("read repository directory") {
        let entry = entry.expect("directory entry");
        if entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        if entry.file_type().expect("file type").is_dir() {
            fs::remove_dir_all(&path).expect("remove stale directory");
        } else {
            fs::remove_file(&path).expect("remove stale file");
        }
    }
}

fn copy_tree(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source).expect("read fixture directory") {
        let entry = entry.expect("directory entry");
        let target = destination.join(entry.file_name());
        let file_type = entry.file_type().expect("file type");
        if file_type.is_dir() {
            fs::create_dir_all(&target).expect("create directory");
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy fixture file");
        }
    }
}

fn configure_identity(repository_path: &Path) {
    run_git(
        repository_path,
        &["config", "user.name", FIXTURE_AUTHOR_NAME],
    );
    run_git(
        repository_path,
        &["config", "user.email", FIXTURE_AUTHOR_EMAIL],
    );
}

fn checkout_worktree(repository_path: &Path, sha: &str, destination: &Path) {
    let destination_str = destination.to_str().expect("utf8 worktree path");
    run_git(
        repository_path,
        &["worktree", "add", "-q", "--detach", destination_str, sha],
    );
}

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/change-explorer")
}

fn load_manifest(case_dir: &Path) -> Value {
    let bytes = fs::read(case_dir.join("expected.json")).expect("read expected.json");
    serde_json::from_slice(&bytes).expect("parse expected.json")
}

fn assert_no_nested_git(dir: &Path) {
    for entry in fs::read_dir(dir).expect("read fixture directory") {
        let entry = entry.expect("directory entry");
        let path = entry.path();
        if entry.file_type().expect("file type").is_dir() {
            assert_ne!(
                entry.file_name(),
                ".git",
                "found a committed .git directory at {}",
                path.display()
            );
            assert_no_nested_git(&path);
        }
    }
}

fn array_field<'a>(value: &'a Value, field: &str) -> &'a [Value] {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn string_array(value: &Value, field: &str) -> Vec<String> {
    array_field(value, field)
        .iter()
        .map(|item| item.as_str().expect("string array entry").to_string())
        .collect()
}

fn worktree_for<'a>(
    case_id: &str,
    worktrees: &'a BTreeMap<String, PathBuf>,
    snapshot: &str,
) -> &'a Path {
    worktrees
        .get(snapshot)
        .unwrap_or_else(|| panic!("case `{case_id}` references unknown snapshot `{snapshot}`"))
        .as_path()
}

fn verify_changed_symbols(case_id: &str, manifest: &Value, worktrees: &BTreeMap<String, PathBuf>) {
    for entry in array_field(manifest, "changed_symbols") {
        let selector = entry["selector"]
            .as_str()
            .expect("changed_symbols[].selector");
        verify_presence(case_id, selector, entry, worktrees);
    }
}

fn verify_unchanged_symbols(
    case_id: &str,
    manifest: &Value,
    worktrees: &BTreeMap<String, PathBuf>,
) {
    for entry in array_field(manifest, "unchanged_symbols") {
        let selector = entry["selector"]
            .as_str()
            .expect("unchanged_symbols[].selector");
        for snapshot in string_array(entry, "present_in") {
            assert_symbol_present(case_id, selector, &snapshot, worktrees, true);
        }
    }
}

fn verify_presence(
    case_id: &str,
    selector: &str,
    entry: &Value,
    worktrees: &BTreeMap<String, PathBuf>,
) {
    for snapshot in string_array(entry, "present_in") {
        assert_symbol_present(case_id, selector, &snapshot, worktrees, true);
    }
    for snapshot in string_array(entry, "absent_in") {
        assert_symbol_present(case_id, selector, &snapshot, worktrees, false);
    }
}

fn assert_symbol_present(
    case_id: &str,
    selector: &str,
    snapshot: &str,
    worktrees: &BTreeMap<String, PathBuf>,
    expected_present: bool,
) {
    let cwd = worktree_for(case_id, worktrees, snapshot);
    let document = run_graph_json(cwd, &["show", selector]);
    let present = !document.is_null();
    assert_eq!(
        present, expected_present,
        "case `{case_id}` selector `{selector}` in snapshot `{snapshot}`: expected present={expected_present}, got {document}"
    );
}

fn reference_entries(document: &Value) -> Vec<Value> {
    let mut entries = array_field(document, "refs").to_vec();
    entries.extend(array_field(document, "relations").iter().cloned());
    entries
}

fn verify_expected_references(
    case_id: &str,
    manifest: &Value,
    worktrees: &BTreeMap<String, PathBuf>,
) {
    for entry in array_field(manifest, "expected_references") {
        let target = entry["target"]
            .as_str()
            .expect("expected_references[].target");
        let snapshot = entry["snapshot"]
            .as_str()
            .expect("expected_references[].snapshot");
        let confidence = entry["confidence"]
            .as_str()
            .expect("expected_references[].confidence");
        let kind = entry["kind"].as_str().expect("expected_references[].kind");
        let file = entry["file"].as_str().expect("expected_references[].file");
        let line = entry["line"].as_u64().expect("expected_references[].line");

        let cwd = worktree_for(case_id, worktrees, snapshot);
        let document = run_graph_json(
            cwd,
            &["refs", target, "--confidence", confidence, "--kind", kind],
        );
        let found = reference_entries(&document).into_iter().any(|item| {
            item["file"].as_str() == Some(file)
                && item["line"].as_u64() == Some(line)
                && item["confidence"].as_str() == Some(confidence)
        });
        assert!(
            found,
            "case `{case_id}` missing expected reference to `{target}` at {file}:{line} ({confidence}/{kind}) in snapshot `{snapshot}`; refs returned {document}"
        );
    }
}

fn verify_expected_impact(case_id: &str, manifest: &Value, worktrees: &BTreeMap<String, PathBuf>) {
    for entry in array_field(manifest, "expected_impact") {
        let origin = entry["origin"].as_str().expect("expected_impact[].origin");
        let snapshot = entry["snapshot"]
            .as_str()
            .expect("expected_impact[].snapshot");
        let confidence = entry["confidence"]
            .as_str()
            .expect("expected_impact[].confidence");
        let qualified_name = entry["qualified_name"]
            .as_str()
            .expect("expected_impact[].qualified_name");
        let edge_kind = entry["edge_kind"]
            .as_str()
            .expect("expected_impact[].edge_kind");
        let distance = entry["distance"]
            .as_u64()
            .expect("expected_impact[].distance");

        let cwd = worktree_for(case_id, worktrees, snapshot);
        let document = run_graph_json(cwd, &["impact", origin, "--confidence", confidence]);
        let found = array_field(&document, "touched").iter().any(|item| {
            item["qualified_name"].as_str() == Some(qualified_name)
                && item["edge_kind"].as_str() == Some(edge_kind)
                && item["distance"].as_u64() == Some(distance)
        });
        assert!(
            found,
            "case `{case_id}` missing expected impact `{qualified_name}` ({edge_kind}, distance {distance}) from `{origin}` in snapshot `{snapshot}`; impact returned {document}"
        );
    }
}

fn verify_candidate_tests(case_id: &str, manifest: &Value, worktrees: &BTreeMap<String, PathBuf>) {
    for entry in array_field(manifest, "candidate_tests") {
        let category = entry["category"]
            .as_str()
            .expect("candidate_tests[].category");
        let snapshot = entry["snapshot"]
            .as_str()
            .expect("candidate_tests[].snapshot");
        let cwd = worktree_for(case_id, worktrees, snapshot);
        match category {
            "call-path" => {
                let target = entry["target"].as_str().expect("candidate_tests[].target");
                let confidence = entry["confidence"]
                    .as_str()
                    .expect("candidate_tests[].confidence");
                let kind = entry["kind"].as_str().expect("candidate_tests[].kind");
                let file = entry["file"].as_str().expect("candidate_tests[].file");
                let line = entry["line"].as_u64().expect("candidate_tests[].line");
                let document = run_graph_json(
                    cwd,
                    &["refs", target, "--confidence", confidence, "--kind", kind],
                );
                let found = reference_entries(&document).into_iter().any(|item| {
                    item["file"].as_str() == Some(file)
                        && item["line"].as_u64() == Some(line)
                        && item["confidence"].as_str() == Some(confidence)
                });
                assert!(
                    found,
                    "case `{case_id}` candidate test call-path to `{target}` at {file}:{line} not found in snapshot `{snapshot}`; refs returned {document}"
                );
            }
            "import" => {
                let test_selector = entry["test_selector"]
                    .as_str()
                    .expect("candidate_tests[].test_selector");
                let import_target_path = entry["import_target_path"]
                    .as_str()
                    .expect("candidate_tests[].import_target_path");
                let file_selector = format!("file:{}", selector_path(test_selector));
                let document = run_graph_json(cwd, &["deps", &file_selector]);
                let found = array_field(&document, "imports")
                    .iter()
                    .any(|item| item["target_path"].as_str() == Some(import_target_path));
                assert!(
                    found,
                    "case `{case_id}` candidate test import of `{import_target_path}` not found for `{test_selector}` in snapshot `{snapshot}`; deps returned {document}"
                );
            }
            "naming-heuristic" => {
                let test_selector = entry["test_selector"]
                    .as_str()
                    .expect("candidate_tests[].test_selector");
                let document = run_graph_json(cwd, &["show", test_selector]);
                assert!(
                    !document.is_null(),
                    "case `{case_id}` naming-heuristic candidate test `{test_selector}` does not exist in snapshot `{snapshot}`"
                );
            }
            other => panic!("case `{case_id}` has unknown candidate_tests category `{other}`"),
        }
    }
}

fn selector_path(selector: &str) -> &str {
    let remainder = selector.strip_prefix("symbol:").expect("symbol selector");
    remainder.split('#').next().expect("selector path")
}

fn verify_known_gaps(case_id: &str, manifest: &Value, worktrees: &BTreeMap<String, PathBuf>) {
    for entry in array_field(manifest, "known_gaps") {
        let Some(check) = entry.get("check") else {
            continue;
        };
        let check_type = check["type"].as_str().expect("known_gaps[].check.type");
        let snapshot = entry["snapshot"].as_str().expect("known_gaps[].snapshot");
        let cwd = worktree_for(case_id, worktrees, snapshot);
        match check_type {
            "absent_reference" => {
                let target = check["target"].as_str().expect("check.target");
                let file = check["file"].as_str().expect("check.file");
                let document = run_graph_json(cwd, &["refs", target, "--confidence", "fuzzy_name"]);
                let present = reference_entries(&document)
                    .into_iter()
                    .any(|item| item["file"].as_str() == Some(file));
                assert!(
                    !present,
                    "case `{case_id}` known_gaps expected no reference to `{target}` at `{file}` in snapshot `{snapshot}`, but one exists: {document}"
                );
            }
            "identical_ambiguous_refs" => {
                let target_a = check["target_a"].as_str().expect("check.target_a");
                let target_b = check["target_b"].as_str().expect("check.target_b");
                let confidence = check["confidence"].as_str().expect("check.confidence");
                let refs_a = run_graph_json(cwd, &["refs", target_a, "--confidence", confidence]);
                let refs_b = run_graph_json(cwd, &["refs", target_b, "--confidence", confidence]);
                assert_eq!(
                    refs_a["refs"], refs_b["refs"],
                    "case `{case_id}` expected identical ambiguous refs for `{target_a}` and `{target_b}` in snapshot `{snapshot}`"
                );
            }
            "unindexed_file" => {
                let path = check["path"].as_str().expect("check.path");
                let search_query = check["search_query"].as_str().expect("check.search_query");
                let file_selector = format!("file:{path}");
                let shown = run_graph_json(cwd, &["show", &file_selector]);
                assert!(
                    shown.is_null(),
                    "case `{case_id}` expected `{path}` to be unindexed in snapshot `{snapshot}`, but show returned {shown}"
                );
                let search_result = run_graph_json(cwd, &["search", search_query]);
                let matched = array_field(&search_result, "matches")
                    .iter()
                    .any(|item| item["path"].as_str() == Some(path));
                assert!(
                    !matched,
                    "case `{case_id}` expected search `{search_query}` to exclude `{path}` in snapshot `{snapshot}`; got {search_result}"
                );
            }
            other => panic!("case `{case_id}` has unknown known_gaps check type `{other}`"),
        }
    }
}

fn run_git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_git_with_env(cwd: &Path, args: &[&str], env: &[(&str, &str)]) {
    let mut command = Command::new("git");
    command.current_dir(cwd).args(args);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().expect("run git with env");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_string()
}

fn run_graph(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orbit-graph"))
        .current_dir(cwd)
        .args(["--format", "json"])
        .args(args)
        .output()
        .expect("run orbit-graph")
}

fn run_graph_json(cwd: &Path, args: &[&str]) -> Value {
    let output = run_graph(cwd, args);
    assert!(
        output.status.success(),
        "orbit-graph {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("orbit-graph JSON output")
}

//! Builder for the shared change-explorer fixture corpus.
//!
//! `crates/orbit-graph-cli/tests/fixtures/change-explorer/<case>/` holds a
//! `base` tree, a `head` tree, optionally a `root` tree, and an `expected.json`
//! manifest. The CLI crate's `tests/change_explorer_fixtures.rs` materializes
//! those cases through the `git` command line to verify the manifest against
//! the `orbit-graph` binary.
//! This builder materializes the same cases through `git2` for the explorer's
//! own tests, keeping the corpus conventions — fixed author and committer
//! identity, fixed timestamps, one commit per named snapshot, no committed
//! `.git` directory — so both harnesses observe identical commit SHAs.

#![allow(dead_code)]
#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use git2::{Repository, Signature, Time};
use serde_json::Value;
use tempfile::TempDir;

/// Author and committer identity used by the corpus, matching
/// `crates/orbit-graph-cli/tests/change_explorer_fixtures.rs`.
pub const FIXTURE_AUTHOR_NAME: &str = "Orbit Graph Fixture";
/// Author and committer email used by the corpus.
pub const FIXTURE_AUTHOR_EMAIL: &str = "fixture@orbit-graph.invalid";

/// `2023-12-31T00:00:00+00:00`, the corpus root-commit timestamp.
const ROOT_COMMIT_EPOCH: i64 = 1_703_980_800;
/// `2024-01-01T00:00:00+00:00`, the corpus base-commit timestamp.
const BASE_COMMIT_EPOCH: i64 = 1_704_067_200;
/// `2024-01-02T00:00:00+00:00`, the corpus head-commit timestamp.
const HEAD_COMMIT_EPOCH: i64 = 1_704_153_600;

/// Every case in the corpus, in manifest order.
pub const CASES: &[&str] = &[
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

/// One materialized corpus case.
pub struct CorpusCase {
    _repository: TempDir,
    /// Working tree of the built repository.
    pub repository: PathBuf,
    /// Commit SHA per named snapshot (`root`, `base`, `head`).
    pub snapshots: BTreeMap<String, String>,
    /// The case's `expected.json`, parsed.
    pub manifest: Value,
    /// The case directory name.
    pub case_id: String,
}

impl CorpusCase {
    /// Commit SHA of a named snapshot.
    pub fn sha(&self, snapshot: &str) -> &str {
        self.snapshots
            .get(snapshot)
            .unwrap_or_else(|| panic!("case `{}` has no snapshot `{snapshot}`", self.case_id))
            .as_str()
    }

    /// Base commit SHA.
    pub fn base(&self) -> &str {
        self.sha("base")
    }

    /// Head commit SHA.
    pub fn head(&self) -> &str {
        self.sha("head")
    }

    /// Manifest entries under `field`, or an empty slice.
    pub fn manifest_array(&self, field: &str) -> &[Value] {
        self.manifest
            .get(field)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

/// Root of the shared fixture corpus.
pub fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../orbit-graph-cli/tests/fixtures/change-explorer")
}

/// Build `case_id` into a fresh temporary Git repository.
pub fn build_case(case_id: &str) -> CorpusCase {
    let case_dir = fixtures_root().join(case_id);
    let manifest: Value = serde_json::from_slice(
        fs::read(case_dir.join("expected.json"))
            .expect("read expected.json")
            .as_slice(),
    )
    .expect("parse expected.json");

    let repository = TempDir::new().expect("create case repository");
    let repository_path = repository.path().to_path_buf();
    let repo = Repository::init(repository_path.as_path()).expect("init case repository");
    let mut snapshots = BTreeMap::new();

    let topology = manifest["topology"]
        .as_str()
        .expect("case manifest has a topology");
    match topology {
        "linear" => {
            snapshots.insert(
                "base".to_string(),
                commit_tree(
                    &repo,
                    repository_path.as_path(),
                    case_dir.join("base").as_path(),
                    "base",
                    BASE_COMMIT_EPOCH,
                    Parent::CurrentHead,
                    Some("HEAD"),
                ),
            );
            snapshots.insert(
                "head".to_string(),
                commit_tree(
                    &repo,
                    repository_path.as_path(),
                    case_dir.join("head").as_path(),
                    "head",
                    HEAD_COMMIT_EPOCH,
                    Parent::CurrentHead,
                    Some("HEAD"),
                ),
            );
        }
        "branch_divergence" => {
            let topology = manifest["branch_topology"]
                .as_object()
                .expect("branch_divergence case has branch_topology");
            let branch = |key: &str| -> String {
                topology[key]
                    .as_str()
                    .unwrap_or_else(|| panic!("branch_topology.{key}"))
                    .to_string()
            };
            let root_branch = branch("root_branch");
            let base_branch = branch("base_branch");
            let head_branch = branch("head_branch");

            let root_sha = commit_tree(
                &repo,
                repository_path.as_path(),
                case_dir.join("root").as_path(),
                "root",
                ROOT_COMMIT_EPOCH,
                Parent::CurrentHead,
                Some("HEAD"),
            );
            set_branch(&repo, root_branch.as_str(), root_sha.as_str());
            snapshots.insert("root".to_string(), root_sha.clone());

            // Both branch tips are written without moving `HEAD`, then anchored
            // with a branch ref, so each descends from the root commit rather
            // than from the other branch.
            let base_sha = commit_tree(
                &repo,
                repository_path.as_path(),
                case_dir.join("base").as_path(),
                "base",
                BASE_COMMIT_EPOCH,
                Parent::Commit(root_sha.clone()),
                None,
            );
            set_branch(&repo, base_branch.as_str(), base_sha.as_str());
            snapshots.insert("base".to_string(), base_sha.clone());

            let head_sha = commit_tree(
                &repo,
                repository_path.as_path(),
                case_dir.join("head").as_path(),
                "head",
                HEAD_COMMIT_EPOCH,
                Parent::Commit(root_sha.clone()),
                None,
            );
            set_branch(&repo, head_branch.as_str(), head_sha.as_str());
            snapshots.insert("head".to_string(), head_sha.clone());

            // The corpus contract: the merge base of the two branch tips is the
            // root commit, so the merge-base reading genuinely differs from the
            // direct base-to-head reading this milestone implements.
            let merge_base = repo
                .merge_base(
                    git2::Oid::from_str(base_sha.as_str()).expect("base oid"),
                    git2::Oid::from_str(head_sha.as_str()).expect("head oid"),
                )
                .expect("merge base")
                .to_string();
            assert_eq!(
                merge_base, root_sha,
                "branch-divergence merge-base must equal the root commit"
            );

            // Leave the checkout on the head branch so the working tree matches
            // a committed revision and the comparison reports it as clean.
            check_out_branch(&repo, head_branch.as_str());
        }
        other => panic!("unknown topology `{other}`"),
    }

    // Leave the working tree on the head snapshot and clean, so a comparison
    // opened against this repository reports no uncommitted changes.
    drop(repo);
    CorpusCase {
        _repository: repository,
        repository: repository_path,
        snapshots,
        manifest,
        case_id: case_id.to_string(),
    }
}

/// Which commit a new commit descends from.
enum Parent {
    /// Whatever `HEAD` currently points at, or none for the first commit.
    CurrentHead,
    /// An explicit commit, for building a divergent branch.
    Commit(String),
}

/// Replace the working tree with `source_dir` and commit it.
///
/// `update_ref` names the reference to move, or `None` to write a dangling
/// commit that a branch ref is attached to afterwards.
fn commit_tree(
    repo: &Repository,
    repository_path: &Path,
    source_dir: &Path,
    message: &str,
    epoch: i64,
    parent: Parent,
    update_ref: Option<&str>,
) -> String {
    clear_worktree(repository_path);
    copy_tree(source_dir, repository_path);

    let mut index = repo.index().expect("open case index");
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .expect("stage case tree");
    index.write().expect("write case index");
    let tree_id = index.write_tree().expect("write case tree");
    let tree = repo.find_tree(tree_id).expect("find case tree");

    let when = Time::new(epoch, 0);
    let author =
        Signature::new(FIXTURE_AUTHOR_NAME, FIXTURE_AUTHOR_EMAIL, &when).expect("signature");
    let parent = match parent {
        Parent::Commit(sha) => Some(
            repo.find_commit(git2::Oid::from_str(sha.as_str()).expect("parent oid"))
                .expect("find parent commit"),
        ),
        Parent::CurrentHead => repo
            .head()
            .ok()
            .and_then(|head| head.target())
            .map(|oid| repo.find_commit(oid).expect("find parent commit")),
    };
    let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
    repo.commit(
        update_ref,
        &author,
        &author,
        message,
        &tree,
        parents.as_slice(),
    )
    .expect("create case commit")
    .to_string()
}

/// Point `HEAD` at `name` and reset the index and working tree to it.
fn check_out_branch(repo: &Repository, name: &str) {
    let reference = format!("refs/heads/{name}");
    repo.set_head(reference.as_str()).expect("set head");
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force().remove_untracked(true);
    repo.checkout_head(Some(&mut checkout))
        .expect("check out head");
}

fn set_branch(repo: &Repository, name: &str, sha: &str) {
    let commit = repo
        .find_commit(git2::Oid::from_str(sha).expect("branch oid"))
        .expect("find branch commit");
    repo.branch(name, &commit, true).expect("create branch");
}

fn clear_worktree(repository_path: &Path) {
    for entry in fs::read_dir(repository_path).expect("read repository directory") {
        let entry = entry.expect("directory entry");
        if entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        if entry.file_type().expect("file type").is_dir() {
            fs::remove_dir_all(path.as_path()).expect("remove stale directory");
        } else {
            fs::remove_file(path.as_path()).expect("remove stale file");
        }
    }
}

fn copy_tree(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source).expect("read fixture directory") {
        let entry = entry.expect("directory entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            fs::create_dir_all(target.as_path()).expect("create directory");
            copy_tree(entry.path().as_path(), target.as_path());
        } else {
            fs::copy(entry.path(), target.as_path()).expect("copy fixture file");
        }
    }
}

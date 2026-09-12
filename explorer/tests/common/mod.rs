//! Shared deterministic Git fixtures for explorer integration tests.
//!
//! Two builders live here. [`build_fixture`] creates a two-commit throwaway
//! repository used by the snapshot tests. [`corpus`] materializes a case from
//! the shared change-explorer fixture corpus under
//! `tests/fixtures/change-explorer/`.

#![allow(dead_code)]
#![allow(clippy::expect_used)]

pub mod corpus;
pub mod http_service;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use git2::{Repository, Signature, Time};
use tempfile::TempDir;

const FIXTURE_TIME: i64 = 1_600_000_000;

/// Base revision source: a helper plus a caller.
pub const BASE_LIB: &str = "\
pub fn removed_helper() -> i32 {
    7
}

pub fn entry() -> i32 {
    removed_helper()
}
";

/// Head revision source: the helper is gone.
pub const HEAD_LIB: &str = "\
pub fn entry() -> i32 {
    7
}
";

/// A throwaway repository with two commits and fixed author/commit times, so
/// the resolved commit SHAs are identical on every machine and run.
pub struct Fixture {
    dir: TempDir,
    pub base: String,
    pub head: String,
}

impl Fixture {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

pub fn build_fixture() -> Fixture {
    let dir = TempDir::new().expect("create fixture repository");
    let repo = Repository::init(dir.path()).expect("init fixture repository");
    let base = commit_tree(
        &repo,
        dir.path(),
        BASE_LIB,
        "base: add helper and caller",
        0,
    );
    let head = commit_tree(&repo, dir.path(), HEAD_LIB, "head: remove helper", 1);
    drop(repo);
    Fixture { dir, base, head }
}

pub fn commit_tree(
    repo: &Repository,
    root: &Path,
    lib_source: &str,
    message: &str,
    offset: i64,
) -> String {
    fs::create_dir_all(root.join("src")).expect("create fixture source directory");
    fs::write(root.join("src/lib.rs"), lib_source).expect("write fixture source");

    let mut index = repo.index().expect("open fixture index");
    index
        .add_path(Path::new("src/lib.rs"))
        .expect("stage fixture source");
    index.write().expect("write fixture index");
    let tree_id = index.write_tree().expect("write fixture tree");
    let tree = repo.find_tree(tree_id).expect("find fixture tree");

    let when = Time::new(FIXTURE_TIME + offset, 0);
    let author =
        Signature::new("Fixture Author", "fixture@example.invalid", &when).expect("signature");
    let parents = match repo.head().ok().and_then(|head| head.target()) {
        Some(parent) => vec![repo.find_commit(parent).expect("find parent commit")],
        None => Vec::new(),
    };
    let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();
    let commit = repo
        .commit(
            Some("HEAD"),
            &author,
            &author,
            message,
            &tree,
            parent_refs.as_slice(),
        )
        .expect("create fixture commit");
    commit.to_string()
}

pub fn head_commit(root: &Path) -> String {
    let repo = Repository::open(root).expect("open fixture repository");
    repo.head()
        .and_then(|head| head.peel_to_commit())
        .map(|commit| commit.id().to_string())
        .expect("resolve fixture HEAD")
}

/// Byte-for-byte fingerprint of every file in the working tree, excluding Git's
/// own `.git` directory (which the snapshot module reads but never writes).
pub fn fingerprint_working_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    collect_files(root, root, &mut files);
    files
}

pub fn collect_files(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let entries = fs::read_dir(dir).expect("read fixture directory");
    for entry in entries {
        let entry = entry.expect("read fixture directory entry");
        let path = entry.path();
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let file_type = entry.file_type().expect("read fixture file type");
        if file_type.is_dir() {
            collect_files(root, path.as_path(), out);
        } else {
            let relative = path
                .strip_prefix(root)
                .expect("fixture path is inside the fixture root")
                .to_path_buf();
            let bytes = fs::read(path.as_path()).expect("read fixture file");
            out.insert(relative, bytes);
        }
    }
}

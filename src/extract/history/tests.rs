use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use super::*;

#[test]
fn real_git_trees_attribute_add_edit_delete_rename_and_nested_symbols() {
    let repo = fixture_repo();
    write(
        repo.path(),
        "src/lib.rs",
        "pub mod outer {\n    pub fn nested() -> i32 {\n        1\n    }\n}\n\npub fn removed() {}\n",
    );
    write(repo.path(), "notes.txt", "old\n");
    write(repo.path(), "removed.rs", "pub fn gone() {}\n");
    write(repo.path(), "format.rs", "pub fn spaced(){1}\n");
    write(repo.path(), "imports.rs", "use std::fmt;\n");
    write(repo.path(), "broken.rs", "???\n");
    commit(repo.path(), "before");
    let before = head(repo.path());

    write(
        repo.path(),
        "src/lib.rs",
        "pub mod outer {\n    pub fn nested() -> i32 {\n        2\n    }\n}\n\npub fn added() {}\n",
    );
    git(repo.path(), &["mv", "notes.txt", "renamed.txt"]);
    fs::remove_file(repo.path().join("removed.rs")).expect("delete fixture file");
    write(repo.path(), "added.rs", "pub fn fresh() {}\n");
    write(repo.path(), "format.rs", "pub fn spaced() { 1 }\n");
    write(repo.path(), "imports.rs", "use std::io;\n");
    write(repo.path(), "broken.rs", "!!!\n");
    write(repo.path(), "unsupported.xyz", "changed\n");
    fs::write(repo.path().join("binary.dat"), [0, 1, 2, 3]).expect("write binary");
    commit(repo.path(), "after");
    let after = head(repo.path());

    let repository = Repository::open(repo.path()).expect("open repository");
    let delivery = DeliveryImport {
        schema_version: DELIVERY_IMPORT_SCHEMA_VERSION,
        repository: repository_identity(&repository).expect("identity"),
        landing_branch: "main".into(),
        before_revision: before,
        after_revision: after,
        delivery_id: "delivery-1".into(),
        evidence: DeliveryEvidence::VerifiedDelivery,
        source: Provenance {
            system: "test".into(),
            record_id: Some("1".into()),
        },
        captured_at: "2026-09-07T00:00:00Z".into(),
        tasks: vec![],
    };
    let change = extract_delivery(&repository, delivery).expect("extract delivery");
    let rust = change
        .files
        .iter()
        .find(|file| file.new_path.as_deref() == Some("src/lib.rs"))
        .expect("rust change");
    assert!(
        rust.symbols.iter().any(|symbol| {
            symbol
                .after
                .as_ref()
                .is_some_and(|item| item.symbol.name == "nested")
        }),
        "smallest nested symbol should receive its changed line: {rust:?}"
    );
    assert!(
        !rust.symbols.iter().any(|symbol| {
            symbol
                .after
                .as_ref()
                .is_some_and(|item| item.symbol.name == "outer" && item.changed_lines.contains(&3))
        }),
        "parent must not be implicated for a child line"
    );
    assert!(rust.symbols.iter().any(|symbol| {
        !symbol.live_after
            && symbol
                .before
                .as_ref()
                .is_some_and(|item| item.symbol.name == "removed")
    }));
    assert!(rust.symbols.iter().any(|symbol| {
        symbol.live_after
            && symbol
                .after
                .as_ref()
                .is_some_and(|item| item.symbol.name == "added")
    }));
    assert!(
        change
            .files
            .iter()
            .any(|file| file.kind == FileChangeKind::Renamed)
    );
    assert!(
        change
            .files
            .iter()
            .any(|file| file.kind == FileChangeKind::Added
                && file.new_path.as_deref() == Some("added.rs"))
    );
    assert!(
        change
            .files
            .iter()
            .any(|file| file.kind == FileChangeKind::Deleted
                && file.old_path.as_deref() == Some("removed.rs"))
    );
    assert!(
        change
            .files
            .iter()
            .any(|file| file.new_path.as_deref() == Some("format.rs") && !file.symbols.is_empty())
    );
    assert!(
        change
            .files
            .iter()
            .any(|file| file.new_path.as_deref() == Some("unsupported.xyz")
                && file.after_fallback == Some(FileFallbackReason::UnsupportedLanguage))
    );
    assert!(
        change
            .files
            .iter()
            .any(|file| file.new_path.as_deref() == Some("binary.dat")
                && file.after_fallback == Some(FileFallbackReason::Binary))
    );
    assert!(change.files.iter().any(|file| {
        file.new_path.as_deref() == Some("imports.rs")
            && file.before_fallback == Some(FileFallbackReason::NoEnclosingNamedSymbol)
            && file.after_fallback == Some(FileFallbackReason::NoEnclosingNamedSymbol)
    }));
    assert!(change.files.iter().any(|file| {
        file.new_path.as_deref() == Some("broken.rs")
            && file.before_fallback == Some(FileFallbackReason::ParseOrExtractionUncertain)
            && file.after_fallback == Some(FileFallbackReason::ParseOrExtractionUncertain)
    }));
}

#[test]
fn real_git_trees_keep_structural_similarity_and_ambiguity_explicit() {
    let repo = fixture_repo();
    write(repo.path(), "moved.rs", "pub fn move_me() -> i32 { 1 }\n");
    write(
        repo.path(),
        "ambiguous.rs",
        "pub fn duplicate() -> i32 { 1 }\n",
    );
    write(
        repo.path(),
        "Overloads.java",
        "class Overloads {\n    int calculate(int value) { return 1; }\n}\n",
    );
    commit(repo.path(), "before");
    let before = head(repo.path());
    write(
        repo.path(),
        "moved.rs",
        "pub mod new_home {\n    pub fn move_me() -> i32 { 2 }\n}\n",
    );
    write(
        repo.path(),
        "ambiguous.rs",
        "pub mod one { pub fn duplicate() -> i32 { 2 } }\npub mod two { pub fn duplicate() -> i32 { 2 } }\n",
    );
    write(
        repo.path(),
        "Overloads.java",
        "class Overloads {\n    long calculate(int value) { return 2; }\n    int calculate(long value) { return 3; }\n}\n",
    );
    commit(repo.path(), "after");
    let after = head(repo.path());
    let repository = Repository::open(repo.path()).expect("open repository");
    let change = extract_delivery(
        &repository,
        DeliveryImport {
            schema_version: DELIVERY_IMPORT_SCHEMA_VERSION,
            repository: repository_identity(&repository).expect("identity"),
            landing_branch: "main".into(),
            before_revision: before,
            after_revision: after,
            delivery_id: "matching".into(),
            evidence: DeliveryEvidence::VerifiedDelivery,
            source: Provenance {
                system: "test".into(),
                record_id: None,
            },
            captured_at: "2026-09-07T00:00:00Z".into(),
            tasks: vec![],
        },
    )
    .expect("extract");
    let moved = change
        .files
        .iter()
        .find(|file| file.new_path.as_deref() == Some("moved.rs"))
        .expect("moved file");
    assert!(
        moved
            .symbols
            .iter()
            .any(|symbol| symbol.match_confidence == SymbolMatchConfidence::Similar)
    );
    let ambiguous = change
        .files
        .iter()
        .find(|file| file.new_path.as_deref() == Some("ambiguous.rs"))
        .expect("ambiguous file");
    assert!(ambiguous.symbols.iter().any(|symbol| {
        symbol.match_confidence == SymbolMatchConfidence::Uncertain && symbol.after.is_none()
    }));
    let overloads = change
        .files
        .iter()
        .find(|file| file.new_path.as_deref() == Some("Overloads.java"))
        .expect("overload file");
    assert!(
        overloads.symbols.iter().any(|symbol| {
            symbol
                .before
                .as_ref()
                .is_some_and(|item| item.symbol.name == "calculate")
                && symbol.match_confidence == SymbolMatchConfidence::Uncertain
                && symbol.after.is_none()
        }),
        "overload ambiguity should remain unpaired: {overloads:#?}"
    );
}

fn fixture_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    git(dir.path(), &["init", "-b", "main"]);
    git(
        dir.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    git(dir.path(), &["config", "user.name", "Test"]);
    dir
}

fn write(root: &Path, path: &str, content: &str) {
    let path = root.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, content).expect("write fixture");
}

fn commit(root: &Path, message: &str) {
    git(root, &["add", "-A"]);
    git(root, &["commit", "-m", message]);
}

fn head(root: &Path) -> String {
    String::from_utf8(git_output(root, &["rev-parse", "HEAD"]))
        .expect("utf8")
        .trim()
        .into()
}

fn git(root: &Path, args: &[&str]) {
    let _ = git_output(root, args);
}

fn git_output(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

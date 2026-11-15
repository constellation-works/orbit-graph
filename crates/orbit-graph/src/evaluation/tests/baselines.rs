use std::fs;
use std::path::Path;

use git2::{Oid, Repository};
use tempfile::TempDir;

use crate::RecommendationLevel;
use crate::evaluation::baselines::{Bm25Index, CodeTree, Mentions, explicit_mentions};
use crate::tests::support::{git_command, init_fixture_repository};

fn fixture() -> (TempDir, Repository, Oid) {
    let dir = TempDir::new().expect("fixture directory");
    let root = dir.path();
    let repo = init_fixture_repository(root, "main");
    for (path, text) in [
        (
            "src/lease.rs",
            "/// Renew a worker lease.\npub fn renew_lease() -> bool { lease_store_renew() }\n\npub fn lease_store_renew() -> bool { true }\n",
        ),
        (
            "src/parser.rs",
            "pub struct TokenStream;\n\nimpl TokenStream {\n    pub fn next_token(&self) -> u8 { 0 }\n}\n\npub fn parse() -> u8 { 1 }\n",
        ),
        ("tests/parser.rs", "fn parse_round_trip() {}\n"),
        ("docs/notes.md", "Release notes about parsing.\n"),
        ("Cargo.lock", "parse lease renew\n"),
    ] {
        let file = root.join(path);
        fs::create_dir_all(file.parent().expect("parent")).expect("create directory");
        fs::write(file, text).expect("write fixture file");
    }
    fs::write(root.join("blob.bin"), [0u8, 1, 2, 3]).expect("write binary");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "fixture"]);
    let head = repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .expect("fixture head")
        .id();
    (dir, repo, head)
}

fn git(root: &Path, args: &[&str]) {
    let output = git_command(root).args(args).output().expect("run git");
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn code_tree_lists_live_files_and_extracted_symbols() {
    let (_dir, repo, head) = fixture();
    let tree = CodeTree::load(&repo, head).expect("load tree");
    assert_eq!(tree.revision(), head);
    assert!(tree.is_live("file:src/lease.rs"));
    assert!(!tree.is_live("file:src/missing.rs"));
    assert!(tree.is_live("symbol:src/lease.rs#renew_lease:function"));
    assert!(!tree.is_live("symbol:src/lease.rs#renew_lease:struct"));
    assert!(!tree.is_live("symbol:src/lease.rs#gone:function"));
}

#[test]
fn bm25_ranks_files_and_symbols_by_task_text_and_skips_non_documents() {
    let (_dir, repo, head) = fixture();
    let tree = CodeTree::load(&repo, head).expect("load tree");
    let index = Bm25Index::build(&repo, &tree).expect("build index");
    // Every blob but the lockfile and the binary one is a file document.
    assert_eq!(index.file_documents, 4);
    assert_eq!(index.skipped.get("excluded_path"), Some(&1));
    assert_eq!(index.skipped.get("binary"), Some(&1));
    assert!(index.symbol_documents >= 5, "{}", index.symbol_documents);

    let files = index
        .rank(RecommendationLevel::File, "Renew the worker lease", 10)
        .expect("rank files");
    assert_eq!(files.first().map(String::as_str), Some("file:src/lease.rs"));
    assert!(!files.iter().any(|selector| selector == "file:Cargo.lock"));

    // Identifier wholes are terms on both sides: `lease_store_renew` names
    // one symbol's body and signature more often than any other.
    let symbols = index
        .rank(RecommendationLevel::Symbol, "fix lease_store_renew", 10)
        .expect("rank symbols");
    assert_eq!(
        symbols.first().map(String::as_str),
        Some("symbol:src/lease.rs#lease_store_renew:function")
    );
    assert!(
        symbols
            .iter()
            .all(|selector| selector.starts_with("symbol:"))
    );

    assert!(
        index
            .rank(RecommendationLevel::File, "the of and", 10)
            .expect("stopword query")
            .is_empty(),
        "a query with no terms ranks nothing"
    );
    let limited = index
        .rank(RecommendationLevel::File, "parse parser lease", 1)
        .expect("limited query");
    assert_eq!(limited.len(), 1);
}

#[test]
fn explicit_mentions_resolve_written_paths_and_code_shaped_names_only() {
    let (_dir, repo, head) = fixture();
    let tree = CodeTree::load(&repo, head).expect("load tree");
    let mentions = explicit_mentions(
        "Fix `renew_lease` in src/lease.rs:12 and TokenStream::next_token; see lease.rs, \
         parser.rs and docs/missing.md. Parse the stream and renew.",
        &tree,
    );
    assert_eq!(
        mentions,
        Mentions {
            // renew_lease's file first: it was written first.
            files: vec![
                "file:src/lease.rs".to_string(),
                "file:src/parser.rs".to_string()
            ],
            symbols: vec![
                "symbol:src/lease.rs#renew_lease:function".to_string(),
                "symbol:src/parser.rs#<TokenStream>::next_token:method".to_string(),
            ],
        }
    );
    // `parser.rs` is ambiguous (src/ and tests/), `docs/missing.md` does not
    // exist, and plain words (`Parse`, `stream`, `renew`) never name a symbol.
    assert_eq!(
        mentions.level(RecommendationLevel::File).len(),
        mentions.files.len()
    );

    let selectors = explicit_mentions(
        "Touch symbol:src/parser.rs#parse:function and file:tests/parser.rs.",
        &tree,
    );
    assert_eq!(
        selectors.files,
        vec![
            "file:src/parser.rs".to_string(),
            "file:tests/parser.rs".to_string()
        ]
    );
    assert_eq!(
        selectors.symbols,
        vec!["symbol:src/parser.rs#parse:function".to_string()]
    );

    assert_eq!(
        explicit_mentions("Rename the lease module", &tree),
        Mentions::default()
    );
}

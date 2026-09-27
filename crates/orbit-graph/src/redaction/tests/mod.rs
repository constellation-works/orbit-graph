use super::redact;

// SQL writers in the two persistence modules are deliberately enumerated.
// New INSERT/UPDATE sites change this inventory and require a decision on
// whether their values are free text or structured metadata (STD-05 §R13).
fn sql_targets(source: &str, needle: &str) -> Vec<String> {
    source
        .match_indices(needle)
        .map(|(at, _)| {
            source[at + needle.len()..]
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                .collect()
        })
        .collect()
}

#[test]
fn persisted_text_writer_inventory_is_explicit() {
    let graph = include_str!("../../sync/pass1.rs");
    let history = include_str!("../../store/history.rs");
    let report = include_str!("../../../../orbit-graph-changes/src/report.rs");
    // `strings` and `strings_fts` contain arbitrary extracted values;
    // their shared `value` passes through redact before either insert.
    // Symbol names and qualified names are identifiers. `signature` is free
    // text (it keeps default-argument literals) and is redacted before both
    // `symbols` and `symbols_fts` (STD-05 §R13).
    assert_eq!(
        sql_targets(graph, "INSERT INTO "),
        [
            "files",
            "symbols",
            "symbols_fts",
            "imports",
            "relations",
            "strings",
            "strings_fts",
            "configs",
            "configs_fts",
            "commands"
        ]
    );
    // Both `commands` updates store a symbol id or clear it. Command names
    // and handler ids are structured metadata, not free text (STD-05 §R13).
    assert_eq!(
        sql_targets(graph, "UPDATE "),
        ["commands", "symbols", "commands"]
    );
    assert!(graph.contains("let value = crate::redaction::redact(&string.value)"));
    assert!(
        graph.contains("crate::redaction::redact(signature)"),
        "symbol signatures must pass through the shared redactor before SQL insert (STD-05 §R13)"
    );

    // Delivery payloads, task columns and caller snapshots carry task prose.
    // Duplicate imports write only snapshots redacted before insert_delivery.
    // Symbol signatures are free text and are redacted with that payload.
    // Scope, path and cursor rows stay structured metadata. The history_meta
    // inserts move a numeric contract version to a credential-free scope key
    // and record that old SQLite pages still need purging.
    assert_eq!(
        sql_targets(history, "INSERT INTO "),
        [
            "history_scopes",
            "history_meta",
            "history_meta",
            "history_scopes",
            "history_supplied_task_snapshots",
            "history_supplied_task_snapshots",
            "history_deliveries",
            "history_tasks",
            "history_supplied_task_snapshots",
            "history_files",
            "history_path_lineage",
            "history_symbols",
            "history_scopes",
            "history_scopes"
        ]
    );
    assert_eq!(
        sql_targets(history, "UPDATE "),
        [
            "SET",
            "history_deliveries",
            "history_deliveries",
            "SET",
            "SET"
        ]
    );
    assert!(history.contains("let change = &redacted;"));
    assert!(history.contains("redact_task(&mut snapshot)"));
    assert!(
        history.contains("redact_symbol_signatures(&mut redacted)"),
        "history symbol signatures must be redacted before payload and column writes (STD-05 §R13)"
    );
    let recommend = include_str!("../../recommend.rs");
    assert!(
        recommend.contains("redact_cached_signatures(&mut symbols)"),
        "target-symbol cache signatures must be redacted before the cache is written (STD-05 §R13)"
    );

    // One declaration plus two excerpt constructors, both redacted.
    assert_eq!(report.matches("SourceExcerpt {").count(), 3);
    assert_eq!(report.matches("orbit_graph::redaction::redact(").count(), 2);
}

#[test]
fn masks_credentials_and_preserves_identifiers() {
    assert_eq!(
        redact("sk-learn-utils task-ORB-1 risk-assessment"),
        "sk-learn-utils task-ORB-1 risk-assessment"
    );
    assert_eq!(
        redact("GITHUB_TOKEN=ghp_12345678901234567890"),
        "GITHUB_TOKEN=[REDACTED_SECRET]"
    );
    assert_eq!(
        redact("https://deploy:password@gitlab.example/repo"),
        "https://deploy:[REDACTED_SECRET]@gitlab.example/repo"
    );
    assert_eq!(
        redact("Authorization: Bearer abc.def.ghi"),
        "Authorization: Bearer [REDACTED_SECRET]"
    );
    assert_eq!(
        redact("xghp_12345678901234567890"),
        "xghp_12345678901234567890"
    );
    assert_eq!(
        redact("-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----"),
        "[REDACTED_SECRET]"
    );
    for secret in [
        "gho_12345678901234567890",
        "github_pat_12345678901234567890",
        "glpat-12345678901234567890",
        "sk-123456789012345678901234",
        "sk-proj-123456789012345678901234",
        "xoxb-12345678901234567890",
        "xoxp-12345678901234567890",
        "AKIAABCDEFGHIJKLMNOP",
    ] {
        assert_eq!(redact(secret), "[REDACTED_SECRET]", "{secret}");
    }
}

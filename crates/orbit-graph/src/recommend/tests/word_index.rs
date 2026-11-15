use super::*;
use crate::recommend::word_index::{self, ContextRecord, Leaf, WindowTask, association, terms};

/// A window task with `words` as its terms and `files` as its file label.
fn task(index: usize, words: &[&str], files: &[&str]) -> WindowTask {
    WindowTask {
        task_id: format!("T-{index}"),
        delivery_id: format!("D-{index}"),
        terms: words.iter().map(|word| (*word).to_string()).collect(),
        files: files
            .iter()
            .map(|file| (format!("file:{file}"), ContextRecord::default()))
            .collect(),
        leaves: files
            .iter()
            .map(|file| (format!("file:{file}"), Leaf::default()))
            .collect(),
        ..WindowTask::default()
    }
}

fn open_options() -> WordIndexOptions {
    WordIndexOptions {
        max_term_share: 1.0,
        symbol_boost: 1.0,
        ..WordIndexOptions::default()
    }
}

fn score_of(scored: &BTreeMap<String, Accumulator>, selector: &str) -> f64 {
    scored.get(selector).map_or(0.0, |entry| entry.score)
}

#[test]
fn defaults_follow_the_design() {
    let options = WordIndexOptions::default();
    assert_eq!(options.text_fields, WordIndexTextFields::Title);
    assert_eq!(options.window_size, 4_000);
    assert_eq!(options.symbol_boost, 1.5);
    assert_eq!(options.min_support, 2);
    assert_eq!(options.smoothing, 1.0);
    assert_eq!(options.max_term_share, 0.3);
    let parsed: WordIndexOptions = serde_json::from_str("{}").expect("empty options");
    assert_eq!(parsed, options);
    assert!(serde_json::from_str::<WordIndexOptions>(r#"{"tags": true}"#).is_err());
}

#[test]
fn association_is_smoothed_positive_pmi() {
    // The design's worked examples (§5.1), in a 4,000-task window.
    let rare = association(18.0, 40.0 * 25.0 / 4_000.0, 1.0);
    assert!((rare - (19.0_f64 / 1.25).ln()).abs() < 1e-12);
    let hot = association(4.0, 40.0 * 280.0 / 4_000.0, 1.0);
    assert!((hot - (5.0_f64 / 3.8).ln()).abs() < 1e-12);
    // One co-occurrence scores at most ln 2, and fewer than expected scores 0.
    assert!(association(1.0, 0.0, 1.0) <= 2.0_f64.ln() + 1e-12);
    assert_eq!(association(1.0, 5.0, 1.0), 0.0);
}

#[test]
fn a_rare_word_outranks_a_common_one() {
    let window = vec![
        task(0, &["lease"], &["src/lease.rs"]),
        task(1, &["lease"], &["src/lease.rs"]),
        task(2, &["cache"], &["src/cache.rs"]),
        task(3, &["cache"], &["src/cache.rs"]),
        task(4, &["cache"], &["src/other.rs"]),
        task(5, &["cache"], &["src/other.rs"]),
        task(6, &["cache"], &["src/more.rs"]),
        task(7, &["cache"], &["src/more.rs"]),
    ];
    // Both files carry a two-task posting from one query word and are touched
    // by two tasks; only the words' document frequencies differ.
    let outcome = word_index::score(
        &window,
        BTreeMap::new(),
        "lease cache",
        RecommendationLevel::File,
        &open_options(),
    );
    let rare = score_of(&outcome.scored, "file:src/lease.rs");
    let common = score_of(&outcome.scored, "file:src/cache.rs");
    assert!((rare - (3.0_f64 / 1.5).ln()).abs() < 1e-12, "{rare}");
    assert!((common - (3.0_f64 / 2.5).ln()).abs() < 1e-12, "{common}");
    assert!(rare > common);
    let ranked = finalize(outcome.scored, window.len());
    assert_eq!(ranked[0].selector, "file:src/lease.rs");
    assert_eq!(ranked[0].reasons[0].kind, "word_index_term");
    assert_eq!(outcome.summary.empty_reason, None);
}

#[test]
fn a_context_touched_by_every_delivery_is_discounted() {
    let window = vec![
        task(0, &["parser"], &["src/parser.rs", "src/hot.rs"]),
        task(1, &["parser"], &["src/parser.rs", "src/hot.rs"]),
        task(2, &["parser"], &["src/parser.rs", "src/hot.rs"]),
        task(3, &["cache"], &["src/cache.rs", "src/hot.rs"]),
        task(4, &["cache"], &["src/cache.rs", "src/hot.rs"]),
        task(5, &["cache"], &["src/cache.rs", "src/hot.rs"]),
    ];
    let outcome = word_index::score(
        &window,
        BTreeMap::new(),
        "parser",
        RecommendationLevel::File,
        &open_options(),
    );
    // The hot file shares the parser file's three-task posting but every task
    // touched it, so the word says nothing about it beyond its base rate.
    assert!(score_of(&outcome.scored, "file:src/parser.rs") > 0.0);
    assert_eq!(score_of(&outcome.scored, "file:src/hot.rs"), 0.0);
    assert!(!outcome.scored.contains_key("file:src/cache.rs"));
}

#[test]
fn postings_below_min_support_and_window_stopwords_score_nothing() {
    let window = (0..10)
        .map(|index| {
            if index == 0 {
                task(index, &["fix", "lease"], &["src/lease.rs"])
            } else {
                task(index, &["fix"], &["src/other.rs"])
            }
        })
        .collect::<Vec<_>>();
    let outcome = word_index::score(
        &window,
        BTreeMap::new(),
        "fix lease",
        RecommendationLevel::File,
        &WordIndexOptions::default(),
    );
    assert_eq!(outcome.summary.window_stopwords, vec!["fix".to_string()]);
    assert_eq!(outcome.summary.known_terms, vec!["lease".to_string()]);
    assert!(outcome.scored.is_empty());
    assert_eq!(
        outcome.summary.empty_reason.as_deref(),
        Some("below_min_support")
    );

    let unknown = word_index::score(
        &window,
        BTreeMap::new(),
        "zebra",
        RecommendationLevel::File,
        &WordIndexOptions::default(),
    );
    assert_eq!(
        unknown.summary.empty_reason.as_deref(),
        Some("no_known_terms")
    );
    let none = word_index::score(
        &[],
        BTreeMap::new(),
        "lease",
        RecommendationLevel::File,
        &WordIndexOptions::default(),
    );
    assert_eq!(
        none.summary.empty_reason.as_deref(),
        Some("no_window_tasks")
    );
}

#[test]
fn the_symbol_boost_adds_the_best_symbol_score_to_its_file() {
    let mut window = Vec::new();
    for index in 0..4 {
        let mut entry = task(index, &["parser"], &["src/parser.rs"]);
        entry.leaves = BTreeMap::from([(
            "symbol:src/parser.rs#parse:function".to_string(),
            Leaf::default(),
        )]);
        window.push(entry);
    }
    for index in 4..8 {
        window.push(task(index, &["cache"], &["src/cache.rs"]));
    }
    let options = WordIndexOptions {
        symbol_boost: 1.5,
        ..open_options()
    };
    let boosted = word_index::score(
        &window,
        BTreeMap::new(),
        "parser",
        RecommendationLevel::File,
        &options,
    );
    let plain = word_index::score(
        &window,
        BTreeMap::new(),
        "parser",
        RecommendationLevel::File,
        &open_options(),
    );
    let file = score_of(&plain.scored, "file:src/parser.rs");
    assert!(file > 0.0);
    // The symbol has the file's posting and base rate, so its score equals the
    // file's, and b = 1.5 adds half of it.
    assert!((score_of(&boosted.scored, "file:src/parser.rs") - 1.5 * file).abs() < 1e-12);
    assert!(
        boosted.scored["file:src/parser.rs"]
            .reasons
            .iter()
            .any(|reason| reason.kind == "word_index_symbol_boost")
    );
    let symbols = word_index::score(
        &window,
        BTreeMap::new(),
        "parser",
        RecommendationLevel::Symbol,
        &options,
    );
    assert!(
        (score_of(&symbols.scored, "symbol:src/parser.rs#parse:function") - file).abs() < 1e-12
    );
    assert!(!symbols.scored.contains_key("file:src/parser.rs"));
}

#[test]
fn tokeniser_keeps_identifier_parts_and_the_whole() {
    let set = |text: &str| terms(text).into_iter().collect::<Vec<_>>();
    assert_eq!(set("context_files"), ["context", "context_file", "file"]);
    assert_eq!(
        set("widenContextFiles"),
        ["context", "file", "widen", "widencontextfile"]
    );
    assert_eq!(
        set("crates/orbit-core/src/application/task/paths.rs"),
        [
            "application",
            "core",
            "crate",
            "orbit",
            "path",
            "src",
            "task"
        ]
    );
    assert_eq!(set("HTTPServer"), ["http", "httpserver", "server"]);
    // URLs, task IDs, hex runs, bare numbers, stopwords and one-letter terms go.
    assert_eq!(
        set("Fix the leases for ORB-12345 at https://example.com/x 4844e721 in 42 x"),
        ["fix", "lease"]
    );
    // Plural folding leaves -ss, -us and -is endings alone.
    assert_eq!(
        set("class status analysis"),
        ["analysis", "class", "status"]
    );
    // A known extension is dropped from a bare file name.
    assert_eq!(set("recommend.rs"), ["recommend"]);
}

#[test]
fn options_are_validated_and_belong_to_the_word_index_variant() {
    for options in [
        WordIndexOptions {
            window_size: 0,
            ..WordIndexOptions::default()
        },
        WordIndexOptions {
            symbol_boost: 0.5,
            ..WordIndexOptions::default()
        },
        WordIndexOptions {
            min_support: 0,
            ..WordIndexOptions::default()
        },
        WordIndexOptions {
            smoothing: 0.0,
            ..WordIndexOptions::default()
        },
        WordIndexOptions {
            max_term_share: 1.5,
            ..WordIndexOptions::default()
        },
    ] {
        assert!(options.validate().is_err(), "{options:?}");
    }
    let mut request = query_request("parser", "HEAD", None);
    request.word_index = Some(WordIndexOptions::default());
    let error = validate_request(&request).expect_err("options need the variant");
    assert!(error.to_string().contains("word_index"), "{error}");
    request.variant = RecommendationVariant::WordIndex;
    validate_request(&request).expect("valid word-index request");
}

/// Import one verified delivery whose single task has `title` and
/// `description`, landed at `unix:<seconds>`.
fn import_task(
    index: &HistoryIndex,
    boundary: (&str, &str),
    delivery_id: &str,
    title: &str,
    description: &str,
    seconds: i64,
) {
    let source = Provenance {
        system: "test".to_string(),
        record_id: Some(delivery_id.to_string()),
    };
    let fact = |value: i64| TemporalFact {
        status: TemporalStatus::Known,
        timestamp: Some(format!("unix:{value}")),
        source: source.clone(),
    };
    index
        .import(DeliveryImport {
            schema_version: crate::DELIVERY_IMPORT_SCHEMA_VERSION,
            repository: index.repository().to_string(),
            landing_branch: "main".to_string(),
            before_revision: boundary.0.to_string(),
            after_revision: boundary.1.to_string(),
            delivery_id: delivery_id.to_string(),
            evidence: DeliveryEvidence::VerifiedDelivery,
            source: source.clone(),
            delivered_at: fact(seconds),
            captured_at: format!("unix:{}", seconds + 1),
            tasks: vec![TaskAssociation {
                task_id: format!("TASK-{delivery_id}"),
                title: title.to_string(),
                description: description.to_string(),
                acceptance_criteria: Vec::new(),
                source: source.clone(),
                created_at: fact(1),
                snapshot_available_at: fact(2),
                text_availability: TaskTextAvailability::KnownPreExecution,
                captured_at: "unix:3".to_string(),
            }],
        })
        .expect("import delivery");
}

/// Four deliveries before `unix:50` (two parser, two cache, all touching
/// `src/common.rs`; the parser ones describe a "zebra ledger") and one after
/// it whose title says "tokenizer" and which touches `src/late.rs`.
fn word_index_fixture() -> (TempDir, String) {
    let fixture = fixture_repo();
    let root = fixture.path();
    for file in ["parser", "cache", "common"] {
        fs::write(
            root.join(format!("src/{file}.rs")),
            format!("pub fn {file}() -> i32 {{ 0 }}\n"),
        )
        .expect("seed file");
    }
    commit_all(root, "base");
    let mut previous = head(root);
    let index = HistoryIndex::open(root, "main").expect("history");
    let deliveries = [
        ("P1", "src/parser.rs", "Parser tokenizer repair", 10),
        ("P2", "src/parser.rs", "Speed up the parser tokenizer", 20),
        ("C1", "src/cache.rs", "Cache eviction fix", 30),
        ("C2", "src/cache.rs", "Rewrite cache eviction", 40),
        ("LATE", "src/late.rs", "Tokenizer for late files", 60),
    ];
    for (step, (id, file, title, seconds)) in deliveries.into_iter().enumerate() {
        fs::write(
            root.join(file),
            format!("pub fn changed_{step}() -> i32 {{ {step} }}\n"),
        )
        .expect("change file");
        fs::write(
            root.join("src/common.rs"),
            format!("pub fn common() -> i32 {{ {step} }}\n"),
        )
        .expect("change common");
        commit_all(root, id);
        let after = head(root);
        let description = if id.starts_with('P') {
            "Touches the zebra ledger"
        } else {
            "Ordinary work"
        };
        import_task(
            &index,
            (previous.as_str(), after.as_str()),
            id,
            title,
            description,
            seconds,
        );
        previous = after;
    }
    (fixture, previous)
}

fn word_index_request(
    query: &str,
    target: &str,
    options: WordIndexOptions,
) -> RecommendationRequest {
    RecommendationRequest {
        variant: RecommendationVariant::WordIndex,
        word_index: Some(options),
        ..query_request(query, target, Some("unix:50"))
    }
}

#[test]
fn a_delivery_after_the_cutoff_contributes_nothing() {
    let (fixture, target) = word_index_fixture();
    let engine = RecommendationEngine::open(fixture.path(), "main").expect("engine");
    let options = WordIndexOptions {
        max_term_share: 1.0,
        ..WordIndexOptions::default()
    };
    let result = engine
        .recommend(&word_index_request("tokenizer", target.as_str(), options))
        .expect("recommend");
    let summary = result.word_index.as_ref().expect("summary");
    assert_eq!(summary.window_tasks, 4, "{summary:?}");
    assert_eq!(result.variant, RecommendationVariant::WordIndex);
    assert!(!result.structure_applied);
    assert_eq!(result.recommendations[0].selector, "file:src/parser.rs");
    assert!(result.recommendations.iter().all(|item| {
        item.selector != "file:src/late.rs"
            && !item.supporting_delivery_ids.contains(&"LATE".to_string())
    }));
    // The file every delivery touched is discounted to nothing.
    assert!(
        result
            .recommendations
            .iter()
            .all(|item| item.selector != "file:src/common.rs")
    );

    // Without the cutoff the late delivery is in the window.
    let live = engine
        .recommend(&RecommendationRequest {
            cutoff: None,
            ..word_index_request("tokenizer", target.as_str(), options)
        })
        .expect("live recommend");
    assert_eq!(live.word_index.as_ref().expect("summary").window_tasks, 5);
}

#[test]
fn description_words_are_ignored_under_the_default_text_source() {
    let (fixture, target) = word_index_fixture();
    let engine = RecommendationEngine::open(fixture.path(), "main").expect("engine");
    let title_only = WordIndexOptions {
        max_term_share: 1.0,
        ..WordIndexOptions::default()
    };
    let result = engine
        .recommend(&word_index_request(
            "zebra ledger",
            target.as_str(),
            title_only,
        ))
        .expect("recommend");
    assert!(result.recommendations.is_empty());
    let summary = result.word_index.as_ref().expect("summary");
    assert_eq!(summary.empty_reason.as_deref(), Some("no_known_terms"));
    assert!(
        result
            .fallbacks
            .iter()
            .any(|fallback| fallback.kind == "word_index_empty"
                && fallback.reason.starts_with("no_known_terms"))
    );

    let wider = engine
        .recommend(&word_index_request(
            "zebra ledger",
            target.as_str(),
            WordIndexOptions {
                text_fields: WordIndexTextFields::TitleDescriptionCriteria,
                ..title_only
            },
        ))
        .expect("recommend wider");
    assert_eq!(wider.recommendations[0].selector, "file:src/parser.rs");
}

#[test]
fn a_task_query_reads_only_the_title_by_default() {
    let (fixture, target) = word_index_fixture();
    let engine = RecommendationEngine::open(fixture.path(), "main").expect("engine");
    let snapshot = TaskAssociation {
        task_id: "NEW-1".to_string(),
        title: "Cache eviction regression".to_string(),
        description: "parser tokenizer parser tokenizer".to_string(),
        acceptance_criteria: vec!["parser tokenizer".to_string()],
        source: Provenance {
            system: "test".to_string(),
            record_id: None,
        },
        created_at: TemporalFact {
            status: TemporalStatus::Known,
            timestamp: Some("unix:45".to_string()),
            source: Provenance {
                system: "test".to_string(),
                record_id: None,
            },
        },
        snapshot_available_at: TemporalFact {
            status: TemporalStatus::Known,
            timestamp: Some("unix:46".to_string()),
            source: Provenance {
                system: "test".to_string(),
                record_id: None,
            },
        },
        text_availability: TaskTextAvailability::KnownPreExecution,
        captured_at: "unix:46".to_string(),
    };
    let options = WordIndexOptions {
        max_term_share: 1.0,
        ..WordIndexOptions::default()
    };
    let request = |options: WordIndexOptions| RecommendationRequest {
        input: RecommendationInput::TaskId("NEW-1".to_string()),
        task_snapshot: Some(snapshot.clone()),
        ..word_index_request("unused", target.as_str(), options)
    };
    let title = engine.recommend(&request(options)).expect("title query");
    let summary = title.word_index.as_ref().expect("summary");
    assert!(!summary.query_terms.contains(&"parser".to_string()));
    assert_eq!(title.recommendations[0].selector, "file:src/cache.rs");
    assert!(
        title
            .recommendations
            .iter()
            .all(|item| item.selector != "file:src/parser.rs")
    );

    let wider = engine
        .recommend(&request(WordIndexOptions {
            text_fields: WordIndexTextFields::TitleDescriptionCriteria,
            ..options
        }))
        .expect("wider query");
    assert!(
        wider
            .recommendations
            .iter()
            .any(|item| item.selector == "file:src/parser.rs")
    );
}

#[test]
fn term_sequence_keeps_order_and_repeats_of_the_same_terms() {
    let text = "Renew leases; renew the lease_store. See ORB-12 and src/lease.rs";
    let sequence = word_index::term_sequence(text);
    assert_eq!(
        sequence,
        [
            "renew",
            "lease",
            "renew",
            "lease",
            "store",
            "lease_store",
            "see",
            "src",
            // A path gives its parts and then its stem.
            "lease",
            "lease"
        ]
    );
    assert_eq!(
        sequence.into_iter().collect::<BTreeSet<_>>(),
        terms(text),
        "the sequence holds exactly the tokeniser's terms"
    );
}

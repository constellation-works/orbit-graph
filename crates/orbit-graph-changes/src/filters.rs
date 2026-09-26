//! Presentation filters and the explanation every filtered response carries.
//!
//! A filter never changes the underlying evidence: it decides what a response
//! shows. Because a reader cannot tell a filtered-out row from a row that never
//! existed, every filtered response states what disappeared and why through
//! [`FilteredOut`] — a reason, a count, and up to
//! [`FILTERED_EXAMPLE_CAP`] examples.

use serde::Serialize;

/// Largest number of examples recorded per [`FilteredOut`] reason.
pub const FILTERED_EXAMPLE_CAP: usize = 5;

/// Why a set of items is missing from a filtered response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum FilterReason {
    /// Excluded by the `confidence` floor.
    Confidence,
    /// Excluded by the `language` filter.
    Language,
    /// Excluded by the `change_kind` filter.
    ChangeKind,
    /// Excluded by the `scope` path-prefix filter.
    Scope,
    /// Excluded because the item's file is not indexed in this snapshot.
    Unindexed,
}

impl FilterReason {
    /// Stable label used in payloads and user-facing output.
    pub fn label(self) -> &'static str {
        match self {
            Self::Confidence => "confidence",
            Self::Language => "language",
            Self::ChangeKind => "change_kind",
            Self::Scope => "scope",
            Self::Unindexed => "unindexed",
        }
    }

    /// Sentence a reader can act on, naming the filter that removed the items.
    pub fn explanation(self) -> &'static str {
        match self {
            Self::Confidence => {
                "Excluded by the `confidence` floor. Lower the floor to see these candidates; \
                 they are weaker evidence, not absent evidence."
            }
            Self::Language => {
                "Excluded by the `language` filter. The underlying evidence is unchanged; only \
                 this response hides it."
            }
            Self::ChangeKind => {
                "Excluded by the `change_kind` filter, which keeps only items whose symbol \
                 changed in one of the requested ways."
            }
            Self::Scope => {
                "Excluded by the `scope` path prefix. Widen or drop `scope` to see these items."
            }
            Self::Unindexed => {
                "The item's file is not indexed in this snapshot, so it carries no symbol \
                 evidence; it is reported as out of scope rather than as absent."
            }
        }
    }
}

/// One reason items are missing from a filtered response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FilteredOut {
    /// Stable reason label.
    pub reason: String,
    /// Human-readable explanation of the filter that removed them.
    pub explanation: String,
    /// How many items the filter removed.
    pub count: usize,
    /// Up to [`FILTERED_EXAMPLE_CAP`] examples of what was removed.
    pub examples: Vec<String>,
}

/// Accumulates filter decisions so a response can explain every omission.
#[derive(Debug, Clone, Default)]
pub struct FilterLog {
    entries: Vec<(FilterReason, usize, Vec<String>)>,
}

impl FilterLog {
    /// Record that `example` was removed for `reason`.
    pub fn record(&mut self, reason: FilterReason, example: &str) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|(recorded, _, _)| *recorded == reason)
        {
            entry.1 += 1;
            if entry.2.len() < FILTERED_EXAMPLE_CAP {
                entry.2.push(example.to_string());
            }
            return;
        }
        self.entries.push((reason, 1, vec![example.to_string()]));
    }

    /// Record `count` removals for `reason` without per-item examples.
    pub fn record_count(&mut self, reason: FilterReason, count: usize) {
        if count == 0 {
            return;
        }
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|(recorded, _, _)| *recorded == reason)
        {
            entry.1 += count;
            return;
        }
        self.entries.push((reason, count, Vec::new()));
    }

    /// The `filtered_out` block, ordered by reason.
    pub fn into_filtered_out(mut self) -> Vec<FilteredOut> {
        self.entries.sort_by_key(|(reason, _, _)| *reason);
        self.entries
            .into_iter()
            .map(|(reason, count, examples)| FilteredOut {
                reason: reason.label().to_string(),
                explanation: reason.explanation().to_string(),
                count,
                examples,
            })
            .collect()
    }
}

/// The presentation filters a request may apply.
///
/// Every field is optional and empty by default: an unfiltered response shows
/// everything the bounds admitted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilterSet {
    /// Keep only items whose file is in this language.
    pub language: Option<String>,
    /// Keep only items whose symbol changed in one of these ways.
    pub change_kind: Vec<String>,
    /// Keep only items whose path starts with this prefix.
    pub scope: Option<String>,
}

impl FilterSet {
    /// Whether any filter is set.
    pub fn is_empty(&self) -> bool {
        self.language.is_none() && self.change_kind.is_empty() && self.scope.is_none()
    }

    /// Whether `path` passes the `language` filter.
    pub fn language_admits(&self, path: &str) -> bool {
        let Some(requested) = self.language.as_deref() else {
            return true;
        };
        language_of(path).is_some_and(|language| language.eq_ignore_ascii_case(requested))
    }

    /// Whether `path` passes the `scope` prefix filter.
    pub fn scope_admits(&self, path: &str) -> bool {
        let Some(prefix) = self.scope.as_deref() else {
            return true;
        };
        let prefix = prefix.trim_start_matches("./").trim_end_matches('/');
        if prefix.is_empty() {
            return true;
        }
        path == prefix || path.starts_with(format!("{prefix}/").as_str())
    }

    /// Whether `status` passes the `change_kind` filter.
    ///
    /// `None` means the item's symbol is not in the changed-symbol slice at
    /// all, which no requested change kind can match.
    pub fn change_kind_admits(&self, status: Option<&str>) -> bool {
        if self.change_kind.is_empty() {
            return true;
        }
        status.is_some_and(|status| {
            self.change_kind
                .iter()
                .any(|kind| kind.eq_ignore_ascii_case(status))
        })
    }
}

/// Language of a path, by extension, using the graph's own language labels.
///
/// `None` means the extractor has no grammar for the path, which is reported as
/// out of scope rather than as "no evidence".
pub fn language_of(path: &str) -> Option<&'static str> {
    let file = path.rsplit('/').next().unwrap_or(path);
    let extension = file.rsplit_once('.').map(|(_, extension)| extension)?;
    let language = match extension.to_ascii_lowercase().as_str() {
        "rs" => "rust",
        "c" | "h" => "c",
        "cs" => "csharp",
        "go" => "go",
        "java" => "java",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "tsx" => "typescript",
        "kt" | "kts" => "kotlin",
        "py" => "python",
        "rb" => "ruby",
        "md" | "markdown" => "markdown",
        "json" | "yaml" | "yml" | "toml" | "env" => "config",
        _ => return None,
    };
    Some(language)
}

/// Split a comma-separated filter value into its non-empty terms.
pub fn split_terms(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_come_from_the_extension() {
        assert_eq!(language_of("src/lib.rs"), Some("rust"));
        assert_eq!(language_of("tests/app.test.ts"), Some("typescript"));
        assert_eq!(language_of("pkg/__init__.py"), Some("python"));
        assert_eq!(language_of("schema.proto"), None);
        assert_eq!(language_of("Makefile"), None);
    }

    #[test]
    fn scope_matches_whole_path_segments_only() {
        let filters = FilterSet {
            scope: Some("src".to_string()),
            ..FilterSet::default()
        };
        assert!(filters.scope_admits("src/lib.rs"));
        assert!(filters.scope_admits("src"));
        assert!(!filters.scope_admits("srcs/lib.rs"));
        assert!(!filters.scope_admits("tests/lib.rs"));

        let unfiltered = FilterSet::default();
        assert!(unfiltered.scope_admits("anything"));
    }

    #[test]
    fn change_kind_requires_a_known_status() {
        let filters = FilterSet {
            change_kind: vec!["removed".to_string(), "added".to_string()],
            ..FilterSet::default()
        };
        assert!(filters.change_kind_admits(Some("removed")));
        assert!(!filters.change_kind_admits(Some("modified")));
        assert!(!filters.change_kind_admits(None));
        assert!(FilterSet::default().change_kind_admits(None));
    }

    #[test]
    fn the_log_counts_every_removal_and_caps_examples() {
        let mut log = FilterLog::default();
        for index in 0..8 {
            log.record(FilterReason::Language, format!("symbol:{index}").as_str());
        }
        log.record_count(FilterReason::Confidence, 3);
        let filtered = log.into_filtered_out();
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].reason, "confidence");
        assert_eq!(filtered[0].count, 3);
        assert!(filtered[0].examples.is_empty());
        assert_eq!(filtered[1].reason, "language");
        assert_eq!(filtered[1].count, 8);
        assert_eq!(filtered[1].examples.len(), FILTERED_EXAMPLE_CAP);
        assert!(!filtered[1].explanation.is_empty());
    }

    #[test]
    fn terms_are_split_and_trimmed() {
        assert_eq!(split_terms("added, removed"), vec!["added", "removed"]);
        assert_eq!(split_terms(" , "), Vec::<String>::new());
    }
}

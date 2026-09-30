//! Resolve caller aliases once, using each snapshot's public graph identity.

use std::collections::BTreeSet;

use orbit_graph::Selector;

use crate::evidence::EvidenceError;
use crate::snapshot::{Comparison, SnapshotSide};

pub(crate) struct ResolvedSelection {
    entries: Vec<Entry>,
    base: BTreeSet<String>,
    head: BTreeSet<String>,
}

struct Entry {
    requested: String,
    base: Option<String>,
    head: Option<String>,
}

impl ResolvedSelection {
    pub(crate) fn resolve(
        comparison: &Comparison,
        requested: &[String],
    ) -> Result<Self, EvidenceError> {
        let mut resolved = Self {
            entries: Vec::with_capacity(requested.len()),
            base: BTreeSet::new(),
            head: BTreeSet::new(),
        };
        for text in requested {
            let selector = text
                .parse::<Selector>()
                .map_err(|error| EvidenceError::Selector(error.to_string()))?;
            if !matches!(selector, Selector::Symbol { .. }) {
                return Err(EvidenceError::Selector(format!(
                    "expected a symbol selector, got `{text}`"
                )));
            }
            let base = canonical(comparison, SnapshotSide::Base, &selector)?;
            let head = canonical(comparison, SnapshotSide::Head, &selector)?;
            if let Some(selector) = &base {
                resolved.base.insert(selector.clone());
            }
            if let Some(selector) = &head {
                resolved.head.insert(selector.clone());
            }
            resolved.entries.push(Entry {
                requested: text.clone(),
                base,
                head,
            });
        }
        Ok(resolved)
    }

    pub(crate) fn selects(&self, base: Option<&str>, head: Option<&str>) -> bool {
        // An unresolved request is still a nonempty selection. It must never
        // turn into the default that selects every changed symbol.
        self.entries.is_empty()
            || base.is_some_and(|selector| self.base.contains(selector))
            || head.is_some_and(|selector| self.head.contains(selector))
    }

    pub(crate) fn unmatched<'a>(
        &self,
        occurrences: impl IntoIterator<Item = (SnapshotSide, &'a str)>,
    ) -> Vec<String> {
        let mut base = BTreeSet::new();
        let mut head = BTreeSet::new();
        for (side, selector) in occurrences {
            match side {
                SnapshotSide::Base => {
                    base.insert(selector);
                }
                SnapshotSide::Head => {
                    head.insert(selector);
                }
            }
        }
        self.entries
            .iter()
            .filter(|entry| {
                !entry
                    .base
                    .as_deref()
                    .is_some_and(|selector| base.contains(selector))
                    && !entry
                        .head
                        .as_deref()
                        .is_some_and(|selector| head.contains(selector))
            })
            .map(|entry| entry.requested.clone())
            .collect()
    }
}

fn canonical(
    comparison: &Comparison,
    side: SnapshotSide,
    selector: &Selector,
) -> Result<Option<String>, EvidenceError> {
    // Metadata-only show preserves qualified-name precedence and the graph's
    // deterministic tie-break for ambiguous short names, without loading source.
    let view = comparison
        .snapshot(side)
        .graph()
        .show(selector, 0)
        .map_err(|error| EvidenceError::Query {
            operation: "resolve selected symbol",
            side,
            reason: error.to_string(),
        })?;
    Ok(view.and_then(|view| {
        let metadata = view.metadata;
        metadata.qualified.or(metadata.name).map(|symbol| {
            Selector::Symbol {
                path: metadata.file,
                symbol,
                kind: metadata.kind,
            }
            .to_string()
        })
    }))
}

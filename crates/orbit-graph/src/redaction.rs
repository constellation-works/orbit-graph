//! Conservative value redaction at graph, history and report durability boundaries.

use std::borrow::Cow;
use std::collections::HashMap;

const MARKER: &str = "[REDACTED_SECRET]";
const FOOTER_PREFIX: &str = "-----END ";
const FOOTER_SUFFIX: &str = "-----";

fn token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn boundary(bytes: &[u8], start: usize) -> bool {
    start == 0 || !token_byte(bytes[start - 1])
}

/// Offsets of each `-----END <label>-----` footer of a PEM private key, grouped
/// by label. Built in one pass, so a header's footer lookup is a binary search
/// instead of a scan of the rest of the input for each header.
struct PemFooters<'a> {
    by_label: HashMap<&'a str, Vec<usize>>,
}

impl<'a> PemFooters<'a> {
    fn new(input: &'a str) -> Self {
        let mut by_label: HashMap<&str, Vec<usize>> = HashMap::new();
        for (at, _) in input.match_indices(FOOTER_PREFIX) {
            // The label ends at the first `-----`, as it does for headers. Footers
            // do not overlap, so each scan ends at or before the next footer and
            // all the scans together stay linear.
            let label_start = at + FOOTER_PREFIX.len();
            let Some(label_len) = input[label_start..].find(FOOTER_SUFFIX) else {
                continue;
            };
            let label = &input[label_start..label_start + label_len];
            if label.ends_with("PRIVATE KEY") {
                by_label.entry(label).or_default().push(at);
            }
        }
        Self { by_label }
    }

    /// The first footer for `label` that starts at or after `from`.
    fn first_from(&self, label: &str, from: usize) -> Option<usize> {
        let offsets = self.by_label.get(label)?;
        offsets
            .get(offsets.partition_point(|&at| at < from))
            .copied()
    }
}

/// Mask high-confidence credential values while preserving ordinary text.
///
/// The returned borrow avoids allocation for the common case with no secret.
pub fn redact(input: &str) -> Cow<'_, str> {
    let bytes = input.as_bytes();
    let mut ranges = Vec::new();
    // Built on the first PEM private key header, so inputs without one pay nothing.
    let mut pem_footers: Option<PemFooters<'_>> = None;
    let mut pos = 0;
    while pos < bytes.len() {
        // Every pattern below starts with a fixed byte, so checking that byte
        // first skips most patterns at most positions.
        let first = bytes[pos];
        // PEM private keys may span many lines, so mask the whole block.
        if first == b'-'
            && input[pos..].starts_with("-----BEGIN ")
            && let Some(label_end) = input[pos + 11..].find("-----")
        {
            let label = &input[pos + 11..pos + 11 + label_end];
            if label.ends_with("PRIVATE KEY") {
                let footers = pem_footers.get_or_insert_with(|| PemFooters::new(input));
                // A header without a footer falls through, so the rest of its
                // text is still scanned by the patterns below.
                if let Some(footer) = footers.first_from(label, pos) {
                    let end = footer + FOOTER_PREFIX.len() + label.len() + FOOTER_SUFFIX.len();
                    ranges.push((pos, end));
                    pos = end;
                    continue;
                }
            }
        }

        // URL userinfo passwords are delimited by the authority's `@`.
        let scheme_len = if first != b'h' {
            0
        } else if input[pos..].starts_with("https://") {
            8
        } else if input[pos..].starts_with("http://") {
            7
        } else {
            0
        };
        if scheme_len > 0 && boundary(bytes, pos) {
            let authority = pos + scheme_len;
            let end = bytes[authority..]
                .iter()
                .position(|byte| {
                    matches!(
                        byte,
                        b'/' | b'?'
                            | b'#'
                            | b' '
                            | b'\n'
                            | b'\r'
                            | b'\t'
                            | b'"'
                            | b'\''
                            | b'<'
                            | b'>'
                    )
                })
                .map_or(bytes.len(), |offset| authority + offset);
            if let Some(at) = bytes[authority..end].iter().position(|byte| *byte == b'@') {
                let at = authority + at;
                if let Some(colon) = bytes[authority..at].iter().position(|byte| *byte == b':') {
                    let start = authority + colon + 1;
                    if start < at {
                        ranges.push((start, at));
                    }
                }
            }
        }

        if matches!(first, b'a' | b'A')
            && boundary(bytes, pos)
            && bytes[pos..].len() >= 22
            && bytes[pos..pos + 22].eq_ignore_ascii_case(b"authorization: bearer ")
        {
            let start = pos + 22;
            let end = bytes[start..]
                .iter()
                .position(|byte| {
                    byte.is_ascii_whitespace() || matches!(byte, b'"' | b'\'' | b'<' | b'>')
                })
                .map_or(bytes.len(), |offset| start + offset);
            if end > start {
                ranges.push((start, end));
            }
        }

        // Token prefixes start with `g`, `x` or `s`; AKIA keys start with `A`.
        if matches!(first, b'g' | b'x' | b's' | b'A') && boundary(bytes, pos) {
            for (prefix, min_tail) in [
                ("github_pat_", 12),
                ("ghp_", 12),
                ("gho_", 12),
                ("glpat-", 12),
                ("xoxb-", 12),
                ("xoxp-", 12),
                ("sk-proj-", 20),
                ("sk-", 20),
            ] {
                if input[pos..].starts_with(prefix) {
                    let start = pos + prefix.len();
                    let end = bytes[start..]
                        .iter()
                        .position(|byte| !token_byte(*byte))
                        .map_or(bytes.len(), |offset| start + offset);
                    let tail = &bytes[start..end];
                    // A plain hyphenated identifier is not an API key.
                    let valid = if prefix.starts_with("sk-") {
                        tail.len() >= min_tail && tail.iter().all(u8::is_ascii_alphanumeric)
                    } else {
                        tail.len() >= min_tail && tail.iter().any(u8::is_ascii_alphanumeric)
                    };
                    if valid {
                        ranges.push((pos, end));
                    }
                    break;
                }
            }
            if input[pos..].starts_with("AKIA") && bytes.len() >= pos + 20 {
                let key = &bytes[pos + 4..pos + 20];
                if key
                    .iter()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
                    && (pos + 20 == bytes.len() || !token_byte(bytes[pos + 20]))
                {
                    ranges.push((pos, pos + 20));
                }
            }
        }
        pos += if first.is_ascii() {
            1
        } else {
            input[pos..].chars().next().map_or(1, char::len_utf8)
        };
    }
    if ranges.is_empty() {
        return Cow::Borrowed(input);
    }
    ranges.sort_unstable();
    let mut output = String::with_capacity(input.len());
    let mut copied = 0;
    for (start, end) in ranges {
        if end <= copied {
            continue;
        }
        if start > copied {
            output.push_str(&input[copied..start]);
        }
        output.push_str(MARKER);
        copied = end;
    }
    output.push_str(&input[copied..]);
    Cow::Owned(output)
}

#[cfg(test)]
#[path = "redaction/tests/mod.rs"]
mod tests;

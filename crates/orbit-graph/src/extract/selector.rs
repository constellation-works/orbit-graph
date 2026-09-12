//! Selector parsing and filesystem-anchor helpers for graph queries.
//!
//! The parser recognizes `dir:`, `file:`, `symbol:`, `module:`, and `command:`
//! selectors. It was recovered from Orbit's shared selector utility; see
//! `PROVENANCE.md` for the immutable source and standalone adaptations.

use std::fmt::{Display, Formatter};
use std::str::FromStr;

use serde::Serialize;
use thiserror::Error;

/// Error returned when a selector cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Error)]
#[error("invalid selector `{input}`: {reason}")]
pub struct SelectorParseError {
    /// The original selector input.
    pub input: String,
    /// Human-readable parse failure reason.
    pub reason: String,
}

/// Canonical graph selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// Directory selector anchored at a repository-relative or absolute path.
    Dir {
        /// Directory anchor path.
        path: String,
    },
    /// File selector anchored at a repository-relative or absolute path.
    File {
        /// File anchor path.
        path: String,
    },
    /// Symbol selector anchored at a source path and symbol identity.
    Symbol {
        /// File anchor path.
        path: String,
        /// Opaque symbol name or qualified symbol path.
        symbol: String,
        /// Symbol kind, such as `function`, `method`, or `trait`.
        kind: String,
    },
    /// Module selector addressed by qualified module name.
    Module {
        /// Qualified module name.
        qualified: String,
    },
    /// Command selector addressed by CLI command name.
    Command {
        /// Command name.
        name: String,
    },
}

impl Selector {
    /// Parse a list of selector strings.
    pub fn parse_many(raw_selectors: &[String]) -> Result<Vec<Self>, SelectorParseError> {
        raw_selectors
            .iter()
            .map(|selector| selector.parse())
            .collect()
    }

    /// Return the filesystem anchor path, or an empty string for graph-only selectors.
    pub fn path(&self) -> &str {
        match self {
            Self::Dir { path } | Self::File { path } | Self::Symbol { path, .. } => path,
            Self::Module { .. } | Self::Command { .. } => "",
        }
    }
}

impl Display for Selector {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dir { path } => write!(f, "dir:{path}"),
            Self::File { path } => write!(f, "file:{path}"),
            Self::Symbol { path, symbol, kind } => {
                write!(f, "symbol:{path}#{symbol}:{kind}")
            }
            Self::Module { qualified } => write!(f, "module:{qualified}"),
            Self::Command { name } => write!(f, "command:{name}"),
        }
    }
}

impl FromStr for Selector {
    type Err = SelectorParseError;

    fn from_str(selector: &str) -> Result<Self, Self::Err> {
        let trimmed = selector.trim();
        if let Some(path) = trimmed.strip_prefix("dir:") {
            return Ok(Self::Dir {
                path: normalize_selector_path(selector, path)?,
            });
        }

        if let Some(path) = trimmed.strip_prefix("file:") {
            return Ok(Self::File {
                path: normalize_selector_path(selector, path)?,
            });
        }

        if let Some(remainder) = trimmed.strip_prefix("symbol:") {
            let (location, kind) =
                remainder
                    .rsplit_once(':')
                    .ok_or_else(|| SelectorParseError {
                        input: selector.to_string(),
                        reason: "symbol selectors must use `symbol:<path>#<symbol>:<kind>`"
                            .to_string(),
                    })?;
            let (path, symbol) = location.split_once('#').ok_or_else(|| SelectorParseError {
                input: selector.to_string(),
                reason: "symbol selectors must include `#<symbol>`".to_string(),
            })?;
            let path = normalize_selector_path(selector, path)?;
            let symbol = symbol.trim();
            let kind = kind.trim();
            if symbol.is_empty() || kind.is_empty() {
                return Err(SelectorParseError {
                    input: selector.to_string(),
                    reason: "symbol selectors must include non-empty path, symbol, and kind"
                        .to_string(),
                });
            }
            return Ok(Self::Symbol {
                path,
                symbol: symbol.to_string(),
                kind: kind.to_string(),
            });
        }

        if let Some(qualified) = trimmed.strip_prefix("module:") {
            let qualified = qualified.trim();
            if qualified.is_empty() {
                return Err(SelectorParseError {
                    input: selector.to_string(),
                    reason: "module selectors must include a qualified module".to_string(),
                });
            }
            return Ok(Self::Module {
                qualified: qualified.to_string(),
            });
        }

        if let Some(name) = trimmed.strip_prefix("command:") {
            let name = name.trim();
            if name.is_empty() {
                return Err(SelectorParseError {
                    input: selector.to_string(),
                    reason: "command selectors must include a command name".to_string(),
                });
            }
            return Ok(Self::Command {
                name: name.to_string(),
            });
        }

        Err(SelectorParseError {
            input: selector.to_string(),
            reason:
                "selectors must start with `dir:`, `file:`, `symbol:`, `module:`, or `command:`"
                    .to_string(),
        })
    }
}

fn normalize_selector_path(
    original_input: &str,
    raw_path: &str,
) -> Result<String, SelectorParseError> {
    normalize_path_text(raw_path).map_err(|reason| SelectorParseError {
        input: original_input.to_string(),
        reason,
    })
}

fn normalize_path_text(raw: &str) -> Result<String, String> {
    let normalized = raw.trim().replace('\\', "/");
    if normalized.is_empty() {
        return Err("selector path must not be empty".to_string());
    }

    let is_absolute = normalized.starts_with('/');
    let mut parts = Vec::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if let Some(last) = parts.last()
                    && *last != ".."
                {
                    parts.pop();
                } else if !is_absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }

    if is_absolute {
        return Ok(if parts.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", parts.join("/"))
        });
    }

    Ok(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

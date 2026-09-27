//! Config extractor for orbit-graph-extract (ORB-00305).
//!
//! Populates ExtractedFile::configs (never refs/relations).
//! Uses workspace serde_norway/toml/serde_json for real parsing + line scan fallback for positions.
//! Kinds: yaml|toml|json|env|serde per spec §6.2.

use std::collections::HashMap;
use std::path::Path;

use super::common::normalize_path;
use crate::{ExtractedFile, Extractor, RawConfig};

/// Config (yaml/toml/json/env) key extractor.
pub struct ConfigExtractor;

impl Extractor for ConfigExtractor {
    fn lang(&self) -> &'static str {
        "config"
    }

    fn supports(&self, path: &Path) -> bool {
        matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("yaml") | Some("yml") | Some("toml") | Some("json") | Some("env")
        )
    }

    fn extract(&self, path: &Path, bytes: &[u8]) -> ExtractedFile {
        let Ok(source) = std::str::from_utf8(bytes) else {
            return ExtractedFile::default();
        };

        let kind = detect_kind(path);
        let mut configs = Vec::new();

        match kind.as_str() {
            "toml" => {
                if let Ok(table) = source.parse::<toml::Table>() {
                    let lines = toml_key_lines(source);
                    collect_toml_keys(&table, &[], &lines, &mut configs, path, &kind);
                } else {
                    scan_keys(source, &mut configs, path, &kind);
                }
            }
            "yaml" | "yml" => {
                if let Ok(value) = serde_norway::from_str::<serde_norway::Value>(source) {
                    let lines = yaml_key_lines(source);
                    collect_yaml_keys(&value, &[], &lines, &mut configs, path, &kind);
                } else {
                    scan_keys(source, &mut configs, path, &kind);
                }
            }
            "json" => {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(source) {
                    let lines = JsonKeyLines::new(source).collect();
                    collect_json_keys(&value, &[], &lines, &mut configs, path, &kind);
                } else {
                    scan_keys(source, &mut configs, path, &kind);
                }
            }
            "env" => {
                scan_keys(source, &mut configs, path, &kind);
            }
            _ => scan_keys(source, &mut configs, path, &kind),
        }

        ExtractedFile {
            configs,
            ..Default::default()
        }
    }
}

fn detect_kind(path: &Path) -> String {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        match ext {
            "yaml" | "yml" => return "yaml".to_string(),
            "toml" => return "toml".to_string(),
            "json" => return "json".to_string(),
            "env" => return "env".to_string(),
            _ => {}
        }
    }
    // handle dotfiles like .env (no extension per Path)
    if let Some(name) = path.file_name().and_then(|n| n.to_str())
        && (name == ".env" || name.ends_with(".env"))
    {
        return "env".to_string();
    }
    "serde".to_string()
}

fn scan_keys(source: &str, out: &mut Vec<RawConfig>, path: &Path, kind: &str) {
    let file_path = normalize_path(path);
    for (i, line) in source.lines().enumerate() {
        let t = line.trim_start();
        if let Some(colon) = t.find(':') {
            let key = t[..colon].trim().to_string();
            if !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')
            {
                out.push(RawConfig {
                    file_path: file_path.clone(),
                    line: i + 1,
                    key,
                    kind: kind.to_string(),
                });
            }
        } else if let Some(eq) = t.find('=') {
            let key = t[..eq].trim().to_string();
            if !key.is_empty() {
                out.push(RawConfig {
                    file_path: file_path.clone(),
                    line: i + 1,
                    key,
                    kind: kind.to_string(),
                });
            }
        }
    }
}

// The parsed values decide which keys exist. These indexes only attach source
// locations, so comments, scalar text, and unsupported key forms cannot add
// spurious config rows. Paths use segments rather than dotted strings because
// a quoted key may itself contain a dot.
type KeyLines = HashMap<Vec<String>, usize>;

fn source_line(lines: &KeyLines, path: &[String]) -> usize {
    // YAML aliases can materialize a mapping whose child keys occur only at
    // the anchor. Point those derived keys at the alias's own mapping line.
    (1..=path.len())
        .rev()
        .find_map(|len| lines.get(&path[..len]).copied())
        .unwrap_or(1)
}

fn record_path(lines: &mut KeyLines, path: &[String], line: usize) {
    for end in 1..=path.len() {
        lines.entry(path[..end].to_vec()).or_insert(line);
    }
}

fn toml_key_lines(source: &str) -> KeyLines {
    let mut lines = KeyLines::new();
    let mut section = Vec::new();
    let mut multiline_string = None;
    let mut inline_end = 0;
    let mut offset = 0;
    for (index, raw_line) in source.split_inclusive('\n').enumerate() {
        if offset < inline_end {
            offset += raw_line.len();
            continue;
        }
        let line = raw_line.trim_start();
        let line_offset = raw_line.len() - line.len();
        if let Some(delimiter) = multiline_string {
            if line.contains(delimiter) {
                multiline_string = None;
            }
            offset += raw_line.len();
            continue;
        }
        if line.starts_with('#') || line.trim().is_empty() {
            offset += raw_line.len();
            continue;
        }
        if line.starts_with('[') {
            let array = line.starts_with("[[");
            let open = if array { 2 } else { 1 };
            if let Some(close) = find_unquoted(&line[open..], ']')
                && let Some(path) = parse_toml_path(&line[open..open + close])
            {
                section = path;
                record_path(&mut lines, &section, index + 1);
            }
        } else if let Some(eq) = find_unquoted(line, '=')
            && let Some(mut path) = parse_toml_path(&line[..eq])
        {
            let mut full = section.clone();
            full.append(&mut path);
            record_path(&mut lines, &full, index + 1);
            let value_offset = offset + line_offset + eq + 1;
            let rest = &source[value_offset..];
            let value = line[eq + 1..].trim_start();
            for delimiter in ["\"\"\"", "'''"] {
                if let Some(tail) = value.strip_prefix(delimiter)
                    && !tail.contains(delimiter)
                {
                    multiline_string = Some(delimiter);
                }
            }
            if rest.trim_start().starts_with('{') {
                let mut scanner = FlowKeyLines::new(rest, index + 1, '=', &mut lines);
                scanner.collect(&full);
                inline_end = value_offset + scanner.cursor;
            }
        }
        offset += raw_line.len();
    }
    lines
}

fn parse_toml_path(text: &str) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\'' || bytes[i] == b'"' {
            i = skip_quoted(bytes, i);
        } else if bytes[i] == b'.' {
            parts.push(parse_toml_segment(&text[start..i])?);
            start = i + 1;
            i += 1;
        } else {
            i += 1;
        }
    }
    parts.push(parse_toml_segment(&text[start..])?);
    Some(parts)
}

fn parse_toml_segment(segment: &str) -> Option<String> {
    let segment = segment.trim();
    if segment.starts_with('"') {
        serde_json::from_str(segment).ok()
    } else if segment.starts_with('\'') && segment.ends_with('\'') {
        Some(segment[1..segment.len() - 1].to_string())
    } else if !segment.is_empty() {
        Some(segment.to_string())
    } else {
        None
    }
}

fn find_unquoted(text: &str, target: char) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\'' || bytes[i] == b'"' {
            i = skip_quoted(bytes, i);
        } else if bytes[i] == target as u8 {
            return Some(i);
        } else if bytes[i] == b'#' {
            break;
        } else {
            i += 1;
        }
    }
    None
}

fn yaml_key_lines(source: &str) -> KeyLines {
    let mut lines = KeyLines::new();
    if source.trim_start().starts_with('{') {
        let first_line = source[..source.len() - source.trim_start().len()]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1;
        FlowKeyLines::new(source.trim_start(), first_line, ':', &mut lines).collect(&[]);
        return lines;
    }
    let mut parents: Vec<(usize, Vec<String>)> = Vec::new();
    let mut sequence_indent = None;
    let mut scalar_indent = None;
    let mut inline_end = 0;
    let mut offset = 0;
    for (index, raw_line) in source.split_inclusive('\n').enumerate() {
        if offset < inline_end {
            offset += raw_line.len();
            continue;
        }
        let line = raw_line.trim_start();
        let indent = raw_line.len() - line.len();
        offset += raw_line.len();
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        if scalar_indent.is_some_and(|level| indent > level)
            || sequence_indent.is_some_and(|level| indent > level)
        {
            continue;
        }
        scalar_indent = None;
        sequence_indent = None;
        if line.starts_with("- ") || line.starts_with("-\n") {
            sequence_indent = Some(indent);
            continue;
        }
        while parents.last().is_some_and(|(level, _)| *level >= indent) {
            parents.pop();
        }
        if line.starts_with('{') {
            let parent = parents.last().map_or(&[][..], |(_, path)| path.as_slice());
            let value_offset = offset - raw_line.len() + indent;
            let mut scanner =
                FlowKeyLines::new(&source[value_offset..], index + 1, ':', &mut lines);
            scanner.collect(parent);
            inline_end = value_offset + scanner.cursor;
            continue;
        }
        let Some(colon) = yaml_separator(line) else {
            continue;
        };
        let Some(key) = parse_yaml_key(&line[..colon]) else {
            continue;
        };
        let mut path = parents
            .last()
            .map_or_else(Vec::new, |(_, path)| path.clone());
        path.push(key);
        record_path(&mut lines, &path, index + 1);
        let value = line[colon + 1..].trim_start();
        if value.starts_with('{') {
            let value_offset = offset - raw_line.len() + indent + colon + 1;
            let mut scanner =
                FlowKeyLines::new(&source[value_offset..], index + 1, ':', &mut lines);
            scanner.collect(&path);
            inline_end = value_offset + scanner.cursor;
        }
        if value.starts_with('|') || value.starts_with('>') {
            scalar_indent = Some(indent);
        }
        parents.push((indent, path));
    }
    lines
}

fn yaml_separator(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\'' || bytes[i] == b'"' {
            i = skip_quoted(bytes, i);
        } else if bytes[i] == b':'
            && bytes
                .get(i + 1)
                .is_none_or(|next| next.is_ascii_whitespace())
        {
            return Some(i);
        } else if bytes[i] == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            break;
        } else {
            i += 1;
        }
    }
    None
}

fn parse_yaml_key(text: &str) -> Option<String> {
    let value = serde_norway::from_str::<serde_norway::Value>(text.trim()).ok()?;
    value.as_str().map(str::to_string)
}

// For inline TOML tables and YAML flow mappings. The structural parser above
// has already accepted the document; this small scanner only tracks key lines.
struct FlowKeyLines<'a> {
    text: &'a str,
    cursor: usize,
    line: usize,
    separator: u8,
    lines: &'a mut KeyLines,
}

impl<'a> FlowKeyLines<'a> {
    fn new(text: &'a str, line: usize, separator: char, lines: &'a mut KeyLines) -> Self {
        Self {
            text,
            cursor: 0,
            line,
            separator: separator as u8,
            lines,
        }
    }

    fn advance(&mut self, end: usize) {
        self.line += self.text.as_bytes()[self.cursor..end]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count();
        self.cursor = end;
    }

    fn skip_space(&mut self) {
        let bytes = self.text.as_bytes();
        while self.cursor < bytes.len() && bytes[self.cursor].is_ascii_whitespace() {
            self.advance(self.cursor + 1);
        }
    }

    fn collect(&mut self, parent: &[String]) {
        self.skip_space();
        if self.text.as_bytes().get(self.cursor) != Some(&b'{') {
            return;
        }
        self.advance(self.cursor + 1);
        loop {
            self.skip_space();
            let bytes = self.text.as_bytes();
            match bytes.get(self.cursor) {
                Some(b'}') => {
                    self.advance(self.cursor + 1);
                    return;
                }
                Some(b',') => {
                    self.advance(self.cursor + 1);
                    continue;
                }
                None => return,
                _ => {}
            }
            let key_line = self.line;
            let start = self.cursor;
            while let Some(&byte) = bytes.get(self.cursor) {
                if byte == b'\'' || byte == b'"' {
                    self.advance(skip_quoted(bytes, self.cursor));
                } else if byte == self.separator {
                    break;
                } else if byte == b'}' || byte == b',' {
                    return;
                } else {
                    self.advance(self.cursor + 1);
                }
            }
            if bytes.get(self.cursor) != Some(&self.separator) {
                return;
            }
            let key = &self.text[start..self.cursor];
            let segments = if self.separator == b'=' {
                parse_toml_path(key)
            } else {
                parse_yaml_key(key).map(|key| vec![key])
            };
            self.advance(self.cursor + 1);
            let mut path = parent.to_vec();
            if let Some(mut segments) = segments {
                path.append(&mut segments);
                record_path(self.lines, &path, key_line);
            }
            self.skip_space();
            if bytes.get(self.cursor) == Some(&b'{') {
                self.collect(&path);
            } else {
                self.skip_value();
            }
        }
    }

    fn skip_value(&mut self) {
        let bytes = self.text.as_bytes();
        let mut depth = 0;
        while let Some(&byte) = bytes.get(self.cursor) {
            if byte == b'\'' || byte == b'"' {
                self.advance(skip_quoted(bytes, self.cursor));
            } else if byte == b'[' || byte == b'{' {
                depth += 1;
                self.advance(self.cursor + 1);
            } else if byte == b']' || (byte == b'}' && depth > 0) {
                depth -= 1;
                self.advance(self.cursor + 1);
            } else if byte == b'#' {
                while let Some(&next) = bytes.get(self.cursor) {
                    self.advance(self.cursor + 1);
                    if next == b'\n' {
                        break;
                    }
                }
            } else if depth == 0 && (byte == b',' || byte == b'}') {
                return;
            } else {
                self.advance(self.cursor + 1);
            }
        }
    }
}

fn skip_quoted(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' && quote == b'"' {
            i += 2;
        } else if bytes[i] == quote {
            if quote == b'\'' && bytes.get(i + 1) == Some(&quote) {
                i += 2;
            } else {
                return i + 1;
            }
        } else {
            i += 1;
        }
    }
    bytes.len()
}

struct JsonKeyLines<'a> {
    source: &'a str,
    cursor: usize,
    line: usize,
    lines: KeyLines,
}

impl<'a> JsonKeyLines<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            cursor: 0,
            line: 1,
            lines: KeyLines::new(),
        }
    }

    fn collect(mut self) -> KeyLines {
        self.value(Some(&[]));
        self.lines
    }

    fn advance(&mut self) {
        if self.source.as_bytes().get(self.cursor) == Some(&b'\n') {
            self.line += 1;
        }
        self.cursor += 1;
    }

    fn whitespace(&mut self) {
        while self
            .source
            .as_bytes()
            .get(self.cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.advance();
        }
    }

    fn string(&mut self) -> Option<String> {
        let start = self.cursor;
        self.cursor = skip_quoted(self.source.as_bytes(), start);
        serde_json::from_str(&self.source[start..self.cursor]).ok()
    }

    fn value(&mut self, parent: Option<&[String]>) {
        self.whitespace();
        match self.source.as_bytes().get(self.cursor) {
            Some(b'{') => {
                self.advance();
                loop {
                    self.whitespace();
                    if self.source.as_bytes().get(self.cursor) == Some(&b'}') {
                        self.advance();
                        break;
                    }
                    let line = self.line;
                    let Some(key) = self.string() else { break };
                    self.whitespace();
                    self.advance(); // colon
                    let path = parent.map(|parent| {
                        let mut path = parent.to_vec();
                        path.push(key);
                        // JSON object maps retain the final value for duplicate keys.
                        self.lines.insert(path.clone(), line);
                        path
                    });
                    self.value(path.as_deref());
                    self.whitespace();
                    if self.source.as_bytes().get(self.cursor) == Some(&b',') {
                        self.advance();
                    }
                }
            }
            Some(b'[') => {
                self.advance();
                loop {
                    self.whitespace();
                    if self.source.as_bytes().get(self.cursor) == Some(&b']') {
                        self.advance();
                        break;
                    }
                    self.value(None); // the existing collector does not enter arrays
                    self.whitespace();
                    if self.source.as_bytes().get(self.cursor) == Some(&b',') {
                        self.advance();
                    }
                }
            }
            Some(b'"') => {
                self.string();
            }
            Some(_) => {
                while self.source.as_bytes().get(self.cursor).is_some_and(|byte| {
                    !byte.is_ascii_whitespace() && !matches!(byte, b',' | b'}' | b']')
                }) {
                    self.advance();
                }
            }
            None => {}
        }
    }
}

fn collect_toml_keys(
    table: &toml::Table,
    prefix: &[String],
    lines: &KeyLines,
    out: &mut Vec<RawConfig>,
    path: &Path,
    kind: &str,
) {
    let file_path = normalize_path(path);
    for (k, v) in table {
        let mut full = prefix.to_vec();
        full.push(k.clone());
        out.push(RawConfig {
            file_path: file_path.clone(),
            line: source_line(lines, &full),
            key: full.join("."),
            kind: kind.to_string(),
        });
        if let toml::Value::Table(sub) = v {
            collect_toml_keys(sub, &full, lines, out, path, kind);
        }
    }
}

fn collect_yaml_keys(
    value: &serde_norway::Value,
    prefix: &[String],
    lines: &KeyLines,
    out: &mut Vec<RawConfig>,
    path: &Path,
    kind: &str,
) {
    let file_path = normalize_path(path);
    if let serde_norway::Value::Mapping(map) = value {
        for (k, v) in map {
            if let Some(kstr) = k.as_str() {
                let mut full = prefix.to_vec();
                full.push(kstr.to_string());
                out.push(RawConfig {
                    file_path: file_path.clone(),
                    line: source_line(lines, &full),
                    key: full.join("."),
                    kind: kind.to_string(),
                });
                collect_yaml_keys(v, &full, lines, out, path, kind);
            }
        }
    }
}

fn collect_json_keys(
    value: &serde_json::Value,
    prefix: &[String],
    lines: &KeyLines,
    out: &mut Vec<RawConfig>,
    path: &Path,
    kind: &str,
) {
    let file_path = normalize_path(path);
    if let serde_json::Value::Object(map) = value {
        for (k, v) in map {
            let mut full = prefix.to_vec();
            full.push(k.clone());
            out.push(RawConfig {
                file_path: file_path.clone(),
                line: source_line(lines, &full),
                key: full.join("."),
                kind: kind.to_string(),
            });
            collect_json_keys(v, &full, lines, out, path, kind);
        }
    }
}

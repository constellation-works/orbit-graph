//! Runtime-invocation sites and the program names a source file ships under.
//!
//! [`Graph::runtime_invocations`] lists the stored `runtime_invocation` refs:
//! calls that start a program the syntax names, such as
//! `subprocess.run(["prog", ...])` or `Command::new(env!("CARGO_BIN_EXE_prog"))`.
//! The target is an opaque program string, never a symbol.
//! [`Graph::program_names`] reads the manifests nearest a source file to name
//! the programs it is built into, so a consumer can associate the two by name.
//! Neither is a claim that the invoked program runs the file's code.

use std::fs;
use std::path::Path;

use rusqlite::{Connection, params};

use super::contained_worktree_source;
use super::refs::LineCache;
use crate::{
    Graph, GraphError, ProgramName, ProgramNameSource, RuntimeInvocation, RuntimeInvocationSymbol,
};

pub(crate) fn invocations(graph: &Graph) -> Result<Vec<RuntimeInvocation>, GraphError> {
    let mut line_cache = LineCache::new(graph.worktree_root.as_path());
    graph.with_read_connection(|conn| {
        let rows = invocation_rows(conn)?;
        let mut invocations = Vec::with_capacity(rows.len());
        for row in rows {
            invocations.push(RuntimeInvocation {
                line: line_cache.line_for(row.file.as_str(), row.span_start)?,
                file: row.file,
                program: row.program,
                symbol: row.symbol,
            });
        }
        Ok(invocations)
    })
}

struct InvocationRow {
    file: String,
    span_start: i64,
    program: String,
    symbol: Option<RuntimeInvocationSymbol>,
}

fn invocation_rows(conn: &Connection) -> Result<Vec<InvocationRow>, GraphError> {
    let mut stmt = conn
        .prepare_cached(
            "SELECT r.from_file, r.from_span_start, r.target_name, s.name, s.kind, s.qualified
             FROM refs r
             LEFT JOIN symbols s ON s.id = (
                 SELECT enclosing.id
                 FROM symbols enclosing
                 WHERE enclosing.file_path = r.from_file
                   AND enclosing.span_start <= r.from_span_start
                   AND enclosing.span_end >= r.from_span_end
                 ORDER BY (enclosing.span_end - enclosing.span_start), enclosing.id
                 LIMIT 1
             )
             WHERE r.kind = 'runtime_invocation'
             ORDER BY r.from_file, r.from_span_start, r.id",
        )
        .map_err(|source| GraphError::sqlite("prepare runtime invocation lookup", source))?;
    stmt.query_map(params![], |row| {
        let name: Option<String> = row.get(3)?;
        let kind: Option<String> = row.get(4)?;
        let qualified: Option<String> = row.get(5)?;
        Ok(InvocationRow {
            file: row.get(0)?,
            span_start: row.get(1)?,
            program: row.get(2)?,
            symbol: match (name, kind, qualified) {
                (Some(name), Some(kind), Some(qualified)) => Some(RuntimeInvocationSymbol {
                    name,
                    kind,
                    qualified,
                }),
                _ => None,
            },
        })
    })
    .map_err(|source| GraphError::sqlite("query runtime invocations", source))?
    .collect::<Result<Vec<_>, _>>()
    .map_err(|source| GraphError::sqlite("collect runtime invocation rows", source))
}

/// Program names from the `Cargo.toml` and `pyproject.toml` nearest `path`.
///
/// Each manifest is looked up independently, walking from the file's directory
/// up to the worktree root: the nearest `Cargo.toml` with a `[package]` table
/// contributes its package name, every `[[bin]]` name, and every
/// `src/bin/<name>.rs` / `src/bin/<name>/main.rs` target; the nearest
/// `pyproject.toml` contributes its `[project.scripts]` and
/// `[tool.poetry.scripts]` keys. A manifest that cannot be read or parsed
/// contributes nothing.
pub(crate) fn program_names(graph: &Graph, path: &str) -> Result<Vec<ProgramName>, GraphError> {
    let root = graph.worktree_root.as_path();
    let resolved = contained_worktree_source(root, path)?;
    let mut names = Vec::new();
    let mut found_cargo = false;
    let mut found_pyproject = false;
    let mut directory = resolved.parent();
    while let Some(current) = directory {
        if !current.starts_with(root) {
            break;
        }
        if !found_cargo && let Some(cargo) = cargo_program_names(root, current) {
            names.extend(cargo);
            found_cargo = true;
        }
        if !found_pyproject && let Some(scripts) = pyproject_program_names(root, current) {
            names.extend(scripts);
            found_pyproject = true;
        }
        if (found_cargo && found_pyproject) || current == root {
            break;
        }
        directory = current.parent();
    }
    names.sort();
    names.dedup();
    Ok(names)
}

fn read_manifest(root: &Path, manifest: &Path) -> Option<(String, toml::Table)> {
    let text = fs::read_to_string(manifest).ok()?;
    let table = text.parse::<toml::Table>().ok()?;
    let relative = manifest
        .strip_prefix(root)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    Some((relative, table))
}

fn cargo_program_names(root: &Path, directory: &Path) -> Option<Vec<ProgramName>> {
    let (manifest, table) = read_manifest(root, directory.join("Cargo.toml").as_path())?;
    let package = table.get("package")?.as_table()?;
    let entry = |name: &str, source| ProgramName {
        name: name.to_string(),
        source,
        manifest: manifest.clone(),
    };
    let mut names = Vec::new();
    if let Some(name) = package.get("name").and_then(toml::Value::as_str) {
        names.push(entry(name, ProgramNameSource::CargoPackage));
    }
    for bin in table
        .get("bin")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(name) = bin.get("name").and_then(toml::Value::as_str) {
            names.push(entry(name, ProgramNameSource::CargoBin));
        }
    }
    if let Ok(entries) = fs::read_dir(directory.join("src/bin")) {
        for dir_entry in entries.flatten() {
            let entry_path = dir_entry.path();
            let name = if entry_path.extension().is_some_and(|ext| ext == "rs") {
                entry_path.file_stem()
            } else if entry_path.join("main.rs").is_file() {
                entry_path.file_name()
            } else {
                None
            };
            if let Some(name) = name.and_then(|name| name.to_str()) {
                names.push(entry(name, ProgramNameSource::CargoBin));
            }
        }
    }
    Some(names)
}

fn pyproject_program_names(root: &Path, directory: &Path) -> Option<Vec<ProgramName>> {
    let (manifest, table) = read_manifest(root, directory.join("pyproject.toml").as_path())?;
    let scripts = [
        table
            .get("project")
            .and_then(|project| project.get("scripts")),
        table
            .get("tool")
            .and_then(|tool| tool.get("poetry"))
            .and_then(|poetry| poetry.get("scripts")),
    ];
    Some(
        scripts
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_table)
            .flat_map(|scripts| scripts.keys())
            .map(|name| ProgramName {
                name: name.clone(),
                source: ProgramNameSource::PyprojectScript,
                manifest: manifest.clone(),
            })
            .collect(),
    )
}

#[cfg(test)]
#[path = "tests/runtime.rs"]
mod tests;

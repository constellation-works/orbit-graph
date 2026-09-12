#![allow(missing_docs)]

//! Entry point for the change explorer.
//!
//! Four commands:
//!
//! - `serve` starts the loopback HTTP service the UI calls. It is the product
//!   surface: it binds `127.0.0.1`, prints a per-launch bearer token once, and
//!   serves the JSON contract in `docs/design/change-explorer.md`.
//! - `snapshot` prints a human diagnostic of the two resolved snapshots. It is
//!   a terminal aid only and explicitly not a machine contract; the JSON
//!   contract lives behind `serve`.
//! - `report` writes the exported change report — `<name>.json` and a
//!   self-contained `<name>.html` — to a chosen directory. This is a machine
//!   contract, per `docs/design/change-explorer.md`'s "Exported change
//!   report".
//! - `clean` removes snapshot-cache entries whose key no longer matches this
//!   binary, and entries for commits the repository no longer has. It touches
//!   nothing outside the cache directory.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use orbit_graph::{Confidence, DEFAULT_IMPACT_DEPTH, IMPACT_NODE_CAP, RefOpts, Selector};
use orbit_graph_explorer::cache::{SnapshotCache, default_cache_dir};
use orbit_graph_explorer::evidence::{
    DEFAULT_TIME_BUDGET_MS, MAX_EVIDENCE_DEPTH, parse_confidence,
};
use orbit_graph_explorer::filters::{FilterSet, split_terms};
use orbit_graph_explorer::report::{ExcerptMode, ReportOptions, build_report, render_html};
use orbit_graph_explorer::service::{ServeOptions, Service, print_launch_banner};
use orbit_graph_explorer::snapshot::{Comparison, ComparisonOptions, Snapshot, SnapshotSide};

const USAGE: &str = "\
orbit-graph-explorer: explain one Git change through source relationships

Usage:
  orbit-graph-explorer serve --base <REF> --head <REF> [--repo <PATH>] [--port <PORT>]
  orbit-graph-explorer snapshot --base <REF> --head <REF> [--repo <PATH>] [--selector <SELECTOR>]
  orbit-graph-explorer report --base <REF> --head <REF> --out <DIR> --name <NAME>
                               [--repo <PATH>] [--force] [--selector <SELECTOR>]...
                               [--excerpts none|controlled|full-span] [--generated-at <RFC3339>]
                               [--include-absolute-paths] [--depth <N>] [--confidence <LEVEL>]
                               [--node-cap <N>] [--time-budget-ms <MS>] [--language <LANG>]
                               [--change-kind <KIND,...>] [--scope <PREFIX>]
  orbit-graph-explorer clean [--repo <PATH>] [--cache-dir <PATH>]

Options:
  --repo <PATH>            Repository to inspect (default: current directory).
  --base <REF>             Base revision; resolved to an immutable commit SHA.
  --head <REF>             Head revision; resolved to an immutable commit SHA.
  --port <PORT>            Port for `serve` (default: an ephemeral port).
  --selector <SELECTOR>    For `snapshot`, a symbol selector queried in both
                           snapshots. For `report`, may be repeated to select
                           specific changed symbols (default: every changed
                           symbol).
  --out <DIR>              Directory `report` writes `<name>.json` and
                           `<name>.html` into. Created if missing.
  --name <NAME>            Base file name for `report`'s two output files.
  --force                  Let `report` overwrite existing output files.
  --excerpts <MODE>        `report` excerpt policy: `none` (references only),
                           `controlled` (default; a bounded window around each
                           cited line), or `full-span` (the whole bounded file).
  --generated-at <RFC3339> Pin `report`'s `generated_at` field, for
                           byte-identical output across runs.
  --include-absolute-paths
                           Let `report` emit the repository's absolute host
                           path. Omitted by default.
  --depth <N>              Evidence and entry-point traversal depth for
                           `serve` and `report`.
  --confidence <LEVEL>     Confidence floor for `report`: `exact`,
                           `import_resolved`, `same_module` (default), or
                           `fuzzy_name`.
  --language <LANG>        `report` presentation filter: keep only items whose
                           file is in this language.
  --change-kind <KINDS>    `report` presentation filter: comma-separated
                           changed-symbol statuses to keep.
  --scope <PREFIX>         `report` presentation filter: keep only items whose
                           path starts with this prefix.
  --cache-dir <PATH>       Snapshot cache directory
                           (default: <repo>/.orbit-graph/explorer/snapshots).
  --no-cache               Index into task-owned temporary trees; reuse nothing.
  --time-budget-ms <MS>    Per-request traversal budget for `serve` and
                           `report`; `0` answers nothing and reports the budget
                           as the bound.
  --node-cap <N>           Traversal node cap for `serve` and `report`.
  -h, --help               Print this message.

`serve` binds 127.0.0.1 only, fixes the repository scope at launch, and prints a
per-launch bearer token to standard error once. Every request must carry that
token, and any Origin or Referer that is not the service's own origin is
refused.

Snapshot trees and indexes are cached per commit, keyed by commit SHA, extractor
version, and store schema version. A key that does not match this binary is
rebuilt, never reused. `clean` removes stale-key entries and entries for commits
the repository no longer has, and nothing outside the cache directory.

`snapshot` output is a human diagnostic, not a stable machine contract.

`report` writes a machine-contract JSON export plus a self-contained, static
HTML rendering (readable from disk, no scripts, no service). It never includes
the whole repository: only the changed symbols in scope are queried, and
every cited source location is either a bounded excerpt or a precise
`file:line-span@sha` reference. It refuses to overwrite `<name>.json` or
`<name>.html` unless `--force` is given.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(args.as_slice()) {
        Ok(Outcome::Text(text)) => {
            if write_out(text.as_str()).is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Ok(Outcome::Served) => ExitCode::SUCCESS,
        Err(message) => {
            let _ = write_err(message.as_str());
            ExitCode::FAILURE
        }
    }
}

enum Outcome {
    Text(String),
    Served,
}

fn run(args: &[String]) -> Result<Outcome, String> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        return Ok(Outcome::Text(USAGE.to_string()));
    }
    let invocation = Invocation::parse(args)?;
    match invocation.command {
        Command::Serve => serve(&invocation),
        Command::Snapshot => snapshot(&invocation).map(Outcome::Text),
        Command::Report => report(&invocation).map(Outcome::Text),
        Command::Clean => clean(&invocation).map(Outcome::Text),
    }
}

fn serve(invocation: &Invocation) -> Result<Outcome, String> {
    let service = Service::start(&ServeOptions {
        repository: invocation.repo.clone(),
        base: invocation.base.clone(),
        head: invocation.head.clone(),
        port: invocation.port.unwrap_or(0),
        cache_dir: invocation.cache_dir.clone(),
        no_cache: invocation.no_cache,
        time_budget_ms: invocation.time_budget_ms.unwrap_or(DEFAULT_TIME_BUDGET_MS),
        node_cap: invocation.node_cap.unwrap_or(IMPACT_NODE_CAP),
    })
    .map_err(|error| error.to_string())?;
    print_launch_banner(&service).map_err(|error| error.to_string())?;
    service.run();
    Ok(Outcome::Served)
}

fn snapshot(invocation: &Invocation) -> Result<String, String> {
    let comparison = Comparison::open_with_options(
        invocation.repo.as_path(),
        invocation.base.as_str(),
        invocation.head.as_str(),
        &ComparisonOptions {
            cache_dir: invocation.cache_dir.clone(),
            no_cache: invocation.no_cache,
        },
    )
    .map_err(|error| error.to_string())?;

    let mut report = String::new();
    push_line(
        &mut report,
        format!("repository\t{}", comparison.repository().display()),
    );
    push_line(&mut report, format!("mode\t{}", comparison.mode()));
    if let Some(notice) = comparison.working_tree().notice() {
        push_line(&mut report, format!("working_tree\tdirty\t{notice}"));
    } else {
        push_line(&mut report, "working_tree\tclean".to_string());
    }
    for side in [SnapshotSide::Base, SnapshotSide::Head] {
        describe_snapshot(&mut report, comparison.snapshot(side));
    }

    if let Some(note) = comparison.cache_note() {
        push_line(&mut report, format!("cache\tunavailable\t{note}"));
    } else if let Some(dir) = comparison.cache_dir() {
        push_line(&mut report, format!("cache\tdir\t{}", dir.display()));
    }

    if let Some(raw_selector) = invocation.selectors.first().map(String::as_str) {
        let selector = raw_selector
            .parse::<Selector>()
            .map_err(|error| error.to_string())?;
        for side in [SnapshotSide::Base, SnapshotSide::Head] {
            let snapshot = comparison.snapshot(side);
            let refs = snapshot
                .refs(&selector, &RefOpts::default())
                .map_err(|error| error.to_string())?;
            let impact = snapshot
                .impact(&selector, DEFAULT_IMPACT_DEPTH, Confidence::default())
                .map_err(|error| error.to_string())?;
            push_line(
                &mut report,
                format!(
                    "{side}\tselector\t{raw_selector}\tresolved\t{}\trefs\t{}\trelations\t{}\timpacted\t{}",
                    refs.target.qualified.is_some(),
                    refs.refs.len(),
                    refs.relations.len(),
                    impact.touched.len(),
                ),
            );
        }
    }

    Ok(report)
}

/// Write the exported change report's `<name>.json` and `<name>.html` into
/// `--out`, refusing to overwrite either file unless `--force` was given.
fn report(invocation: &Invocation) -> Result<String, String> {
    let out = invocation
        .out
        .as_ref()
        .ok_or_else(|| "internal error: `--out` missing after validation".to_string())?;
    let name = invocation
        .name
        .as_deref()
        .ok_or_else(|| "internal error: `--name` missing after validation".to_string())?;

    let json_path = out.join(format!("{name}.json"));
    let html_path = out.join(format!("{name}.html"));
    if !invocation.force {
        for path in [&json_path, &html_path] {
            if path.exists() {
                return Err(format!(
                    "{} already exists; pass `--force` to overwrite\n\n{USAGE}",
                    path.display()
                ));
            }
        }
    }

    let comparison = Comparison::open_with_options(
        invocation.repo.as_path(),
        invocation.base.as_str(),
        invocation.head.as_str(),
        &ComparisonOptions {
            cache_dir: invocation.cache_dir.clone(),
            no_cache: invocation.no_cache,
        },
    )
    .map_err(|error| error.to_string())?;

    let options = report_options(invocation)?;
    let exported = build_report(&comparison, &options).map_err(|error| error.to_string())?;
    let json = serde_json::to_string_pretty(&exported)
        .map_err(|error| format!("encode report JSON: {error}"))?;
    let html = render_html(&exported);

    std::fs::create_dir_all(out.as_path())
        .map_err(|error| format!("create {}: {error}", out.display()))?;
    std::fs::write(json_path.as_path(), json.as_bytes())
        .map_err(|error| format!("write {}: {error}", json_path.display()))?;
    std::fs::write(html_path.as_path(), html.as_bytes())
        .map_err(|error| format!("write {}: {error}", html_path.display()))?;

    Ok(format!(
        "wrote {}\nwrote {}\n",
        json_path.display(),
        html_path.display()
    ))
}

/// Build [`ReportOptions`] from the command-line invocation.
fn report_options(invocation: &Invocation) -> Result<ReportOptions, String> {
    let mut options = ReportOptions {
        selection: invocation.selectors.clone(),
        ..ReportOptions::default()
    };

    if let Some(raw) = invocation.excerpts.as_deref() {
        options.excerpts = ExcerptMode::parse(raw).ok_or_else(|| {
            format!("`--excerpts` must be `none`, `controlled`, or `full-span`, not `{raw}`")
        })?;
    }
    if let Some(raw) = invocation.confidence.as_deref() {
        options.min_confidence = parse_confidence(raw).ok_or_else(|| {
            format!(
                "`--confidence` must be `exact`, `import_resolved`, `same_module`, or \
                 `fuzzy_name`, not `{raw}`"
            )
        })?;
    }
    if let Some(depth) = invocation.depth {
        if depth > MAX_EVIDENCE_DEPTH {
            return Err(format!(
                "`--depth={depth}` exceeds the maximum of {MAX_EVIDENCE_DEPTH}"
            ));
        }
        options.bounds.depth = depth;
    }
    if let Some(node_cap) = invocation.node_cap {
        options.bounds.node_cap = node_cap;
    }
    if let Some(time_budget_ms) = invocation.time_budget_ms {
        options.bounds.time_budget_ms = time_budget_ms;
    }
    options.generated_at = invocation.generated_at.clone();
    options.include_absolute_paths = invocation.include_absolute_paths;
    options.filters = FilterSet {
        language: invocation.language.clone(),
        change_kind: invocation
            .change_kind
            .as_deref()
            .map(split_terms)
            .unwrap_or_default(),
        scope: invocation.scope.clone(),
    };
    Ok(options)
}

fn describe_snapshot(report: &mut String, snapshot: &Snapshot) {
    let side = snapshot.side();
    push_line(
        report,
        format!(
            "{side}\tref\t{}\tcommit\t{}\tfiles_indexed\t{}\tfiles_written\t{}\texcluded\t{}\tcache\t{}\tprepare_ms\t{}",
            snapshot.requested_ref(),
            snapshot.commit_sha(),
            snapshot.files_indexed(),
            snapshot.materialization().files_written,
            snapshot.materialization().excluded.len(),
            snapshot.cache_outcome().label(),
            snapshot.prepared_in().as_millis(),
        ),
    );
}

/// Remove snapshot-cache entries this binary can no longer use.
///
/// An entry is removed when its key does not match this binary's extractor and
/// store schema versions, when its commit is no longer in the repository, or
/// when it is a staging directory an interrupted build left behind. Anything
/// else in the directory is reported and left alone.
fn clean(invocation: &Invocation) -> Result<String, String> {
    let repo = git2::Repository::discover(invocation.repo.as_path()).map_err(|error| {
        format!(
            "{} is not a usable Git working tree: {}",
            invocation.repo.display(),
            error.message()
        )
    })?;
    let workdir = repo
        .workdir()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| invocation.repo.clone());
    let dir = invocation
        .cache_dir
        .clone()
        .unwrap_or_else(|| default_cache_dir(workdir.as_path()));
    let cache = SnapshotCache::open(dir.as_path()).map_err(|error| error.to_string())?;

    let is_live = |commit: &str| -> bool {
        git2::Oid::from_str(commit)
            .ok()
            .is_some_and(|oid| repo.find_commit(oid).is_ok())
    };
    let report = cache.clean(&is_live).map_err(|error| error.to_string())?;

    let mut text = String::new();
    push_line(
        &mut text,
        format!("cache\tdir\t{}", report.cache_dir.display()),
    );
    for entry in &report.removed {
        push_line(
            &mut text,
            format!(
                "removed\t{}\t{}",
                entry.reason.label(),
                entry.path.display()
            ),
        );
    }
    for entry in &report.kept {
        push_line(
            &mut text,
            format!("kept\t{}\t{}", entry.reason.label(), entry.path.display()),
        );
    }
    push_line(
        &mut text,
        format!(
            "summary\tremoved\t{}\tkept\t{}",
            report.removed.len(),
            report.kept.len()
        ),
    );
    Ok(text)
}

fn push_line(report: &mut String, line: String) {
    report.push_str(line.as_str());
    report.push('\n');
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Serve,
    Snapshot,
    Report,
    Clean,
}

struct Invocation {
    command: Command,
    repo: PathBuf,
    base: String,
    head: String,
    port: Option<u16>,
    selectors: Vec<String>,
    cache_dir: Option<PathBuf>,
    no_cache: bool,
    time_budget_ms: Option<u64>,
    node_cap: Option<usize>,
    out: Option<PathBuf>,
    name: Option<String>,
    force: bool,
    excerpts: Option<String>,
    generated_at: Option<String>,
    include_absolute_paths: bool,
    depth: Option<u8>,
    confidence: Option<String>,
    language: Option<String>,
    change_kind: Option<String>,
    scope: Option<String>,
}

impl Invocation {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut rest = args.iter();
        let command = match rest.next().map(String::as_str) {
            Some("serve") => Command::Serve,
            Some("snapshot") => Command::Snapshot,
            Some("report") => Command::Report,
            Some("clean") => Command::Clean,
            Some(other) => return Err(format!("unknown command `{other}`\n\n{USAGE}")),
            None => return Err(format!("missing command\n\n{USAGE}")),
        };

        let mut repo = None;
        let mut base = None;
        let mut head = None;
        let mut port = None;
        let mut selectors = Vec::new();
        let mut cache_dir = None;
        let mut no_cache = false;
        let mut time_budget_ms = None;
        let mut node_cap = None;
        let mut out = None;
        let mut name = None;
        let mut force = false;
        let mut excerpts = None;
        let mut generated_at = None;
        let mut include_absolute_paths = false;
        let mut depth = None;
        let mut confidence = None;
        let mut language = None;
        let mut change_kind = None;
        let mut scope = None;
        while let Some(flag) = rest.next() {
            let mut take_value = || -> Result<String, String> {
                rest.next()
                    .cloned()
                    .ok_or_else(|| format!("`{flag}` requires a value\n\n{USAGE}"))
            };
            match flag.as_str() {
                "--repo" => repo = Some(PathBuf::from(take_value()?)),
                "--base" => base = Some(take_value()?),
                "--head" => head = Some(take_value()?),
                "--port" => {
                    let raw = take_value()?;
                    port = Some(
                        raw.parse::<u16>()
                            .map_err(|error| format!("`--port` must be a port number: {error}"))?,
                    );
                }
                "--selector" => selectors.push(take_value()?),
                "--cache-dir" => cache_dir = Some(PathBuf::from(take_value()?)),
                "--no-cache" => no_cache = true,
                "--time-budget-ms" => {
                    let raw = take_value()?;
                    time_budget_ms = Some(raw.parse::<u64>().map_err(|error| {
                        format!("`--time-budget-ms` must be a whole number: {error}")
                    })?);
                }
                "--node-cap" => {
                    let raw = take_value()?;
                    node_cap = Some(raw.parse::<usize>().map_err(|error| {
                        format!("`--node-cap` must be a whole number: {error}")
                    })?);
                }
                "--out" => out = Some(PathBuf::from(take_value()?)),
                "--name" => name = Some(take_value()?),
                "--force" => force = true,
                "--excerpts" => excerpts = Some(take_value()?),
                "--generated-at" => generated_at = Some(take_value()?),
                "--include-absolute-paths" => include_absolute_paths = true,
                "--depth" => {
                    let raw = take_value()?;
                    depth =
                        Some(raw.parse::<u8>().map_err(|error| {
                            format!("`--depth` must be a whole number: {error}")
                        })?);
                }
                "--confidence" => confidence = Some(take_value()?),
                "--language" => language = Some(take_value()?),
                "--change-kind" => change_kind = Some(take_value()?),
                "--scope" => scope = Some(take_value()?),
                other => return Err(format!("unknown option `{other}`\n\n{USAGE}")),
            }
        }

        if command != Command::Serve && port.is_some() {
            return Err(format!("`--port` applies to `serve` only\n\n{USAGE}"));
        }
        if !matches!(command, Command::Snapshot | Command::Report) && !selectors.is_empty() {
            return Err(format!(
                "`--selector` applies to `snapshot` and `report` only\n\n{USAGE}"
            ));
        }
        if command == Command::Snapshot && selectors.len() > 1 {
            return Err(format!(
                "`snapshot` accepts at most one `--selector`\n\n{USAGE}"
            ));
        }
        if !matches!(command, Command::Serve | Command::Report)
            && (time_budget_ms.is_some() || node_cap.is_some())
        {
            return Err(format!(
                "`--time-budget-ms` and `--node-cap` apply to `serve` and `report` only\n\n{USAGE}"
            ));
        }
        if command != Command::Report
            && (out.is_some()
                || name.is_some()
                || force
                || excerpts.is_some()
                || generated_at.is_some()
                || include_absolute_paths
                || depth.is_some()
                || confidence.is_some()
                || language.is_some()
                || change_kind.is_some()
                || scope.is_some())
        {
            return Err(format!(
                "`--out`, `--name`, `--force`, `--excerpts`, `--generated-at`, \
                 `--include-absolute-paths`, `--depth`, `--confidence`, `--language`, \
                 `--change-kind`, and `--scope` apply to `report` only\n\n{USAGE}"
            ));
        }
        if command == Command::Report {
            if out.is_none() {
                return Err(format!("`--out` is required for `report`\n\n{USAGE}"));
            }
            if name.is_none() {
                return Err(format!("`--name` is required for `report`\n\n{USAGE}"));
            }
        }
        // `clean` inspects the cache, not a comparison, so it names no
        // revisions.
        let (base, head) = if command == Command::Clean {
            (String::new(), String::new())
        } else {
            (
                base.ok_or_else(|| format!("`--base` is required\n\n{USAGE}"))?,
                head.ok_or_else(|| format!("`--head` is required\n\n{USAGE}"))?,
            )
        };

        Ok(Self {
            command,
            repo: repo.unwrap_or_else(|| PathBuf::from(".")),
            base,
            head,
            port,
            selectors,
            cache_dir,
            no_cache,
            time_budget_ms,
            node_cap,
            out,
            name,
            force,
            excerpts,
            generated_at,
            include_absolute_paths,
            depth,
            confidence,
            language,
            change_kind,
            scope,
        })
    }
}

fn write_out(text: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(text.as_bytes())?;
    stdout.flush()
}

fn write_err(text: &str) -> io::Result<()> {
    let mut stderr = io::stderr().lock();
    stderr.write_all(text.as_bytes())?;
    stderr.write_all(b"\n")?;
    stderr.flush()
}

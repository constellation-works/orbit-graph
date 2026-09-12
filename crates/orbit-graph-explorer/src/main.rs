#![allow(missing_docs)]

//! Entry point for the change explorer.
//!
//! Two commands:
//!
//! - `serve` starts the loopback HTTP service the UI calls. It is the product
//!   surface: it binds `127.0.0.1`, prints a per-launch bearer token once, and
//!   serves the JSON contract in `docs/design/change-explorer.md`.
//! - `snapshot` prints a human diagnostic of the two resolved snapshots. It is
//!   a terminal aid only and explicitly not a machine contract; the JSON
//!   contract lives behind `serve`.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use orbit_graph::{Confidence, DEFAULT_IMPACT_DEPTH, RefOpts, Selector};
use orbit_graph_explorer::service::{ServeOptions, Service, print_launch_banner};
use orbit_graph_explorer::snapshot::{Comparison, Snapshot, SnapshotSide};

const USAGE: &str = "\
orbit-graph-explorer: explain one Git change through source relationships

Usage:
  orbit-graph-explorer serve --base <REF> --head <REF> [--repo <PATH>] [--port <PORT>]
  orbit-graph-explorer snapshot --base <REF> --head <REF> [--repo <PATH>] [--selector <SELECTOR>]

Options:
  --repo <PATH>          Repository to inspect (default: current directory).
  --base <REF>           Base revision; resolved to an immutable commit SHA.
  --head <REF>           Head revision; resolved to an immutable commit SHA.
  --port <PORT>          Port for `serve` (default: an ephemeral port).
  --selector <SELECTOR>  Optional symbol selector queried in both snapshots.
  -h, --help             Print this message.

`serve` binds 127.0.0.1 only, fixes the repository scope at launch, and prints a
per-launch bearer token to standard error once. Every request must carry that
token, and any Origin or Referer that is not the service's own origin is
refused.

`snapshot` output is a human diagnostic, not a stable machine contract.
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
    }
}

fn serve(invocation: &Invocation) -> Result<Outcome, String> {
    let service = Service::start(&ServeOptions {
        repository: invocation.repo.clone(),
        base: invocation.base.clone(),
        head: invocation.head.clone(),
        port: invocation.port.unwrap_or(0),
    })
    .map_err(|error| error.to_string())?;
    print_launch_banner(&service).map_err(|error| error.to_string())?;
    service.run();
    Ok(Outcome::Served)
}

fn snapshot(invocation: &Invocation) -> Result<String, String> {
    let comparison = Comparison::open(
        invocation.repo.as_path(),
        invocation.base.as_str(),
        invocation.head.as_str(),
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

    if let Some(raw_selector) = invocation.selector.as_deref() {
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

fn describe_snapshot(report: &mut String, snapshot: &Snapshot) {
    let side = snapshot.side();
    push_line(
        report,
        format!(
            "{side}\tref\t{}\tcommit\t{}\tfiles_indexed\t{}\tfiles_written\t{}\texcluded\t{}",
            snapshot.requested_ref(),
            snapshot.commit_sha(),
            snapshot.files_indexed(),
            snapshot.materialization().files_written,
            snapshot.materialization().excluded.len(),
        ),
    );
}

fn push_line(report: &mut String, line: String) {
    report.push_str(line.as_str());
    report.push('\n');
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Serve,
    Snapshot,
}

struct Invocation {
    command: Command,
    repo: PathBuf,
    base: String,
    head: String,
    port: Option<u16>,
    selector: Option<String>,
}

impl Invocation {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut rest = args.iter();
        let command = match rest.next().map(String::as_str) {
            Some("serve") => Command::Serve,
            Some("snapshot") => Command::Snapshot,
            Some(other) => return Err(format!("unknown command `{other}`\n\n{USAGE}")),
            None => return Err(format!("missing command\n\n{USAGE}")),
        };

        let mut repo = None;
        let mut base = None;
        let mut head = None;
        let mut port = None;
        let mut selector = None;
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
                "--selector" => selector = Some(take_value()?),
                other => return Err(format!("unknown option `{other}`\n\n{USAGE}")),
            }
        }

        if command == Command::Snapshot && port.is_some() {
            return Err(format!("`--port` applies to `serve` only\n\n{USAGE}"));
        }
        if command == Command::Serve && selector.is_some() {
            return Err(format!(
                "`--selector` applies to `snapshot` only\n\n{USAGE}"
            ));
        }

        Ok(Self {
            command,
            repo: repo.unwrap_or_else(|| PathBuf::from(".")),
            base: base.ok_or_else(|| format!("`--base` is required\n\n{USAGE}"))?,
            head: head.ok_or_else(|| format!("`--head` is required\n\n{USAGE}"))?,
            port,
            selector,
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

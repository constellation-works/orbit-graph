#![allow(missing_docs)]

//! Milestone 1 diagnostic entry point for the change explorer.
//!
//! The explorer's delivery mechanism is a loopback HTTP service with an
//! embedded UI (see `docs/design/change-explorer.md`). Neither the service nor
//! the UI exists yet. This binary exercises the snapshot foundation from the
//! terminal so the behavior is observable through a real executable, and it
//! prints a human diagnostic only: it is explicitly not a machine contract.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use orbit_graph::{Confidence, DEFAULT_IMPACT_DEPTH, RefOpts, Selector};
use orbit_graph_explorer::snapshot::{Comparison, Snapshot, SnapshotSide};

const USAGE: &str = "\
orbit-graph-explorer: change-explorer snapshot diagnostic (milestone 1)

Usage:
  orbit-graph-explorer snapshot --base <REF> --head <REF> [--repo <PATH>] [--selector <SELECTOR>]

Options:
  --repo <PATH>          Repository to inspect (default: current directory).
  --base <REF>           Base revision; resolved to an immutable commit SHA.
  --head <REF>           Head revision; resolved to an immutable commit SHA.
  --selector <SELECTOR>  Optional symbol selector queried in both snapshots.
  -h, --help             Print this message.

Output is a human diagnostic, not a stable machine contract. The loopback
service and its JSON contract arrive with milestone 2.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(args.as_slice()) {
        Ok(text) => {
            if write_out(text.as_str()).is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            let _ = write_err(message.as_str());
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<String, String> {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        return Ok(USAGE.to_string());
    }
    let invocation = Invocation::parse(args)?;
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

struct Invocation {
    repo: PathBuf,
    base: String,
    head: String,
    selector: Option<String>,
}

impl Invocation {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut rest = args.iter();
        match rest.next().map(String::as_str) {
            Some("snapshot") => {}
            Some(other) => return Err(format!("unknown command `{other}`\n\n{USAGE}")),
            None => return Err(format!("missing command\n\n{USAGE}")),
        }

        let mut repo = None;
        let mut base = None;
        let mut head = None;
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
                "--selector" => selector = Some(take_value()?),
                other => return Err(format!("unknown option `{other}`\n\n{USAGE}")),
            }
        }

        Ok(Self {
            repo: repo.unwrap_or_else(|| PathBuf::from(".")),
            base: base.ok_or_else(|| format!("`--base` is required\n\n{USAGE}"))?,
            head: head.ok_or_else(|| format!("`--head` is required\n\n{USAGE}"))?,
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

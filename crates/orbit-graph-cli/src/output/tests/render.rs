use std::io::{self, Write};

use serde_json::json;

use crate::output::payload::{CommandOutput, View, ViewBlock};
use crate::output::render::emit;
use crate::output::sink::{FormatArg, OutputSink, SinkEnvironment};
use crate::output::table::{Column, TableView};

#[test]
fn redirected_table_is_complete_plain_records() {
    let mut table = TableView::new(vec![Column::text("path"), Column::number("count")]);
    table.push_row(["a/very/long/path.rs", "12"]);
    let output = CommandOutput::with_view(
        json!({"records": [{"path": "a/very/long/path.rs", "count": 12}]}),
        View::Blocks(vec![ViewBlock::table(table)]),
    );
    let sink = OutputSink::resolve(false, &SinkEnvironment::default(), None, None);
    let mut stdout = Vec::new();
    emit(&output, sink, &mut stdout, &mut Vec::new()).expect("render plain output");
    assert_eq!(stdout, b"a/very/long/path.rs\t12\n");
}

#[test]
fn plain_rows_escape_separators_and_controls_losslessly() {
    let mut table = TableView::new(vec![Column::text("value"), Column::fixed("kind")]);
    table.push_row(["line 1\nline\t2\\tail\u{7}", "text"]);
    let output = CommandOutput::with_view(
        json!({"records": [{"value": "line 1\nline\t2\\tail\u{7}"}]}),
        View::Blocks(vec![ViewBlock::table(table)]),
    );
    let sink = OutputSink::resolve(false, &SinkEnvironment::default(), None, None);
    let mut stdout = Vec::new();
    emit(&output, sink, &mut stdout, &mut Vec::new()).expect("render escaped row");
    assert_eq!(stdout, b"line 1\\nline\\t2\\\\tail\\x07\ttext\n");
    assert_eq!(stdout.iter().filter(|byte| **byte == b'\n').count(), 1);
}

#[test]
fn empty_table_routes_its_diagnostic_to_stderr() {
    let table = TableView::new(vec![Column::text("path")]).with_empty_message("no matching paths");
    let output = CommandOutput::with_view(
        json!({"records": []}),
        View::Blocks(vec![ViewBlock::table(table)]),
    );
    let sink = OutputSink::resolve(false, &SinkEnvironment::default(), None, None);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    emit(&output, sink, &mut stdout, &mut stderr).expect("render empty output");
    assert!(stdout.is_empty());
    assert_eq!(stderr, b"no matching paths\n");
}

#[test]
fn ndjson_uses_declared_record_units() {
    let output = CommandOutput::document(json!({"records": [1, 2]}))
        .with_ndjson_records(vec![json!({"value": 1}), json!({"value": 2})]);
    let sink = OutputSink::resolve(
        false,
        &SinkEnvironment::default(),
        None,
        Some(FormatArg::Ndjson),
    );
    let mut stdout = Vec::new();
    emit(&output, sink, &mut stdout, &mut Vec::new()).expect("render NDJSON records");
    assert_eq!(stdout, b"{\"value\":1}\n{\"value\":2}\n");
}

#[test]
fn broken_stdout_pipe_is_classified_for_silent_success() {
    struct BrokenWriter;
    impl Write for BrokenWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let output = CommandOutput::document(json!({"ok": true}));
    let sink = OutputSink::resolve(false, &SinkEnvironment::default(), None, None);
    let error = emit(&output, sink, &mut BrokenWriter, &mut Vec::new())
        .expect_err("broken pipe must reach the process boundary");
    assert!(error.is_broken_pipe());
}

use serde_json::json;

use crate::output::payload::{CommandOutput, View, ViewBlock};
use crate::output::render::emit;
use crate::output::sink::{FormatArg, OutputSink, SinkEnvironment};
use crate::output::table::{Column, TableView, Truncation, display_width, table_layout, truncate};

#[test]
fn unicode_width_and_padding_use_display_columns_and_graphemes() {
    assert_eq!(display_width("e\u{301}"), 1);
    assert_eq!(display_width("界"), 2);
    assert_eq!(truncate("Ae\u{301}BC", 3, Truncation::Tail), "Ae\u{301}…");

    let mut table = TableView::new(vec![Column::text("name"), Column::number("count")]);
    table.push_row(["界", "2"]);
    table.push_row(["e\u{301}", "10"]);
    let output = CommandOutput::with_view(
        json!({"records": []}),
        View::Blocks(vec![ViewBlock::table(table)]),
    );
    let sink = OutputSink::resolve(
        true,
        &SinkEnvironment {
            columns: Some("20".to_owned()),
            ..SinkEnvironment::default()
        },
        None,
        Some(FormatArg::Table),
    );
    let mut stdout = Vec::new();
    emit(&output, sink, &mut stdout, &mut Vec::new()).expect("render Unicode table");
    assert_eq!(
        String::from_utf8(stdout).expect("UTF-8"),
        "NAME  COUNT\n界        2\ne\u{301}        10\n"
    );
}

#[test]
fn narrow_layout_preserves_fixed_columns_and_drops_flexible_from_the_right() {
    let mut table = TableView::new(vec![
        Column::fixed("id"),
        Column::path("path"),
        Column::text("description"),
        Column::number("count"),
    ]);
    table.push_row([
        "ORB-12345",
        "crates/very/long/path/to/file.rs",
        "a long human description",
        "1234",
    ]);
    let layout = table_layout(&table, 33);
    assert_eq!(layout.indices, vec![0, 1, 3]);
    assert_eq!(layout.dropped, vec!["DESCRIPTION"]);
    assert_eq!(layout.widths[0], 9);
    assert_eq!(layout.widths[1], 15);
    assert_eq!(layout.widths[2], 5);

    let output = CommandOutput::with_view(
        json!({"records": []}),
        View::Blocks(vec![ViewBlock::table(table)]),
    );
    let sink = OutputSink::resolve(
        true,
        &SinkEnvironment {
            columns: Some("33".to_owned()),
            ..SinkEnvironment::default()
        },
        None,
        Some(FormatArg::Table),
    );
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    emit(&output, sink, &mut stdout, &mut stderr).expect("render narrow table");
    let stdout = String::from_utf8(stdout).expect("UTF-8");
    assert!(stdout.contains("crates/…file.rs"), "{stdout}");
    assert_eq!(stdout.lines().count(), 2);
    assert!(
        String::from_utf8(stderr)
            .expect("UTF-8")
            .contains("omitted columns DESCRIPTION")
    );
}

#[test]
fn terminal_narrower_than_fixed_fields_reports_the_required_width() {
    let mut table = TableView::new(vec![Column::fixed("status"), Column::number("count")]);
    table.push_row(["in-progress", "1234"]);
    let output = CommandOutput::with_view(
        json!({"records": []}),
        View::Blocks(vec![ViewBlock::table(table)]),
    );
    let sink = OutputSink::resolve(
        true,
        &SinkEnvironment {
            columns: Some("8".to_owned()),
            ..SinkEnvironment::default()
        },
        None,
        Some(FormatArg::Table),
    );
    let mut stderr = Vec::new();
    emit(&output, sink, &mut Vec::new(), &mut stderr).expect("render fixed table");
    assert!(
        String::from_utf8(stderr)
            .expect("UTF-8")
            .contains("required by fixed fields")
    );
}

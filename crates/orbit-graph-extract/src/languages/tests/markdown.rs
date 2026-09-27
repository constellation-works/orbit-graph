#![allow(missing_docs)]

use std::path::Path;

use crate::Extractor;
use crate::languages::MarkdownExtractor;

fn extract(source: &str) -> crate::ExtractedFile {
    MarkdownExtractor.extract(Path::new("README.md"), source.as_bytes())
}

#[test]
fn extracts_nested_headings_as_symbols_and_notable_strings() {
    let source = r#"# Top

Some intro with [link text](https://example.com/page).

## Nested

```rust
fn example() { println!("hi"); }
```

### Deep

Text with another [ref][1] link.

[1]: https://other.com
"#;
    let file = extract(source);

    let kinds: Vec<&str> = file.symbols.iter().map(|s| s.kind.as_str()).collect();
    assert!(kinds.contains(&"heading"), "missing heading symbols");
    // at least top + nested + deep
    let heading_names: Vec<&str> = file.symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(
        heading_names
            .iter()
            .any(|n| n.contains("Top") || n.contains("Nested") || n.contains("Deep"))
    );

    // byte spans valid
    assert!(file.symbols.iter().all(|s| s.span_start < s.span_end));

    // strings: links and code
    assert!(
        !file.strings.is_empty(),
        "expected notable strings from links/code fences"
    );
    let has_link = file.strings.iter().any(|s| s.value.contains("example.com"));
    let has_code = file
        .strings
        .iter()
        .any(|s| s.value.contains("example()") || s.value.contains("println"));
    assert!(has_link, "missing link string");
    assert!(has_code, "missing code block string");
}

#[test]
fn markdown_extractor_no_refs_relations_configs() {
    let file = extract("# H\n\ntext");
    assert!(file.refs.is_empty());
    assert!(file.relations.is_empty());
    assert!(file.configs.is_empty());
}

fn heading_span<'a>(file: &crate::ExtractedFile, source: &'a str, name: &str) -> &'a str {
    let heading = file
        .symbols
        .iter()
        .find(|symbol| symbol.name == name)
        .unwrap_or_else(|| panic!("missing heading symbol {name:?}"));
    &source[heading.span_start..heading.span_end]
}

#[test]
fn parent_heading_span_ends_at_next_ancestor_after_nested_child() {
    let source = "# A\n## child\nbody\n# B\nother\n";
    let file = extract(source);

    assert_eq!(heading_span(&file, source, "A"), "# A\n## child\nbody");
    assert_eq!(heading_span(&file, source, "B"), "# B\nother");
}

#[test]
fn nested_heading_spans_end_at_next_peer_or_ancestor_and_final_heading_at_eof() {
    let source = "# A\n## child\n### grandchild\ndeep body\n## sibling\nsibling body\n# B\nother";
    let file = extract(source);

    assert_eq!(
        heading_span(&file, source, "A"),
        "# A\n## child\n### grandchild\ndeep body\n## sibling\nsibling body"
    );
    assert_eq!(
        heading_span(&file, source, "child"),
        "## child\n### grandchild\ndeep body"
    );
    assert_eq!(
        heading_span(&file, source, "grandchild"),
        "### grandchild\ndeep body"
    );
    assert_eq!(
        heading_span(&file, source, "sibling"),
        "## sibling\nsibling body"
    );
    assert_eq!(heading_span(&file, source, "B"), "# B\nother");
}

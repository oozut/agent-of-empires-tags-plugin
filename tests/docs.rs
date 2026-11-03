//! The support matrix is prose; this keeps it from drifting from the table.
//!
//! `docs/harness-adapters.md` §Support matrix is what a user reads before
//! choosing a harness, so a row that gains or loses a channel must change there
//! too — and a `✓` for a kind `setup --apply` refuses is exactly the kind of lie
//! this plugin exists not to tell. Parsing the document rather than duplicating
//! the data is the point: the test fails only when the two disagree.

use std::collections::BTreeMap;

const DOC: &str = "docs/harness-adapters.md";
const KINDS: [&str; 3] = ["header", "query", "body"];

/// `agent → (cell per kind, evidence cell)` from the matrix, in document order.
fn matrix() -> BTreeMap<String, (Vec<String>, String)> {
    let text = std::fs::read_to_string(DOC).expect("docs/harness-adapters.md");
    let start = text.find("### Support matrix").expect("the matrix section");
    let body = &text[start..];
    let end = body
        .find("Where the value comes from")
        .expect("the section end");
    let mut rows = BTreeMap::new();
    for line in body[..end].lines() {
        let line = line.trim();
        if !line.starts_with("| `") {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        assert_eq!(cells.len(), 7, "a matrix row needs 7 cells: {line}");
        let agent = cells[0].trim_matches('`').to_string();
        let kinds = cells[1..4].iter().map(|cell| cell.to_string()).collect();
        rows.insert(agent, (kinds, cells[6].to_string()));
    }
    rows
}

#[test]
fn the_support_matrix_matches_the_adapter_table() {
    let (rows, warnings) = llm_tags::adapters::load(None);
    assert!(warnings.is_empty(), "{warnings:?}");
    let doc = matrix();
    assert!(!doc.is_empty(), "no matrix rows parsed from {DOC}");

    for row in &rows {
        let (cells, evidence) = doc
            .get(&row.agent)
            .unwrap_or_else(|| panic!("{} has no row in the support matrix", row.agent));
        assert_eq!(
            *evidence, row.status,
            "{}: the matrix says {evidence}, adapters.toml says {}",
            row.agent, row.status
        );
        for (index, kind) in KINDS.iter().enumerate() {
            let claimed = cells[index] == "✓";
            let declared = row.supports.iter().any(|listed| listed == kind);
            assert_eq!(
                claimed, declared,
                "{}: the matrix says {:?} for {kind}, adapters.toml declares {:?}",
                row.agent, cells[index], row.supports
            );
        }
    }
    for agent in doc.keys() {
        assert!(
            rows.iter().any(|row| &row.agent == agent),
            "{agent} is in the matrix but not in adapters.toml"
        );
    }
}

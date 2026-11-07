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

/// A capability is a grant the user approves, so the manifest asks only for the
/// ones the plugin uses. The concrete case is `net`: the plugin hands a header to
/// the harness and never opens a connection itself, so declaring it would buy a
/// second approval prompt and nothing else.
#[test]
fn the_manifest_asks_only_for_the_capabilities_the_plugin_uses() {
    let manifest = std::fs::read_to_string("aoe-plugin.toml").expect("aoe-plugin.toml");
    let capabilities = manifest
        .lines()
        .find_map(|line| line.trim().strip_prefix("capabilities = "))
        .expect("a capabilities line");
    for needed in ["runtime.worker", "session.read"] {
        assert!(
            capabilities.contains(&format!("\"{needed}\"")),
            "the plugin runs a worker and reads session rows, so it needs {needed}: {capabilities}"
        );
    }
    if capabilities.contains("\"net\"") {
        panic!("`net` is declared but unused: {capabilities}");
    }

    // And the source agrees: nothing in the plugin opens a socket.
    let mut sources = Vec::new();
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("src").flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    walk(std::path::Path::new("src"), &mut sources);
    assert!(!sources.is_empty(), "no sources found");
    for path in sources {
        let body = std::fs::read_to_string(&path).expect("source");
        for needle in [
            "std::net::",
            "TcpStream",
            "UdpSocket",
            "TcpListener",
            "reqwest",
        ] {
            assert!(
                !body.contains(needle),
                "{} opens a connection ({needle}) without declaring `net`",
                path.display()
            );
        }
    }
}

/// The editor page is declared in the manifest, its entry must exist in the
/// tree, and the page must speak the same bridge methods the worker pushes to.
/// A page whose script drifted from the worker would render an empty editor and
/// never say why, which is worse than no page.
#[test]
fn the_editor_page_exists_and_speaks_the_bridge_it_is_given() {
    let manifest = std::fs::read_to_string("aoe-plugin.toml").expect("aoe-plugin.toml");
    let mut entries: Vec<(String, String)> = Vec::new();
    let mut slot = String::new();
    for line in manifest.lines() {
        let line = line.trim();
        if line == "[[ui]]" {
            slot.clear();
            continue;
        }
        if let Some(value) = line.strip_prefix("slot = ") {
            slot = value.trim_matches('"').to_string();
        }
        if slot == "page" {
            if let Some(id) = line.strip_prefix("id = ") {
                entries.push((id.trim_matches('"').to_string(), String::new()));
            }
            if let Some(entry) = line.strip_prefix("entry = ") {
                if let Some(last) = entries.last_mut() {
                    last.1 = entry.trim_matches('"').to_string();
                }
            }
        }
    }
    assert!(!entries.is_empty(), "the manifest declares no page");
    for (id, entry) in entries {
        assert!(!entry.is_empty(), "page {id} has no entry file");
        let body = std::fs::read_to_string(&entry)
            .unwrap_or_else(|err| panic!("page {id} entry {entry:?}: {err}"));
        // The dashboard opens a page at /plugins/<id>/<page id>, so the link the
        // worker pushes and the route the host serves must agree.
        let route = format!("/plugins/dev.oozut.llm-tags/{id}");
        assert!(
            std::fs::read_to_string("src/worker.rs")
                .expect("worker source")
                .contains(&route),
            "the worker never links to {route}"
        );
        for method in ["plugins.uiState", "action", "ready"] {
            assert!(
                body.contains(method),
                "page {id} never calls the {method} bridge method"
            );
        }
        // The bridge helper takes a message object and a transport. A call site
        // that passed its transport where the params belong made the page die
        // with "send is not a function" the moment the document ran, which no
        // static check of the method *names* would have caught.
        assert!(
            body.contains("function request(message, send)"),
            "page {id} changed the bridge helper's shape: it takes a message object and a \
             transport, and a third parameter turns every two-argument call into the \
             \"send is not a function\" crash"
        );
        let mut rest = body.as_str();
        while let Some(start) = rest.find("request(") {
            let after = &rest[start + "request(".len()..];
            if !after.starts_with("message") {
                assert!(
                    after.trim_start().starts_with('{'),
                    "page {id} calls request() with something other than a message object: {:?}",
                    &after[..after.len().min(40)]
                );
            }
            rest = after;
        }
        // One self-contained file: the host serves only this document.
        for forbidden in ["<script src=", "<link ", "fetch(", "XMLHttpRequest"] {
            assert!(
                !body.contains(forbidden),
                "page {id} leans on {forbidden:?}, which the sandbox blocks"
            );
        }
    }
}

/// The host forwards a pane or page `method` to the worker **verbatim**, so the
/// strings in the page and the ones the worker dispatches on have to be the same
/// strings. A page that prefixed them with the plugin id — a natural mistake,
/// since a host command is `plugin.<id>.<command>` — would send edits that the
/// worker silently ignores.
#[test]
fn the_page_fires_the_methods_the_worker_dispatches_on() {
    let page = std::fs::read_to_string("ui/tags.html").expect("ui/tags.html");
    let mut fired: Vec<String> = Vec::new();
    // `edit` is a session edit; `extensionAction` is the machine-wide pair, which
    // deliberately has no session to send. Both reach the worker the same way.
    for call in ["edit(\"", "extensionAction(\""] {
        let mut rest = page.as_str();
        while let Some(start) = rest.find(call) {
            let after = &rest[start + call.len()..];
            let end = after.find('"').expect("a closing quote");
            fired.push(after[..end].to_string());
            rest = &after[end..];
        }
    }
    assert!(!fired.is_empty(), "the page fires no worker method");
    for method in &fired {
        assert!(
            llm_tags::worker::EDITS.contains(&method.as_str()),
            "the page fires {method:?}, which the worker does not dispatch on {:?}",
            llm_tags::worker::EDITS
        );
    }
    // And the reverse: every edit the worker knows is reachable from the page.
    for method in llm_tags::worker::EDITS {
        assert!(
            fired.contains(&method.to_string()),
            "the worker dispatches {method:?} but the page never fires it"
        );
    }
}

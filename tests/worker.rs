//! The worker over its real wire: one JSON-RPC line in, one out.
//!
//! `src/worker.rs`'s own tests cover a single line in isolation. These start the
//! binary the host starts — no arguments, newline-delimited JSON-RPC on stdio —
//! answer the host calls it makes (`sessions.list`, `config.get`) and read the
//! state it pushes back. That is the only way to see a tab button, the pane it
//! re-renders, the file behind both and the `references` verb agree, because the
//! agreement is what a user clicks on.

mod common;

use common::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const WORKER: &str = env!("CARGO_BIN_EXE_aoe-llm-tags-worker");
const SESSION: &str = "cf11af87da7e4bf0";
const OTHER: &str = "0123456789abcdef";
const STATUS: &str = "plugin.dev.oozut.llm-tags.status";

/// One target naming a git fact, so every render in this file reads the checkout.
const BRANCH_HEADER: &str = "X-AoE-Branch";
const BRANCH_TARGET: &str =
    "targets = [ { header = \"X-AoE-Branch\", template = \"{git.branch}\", env_var = \"AOE_HDR_BRANCH\" } ]\n";

// --- a real checkout --------------------------------------------------------

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git must be installed to run these tests");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A one-commit repository on `main`, so `{git.branch}`/`{git.commit}` are known.
fn repository(sandbox: &Sandbox, name: &str) -> PathBuf {
    let path = sandbox.path(name);
    std::fs::create_dir_all(&path).expect("repo dir");
    git(&path, &["init", "-q", "-b", "main"]);
    std::fs::write(path.join("README.md"), "first\n").expect("file");
    git(&path, &["add", "-A"]);
    git(
        &path,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            "first commit",
        ],
    );
    path
}

fn row(id: &str, repo: &Path) -> Value {
    let path = repo.display().to_string();
    json!({
        "id": id,
        "title": format!("Session {id}"),
        "path": path,
        "project_path": path,
        "group": "",
        "tool": "pi",
        "profile": "main",
    })
}

// --- the wire ---------------------------------------------------------------

struct Wire {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<Value>,
    settings: BTreeMap<String, Value>,
    /// `config.get` answers for the project the host was asked about, the way the
    /// host does: the project's value when it has one, the global one otherwise.
    project_settings: BTreeMap<(String, String), Value>,
    rows: Vec<Value>,
    /// Every host call the worker made, in order: `(method, params)`.
    calls: Vec<(String, Value)>,
    /// Every `ui.state.set` it pushed: the pane, and nothing else.
    pushes: Vec<Value>,
    log: Arc<Mutex<String>>,
    next_id: i64,
}

impl Wire {
    fn open(sandbox: &Sandbox, rows: &[Value], extra: &[(&str, &str)]) -> Self {
        let mut command = Command::new(WORKER);
        command
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &sandbox.env {
            command.env(key, value);
        }
        for (key, value) in extra {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn the worker");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
        // Diagnostics go to stderr; a failing assertion wants them.
        let log = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&log);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if let Ok(mut text) = sink.lock() {
                    text.push_str(&line);
                    text.push('\n');
                }
            }
        });
        let mut wire = Self {
            child,
            stdin,
            lines,
            settings: settings_of(sandbox),
            project_settings: BTreeMap::new(),
            rows: rows.to_vec(),
            calls: Vec::new(),
            pushes: Vec::new(),
            log,
            next_id: 900,
        };
        wire.drain_startup();
        wire
    }

    /// Answer the publish the worker makes before it reads anything: it owns a
    /// fresh UI generation at spawn, so the host has dropped what it showed before
    /// the reload. Every test starts from that publish, and `pushes` keeps it, so a
    /// test can still assert it happened.
    fn drain_startup(&mut self) {
        // A workspace with no sessions has no pane to push, so "quiet" is the end
        // of the publish as much as a push is.
        for _ in 0..400 {
            match self.lines.recv_timeout(Duration::from_millis(300)) {
                Ok(line) => {
                    if line.get("method").is_some() {
                        if line["method"] == "ui.state.set" {
                            self.pushes.push(line["params"].clone());
                        }
                        let answer = self.answer(&line);
                        self.send(&answer);
                    }
                }
                Err(_) => return,
            }
        }
        panic!(
            "the worker never finished publishing at startup; stderr:\n{}",
            self.log()
        );
    }

    /// A `per_project` setting as the host would hand it back for one project.
    fn set_project(&mut self, path: &Path, key: &str, value: Value) {
        self.project_settings
            .insert((path.display().to_string(), key.to_string()), value);
    }

    fn log(&self) -> String {
        self.log.lock().map(|text| text.clone()).unwrap_or_default()
    }

    fn send(&mut self, value: &Value) {
        writeln!(self.stdin, "{value}").expect("write to the worker");
        self.stdin.flush().expect("flush");
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// A request, waiting for the response with the id this test picked.
    fn ask(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        for _ in 0..400 {
            let line = self.next_line();
            if line.get("method").is_none() && line["id"] == json!(id) {
                return line;
            }
        }
        panic!("no response to {method}; stderr:\n{}", self.log());
    }

    fn next_line(&mut self) -> Value {
        match self.lines.recv_timeout(Duration::from_secs(10)) {
            Ok(line) => {
                if line.get("method").is_some() {
                    if line["method"] == "ui.state.set" {
                        self.pushes.push(line["params"].clone());
                    }
                    let answer = self.answer(&line);
                    self.send(&answer);
                }
                line
            }
            Err(_) => panic!("the worker went quiet; stderr:\n{}", self.log()),
        }
    }

    /// The host's side of one call: sessions and settings, nothing else.
    fn answer(&mut self, request: &Value) -> Value {
        let method = request["method"].as_str().unwrap_or_default().to_string();
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        let result = match method.as_str() {
            "sessions.list" => json!({ "sessions": self.rows }),
            "config.get" => {
                let key = params["key"].as_str().unwrap_or_default();
                let project = params["project_path"].as_str().unwrap_or_default();
                let value = self
                    .project_settings
                    .get(&(project.to_string(), key.to_string()))
                    .or_else(|| self.settings.get(key))
                    .cloned()
                    .unwrap_or(Value::Null);
                json!({ "value": value })
            }
            _ => json!({}),
        };
        self.calls.push((method, params));
        json!({
            "jsonrpc": "2.0",
            "id": request["id"].clone(),
            "result": result,
        })
    }

    /// The last payload of the one pane, as that session's open tab has it.
    fn pane(&self, session: &str) -> &Value {
        self.pushes
            .iter()
            .rev()
            .find(|params| {
                params["slot"] == json!("pane")
                    && params["session_id"] == json!(session)
                    && params["id"] == json!("llm_tags")
            })
            .map(|params| &params["payload"])
            .unwrap_or_else(|| panic!("no llm_tags pane was pushed for {session}"))
    }

    /// Click a tab and read what the pane renders for it. The click is a worker
    /// method like any other, and the republish after it is the answer.
    fn pane_tab(&mut self, session: &str, tab: &str) -> Value {
        self.notify("pane.tab", json!({"tab": tab, "session_id": session}));
        for _ in 0..400 {
            let line = self.next_line();
            if line["method"] == json!("ui.state.set")
                && line["params"]["slot"] == json!("pane")
                && line["params"]["id"] == json!("llm_tags")
                && line["params"]["session_id"] == json!(session)
            {
                return line["params"]["payload"].clone();
            }
        }
        panic!(
            "the pane never re-rendered for tab {tab}; stderr:\n{}",
            self.log()
        );
    }

    /// Every payload pushed so far, as one string: a value that must not leave the
    /// session file has no business anywhere in here.
    fn pushed_text(&self) -> String {
        self.pushes
            .iter()
            .map(|params| params.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Read pushes until the pane for a session arrives, answering each call.
    fn wait_pane(&mut self, session: &str) -> Value {
        for _ in 0..400 {
            let line = self.next_line();
            if line["method"] == "ui.state.set"
                && line["params"]["slot"] == json!("pane")
                && line["params"]["session_id"] == json!(session)
            {
                return line["params"]["payload"].clone();
            }
        }
        panic!(
            "the worker never pushed a pane for {session}; stderr:\n{}",
            self.log()
        );
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The plugin's settings as the host hands them back: TOML in, JSON out.
fn settings_of(sandbox: &Sandbox) -> BTreeMap<String, Value> {
    let body = std::fs::read_to_string(sandbox.config_path("")).unwrap_or_default();
    let table: toml::Table = body.parse().unwrap_or_default();
    let settings = table
        .get("plugins")
        .and_then(|plugins| plugins.get(PLUGIN_ID))
        .and_then(|plugin| plugin.get("settings"))
        .and_then(|settings| settings.as_table())
        .cloned()
        .unwrap_or_default();
    settings
        .iter()
        .map(|(key, value)| {
            let value = serde_json::to_value(value).expect("settings as JSON");
            (key.clone(), value)
        })
        .collect()
}

fn overrides_path(sandbox: &Sandbox, session: &str) -> PathBuf {
    let env = llm_tags::env::Env::new(sandbox.env.clone());
    let app_dir = llm_tags::settings::app_dir(&env).expect("app dir");
    llm_tags::session_tags::path(&app_dir, session)
}

/// The session tab's tag rows: the pane's answer to "what does this session
/// carry, and where did each entry come from". A row's label is the key, except
/// for the activity row, which names what the session is doing instead — so
/// `tag(rows, "activity")` finds it by the method that edits it, not the label.
fn pane_tags(pane: &Value) -> Vec<Value> {
    pane_rows(pane)
        .into_iter()
        .filter(|row| row["method"] == json!("tag.remove") && row["params"]["origin"].is_string())
        .collect()
}

/// One tag row by key: the session tab's rows are keyed by label, except the
/// activity row, which names what the session is doing — so an activity lookup
/// goes through the method that edits it.
fn tag<'a>(tags: &'a [Value], key: &str) -> &'a Value {
    tags.iter()
        .find(|row| {
            row["label"] == json!(key)
                || (row["params"]["key"] == json!(key) && row["params"]["origin"].is_string())
        })
        .unwrap_or(&Value::Null)
}

/// Where a tag row says its entry came from, in the pane's own words.
fn origin_of(row: &Value) -> String {
    row["badges"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The header names the "What this session sends" section lists, top to bottom:
/// a target row is the only row with no method (it is a read-out, not a button).
fn pane_targets(pane: &Value) -> Vec<String> {
    pane_rows(pane)
        .into_iter()
        .filter(|row| row.get("method").is_none() && row["badges"].is_null())
        .map(|row| row["label"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Every block in a pane, in render order, however deeply nested — the tab strip
/// is a `columns` inside a `columns`, and its buttons are two levels down.
fn pane_blocks(pane: &Value) -> Vec<Value> {
    fn walk(block: &Value, out: &mut Vec<Value>) {
        out.push(block.clone());
        for child in block["children"].as_array().into_iter().flatten() {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    for block in pane["blocks"].as_array().expect("pane blocks") {
        walk(block, &mut out);
    }
    out
}

/// Every row in a pane, whether it sits at the top level, inside a section or in
/// one of the tab strip's columns.
fn pane_rows(pane: &Value) -> Vec<Value> {
    pane_blocks(pane)
        .into_iter()
        .filter(|block| block["kind"] == json!("row"))
        .collect()
}

/// The labels of the pane's tab strip, left to right, and which one is open. The
/// open tab is the host's `primary` action, since an action has no `selected`.
fn tab_labels(pane: &Value) -> Vec<String> {
    pane_blocks(pane)
        .into_iter()
        .filter(|block| block["method"] == json!("pane.tab"))
        .map(|block| block["label"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// One tab button, by its label — the strip's buttons are actions, not rows.
fn tab_button(pane: &Value, label: &str) -> Value {
    pane_blocks(pane)
        .into_iter()
        .find(|block| block["method"] == json!("pane.tab") && block["label"] == json!(label))
        .unwrap_or_else(|| panic!("no {label:?} tab button in {pane}"))
}

fn open_tab(pane: &Value) -> String {
    pane_blocks(pane)
        .into_iter()
        .find(|block| block["method"] == json!("pane.tab") && block["variant"] == json!("primary"))
        .and_then(|block| block["label"].as_str().map(str::to_string))
        .unwrap_or_default()
}

fn pane_row(rows: &[Value], label: &str) -> Value {
    rows.iter()
        .find(|row| row["label"] == json!(label))
        .unwrap_or_else(|| panic!("no {label:?} row in {rows:?}"))
        .clone()
}

// --- the tests --------------------------------------------------------------

#[test]
fn ticking_the_git_box_writes_the_sample_and_the_pane_renders_it() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    sandbox.write_config(&settings_config(BRANCH_TARGET, ""));

    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);
    wire.notify(
        "tag.preset",
        json!({"preset": "git", "applied": true, "session_id": SESSION}),
    );
    let pane = wire.wait_pane(SESSION);

    // The pane renders the set: `git.branch` is the branch, not the reference it
    // was written as.
    let rows = pane_rows(&pane);
    let branch = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let commit = git(&repo, &["rev-parse", "--short", "HEAD"]);
    assert_eq!(pane_row(&rows, "git.branch")["value"], json!(branch));
    assert_eq!(pane_row(&rows, "git.commit")["value"], json!(commit));
    assert_eq!(pane_row(&rows, "vcs")["value"], json!("git"));
    // Every entry came from this session's file, not from the settings.
    let tags = pane_tags(&pane);
    assert!(
        tags.iter().all(|row| origin_of(row) == "this session"),
        "{tags:?}"
    );
    assert_eq!(pane_targets(&pane), vec![BRANCH_HEADER.to_string()]);
    assert_eq!(pane_row(&rows, BRANCH_HEADER)["value"], json!(branch));

    // Behind the pane: ordinary tag grammar, one session's file, settings
    // untouched. The stored entry is the reference — editing a rendered branch
    // would freeze it — and the pane is what turns it into a value.
    let file = overrides_path(&sandbox, SESSION);
    let text = std::fs::read_to_string(&file).expect("overrides.toml");
    assert!(text.contains(r#""vcs:git""#), "{text}");
    assert!(text.contains(r#""git.branch:{git.branch}""#), "{text}");
    assert!(text.contains(r#""git.commit:{git.commit}""#), "{text}");
    let settings = std::fs::read_to_string(sandbox.config_path("")).expect("settings");
    assert!(!settings.contains("vcs:git"), "{settings}");

    // The tick writes what the preset spells, and the sample's references are
    // ones the resolver knows: a reference that could not be resolved would be
    // written into the file and rendered as its own braces.
    for raw in llm_tags::resolver::GIT_PRESET {
        assert!(text.contains(raw), "the tick did not write {raw:?}: {text}");
    }
    let known: Vec<String> = llm_tags::resolver::reference_catalog()
        .iter()
        .map(|item| item.name.to_string())
        .collect();
    assert!(known.iter().any(|name| name == "git.subject"), "{known:?}");
}

#[test]
fn unticking_the_box_brings_the_configured_value_back() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    // A tag the settings own, which the sample writes over.
    sandbox.write_config(&settings_config(
        "tags = [\"vcs:prod\", \"team:core\"]\n",
        "",
    ));

    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);
    wire.notify(
        "tag.preset",
        json!({"preset": "git", "applied": true, "session_id": SESSION}),
    );
    let pane = wire.wait_pane(SESSION);
    let tags = pane_tags(&pane);
    assert_eq!(pane_row(&pane_rows(&pane), "vcs")["value"], json!("git"));
    assert_eq!(origin_of(tag(&tags, "vcs")), "this session");

    wire.notify(
        "tag.preset",
        json!({"preset": "git", "applied": false, "session_id": SESSION}),
    );
    let pane = wire.wait_pane(SESSION);
    let rows = pane_rows(&pane);
    // Unticking is an undo: the settings' value is back, the sample's is gone.
    assert_eq!(pane_row(&rows, "vcs")["value"], json!("prod"));
    assert_eq!(origin_of(&pane_row(&rows, "vcs")), "settings");
    assert_eq!(pane_row(&rows, "team")["value"], json!("core"));
    assert!(
        !rows.iter().any(|row| row["label"] == json!("git.branch")),
        "the sample's keys are gone: {pane}"
    );
    assert!(
        !overrides_path(&sandbox, SESSION).exists(),
        "nothing is left to hide"
    );
}

/// The switch is a declaration, not a copy: a session can hide a sample that a
/// level above it includes, and turning it back on makes the entries its own.
#[test]
fn the_git_switch_can_hide_an_inherited_sample() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    sandbox.write_config(&settings_config("", ""));
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);

    wire.notify(
        "tag.level.git",
        json!({"level": "aoe", "enabled": true, "session_id": SESSION}),
    );
    let pane = wire.wait_pane(SESSION);
    assert_eq!(origin_of(tag(&pane_tags(&pane), "git.branch")), "AoE");
    let aoe = wire.pane_tab(SESSION, "aoe");
    assert_eq!(
        pane_row(&pane_rows(&aoe), "Standard git information")["selected"],
        json!(true)
    );

    // Hiding it here must win over the AoE default, which a per-key `tag.remove`
    // could not do: the level that set it would still carry it.
    wire.notify("tag.git", json!({"enabled": false, "session_id": SESSION}));
    let pane = wire.wait_pane(SESSION);
    assert!(
        !pane_rows(&pane)
            .iter()
            .any(|row| row["label"] == json!("git.branch")),
        "{pane}"
    );
    let session = wire.pane_tab(SESSION, "session");
    let switch = pane_row(&pane_rows(&session), "Standard git information");
    assert_eq!(switch["value"], json!("off here"));
    assert_eq!(switch["method"], json!("tag.git"));
    assert_eq!(switch["params"]["enabled"], json!(true));
    let file = overrides_path(&sandbox, SESSION);
    let text = std::fs::read_to_string(&file).expect("overrides.toml");
    assert!(text.contains("git_tags = false"), "{text}");

    // Turning it back on materialises the sample for this session, so the pane
    // can label it as this session's own again.
    wire.notify("tag.git", json!({"enabled": true, "session_id": SESSION}));
    let pane = wire.wait_pane(SESSION);
    assert_eq!(
        origin_of(tag(&pane_tags(&pane), "git.branch")),
        "this session"
    );
    let text = std::fs::read_to_string(&file).expect("overrides.toml");
    assert!(text.contains("git_tags = true"), "{text}");
}

#[test]
fn a_status_request_over_the_wire_renders_this_sessions_git_fact() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    sandbox.write_config(&settings_config(BRANCH_TARGET, ""));
    // `aoe session show` is what tells the worker which checkout this session is
    // in, so the fake host has to answer for this repository.
    let aoe = sandbox.fake_aoe(&row(SESSION, &repo).to_string());
    let aoe = aoe.display().to_string();

    let mut wire = Wire::open(&sandbox, &[], &[("AOE_BIN", &aoe)]);
    let response = wire.ask(STATUS, json!({}));
    let result = &response["result"];
    assert_eq!(result["ok"], json!(true), "{response}");
    let branch = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(
        result["headers"]["X-AoE-Branch"],
        json!(branch),
        "{response}"
    );
    assert!(
        result["warnings"]
            .as_array()
            .map(Vec::is_empty)
            .unwrap_or(false),
        "a work tree needs no warning: {response}"
    );
}

#[test]
fn two_sessions_keep_their_own_edits() {
    let sandbox = Sandbox::new();
    let one = repository(&sandbox, "repo-one");
    let two = repository(&sandbox, "repo-two");
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &one), row(OTHER, &two)], &[]);

    wire.notify(
        "tag.set",
        json!({"key": "team", "value": "core", "session_id": SESSION}),
    );
    wire.wait_pane(SESSION);
    wire.notify(
        "tag.set",
        json!({"key": "team", "value": "platform", "session_id": OTHER}),
    );
    let other = wire.wait_pane(OTHER);

    for (id, expected, pane) in [
        (SESSION, "core", wire.pane(SESSION).clone()),
        (OTHER, "platform", other),
    ] {
        assert_eq!(
            pane_row(&pane_rows(&pane), "team")["value"],
            json!(expected),
            "{id}"
        );
        let env = llm_tags::env::Env::new(sandbox.env.clone());
        let app_dir = llm_tags::settings::app_dir(&env).expect("app dir");
        let (edits, warnings) = llm_tags::session_tags::load(&app_dir, id);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            edits.entries(),
            vec![("team".to_string(), json!(expected))],
            "{id} carries someone else's edit"
        );
    }
}

/// A literal token stands in for the rendered payload, so the row that shows what
/// a session sends would be the token itself. The host keeps every `ui.state.set`
/// payload and renders it to everyone who can see the session, so the value never
/// leaves the session file: the pane says one is set and names it in the row.
///
/// The token is written the way the CLI writes it — the pane has no box for it —
/// and the test reads the republish, which is the path that could leak it.
#[test]
fn a_token_override_is_never_published() {
    const SECRET: &str = "sk-live-3f9c2a1b-do-not-publish";
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    // The default target has no template, so the token is what it renders.
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);

    let env = llm_tags::env::Env::new(sandbox.env.clone());
    let app_dir = llm_tags::settings::app_dir(&env).expect("app dir");
    llm_tags::session_tags::set_token(&app_dir, SESSION, SECRET).expect("write the token");
    wire.notify("session.state", json!({}));
    let pane = wire.wait_pane(SESSION);

    let rows = pane_rows(&pane);
    assert_eq!(
        pane_row(&rows, "X-AoE-Tags")["value"],
        json!("(the session's token, not shown here)"),
        "{pane}"
    );
    let pushed = wire.pushed_text();
    assert!(
        !pushed.contains(SECRET),
        "the value was published: {pushed}"
    );
    assert!(pushed.contains("a token override is set"), "{pushed}");
    assert!(!wire.log().contains(SECRET), "{}", wire.log());

    // It is kept, though: it is what the launch sends instead of the payload.
    let (edits, _) = llm_tags::session_tags::load(&app_dir, SESSION);
    assert_eq!(edits.token, SECRET);

    // Clearing it takes that back, still without publishing it.
    llm_tags::session_tags::set_token(&app_dir, SESSION, "").expect("clear the token");
    wire.notify("session.state", json!({}));
    let pane = wire.wait_pane(SESSION);
    assert!(
        !pane_rows(&pane)
            .iter()
            .any(|row| row["value"] == json!("(the session's token, not shown here)")),
        "{pane}"
    );
    assert!(!wire.pushed_text().contains(SECRET));
}

#[test]
fn an_edit_without_a_session_id_is_refused_and_the_worker_keeps_answering() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);

    // The host always merges `session_id` in; a guess here would edit the wrong
    // session's file, so the edit is refused — and a refusal is not a crash.
    wire.notify("tag.set", json!({"key": "team", "value": "core"}));
    wire.wait_pane(SESSION);
    assert!(!overrides_path(&sandbox, SESSION).exists());
    assert!(wire.log().contains("no session_id"), "{}", wire.log());

    let response = wire.ask(STATUS, json!({}));
    assert_eq!(response["result"]["ok"], json!(true), "{response}");

    // A method the pane does not offer is not an edit either.
    wire.notify(
        "tag.nonsense",
        json!({"session_id": SESSION, "key": "team"}),
    );
    wire.wait_pane(SESSION);
    assert!(!overrides_path(&sandbox, SESSION).exists());
}

#[test]
fn an_unknown_request_is_answered_not_found() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);

    let response = wire.ask("plugin.dev.oozut.llm-tags.nonsense", json!({}));
    assert_eq!(
        response["error"]["code"],
        json!(llm_tags::worker::METHOD_NOT_FOUND),
        "{response}"
    );
}

/// A live file exists only for a session launched with an approved extension, and
/// it is how an edit reaches a request the session has not made yet. The worker is
/// the only place that sees the edit and the new resolution together.
#[test]
fn a_live_file_follows_an_edit_and_a_revoke_without_a_relaunch() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    sandbox.write_config(&settings_config(
        "harness_extension = [\"pi\"]\n\
         [[plugins.\"dev.oozut.llm-tags\".settings.targets]]\n\
         kind = \"header\"\nname = \"X-AoE-Tags\"\ntemplate = \"aoe,{tags}\"\nenv_var = \"AOE_LLM_TAGS\"\n",
        "",
    ));
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    let printed = sandbox.run(&["extension", "enable", "pi", "--print"]);
    printed.assert_ok();
    let digest = printed
        .stdout
        .lines()
        .find_map(|line| line.split("--accept ").nth(1))
        .map(str::trim)
        .expect("the printed approve command");
    sandbox
        .run(&["extension", "enable", "pi", "--accept", digest])
        .assert_ok();

    // A launch: this is what puts the extension and the live file on disk.
    let launched = sandbox.run(&["hook"]);
    launched.assert_ok();
    let live = std::path::PathBuf::from(
        launched
            .lines()
            .into_iter()
            .find_map(|line| line.strip_prefix("AOE_LLM_TAGS_LIVE="))
            .expect("the live file"),
    );
    let read = || -> Value {
        serde_json::from_str(&std::fs::read_to_string(&live).expect("live.json")).expect("json")
    };
    assert_eq!(read()["enabled"], json!(true));

    // An edit from the pane: no relaunch, no second hook, one publish.
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);
    wire.notify(
        "tag.set",
        json!({"session_id": SESSION, "key": "activity", "value": "test"}),
    );
    wire.wait_pane(SESSION);
    let body = read();
    assert_eq!(
        body["targets"][0]["value"],
        json!("aoe,activity:test"),
        "{body}"
    );
    assert_eq!(body["enabled"], json!(true));

    // A revoke: the same file flips, so a session that is already running stops
    // sending dynamic values on its next request rather than at its next launch.
    sandbox.run(&["extension", "disable", "pi"]).assert_ok();
    wire.notify(
        "tag.set",
        json!({"session_id": SESSION, "key": "activity", "value": "review"}),
    );
    wire.wait_pane(SESSION);
    let body = read();
    assert_eq!(body["enabled"], json!(false), "{body}");
    assert_eq!(body["targets"][0]["value"], json!("aoe,activity:review"));
}

/// A session that never got an extension gets no live file: the worker must not
/// invent a delivery the launch did not set up.
#[test]
fn no_live_file_is_written_for_a_session_without_an_extension() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);
    wire.notify(
        "tag.set",
        json!({"session_id": SESSION, "key": "team", "value": "core"}),
    );
    wire.wait_pane(SESSION);
    let env = llm_tags::env::Env::new(sandbox.env.clone());
    let app_dir = llm_tags::settings::app_dir(&env).expect("app dir");
    assert!(!llm_tags::harness_extension::live_path(&app_dir, SESSION).exists());
}

/// The extension approval was a page: the disclosure, the digest a click approves,
/// and the revoke. A page is gone, and the pane never had those rows — so the
/// methods that served them are gone from the worker too, rather than left as
/// surface nobody can reach. `aoe-llm-tags extension enable|disable` is the way in,
/// and `tests/extension.rs` is where that path is checked.
#[test]
fn the_methods_only_a_page_could_fire_are_gone() {
    for method in [
        "token.set",
        "token.clear",
        "extension.enable",
        "extension.disable",
    ] {
        assert!(
            !llm_tags::worker::EDITS.contains(&method),
            "{method} is still offered to a surface, and no surface can fire it"
        );
    }
    // Nothing in the pane offers one either: the payload is the only surface.
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);
    wire.notify("session.state", json!({}));
    let pane = wire.wait_pane(SESSION);
    let offered: Vec<String> = pane_blocks(&pane)
        .into_iter()
        .filter_map(|block| block["method"].as_str().map(str::to_string))
        .collect();
    for method in [
        "token.set",
        "token.clear",
        "extension.enable",
        "extension.disable",
    ] {
        assert!(!offered.contains(&method.to_string()), "{offered:?}");
    }
}

/// The pane's dock tab and the header toggle draw the payload icon; with none the
/// host falls back to a puzzle piece that says nothing about the pane.
#[test]
fn the_pane_names_its_own_icon() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    sandbox.write_config(&settings_config("tags = [\"team:core\"]\n", ""));
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);
    wire.notify("session.state", json!({}));
    let pane = wire.pane(SESSION).clone();
    assert_eq!(pane["icon"], json!("tags"), "{pane}");
    assert_eq!(pane["title"], json!("LLM Tags"), "{pane}");

    // One dock entry means one glyph, whichever tab is open: there is no longer a
    // pane per level for a per-level icon to sit on, so the levels are told apart
    // by their button's word and no tab button carries a glyph either.
    for tab in ["session", "project", "aoe"] {
        let pane = wire.pane_tab(SESSION, tab);
        assert_eq!(pane["icon"], json!("tags"), "{tab}: {pane}");
        let blocks = pane_blocks(&pane);
        let buttons: Vec<&Value> = blocks
            .iter()
            .filter(|block| block["method"] == json!("pane.tab"))
            .collect();
        assert_eq!(buttons.len(), 3, "{tab}: {pane}");
        assert!(
            buttons.iter().all(|block| block.get("icon").is_none()),
            "{tab}: a tab button carries no glyph: {buttons:?}"
        );
    }
}

/// A reloaded worker owns a fresh UI generation, so the host has already dropped
/// what this plugin showed. Waiting for the next host event to publish leaves the
/// pane and its header button missing until some session happens to change state —
/// which for an idle workspace is never.
#[test]
fn the_worker_publishes_before_any_host_event() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    sandbox.write_config(&settings_config("tags = [\"team:core\"]\n", ""));
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    // No `notify`: `open` only answers what the worker asks for itself.
    let wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);
    assert!(
        wire.pushes
            .iter()
            .any(|p| p["slot"] == json!("pane") && p["session_id"] == json!(SESSION)),
        "the pane is published before any event: {:?}",
        wire.pushes
    );
    assert!(
        wire.pushes.iter().all(|p| p["slot"] == json!("pane")),
        "the pane is the only surface: {:?}",
        wire.pushes
    );
}

/// A tag has one home: the pane. The session header used to carry an activity
/// badge — a second copy of a fact the pane already states, and one more place to
/// keep in step — so nothing but the pane renders tag state now.
#[test]
fn the_pane_is_the_only_place_a_tag_shows() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    sandbox.write_config(&settings_config(
        "tags = [\"activity:test\", \"team:core\"]\n",
        "",
    ));
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);
    wire.notify("session.state", json!({}));
    wire.wait_pane(SESSION);

    assert!(
        wire.pushes
            .iter()
            .all(|params| params["slot"] == json!("pane")),
        "the pane is the only surface: {:?}",
        wire.pushes
    );
    let manifest = std::fs::read_to_string("aoe-plugin.toml").expect("the manifest");
    assert!(
        !manifest.contains("row-badge"),
        "the manifest declares no row badge"
    );
    assert!(
        !manifest.contains("slot = \"page\""),
        "the manifest declares no page, so the dashboard header carries no LLM Tags button"
    );

    // The pane still names what the session is doing, with the family's own icon
    // and tone, so dropping the badge lost no information.
    let pane = wire.pane(SESSION).clone();
    let section = pane_blocks(&pane)
        .into_iter()
        .find(|block| block["title"] == json!("Activity"))
        .unwrap_or_else(|| panic!("no activity section in {pane}"));
    assert_eq!(section["value"], json!("testing (inherited)"), "{section}");
    let row = pane_row(&pane_rows(&pane), "testing");
    assert_eq!(row["icon"], json!("flask-conical"), "{row}");
    assert_eq!(row["tone"], json!("warn"), "{row}");
}

#[test]
fn a_session_without_an_activity_shows_none_in_the_pane() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    sandbox.write_config(&settings_config("tags = [\"team:core\"]\n", ""));
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);
    wire.notify("session.state", json!({}));
    wire.wait_pane(SESSION);
    let pane = wire.pane(SESSION).clone();
    let section = pane_blocks(&pane)
        .into_iter()
        .find(|block| block["title"] == json!("Activity"))
        .expect("an activity section");
    assert_eq!(section["value"], json!("not set"), "{section}");
}

#[test]
fn a_typo_in_the_activity_is_reported_not_iconified() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    sandbox.write_config(&settings_config(
        "tags = [\"activity:testng\", \"team:core\"]\n",
        "",
    ));
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);
    wire.notify("session.state", json!({}));
    wire.wait_pane(SESSION);
    assert!(wire.log().contains("is not an activity"), "{}", wire.log());
    // A typo is not an activity, so the pane leaves the section unset rather than
    // dressing the misspelling up as one.
    let pane = wire.pane(SESSION).clone();
    let section = pane_blocks(&pane)
        .into_iter()
        .find(|block| block["title"] == json!("Activity"))
        .expect("an activity section");
    assert_eq!(section["value"], json!("not set"), "{section}");
}

/// The activity has to be changeable from inside the session, not only from the
/// dashboard page: the pane is what the session view and the terminal UI show, so
/// each value is a row that fires `tag.set`, with the current one marked.
#[test]
fn the_pane_offers_every_activity_and_marks_the_current_one() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    sandbox.write_config(&settings_config("tags = [\"activity:debug\"]\n", ""));
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);
    wire.notify("session.state", json!({}));
    wire.wait_pane(SESSION);

    let pane = wire.pane(SESSION);
    let blocks = pane["blocks"].as_array().expect("blocks");
    let section = blocks
        .iter()
        .find(|block| block["kind"] == json!("section") && block["title"] == json!("Activity"))
        .unwrap_or_else(|| panic!("no Activity section in {pane}"));
    // A row per word, then one note saying which family those words are.
    let children = section["children"].as_array().expect("children");
    let rows: Vec<&serde_json::Value> = children
        .iter()
        .filter(|child| child["kind"] == json!("row"))
        .collect();
    let candidates: Vec<&serde_json::Value> = rows
        .iter()
        .copied()
        .filter(|row| row["method"] == json!("tag.set"))
        .collect();
    assert_eq!(
        candidates.len(),
        9,
        "the whole vocabulary is offered: {section}"
    );
    let selected: Vec<&str> = rows
        .iter()
        .filter(|row| row["selected"] == json!(true))
        .map(|row| row["label"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(selected, vec!["debugging"], "{section}");
    for row in candidates {
        assert_eq!(row["params"]["key"], json!("activity"), "{row}");
    }
    assert!(
        rows.iter().any(|row| row["method"] == json!("tag.restore")),
        "{section}"
    );
    assert!(
        rows.iter().any(|row| row["method"] == json!("tag.remove")),
        "{section}"
    );
    // The pane is text-and-tone first: every row has a label, and the icon is
    // decoration the web renders and the terminal ignores.
    assert!(children
        .iter()
        .all(|row| row["label"].is_string() || row["kind"] == json!("note")));
    // The pane is now the only surface that says which family a session is in.
    let note = children
        .iter()
        .find(|child| child["kind"] == json!("note"))
        .expect("the family note");
    assert_eq!(note["text"], json!("family default (built-in)"), "{note}");
}

/// A family is per project, so the pane offers the project's words — not the
/// built-in nine, and not another project's — and names the family they came
/// from. The pane is the surface that edits the activity, so this is the whole
/// contract: one resolution, one place it is shown.
#[test]
fn the_project_family_is_what_the_pane_offers() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    sandbox.write_config(&settings_config("tags = [\"activity:write\"]\n", ""));
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);
    wire.set_project(&repo, "activity_family", json!("work"));
    wire.set_project(
        &repo,
        "activity_families",
        json!([
            {"family": "work", "value": "write", "label": "writing", "icon": "pen-line", "tone": "info"},
            {"family": "work", "value": "measure", "label": "measuring", "icon": "ruler"},
        ]),
    );
    wire.notify("session.state", json!({}));
    wire.wait_pane(SESSION);

    let pane = wire.pane(SESSION);
    let blocks = pane["blocks"].as_array().expect("blocks");
    // Exactly one: the activity is offered once, in the session's own section,
    // and a level above it does not repeat the picker.
    let sections: Vec<&serde_json::Value> = blocks
        .iter()
        .filter(|block| block["kind"] == json!("section") && block["title"] == json!("Activity"))
        .collect();
    assert_eq!(sections.len(), 1, "{pane}");
    let section = sections[0];
    let children = section["children"].as_array().expect("children");
    let rows: Vec<&serde_json::Value> = children
        .iter()
        .filter(|child| child["kind"] == json!("row"))
        .collect();
    let labels: Vec<&str> = rows
        .iter()
        .filter(|row| row["method"] == json!("tag.set"))
        .filter_map(|row| row["label"].as_str())
        .collect();
    assert_eq!(labels, vec!["writing", "measuring"], "{section}");
    let selected: Vec<&str> = rows
        .iter()
        .filter(|row| row["selected"] == json!(true))
        .map(|row| row["label"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(selected, vec!["writing"], "{section}");

    // The words come from the project, and the pane says so — including the
    // built-in family the project did not replace, which is still defined here.
    let note = children
        .iter()
        .find(|child| child["kind"] == json!("note"))
        .expect("the family note");
    let text = note["text"].as_str().unwrap_or_default();
    assert!(text.starts_with("family work ("), "{text}");
    assert!(text.contains("also defined here: default"), "{text}");
}

/// The header that carries the tags is a decision per project, so what a session's
/// pane lists as "what this session sends" follows its project's own choice while
/// another project keeps the plugin's header. Nothing about it is per session: a
/// session cannot name it.
#[test]
fn the_project_chooses_which_header_carries_its_tags() {
    let sandbox = Sandbox::new();
    let tagged = repository(&sandbox, "tagged");
    let plain = repository(&sandbox, "plain");
    sandbox.write_config(&settings_config("tags = [\"team:core\"]\n", ""));
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &tagged), row(OTHER, &plain)], &[]);
    wire.set_project(&tagged, "request_tag_header", json!("databricks"));
    wire.notify("session.state", json!({}));
    let tagged_pane = wire.wait_pane(SESSION);
    let plain_pane = wire.wait_pane(OTHER);

    assert_eq!(
        pane_targets(&tagged_pane),
        vec!["Databricks-Ai-Gateway-Request-Tags".to_string()],
        "the project named the header"
    );
    assert_eq!(
        pane_targets(&plain_pane),
        vec!["X-AoE-Tags".to_string()],
        "another project keeps the plugin's own header"
    );

    // The switch is per project and per AoE, never per session: no block of the
    // pane offers a header name to a session.
    let pane = wire.pane(SESSION);
    let mut blocks: Vec<&Value> = Vec::new();
    fn walk<'a>(list: &'a [Value], out: &mut Vec<&'a Value>) {
        for block in list {
            out.push(block);
            if let Some(children) = block["children"].as_array() {
                walk(children, out);
            }
        }
    }
    walk(pane["blocks"].as_array().expect("blocks"), &mut blocks);
    assert!(
        !blocks
            .iter()
            .any(|block| block["label"] == json!("Header that carries the tags")),
        "a session must not be offered the header choice: {pane}"
    );
}

#[test]
fn a_session_without_a_project_has_no_project_tab() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    let mut session = row(SESSION, &repo);
    session["project_path"] = json!("");
    sandbox.write_config(&settings_config("", ""));
    let mut wire = Wire::open(&sandbox, &[session], &[]);
    wire.notify("session.state", json!({}));
    wire.wait_pane(SESSION);

    // One pane with two tabs, not three: a session with no project has no
    // project default to edit, so the strip leaves the row out instead of
    // offering a dead end.
    let session = wire.pane(SESSION).clone();
    assert_eq!(tab_labels(&session), vec!["Session", "AoE"]);
    // Asking for it anyway (a stale click, a dashboard left open) stays on the
    // session rather than showing an empty half.
    let after = wire.pane_tab(SESSION, "project");
    assert_eq!(open_tab(&after), "Session");
    let titles: Vec<&str> = after["blocks"]
        .as_array()
        .expect("blocks")
        .iter()
        .filter_map(|block| block["title"].as_str())
        .collect();
    assert_eq!(titles, vec!["Activity"], "{after}");
}

#[test]
fn the_three_tabs_split_session_project_and_aoe() {
    let sandbox = Sandbox::new();
    let one = repository(&sandbox, "one");
    let two = repository(&sandbox, "two");
    sandbox.write_config(&settings_config(
        &format!("{BRANCH_TARGET}tags = [\"team:config\", \"cost:=1\"]\n"),
        "",
    ));
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &one), row(OTHER, &two)], &[]);
    // Any host event republishes every surface; this one carries no edit.
    wire.notify("session.status.changed", json!({"session_id": SESSION}));
    wire.wait_pane(SESSION);

    // One dock entry, three tabs: the right panel carries a single LLM Tags
    // pane, and the strip inside it names the levels from the session outwards.
    assert!(
        wire.pushes
            .iter()
            .filter(|params| params["slot"] == json!("pane"))
            .all(|params| params["id"] == json!("llm_tags")),
        "only one pane is pushed: {:?}",
        wire.pushes
    );
    let session = wire.pane(SESSION).clone();
    assert_eq!(session["title"], json!("LLM Tags"));
    assert_eq!(tab_labels(&session), vec!["Session", "Project", "AoE"]);
    assert_eq!(open_tab(&session), "Session");
    // The strip is one row of buttons, not a list: three tabs side by side at the
    // top, each a `columns` cell, and the open one is the host's primary action.
    let strip = pane_blocks(&session);
    let buttons: Vec<&Value> = strip
        .iter()
        .filter(|block| block["method"] == json!("pane.tab"))
        .collect();
    assert_eq!(buttons.len(), 3, "{session}");
    assert!(
        buttons.iter().all(|block| block["kind"] == json!("action")),
        "a tab is a button: {buttons:?}"
    );
    // Every button's tooltip says what its level holds, in the same words, so the
    // strip answers "what have I set here" without opening the tab.
    for label in ["Session", "Project", "AoE"] {
        let tooltip = buttons
            .iter()
            .find(|block| block["label"] == json!(label))
            .and_then(|block| block["tooltip"].as_str())
            .unwrap_or_default()
            .to_string();
        assert!(
            tooltip.contains("0 override(s), 0 hidden"),
            "{label}: {tooltip}"
        );
    }
    let titles: Vec<&str> = session["blocks"]
        .as_array()
        .expect("blocks")
        .iter()
        .filter_map(|block| block["title"].as_str())
        .collect();
    assert_eq!(titles, vec!["Activity"], "{session}");

    let project = wire.pane_tab(SESSION, "project");
    assert_eq!(open_tab(&project), "Project");
    let aoe = wire.pane_tab(SESSION, "aoe");
    assert_eq!(open_tab(&aoe), "AoE");
    // Back to the session tab: that is where a tag's origin is read.
    let session = wire.pane_tab(SESSION, "session");
    assert_eq!(open_tab(&session), "Session");

    // A project default for one checkout: the session in it picks it up, the
    // session in the other checkout keeps the settings' value.
    let entries = vec!["team:core", "feature:billing"];
    wire.notify(
        "tag.level.replace",
        json!({
            "level": "project",
            "project_path": one.display().to_string(),
            "entries": entries,
            "session_id": SESSION,
        }),
    );
    let one_pane = wire.wait_pane(SESSION);
    let two_pane = wire.wait_pane(OTHER);
    let one_rows = pane_rows(&one_pane);
    let two_rows = pane_rows(&two_pane);
    assert_eq!(pane_row(&one_rows, "team")["value"], json!("core"));
    assert_eq!(pane_row(&one_rows, "feature")["value"], json!("billing"));
    assert_eq!(pane_row(&two_rows, "team")["value"], json!("config"));

    // The row says where it came from, so a project default is not mistaken for
    // something this session wrote.
    assert_eq!(origin_of(&pane_row(&one_rows, "team")), "project");
    assert_eq!(origin_of(&pane_row(&two_rows, "team")), "settings");

    // A hide at the project level drops the settings' `cost`, and the project
    // tab says how many entries and hides it owns.
    wire.notify(
        "tag.level.unset",
        json!({
            "level": "project",
            "project_path": one.display().to_string(),
            "key": "cost",
            "session_id": SESSION,
        }),
    );
    let one_pane = wire.wait_pane(SESSION);
    // A hide is not a deletion: the row says "(hidden)" and offers the way back,
    // and the project tab says what it owns.
    assert!(
        !pane_rows(&one_pane)
            .iter()
            .any(|row| row["label"] == json!("cost") && row["method"] == json!("tag.remove")),
        "the hide is still rendered as a live row: {one_pane}"
    );
    let project = wire.pane_tab(SESSION, "project");
    assert_eq!(project["footer"]["value"], json!("2 override(s), 1 hidden"));
    // The strip says the same thing the footer does, one row up: a closed tab is
    // not a question mark.
    assert!(tab_button(&project, "Project")["tooltip"]
        .as_str()
        .unwrap_or_default()
        .contains("2 override(s), 1 hidden"));
    let project_rows = pane_rows(&project);
    assert_eq!(pane_row(&project_rows, "cost")["value"], json!("(hidden)"));
    assert_eq!(
        pane_row(&project_rows, "cost")["method"],
        json!("tag.level.restore")
    );

    // AoE entries reach both sessions, including the other project.
    wire.notify(
        "tag.level.replace",
        json!({
            "level": "aoe",
            "entries": ["owner:me"],
            "session_id": SESSION,
        }),
    );
    // The open tab is per session, and this one is still on the project level, so
    // the session tab is asked for by name.
    let session_pane = wire.pane_tab(SESSION, "session");
    assert_eq!(
        pane_row(&pane_rows(&session_pane), "owner")["value"],
        json!("me"),
        "{SESSION}"
    );
    let other_pane = wire.wait_pane(OTHER);
    assert_eq!(
        pane_row(&pane_rows(&other_pane), "owner")["value"],
        json!("me"),
        "{OTHER}"
    );
    let aoe = wire.pane_tab(SESSION, "aoe");
    assert_eq!(aoe["footer"]["value"], json!("1 override(s), 0 hidden"));
    assert!(tab_button(&aoe, "AoE")["tooltip"]
        .as_str()
        .unwrap_or_default()
        .contains("1 override(s), 0 hidden"));
    assert_eq!(
        pane_row(&pane_rows(&aoe), "owner")["badges"][0]["text"],
        json!("AoE")
    );

    // Both levels live in the plugin's own file, and the settings are untouched.
    let env = llm_tags::env::Env::new(sandbox.env.clone());
    let app_dir = llm_tags::settings::app_dir(&env).expect("app dir");
    let levels = llm_tags::levels::path(&app_dir);
    let text = std::fs::read_to_string(&levels).expect("tags.toml");
    assert!(text.contains("[[project]]"), "{text}");
    assert!(text.contains("[aoe]"), "{text}");
    assert!(
        text.contains("unset = [\"cost\"]"),
        "the hide is recorded, not turned into a deletion: {text}"
    );
    let settings = std::fs::read_to_string(sandbox.config_path("")).expect("settings");
    assert!(!settings.contains("team:core"), "{settings}");
    assert!(!settings.contains("owner:me"), "{settings}");
}

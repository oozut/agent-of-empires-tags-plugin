//! The worker over its real wire: one JSON-RPC line in, one out.
//!
//! `src/worker.rs`'s own tests cover a single line in isolation. These start the
//! binary the host starts — no arguments, newline-delimited JSON-RPC on stdio —
//! answer the host calls it makes (`sessions.list`, `config.get`) and read the
//! state it pushes back. That is the only way to see the tick box, the page
//! payload, the file behind them and the `references` verb agree, because the
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
    rows: Vec<Value>,
    /// Every host call the worker made, in order: `(method, params)`.
    calls: Vec<(String, Value)>,
    /// Every `ui.state.set` it pushed, so a test can read the pane as well as the page.
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
        Self {
            child,
            stdin,
            lines,
            settings: settings_of(sandbox),
            rows: rows.to_vec(),
            calls: Vec::new(),
            pushes: Vec::new(),
            log,
            next_id: 900,
        }
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
                let value = params["key"]
                    .as_str()
                    .and_then(|key| self.settings.get(key))
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

    /// The last pane payload pushed for a session.
    fn pane(&self, session: &str) -> &Value {
        self.pushes
            .iter()
            .rev()
            .find(|params| {
                params["slot"] == json!("pane") && params["session_id"] == json!(session)
            })
            .unwrap_or_else(|| panic!("no pane was pushed for {session}"))
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

    /// Read pushes until the editor page's payload arrives, answering each call.
    fn wait_page(&mut self) -> Value {
        for _ in 0..400 {
            let line = self.next_line();
            if line["method"] == "ui.state.set" && line["params"]["slot"] == "page" {
                return line["params"]["payload"].clone();
            }
        }
        panic!("the worker never pushed a page; stderr:\n{}", self.log());
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

fn tags_of(session: &Value) -> Vec<Value> {
    session["tags"].as_array().cloned().unwrap_or_default()
}

fn tag<'a>(tags: &'a [Value], key: &str) -> &'a Value {
    tags.iter()
        .find(|tag| tag["key"] == json!(key))
        .unwrap_or(&Value::Null)
}

/// The rows of one titled section in a pane payload.
fn section_rows(pane: &Value, title: &str) -> Vec<Value> {
    pane["payload"]["blocks"]
        .as_array()
        .expect("pane blocks")
        .iter()
        .find(|block| block["title"] == json!(title))
        .unwrap_or_else(|| panic!("no {title:?} section in {pane}"))
        .get("children")
        .and_then(Value::as_array)
        .cloned()
        .expect("section children")
}

fn pane_row(rows: &[Value], label: &str) -> Value {
    rows.iter()
        .find(|row| row["label"] == json!(label))
        .unwrap_or_else(|| panic!("no {label:?} row in {rows:?}"))
        .clone()
}

// --- the tests --------------------------------------------------------------

#[test]
fn ticking_the_git_box_writes_the_sample_and_the_page_matches_the_verb() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    sandbox.write_config(&settings_config(BRANCH_TARGET, ""));

    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);
    wire.notify(
        "tag.preset",
        json!({"preset": "git", "applied": true, "session_id": SESSION}),
    );
    let page = wire.wait_page();

    let session = &page["sessions"][0];
    assert_eq!(session["id"], json!(SESSION));
    let tags = tags_of(session);
    let keys: Vec<&str> = tags
        .iter()
        .map(|tag| tag["key"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(keys, vec!["vcs", "git.branch", "git.commit", "git.tag"]);

    // The editor shows the entry as stored — editing a rendered branch would
    // freeze it — while the pane and the target show what is sent.
    let branch = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    let commit = git(&repo, &["rev-parse", "--short", "HEAD"]);
    assert_eq!(tag(&tags, "vcs")["value"], json!("git"));
    assert_eq!(tag(&tags, "git.branch")["value"], json!("{git.branch}"));
    assert_eq!(tag(&tags, "git.commit")["value"], json!("{git.commit}"));
    assert_eq!(session["targets"][0]["value"], json!(branch));

    let pane = wire.pane(SESSION);
    let blocks = pane["payload"]["blocks"].as_array().expect("pane blocks");
    let rows: Vec<Value> = blocks
        .iter()
        .flat_map(|block| block["children"].as_array().cloned().unwrap_or_default())
        .filter(|row| row["kind"] == json!("row"))
        .collect();
    let pane_tag = |key: &str| -> Value {
        rows.iter()
            .find(|row| row["label"] == json!(key))
            .unwrap_or_else(|| panic!("no {key} row in the pane: {pane}"))
            .clone()
    };
    assert_eq!(pane_tag("git.branch")["value"], json!(branch));
    assert_eq!(pane_tag("git.commit")["value"], json!(commit));
    assert_eq!(
        pane_tag("git.branch")["badges"][0]["text"],
        json!("this session")
    );
    // Every entry came from this session's file, not from the settings.
    assert!(
        tags.iter().all(|tag| tag["origin"] == json!("session")),
        "{tags:?}"
    );

    // Behind the page: ordinary tag grammar, one session's file, settings untouched.
    let file = overrides_path(&sandbox, SESSION);
    let text = std::fs::read_to_string(&file).expect("overrides.toml");
    assert!(text.contains(r#""vcs:git""#), "{text}");
    assert!(text.contains(r#""git.branch:{git.branch}""#), "{text}");
    let settings = std::fs::read_to_string(sandbox.config_path("")).expect("settings");
    assert!(!settings.contains("vcs:git"), "{settings}");

    // The tick box and the dropdown are the same catalogue the verb prints: a
    // reference cannot be offered here and unknown to the resolver.
    let offered: Vec<String> = page["references"]
        .as_array()
        .expect("references")
        .iter()
        .map(|item| item["name"].as_str().unwrap_or_default().to_string())
        .collect();
    let verb = sandbox.run(&["references", "--json"]);
    verb.assert_ok();
    let printed: Value = serde_json::from_str(&verb.stdout).expect("references --json");
    let listed: Vec<String> = printed["references"]
        .as_array()
        .expect("references")
        .iter()
        .map(|item| item["name"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(offered, listed);
    assert!(offered.iter().any(|name| name == "git.subject"));

    let preset = &page["presets"][0];
    assert_eq!(preset["id"], json!("git"));
    assert_eq!(preset["label"], json!("Add standard git information"));
    assert_eq!(
        preset["tags"],
        json!(llm_tags::resolver::GIT_PRESET),
        "the tick writes what the preset spells"
    );
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
    let page = wire.wait_page();
    let tags = tags_of(&page["sessions"][0]);
    assert_eq!(tag(&tags, "vcs")["value"], json!("git"));
    assert_eq!(tag(&tags, "vcs")["origin"], json!("session"));

    wire.notify(
        "tag.preset",
        json!({"preset": "git", "applied": false, "session_id": SESSION}),
    );
    let page = wire.wait_page();
    let tags = tags_of(&page["sessions"][0]);
    // Unticking is an undo: the settings' value is back, the sample's is gone.
    assert_eq!(tag(&tags, "vcs")["value"], json!("prod"));
    assert_eq!(tag(&tags, "vcs")["origin"], json!("config"));
    assert_eq!(tag(&tags, "team")["value"], json!("core"));
    assert!(
        tag(&tags, "git.branch").is_null(),
        "the sample's keys are gone: {tags:?}"
    );
    assert!(
        !overrides_path(&sandbox, SESSION).exists(),
        "nothing is left to hide"
    );
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
    wire.wait_page();
    wire.notify(
        "tag.set",
        json!({"key": "team", "value": "platform", "session_id": OTHER}),
    );
    let page = wire.wait_page();

    let sessions = page["sessions"].as_array().expect("sessions");
    for (id, expected) in [(SESSION, "core"), (OTHER, "platform")] {
        let session = sessions
            .iter()
            .find(|session| session["id"] == json!(id))
            .unwrap_or_else(|| panic!("no {id} in {page}"));
        assert_eq!(tag(&tags_of(session), "team")["value"], json!(expected));
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

#[test]
fn a_token_override_is_never_published() {
    const SECRET: &str = "sk-live-3f9c2a1b-do-not-publish";
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    // The default target has no template, so the token is what it renders: the row
    // that shows the wire value would otherwise be the token itself.
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);

    wire.notify("token.set", json!({"token": SECRET, "session_id": SESSION}));
    let page = wire.wait_page();
    let session = &page["sessions"][0];
    // The page learns that a token is set and nothing about what it says: the host
    // keeps every `ui.state.set` payload and renders it to everyone who can see
    // the session, so the value stays in the session file.
    assert_eq!(session["token_set"], json!(true), "{session}");
    assert!(session.get("token").is_none(), "{session}");
    assert_eq!(
        session["targets"][0]["value"],
        json!("(the session's token, not shown here)"),
        "{session}"
    );
    // The pane names it too, in the row that would otherwise show it.
    let pane = wire.pane(SESSION).to_string();
    assert!(!pane.contains(SECRET), "{pane}");
    assert!(pane.contains("not shown here"), "{pane}");
    let pushed = wire.pushed_text();
    assert!(
        !pushed.contains(SECRET),
        "the value was published: {pushed}"
    );
    assert!(pushed.contains("a token override is set"), "{pushed}");
    assert!(!wire.log().contains(SECRET), "{}", wire.log());

    // It is kept, though: it is what the launch sends instead of the payload.
    let env = llm_tags::env::Env::new(sandbox.env.clone());
    let app_dir = llm_tags::settings::app_dir(&env).expect("app dir");
    let (edits, _) = llm_tags::session_tags::load(&app_dir, SESSION);
    assert_eq!(edits.token, SECRET);

    // Clearing it takes that back, still without publishing it.
    wire.notify("token.clear", json!({"session_id": SESSION}));
    let page = wire.wait_page();
    assert_eq!(page["sessions"][0]["token_set"], json!(false));
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
    wire.wait_page();
    assert!(!overrides_path(&sandbox, SESSION).exists());
    assert!(wire.log().contains("no session_id"), "{}", wire.log());

    let response = wire.ask(STATUS, json!({}));
    assert_eq!(response["result"]["ok"], json!(true), "{response}");

    // A method the pane does not offer is not an edit either.
    wire.notify(
        "tag.nonsense",
        json!({"session_id": SESSION, "key": "team"}),
    );
    wire.wait_page();
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
    wire.wait_page();
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
    wire.wait_page();
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
    wire.wait_page();
    let env = llm_tags::env::Env::new(sandbox.env.clone());
    let app_dir = llm_tags::settings::app_dir(&env).expect("app dir");
    assert!(!llm_tags::harness_extension::live_path(&app_dir, SESSION).exists());
}

/// The page is where a user reads the disclosure and gives the approval, so its
/// rows are the same verdict the launch uses — and the checkbox approves *those*
/// bytes by echoing the digest it displayed.
#[test]
fn the_page_shows_the_disclosure_and_its_checkbox_is_the_approval() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    sandbox.write_config(&settings_config(
        "harness_extension = [\"pi\"]\n\
         [[plugins.\"dev.oozut.llm-tags\".settings.targets]]\n\
         kind = \"header\"\nname = \"X-AoE-Tags\"\ntemplate = \"aoe,{tags}\"\nenv_var = \"AOE_LLM_TAGS\"\n",
        "",
    ));
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);

    // The worker publishes on a host event, not at startup.
    wire.notify("session.state", json!({}));
    let page = wire.wait_page();
    let rows = page["extensions"].as_array().expect("extensions");
    let pi = rows
        .iter()
        .find(|row| row["harness"] == json!("pi"))
        .unwrap_or_else(|| panic!("no pi row in {page}"));
    assert_eq!(pi["requested"], json!(true));
    assert_eq!(pi["approved"], json!(false));
    assert_eq!(pi["state"], json!("no_receipt"), "{pi}");
    // Before anything is approved the page already carries the text that would be
    // approved, so enabling is a decision the user could actually read first.
    let disclosure = pi["disclosure"].as_str().expect("disclosure");
    assert!(disclosure.contains("does not"), "{disclosure}");
    assert!(disclosure.contains("session config"), "{disclosure}");
    let digest = pi["sha256"].as_str().expect("the digest").to_string();
    assert_eq!(digest.len(), 64);

    // A stale page approves nothing: the digest it shows is the whole point.
    wire.notify(
        "extension.enable",
        json!({"harness": "pi", "accept": "0".repeat(64), "session_id": SESSION}),
    );
    let page = wire.wait_page();
    assert_eq!(page["extensions"][0]["approved"], json!(false), "{page}");
    assert!(
        wire.log().contains("not the file this build ships"),
        "{}",
        wire.log()
    );

    wire.notify(
        "extension.enable",
        json!({"harness": "pi", "accept": digest, "session_id": SESSION}),
    );
    let page = wire.wait_page();
    let pi = &page["extensions"][0];
    assert_eq!(pi["approved"], json!(true), "{pi}");
    assert_eq!(pi["state"], json!("ok"));
    assert!(pi["accepted_at"].as_str().unwrap_or_default().len() >= 10);

    // The page has to say which of the two grants a row needs, and where the
    // files go: a session copy and the user's own config are not the same offer.
    let pi = &page["extensions"][0];
    assert_eq!(pi["scope"], json!("session"), "{pi}");
    let files = pi["files"].as_array().expect("the disclosed files");
    assert_eq!(files.len(), 3, "{pi}");
    assert!(files.iter().all(|path| path
        .as_str()
        .unwrap_or_default()
        .starts_with("<session config>/")));

    // And the button that stops it removes the approval, so the next launch of
    // that harness installs nothing.
    wire.notify(
        "extension.disable",
        json!({"harness": "pi", "session_id": SESSION}),
    );
    let page = wire.wait_page();
    assert_eq!(page["extensions"][0]["approved"], json!(false), "{page}");
}

/// The activity is the thing a row should show at a glance, so it leads the badge
/// with its own icon; the other tags follow with the generic one, and the icon is
/// presentation — the text is what a terminal renders.
#[test]
fn the_row_badge_leads_with_the_activity_and_its_icon() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    sandbox.write_config(&settings_config(
        "tags = [\"activity:test\", \"team:core\"]\n",
        "",
    ));
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);
    wire.notify("session.state", json!({}));
    wire.wait_page();

    let badge = wire
        .pushes
        .iter()
        .rev()
        .find(|params| params["slot"] == json!("row-badge"))
        .unwrap_or_else(|| panic!("no badge was pushed: {:?}", wire.pushes));
    let items = badge["payload"]["items"].as_array().expect("items");
    assert_eq!(items[0]["text"], json!("testing"), "{badge}");
    assert_eq!(items[0]["icon"], json!("flask-conical"), "{badge}");
    assert_eq!(items[0]["tone"], json!("warn"), "{badge}");
    // The raw tag is still what travels; the icon never does.
    assert!(!badge.to_string().contains("activity:test"), "{badge}");
    let team = items
        .iter()
        .find(|item| item["text"] == json!("team"))
        .unwrap_or_else(|| panic!("no team item in {badge}"));
    assert_eq!(team["icon"], json!("tag"));
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
    wire.wait_page();
    assert!(wire.log().contains("is not an activity"), "{}", wire.log());
    let badge = wire
        .pushes
        .iter()
        .rev()
        .find(|params| params["slot"] == json!("row-badge"))
        .expect("a badge");
    let items = badge["payload"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("no items in {badge}"));
    assert!(
        items
            .iter()
            .all(|item| item["icon"] != json!("flask-conical")),
        "a typo must not borrow the icon of the word it nearly is: {badge}"
    );
    let activity = items
        .iter()
        .find(|item| item["text"] == json!("activity"))
        .unwrap_or_else(|| panic!("the activity tag is still shown: {badge}"));
    assert_eq!(
        activity["icon"],
        json!("tag"),
        "an unknown value is a tag, not an icon"
    );
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
    wire.wait_page();

    let pane = wire
        .pushes
        .iter()
        .rev()
        .find(|params| params["slot"] == json!("pane"))
        .expect("a pane");
    let blocks = pane["payload"]["blocks"].as_array().expect("blocks");
    let section = blocks
        .iter()
        .find(|block| block["kind"] == json!("section") && block["title"] == json!("Activity"))
        .unwrap_or_else(|| panic!("no Activity section in {pane}"));
    let rows = section["children"].as_array().expect("rows");
    assert_eq!(rows.len(), 9, "the whole vocabulary is offered: {section}");
    let selected: Vec<&str> = rows
        .iter()
        .filter(|row| row["selected"] == json!(true))
        .map(|row| row["label"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(selected, vec!["debugging"], "{section}");
    for row in rows {
        assert_eq!(row["method"], json!("tag.set"), "{row}");
        assert_eq!(row["params"]["key"], json!("activity"), "{row}");
    }
    // The pane is text-and-tone first: every row has a label, and the icon is
    // decoration the web renders and the terminal ignores.
    assert!(rows.iter().all(|row| row["label"].is_string()));
}

/// A user-scope row on the page says so: the same card, a wider grant, and the
/// paths it would write named before anything is written.
#[test]
fn the_page_names_the_users_own_config_for_a_user_scope_row() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    sandbox.write_config(&settings_config(
        "harness_extension = [\"claude\"]\n\
         [[plugins.\"dev.oozut.llm-tags\".settings.targets]]\n\
         kind = \"header\"\nname = \"X-AoE-Tags\"\ntemplate = \"aoe,{tags}\"\nenv_var = \"AOE_LLM_TAGS\"\n",
        "",
    ));
    let mut wire = Wire::open(&sandbox, &[row(SESSION, &repo)], &[]);
    wire.notify("session.state", json!({}));
    let page = wire.wait_page();
    let claude = page["extensions"]
        .as_array()
        .expect("extensions")
        .iter()
        .find(|row| row["harness"] == json!("claude"))
        .unwrap_or_else(|| panic!("no claude row in {page}"))
        .clone();
    assert_eq!(claude["requested"], json!(true));
    assert_eq!(claude["scope"], json!("user"), "{claude}");
    let files: Vec<&str> = claude["files"]
        .as_array()
        .expect("the disclosed files")
        .iter()
        .map(|path| path.as_str().unwrap_or_default())
        .collect();
    assert!(files.iter().all(|path| path.starts_with("~/")), "{files:?}");
    assert!(
        files.contains(&"~/.claude/CLAUDE.md"),
        "the instruction file is not disclosed: {files:?}"
    );
    let disclosure = claude["disclosure"].as_str().expect("the disclosure");
    assert!(disclosure.contains("your own"), "{disclosure}");
    assert!(disclosure.contains("marker"), "{disclosure}");
    assert!(!disclosure.contains("private config copy"), "{disclosure}");
}

#[test]
fn the_pane_has_three_levels_and_a_project_edit_stays_in_its_project() {
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
    wire.wait_page();

    // The pane opens on the session and still offers the two levels above it.
    let pane = wire.pane(SESSION).clone();
    let titles: Vec<&str> = pane["payload"]["blocks"]
        .as_array()
        .expect("blocks")
        .iter()
        .filter_map(|block| block["title"].as_str())
        .collect();
    assert_eq!(
        titles,
        vec![
            "This session",
            "Activity",
            "This project",
            "AoE — every session"
        ]
    );

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
    let page = wire.wait_page();
    let sessions = page["sessions"].as_array().expect("page sessions");
    let one_page = sessions
        .iter()
        .find(|session| session["id"] == json!(SESSION))
        .expect("this session");
    let two_page = sessions
        .iter()
        .find(|session| session["id"] == json!(OTHER))
        .expect("the other session");
    assert_eq!(tag(&tags_of(one_page), "team")["value"], json!("core"));
    assert_eq!(
        tag(&tags_of(one_page), "feature")["value"],
        json!("billing")
    );
    assert_eq!(tag(&tags_of(two_page), "team")["value"], json!("config"));

    // The row says where it came from, so a project default is not mistaken for
    // something this session wrote.
    let pane = wire.pane(SESSION).clone();
    let rows = section_rows(&pane, "This session");
    assert_eq!(
        pane_row(&rows, "team")["badges"][0]["text"],
        json!("project")
    );

    // A hide at the project level drops the settings' `cost`, and the project
    // section says how many entries and hides it owns.
    wire.notify(
        "tag.level.unset",
        json!({
            "level": "project",
            "project_path": one.display().to_string(),
            "key": "cost",
            "session_id": SESSION,
        }),
    );
    let page = wire.wait_page();
    let sessions = page["sessions"].as_array().expect("page sessions");
    let one_page = sessions
        .iter()
        .find(|session| session["id"] == json!(SESSION))
        .expect("this session");
    assert_eq!(tag(&tags_of(one_page), "cost"), &Value::Null);
    let pane = wire.pane(SESSION).clone();
    let project = pane["payload"]["blocks"]
        .as_array()
        .expect("blocks")
        .iter()
        .find(|block| block["title"] == json!("This project"))
        .expect("project section");
    assert_eq!(project["value"], json!("2 override(s), 1 hidden"));

    // AoE entries reach both sessions, including the other project.
    wire.notify(
        "tag.level.replace",
        json!({
            "level": "aoe",
            "entries": ["owner:me"],
            "session_id": SESSION,
        }),
    );
    let page = wire.wait_page();
    let sessions = page["sessions"].as_array().expect("page sessions");
    for id in [SESSION, OTHER] {
        let session = sessions
            .iter()
            .find(|session| session["id"] == json!(id))
            .expect("a session");
        assert_eq!(
            tag(&tags_of(session), "owner")["value"],
            json!("me"),
            "{id}"
        );
    }

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

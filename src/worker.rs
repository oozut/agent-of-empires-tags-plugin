//! The plugin worker: JSON-RPC 2.0 over newline-delimited JSON on stdio.
//!
//! The host spawns this binary, sends one request per line, and reads one
//! response per line; the worker exits when stdin reaches EOF. Stdout carries
//! JSON-RPC only — diagnostics go to stderr, which the host captures in
//! `<app_dir>/plugin-workers/<plugin-id>_<worker-uuid>.log`.
//!
//! M0 answers one command, `status` (`plugin.dev.oozut.llm-tags.status`), with
//! the headers this install would send. Anything else is `-32601`; a message
//! with no `id` is a notification and is never answered.

use std::io::{BufRead, Write};

use serde_json::{json, Value};

use crate::env::Env;
use crate::resolver::{self, Flags, Given};

pub const METHOD_NOT_FOUND: i64 = -32601;

/// Read requests until stdin closes, supporting outbound host RPCs.
pub fn run() -> i32 {
    let env = Env::current();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(line) => {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    let mut host = Host {
        env,
        rx,
        stdout: std::sync::Mutex::new(std::io::stdout()),
        pending: std::collections::HashMap::new(),
        tabs: std::collections::HashMap::new(),
        next_id: 1_000_000,
    };
    // Publish before reading anything: a spawned worker owns a fresh UI generation,
    // so the host has already dropped whatever this plugin showed before the
    // reload. A worker that waits for the next host event leaves its pane — and
    // the page button in the header — missing until some session happens to change
    // state, which for an idle workspace is never.
    host.publish_state();
    while host.process_one() {}
    0
}

struct Host {
    env: Env,
    rx: std::sync::mpsc::Receiver<String>,
    stdout: std::sync::Mutex<std::io::Stdout>,
    pending: std::collections::HashMap<i64, std::sync::mpsc::Sender<serde_json::Value>>,
    /// Which tab of the one LLM Tags pane each session is showing. The host has
    /// no tab kind in its pane vocabulary, so the open tab is the worker's own
    /// state: a click on a tab row stores it here and republishes.
    tabs: std::collections::HashMap<String, String>,
    next_id: i64,
}

impl Host {
    fn write_line(&self, value: &serde_json::Value) {
        if let Ok(mut out) = self.stdout.lock() {
            let _ = writeln!(out, "{value}");
            let _ = out.flush();
        }
    }

    fn process_one(&mut self) -> bool {
        match self.rx.recv() {
            Ok(line) => {
                self.dispatch_line(&line);
                true
            }
            Err(_) => false,
        }
    }

    fn dispatch_line(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return;
        };
        if value.get("method").is_none() {
            if let Some(id) = value.get("id").and_then(serde_json::Value::as_i64) {
                if let Some(sender) = self.pending.remove(&id) {
                    let _ = sender.send(value);
                }
            }
            return;
        }
        let method = value
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let params = value
            .get("params")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if let Some(id) = value.get("id").cloned() {
            let result = self.handle_request(method, &params);
            let response = match result {
                Ok(result) => serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result,
                }),
                Err((code, message)) => serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": code, "message": message },
                }),
            };
            self.write_line(&response);
        } else {
            self.handle_notification(method, &params);
        }
    }

    fn handle_request(
        &mut self,
        method: &str,
        _params: &serde_json::Value,
    ) -> Result<serde_json::Value, (i64, String)> {
        let command = method.rsplit('.').next().unwrap_or_default();
        match command {
            "status" => Ok(status(&self.env)),
            _ => Err((METHOD_NOT_FOUND, format!("unknown method {method:?}"))),
        }
    }

    fn handle_notification(&mut self, method: &str, params: &serde_json::Value) {
        // A pane button and a page form both arrive here: the host forwards the
        // block's `method` **verbatim** and merges in the authoritative
        // `session_id`. The call has no reply, so the edit is applied and the
        // state re-pushed, which is what clears the clicked control's spinner.
        if EDITS.contains(&method) {
            self.apply_session_edit(method, params);
        }
        // Every host event, and every edit, republishes; failures are best-effort.
        self.publish_state();
    }

    /// The page's checkbox: the approval a receipt records.
    ///
    /// The page echoes the digest it displayed, so approving is approving *those*
    /// bytes — exactly what `--accept` does on the command line. Anything else
    /// writes nothing, which is the only safe way for a stale page to behave.
    fn apply_extension_edit(&mut self, command: &str, params: &serde_json::Value) {
        let Some(harness) = params.get("harness").and_then(serde_json::Value::as_str) else {
            eprintln!("{command}: no harness named; refusing to guess one");
            return;
        };
        let app_dir = match crate::settings::app_dir(&self.env) {
            Ok(dir) => dir,
            Err(message) => {
                eprintln!("{command} for {harness}: {message}");
                return;
            }
        };
        let (adapters, _) = crate::adapters::load(None);
        let Some(adapter) = crate::adapters::find(&adapters, harness) else {
            eprintln!("{command}: {harness} is not a harness this build knows");
            return;
        };
        let home = crate::harness_extension::home();
        let result = if command == "extension.disable" {
            crate::harness_extension::revoke(&app_dir, home.as_deref(), adapter).map(|revoked| {
                // The page has no terminal: a copy left to AoE is reported here,
                // because a user who clicks disable and then finds the directory
                // still there deserves the reason.
                for (path, why) in &revoked.kept {
                    eprintln!("kept: {} ({why})", path.display());
                }
            })
        } else {
            let Some(carrier) = crate::harness_extension::carrier(adapter) else {
                eprintln!(
                    "{command} for {harness}: {}",
                    crate::harness_extension::Refusal::NoDestination {
                        harness: adapter.agent.clone()
                    }
                    .detail()
                );
                return;
            };
            let accepted = params
                .get("accept")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            match crate::harness_extension::bundle_sha(adapter, carrier) {
                Ok(shipped) if accepted.eq_ignore_ascii_case(&shipped) => {
                    // The page names no harness version, and the receipt's copy of
                    // it is informational, so it stays empty rather than guessed.
                    crate::harness_extension::accept(
                        &app_dir,
                        home.as_deref(),
                        adapter,
                        &shipped,
                        "",
                    )
                    .map(|_| ())
                }
                Ok(shipped) => Err(format!(
                    "{harness}: that is not the file this build ships (sha256 {shipped})"
                )),
                Err(refusal) => Err(refusal.detail()),
            }
        };
        if let Err(why) = result {
            eprintln!("{command} for {harness}: {why}");
        }
    }

    /// One edit from the pane or the page, written to that session's own file.
    ///
    /// Every arm is a per-session write: the global settings are never touched,
    /// so two sessions cannot collide even when both are edited at once.
    fn apply_session_edit(&mut self, command: &str, params: &serde_json::Value) {
        // Approving an extension is not a session edit: it is one machine-wide
        // decision, so it is taken here before the session is looked at (the host
        // merges a `session_id` into every page action, and this one has no use
        // for it).
        if command == "extension.enable" || command == "extension.disable" {
            self.apply_extension_edit(command, params);
            return;
        }
        // A level edit is not tied to one session; the pane names the level and,
        // for a project, the path. The host still merges a `session_id` into the
        // action, so the path can be recovered from it when the pane omitted it.
        if command.starts_with("tag.level.") {
            self.apply_level_edit(command, params);
            return;
        }
        // The pane is one surface with three tabs, so the open tab belongs to the
        // session it was clicked in; the republish after this call draws it.
        if command == "pane.tab" {
            self.apply_tab_edit(command, params);
            return;
        }
        let Some(session) = params.get("session_id").and_then(serde_json::Value::as_str) else {
            eprintln!("{command}: the host sent no session_id; refusing to guess one");
            return;
        };
        let app_dir = match crate::settings::app_dir(&self.env) {
            Ok(dir) => dir,
            Err(message) => {
                eprintln!("{command} for session {session}: {message}");
                return;
            }
        };
        let result = match command {
            "tag.set" => match tag_entry(params) {
                Some(entry) => crate::session_tags::set_tag(&app_dir, session, &entry),
                None => Err("a tag needs a `key` and/or a `tag`".to_string()),
            },
            "tag.remove" => match params.get("key").and_then(serde_json::Value::as_str) {
                Some(key) => crate::session_tags::unset_tag(&app_dir, session, key),
                None => Err("dropping a tag needs its `key`".to_string()),
            },
            // Undo a hide (or drop an entry this session wrote): whichever way the
            // key was suppressed, the inherited value shows again.
            "tag.restore" => match params.get("key").and_then(serde_json::Value::as_str) {
                Some(key) => crate::session_tags::drop_tag(&app_dir, session, key),
                None => Err("restoring a tag needs its `key`".to_string()),
            },
            // The session's own include/hide switch for the git sample. Unlike
            // `tag.preset`, `false` is explicit and hides a project/AoE default.
            "tag.git" => {
                let enabled = params
                    .get("enabled")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                crate::session_tags::set_git(&app_dir, session, enabled)
            }
            "token.set" => {
                let token = params
                    .get("token")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                crate::session_tags::set_token(&app_dir, session, token)
            }
            "token.clear" => crate::session_tags::set_token(&app_dir, session, ""),
            // A tick box: one action writes (or removes) the whole sample set,
            // through the same per-session file every other edit lands in.
            "tag.preset" => {
                let preset = params
                    .get("preset")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("git");
                let applied = params
                    .get("applied")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                if applied {
                    crate::session_tags::apply_preset(&app_dir, session, preset)
                } else {
                    crate::session_tags::remove_preset(&app_dir, session, preset)
                }
            }
            _ => crate::session_tags::reset(&app_dir, session).map(|()| Default::default()),
        };
        if let Err(err) = result {
            eprintln!("{command} for session {session}: {err}");
        }
    }

    /// Which tab of the pane a session shows. A tab is UI state, never written to
    /// a file: it is what the click asked for and nothing else.
    fn apply_tab_edit(&mut self, command: &str, params: &serde_json::Value) {
        let Some(session) = params.get("session_id").and_then(serde_json::Value::as_str) else {
            eprintln!("{command}: the host sent no session_id; refusing to guess one");
            return;
        };
        let tab = params
            .get("tab")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if !TABS.contains(&tab) {
            eprintln!("{command}: {tab:?} is not a tab of this pane");
            return;
        }
        self.tabs.insert(session.to_string(), tab.to_string());
    }

    /// Which tab a session's pane is showing: the one it last clicked, else the
    /// session's own — and never the project tab of a session with no project.
    fn active_tab(&self, session: &str, has_project: bool) -> &str {
        match self.tabs.get(session).map(String::as_str) {
            Some(tab) if TABS.contains(&tab) && (has_project || tab != crate::levels::PROJECT) => {
                tab
            }
            _ => TABS[0],
        }
    }

    /// A write to a level above one session: the project's defaults or the
    /// AoE-wide ones. The settings' file is still never opened for writing; the
    /// entries land in `<app dir>/tags.toml` and the resolver folds them in.
    fn apply_level_edit(&mut self, command: &str, params: &serde_json::Value) {
        let level = params
            .get("level")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if !crate::levels::is_level(level) {
            eprintln!("{command}: {level:?} is not a tag level");
            return;
        }
        let project = if level == crate::levels::PROJECT {
            match self.project_path_for(params) {
                Some(path) => path,
                None => {
                    eprintln!("{command}: a project level edit needs a project path");
                    return;
                }
            }
        } else {
            String::new()
        };
        let app_dir = match crate::settings::app_dir(&self.env) {
            Ok(dir) => dir,
            Err(message) => {
                eprintln!("{command} for level {level}: {message}");
                return;
            }
        };
        let key = params.get("key").and_then(serde_json::Value::as_str);
        let result = match command {
            "tag.level.unset" => match key {
                Some(key) => crate::levels::unset_tag(&app_dir, level, &project, key),
                None => Err("hiding a tag needs its `key`".to_string()),
            },
            "tag.level.restore" => match key {
                Some(key) => crate::levels::restore(&app_dir, level, &project, key),
                None => Err("restoring a tag needs its `key`".to_string()),
            },
            "tag.level.set" => match tag_entry(params) {
                Some(entry) => crate::levels::set_tag(&app_dir, level, &project, &entry),
                None => Err("a tag needs a `key` and/or a `tag`".to_string()),
            },
            "tag.level.git" => {
                let enabled = params
                    .get("enabled")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                crate::levels::set_git(&app_dir, level, &project, enabled)
            }
            "tag.level.replace" => {
                let entries: Vec<String> = params
                    .get("entries")
                    .and_then(serde_json::Value::as_array)
                    .map(|rows| {
                        rows.iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                crate::levels::replace(&app_dir, level, &project, &entries)
            }
            "tag.level.reset" => crate::levels::reset(&app_dir, level, &project),
            other => Err(format!("{other} is not a tag level command")),
        };
        if let Err(err) = result {
            eprintln!("{command} for level {level}: {err}");
        }
    }

    /// The project path a level action is about: the one the pane sent, else the
    /// session's own, looked up because the host gives an action a session id but
    /// not a path.
    fn project_path_for(&mut self, params: &serde_json::Value) -> Option<String> {
        if let Some(path) = params
            .get("project_path")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())
        {
            return Some(path.to_string());
        }
        let session = params
            .get("session_id")
            .and_then(serde_json::Value::as_str)?;
        let listed = self
            .call_host("sessions.list", serde_json::json!({}))
            .ok()?;
        listed
            .get("sessions")
            .and_then(serde_json::Value::as_array)?
            .iter()
            .find(|row| row.get("id").and_then(serde_json::Value::as_str) == Some(session))
            .and_then(|row| row.get("project_path"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    }

    fn call_host(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let (sender, receiver) = std::sync::mpsc::channel();
        self.pending.insert(id, sender);
        self.write_line(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Ok(response) = receiver.try_recv() {
                self.pending.remove(&id);
                if let Some(error) = response.get("error") {
                    return Err(error.to_string());
                }
                return Ok(response
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null));
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                self.pending.remove(&id);
                return Err("timeout".to_string());
            }
            match self.rx.recv_timeout(remaining) {
                Ok(line) => self.dispatch_line(&line),
                Err(_) => {
                    self.pending.remove(&id);
                    return Err("timeout".to_string());
                }
            }
        }
    }

    /// Push every surface the plugin renders: the one pane and the editor page.
    /// The session header carries no badge — a tag's own home is the pane, and a
    /// second copy in the row would be one more place to keep in step.
    ///
    /// Called on every host event and after every edit, so a click never leaves a
    /// stale pane behind. Everything a surface shows is derived from the same
    /// `snapshot` the hook runs, so the UI cannot claim a value the launch
    /// environment does not carry.
    fn publish_state(&mut self) {
        let sessions = match self.call_host("sessions.list", serde_json::json!({})) {
            Ok(value) => value,
            Err(_) => return,
        };
        let sessions = sessions
            .get("sessions")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();

        // A session's generated config must not outlive the session: the worker
        // already holds the live list, so it is the natural place to sweep. The
        // session's own `overrides.toml` is not a generated copy, though, and a
        // list that has merely not caught up must not take a user's tags with it,
        // so the sweep leaves that file alone (`overlay::prune`).
        let live: Vec<String> = sessions
            .iter()
            .filter_map(|session| session.get("id"))
            .filter_map(serde_json::Value::as_str)
            .map(str::to_string)
            .collect();
        if !live.is_empty() {
            let app_dir = crate::settings::app_dir(&self.env);
            let removed = match &app_dir {
                Ok(app_dir) => crate::overlay::prune(app_dir, &live),
                Err(_) => 0,
            };
            if removed > 0 {
                eprintln!("pruned {removed} stale session config(s)");
            }
        }

        let setting = |host: &mut Self, key: &str| -> Option<serde_json::Value> {
            host.call_host("config.get", serde_json::json!({"key": key}))
                .ok()
                .and_then(|v| v.get("value").cloned())
        };
        let tags_val = setting(self, "tags");
        let tags_file_val = setting(self, "tags_file");
        let targets_val = setting(self, "targets");
        let rules_val = setting(self, "rules");
        let session_tagging_val = setting(self, "session_tagging");
        let harness_extension_val = setting(self, "harness_extension");
        // These are per_project; fetch the global value once as a fallback for a
        // session with no project, or for a key the project did not set.
        let per_project_global = [
            ("feature_tag", setting(self, "feature_tag")),
            ("activity_family", setting(self, "activity_family")),
            ("activity_families", setting(self, "activity_families")),
        ];

        let mut table = toml::Table::new();
        for (key, value) in [
            ("tags", tags_val),
            ("tags_file", tags_file_val),
            ("targets", targets_val),
            ("rules", rules_val),
            ("session_tagging", session_tagging_val),
            ("harness_extension", harness_extension_val.clone()),
        ] {
            if let Some(value) = value {
                if let Some(converted) = crate::settings::json_to_toml(&value) {
                    table.insert(key.to_string(), converted);
                }
            }
        }

        let flags = Flags::from_given(&Given::default(), &self.env);
        let app_dir = crate::settings::app_dir(&self.env);
        // The two overlays above a session, read once per publish: the pane shows
        // their entries and the resolver folds them in per session, so both use
        // this one copy.
        let levels = match &app_dir {
            Ok(dir) => {
                let (levels, warnings) = crate::levels::load(dir);
                for warning in warnings {
                    eprintln!("{warning}");
                }
                Some(levels)
            }
            Err(_) => None,
        };
        // The adapter rows are how a session's tool becomes a delivery and an
        // extension decision; loaded once, because every session needs them.
        let adapters = crate::adapters::load(None).0;
        let mut page_sessions: Vec<serde_json::Value> = Vec::new();

        for session in sessions {
            let id = session
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            if id.is_empty() {
                continue;
            }
            let title = session
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let tool = session
                .get("tool")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let path = session
                .get("project_path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            // The row's own profile, not a guess: a profile decides which config
            // applies, so a wrong one would show a session the tags it does not have.
            let profile = session
                .get("profile")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let facts_override = resolver::SessionFacts {
                id: id.clone(),
                title: title.clone(),
                tool: tool.clone(),
                path: path.clone(),
                profile,
                group: String::new(),
                branch: String::new(),
            };

            // `feature_tag` is one tag entry, and the project value is parsed by
            // the same `parse_feature_tag` the global config and a profile go
            // through — the host may hand it back as a string (`feature:billing`),
            // a bare word, or a stored `{key, value}` pair, and all three mean
            // the same entry.
            let mut session_table = table.clone();
            for (key, global) in &per_project_global {
                let mut params = serde_json::json!({"key": key});
                if let Some(project_path) = session
                    .get("project_path")
                    .and_then(serde_json::Value::as_str)
                {
                    if !project_path.is_empty() {
                        params["project_path"] =
                            serde_json::Value::String(project_path.to_string());
                    }
                }
                let value = self
                    .call_host("config.get", params)
                    .ok()
                    .and_then(|v| v.get("value").cloned())
                    .or_else(|| global.clone());
                if let Some(value) = value {
                    if let Some(converted) = crate::settings::json_to_toml(&value) {
                        session_table.insert((*key).to_string(), converted);
                    }
                }
            }

            let snap = resolver::snapshot_with_table(
                &flags,
                &self.env,
                None,
                Some(session_table),
                Some(facts_override),
            );
            // A session launched with an approved extension has a live file the
            // extension re-reads before each request. The worker is the one place
            // that sees an edit and the new resolution together, so it keeps that
            // file true: an edit reaches the next request, and a revoke flips
            // `enabled` without waiting for a relaunch.
            if let Ok(app_dir) = &app_dir {
                if let Some(Err(why)) = crate::harness_extension::refresh_live(
                    app_dir,
                    &id,
                    crate::adapters::match_tool(&adapters, &tool),
                    &snap.settings.harness_extension,
                    &snap.resolved,
                    crate::harness_extension::home().as_deref(),
                ) {
                    eprintln!("{id}: {why}");
                }
            }
            // What this session adds on top of the settings, and what it drops:
            // the pane and the page label each tag with where it came from, so a
            // "make it platform for this one session" reads as exactly that.
            let (edits, edit_warnings) = match &app_dir {
                Ok(app_dir) => crate::session_tags::load(app_dir, &id),
                // No app dir means no session file to read: say so once per
                // session instead of showing edits the launch will not see.
                Err(message) => (Default::default(), vec![message.clone()]),
            };
            for warning in &edit_warnings {
                eprintln!("{id}: {warning}");
            }
            let session_keys: Vec<String> =
                edits.entries().into_iter().map(|(key, _)| key).collect();
            // The two overlays live in the plugin's own file. Reading them here is
            // what lets a row say *where* a tag came from, and what gives the
            // project and AoE sections the entries they own (as opposed to the
            // ones they merely inherit).
            let project_edits = levels
                .as_ref()
                .map(|levels| levels.project(&path).clone())
                .unwrap_or_default();
            let aoe_edits = levels
                .as_ref()
                .map(|levels| levels.aoe.clone())
                .unwrap_or_default();
            let project_keys = owned_keys(&project_edits);
            let aoe_keys = owned_keys(&aoe_edits);
            let mut tag_rows: Vec<serde_json::Value> = Vec::new();
            // The pane answers "what is this session sending", so it shows the
            // rendered tag set — `git.branch = git-tags`, not the reference it was
            // written as. The editor page below shows the stored entry instead,
            // because that is what a user edits.
            // The family this session was launched with, not the one the settings
            // hold now: an edited family applies to the next session, and the pane
            // must not relabel a running one.
            let (families, family_warnings) = match &app_dir {
                Ok(app_dir) => crate::activity::for_session(app_dir, &id, &snap.families),
                Err(_) => (snap.families.clone(), Vec::new()),
            };
            for warning in family_warnings {
                eprintln!("{id}: {warning}");
            }
            let activity = crate::activity::of(&families, &snap.tags);
            for value in crate::activity::unknown(&families, &snap.tags) {
                // A family is closed too: a typo is reported rather than given a
                // guessed icon: "testing" is not "test" unless the family says so.
                eprintln!(
                    "{id}: `{KEY}:{value}` is not an activity ({})",
                    families.names_in_use(),
                    KEY = crate::activity::KEY
                );
            }
            for tag in &snap.tags {
                let (origin, origin_text, origin_tone) = if session_keys.contains(&tag.key) {
                    ("session", "this session", "info")
                } else if project_keys.contains(&tag.key) {
                    ("project", "project", "success")
                } else if aoe_keys.contains(&tag.key) {
                    ("aoe", "AoE", "warn")
                } else {
                    ("config", "settings", "neutral")
                };
                // The activity row names what the session is doing instead of
                // repeating the key, and carries the vocabulary's own icon.
                let named = (tag.key == crate::activity::KEY)
                    .then(|| {
                        tag.value
                            .as_str()
                            .and_then(|value| families.find(value))
                            .map(|found| {
                                (
                                    found.label.to_string(),
                                    found.icon.clone(),
                                    found.tone.clone(),
                                )
                            })
                    })
                    .flatten();
                let mut row = serde_json::json!({
                    "kind": "row",
                    "label": named
                        .as_ref()
                        .map(|(label, _, _)| label.clone())
                        .unwrap_or_else(|| tag.key.clone()),
                    "value": match &tag.value {
                        serde_json::Value::String(text) => text.clone(),
                        other => other.to_string(),
                    },
                    "mono": true,
                    "badges": [{"text": origin_text, "tone": origin_tone}],
                    // Hiding is a per-session action even when the entry came from
                    // above: the project and AoE sections are the ones that edit
                    // those levels, and a click on a row here must not do that.
                    "tooltip": "hide this tag for this session (the level that set it keeps it)",
                    "method": "tag.remove",
                    "params": {"key": tag.key, "origin": origin},
                });
                if let Some((_, icon, tone)) = &named {
                    // Only a recognised activity carries an icon; `null` would
                    // have to be filtered by the host, and a plain tag is fine
                    // without one.
                    row["icon"] = serde_json::json!(icon);
                    row["tone"] = serde_json::json!(tone);
                }
                tag_rows.push(row);
            }
            // A key this session hid is not gone: offer the way back, so the
            // panel can undo a drop without a terminal.
            for key in &edits.unset {
                tag_rows.push(serde_json::json!({
                    "kind": "row",
                    "label": key,
                    "value": "(hidden)",
                    "mono": true,
                    "badges": [{"text": "hidden here", "tone": "warn"}],
                    "tooltip": "let the inherited value show again for this session",
                    "method": "tag.restore",
                    "params": {"key": key},
                }));
            }
            let git_keys = crate::levels::git_tag_keys();
            let git_present = !git_keys.is_empty()
                && git_keys
                    .iter()
                    .all(|key| snap.tags.iter().any(|tag| &tag.key == key));
            let git_own = edits.git_tags;
            let git_own_entries = !git_keys.is_empty()
                && git_keys.iter().all(|key| {
                    edits
                        .tags
                        .iter()
                        .filter_map(|raw| resolver::Tag::parse(raw).ok())
                        .any(|tag| &tag.key == key)
                });
            let git_on = git_own != Some(false) && git_present;
            // Unticking an explicit copy is the old "undo" (`tag.preset`): the
            // entries were written here, so dropping them lets an inherited value
            // show. Turning off a default that lives above this session needs the
            // explicit `tag.git` instead.
            let (git_method, git_params) = if !git_on {
                ("tag.git", serde_json::json!({"enabled": true}))
            } else if git_own_entries && git_own.is_none() {
                (
                    "tag.preset",
                    serde_json::json!({"preset": "git", "applied": false}),
                )
            } else {
                ("tag.git", serde_json::json!({"enabled": false}))
            };
            let git_value = match git_own {
                Some(true) => "on here",
                Some(false) => "off here",
                None if git_present => "inherited on",
                None => "off",
            };
            let own_activity = activity_own(&edits.tags, &edits.unset);
            let activity_value = match &own_activity {
                Some(Some(key)) => families
                    .find(key)
                    .map(|found| format!("{} (this session)", found.label))
                    .unwrap_or_else(|| format!("{key} (this session)")),
                Some(None) => "none here".to_string(),
                None => activity
                    .map(|now| format!("{} (inherited)", now.label))
                    .unwrap_or_else(|| "not set".to_string()),
            };

            let mut session_rows = tag_rows;
            session_rows.push(serde_json::json!({
                "kind": "row",
                "label": "Standard git information",
                "icon": "git-branch",
                "value": git_value,
                "mono": true,
                "selected": git_on,
                "badges": [{"text": "preset", "tone": "neutral"}],
                "tooltip": if git_on {
                    "hide the sample git entries for this session"
                } else {
                    "include vcs:git, git.branch, git.commit and git.tag for this session"
                },
                "method": git_method,
                "params": git_params,
            }));
            session_rows.push(serde_json::json!({
                "kind": "row",
                "label": "Reset this session's edits",
                "icon": "rotate-ccw",
                "tone": "danger",
                "tooltip": "forget every entry and every hide this session has",
                "method": "tags.reset",
                "params": {},
            }));

            let mut target_rows: Vec<serde_json::Value> = snap
                .resolved
                .iter()
                .map(|item| {
                    serde_json::json!({
                        "kind": "row",
                        "label": item.header,
                        "value": elide(&unpublished(&item.value, &edits.token), 80),
                        "mono": true,
                    })
                })
                .collect();

            // The entries a session carries, minus the dynamic activity key: a
            // default that says "this session is coding" would be wrong the
            // moment it is copied. Activity has its own default picker at each
            // level, so it is still one click away.
            let default_entries: Vec<String> = snap
                .settings
                .tags
                .iter()
                .filter(|raw| {
                    resolver::Tag::parse(raw)
                        .map(|tag| tag.key != crate::activity::KEY)
                        .unwrap_or(false)
                })
                .cloned()
                .collect();

            let activity_rows: Vec<serde_json::Value> = families
                .chosen()
                .activities
                .iter()
                .map(|candidate| {
                    let selected = match own_activity.as_ref() {
                        Some(Some(now)) => now == &candidate.key,
                        Some(None) => false,
                        None => activity.is_some_and(|now| now.key == candidate.key),
                    };
                    serde_json::json!({
                        "kind": "row",
                        "label": candidate.label,
                        "icon": candidate.icon,
                        "tone": candidate.tone,
                        "mono": true,
                        "selected": selected,
                        "badges": [{"text": crate::activity::KEY, "tone": "neutral"}],
                        "tooltip": if selected {
                            "this is what the session is doing now"
                        } else {
                            "set this session's activity"
                        },
                        "method": "tag.set",
                        "params": {"key": crate::activity::KEY, "value": candidate.key},
                    })
                })
                .collect();
            let others = families
                .names()
                .into_iter()
                .filter(|name| *name != families.chosen)
                .collect::<Vec<_>>();
            let mut activity_children = activity_rows;
            activity_children.push(serde_json::json!({
                "kind": "row",
                "label": "Inherit from the project or AoE",
                "icon": "corner-up-left",
                "mono": true,
                "selected": own_activity.is_none() && activity.is_none(),
                "tooltip": "stop setting an activity for this session and use the inherited default",
                "method": "tag.restore",
                "params": {"key": crate::activity::KEY},
            }));
            activity_children.push(serde_json::json!({
                "kind": "row",
                "label": "No activity here",
                "icon": "circle-slash",
                "mono": true,
                "tone": "warn",
                "selected": own_activity == Some(None),
                "tooltip": "hide an inherited activity for this session",
                "method": "tag.remove",
                "params": {"key": crate::activity::KEY},
            }));
            activity_children.push(serde_json::json!({
                "kind": "note",
                "text": if others.is_empty() {
                    format!("family {} ({})", families.chosen, families.source)
                } else {
                    format!(
                        "family {} ({}) — also defined here: {}",
                        families.chosen,
                        families.source,
                        others.join(", ")
                    )
                },
                "tone": "neutral",
            }));

            let mut session_blocks: Vec<serde_json::Value> = Vec::new();
            if !edits.token.is_empty() {
                session_blocks.push(serde_json::json!({
                    "kind": "note",
                    "text": "a token override is set for this session (value not shown here)",
                    "tone": "info",
                }));
            }
            if !target_rows.is_empty() {
                session_blocks.push(serde_json::json!({
                    "kind": "heading",
                    "text": "What this session sends",
                }));
                session_blocks.append(&mut target_rows);
                session_blocks.push(serde_json::json!({"kind": "divider"}));
            }
            session_blocks.push(serde_json::json!({
                "kind": "heading",
                "text": "Tags",
            }));
            if session_rows.is_empty() {
                session_blocks.push(serde_json::json!({
                    "kind": "note",
                    "text": "no tags are set for this session",
                    "tone": "neutral",
                }));
            } else {
                session_blocks.append(&mut session_rows);
            }
            session_blocks.push(serde_json::json!({"kind": "divider"}));
            session_blocks.push(serde_json::json!({
                "kind": "section",
                "title": "Activity",
                "value": activity_value,
                "collapsible": true,
                "collapsed": false,
                "children": activity_children,
            }));
            session_blocks.push(serde_json::json!({"kind": "divider"}));
            session_blocks.push(serde_json::json!({
                "kind": "action",
                "label": "Edit tags as JSON",
                "href": EDITOR_PAGE,
            }));
            session_blocks.push(serde_json::json!({
                "kind": "note",
                "text": "The session tab overrides this one session. The project and AoE tabs set defaults for everything below them.",
                "tone": "neutral",
            }));

            // One pane, three tabs. The host's pane vocabulary has no tab kind,
            // so a tab is a `row` carrying a method: the open one is drawn with
            // the host's own `selected` accent, and each row states the summary
            // the old three-tab layout put in that pane's footer.
            let has_project = !path.is_empty();
            let active = self.active_tab(&id, has_project).to_string();
            let project_summary = format!(
                "{} override(s), {} hidden",
                project_edits.tags.len(),
                project_edits.unset.len()
            );
            let aoe_summary = format!(
                "{} override(s), {} hidden",
                aoe_edits.tags.len(),
                aoe_edits.unset.len()
            );
            // All three rows speak the same language: what that level holds of its
            // own, not what the session ends up sending.
            let session_summary = format!(
                "{} override(s), {} hidden",
                edits.tags.len(),
                edits.unset.len()
            );
            let mut pane_blocks = vec![tab_blocks(
                &active,
                has_project,
                &[
                    ("session", "Session", session_summary.as_str()),
                    ("project", "Project", project_summary.as_str()),
                    ("aoe", "AoE", aoe_summary.as_str()),
                ],
            )];
            pane_blocks.push(serde_json::json!({"kind": "divider"}));
            let footer = match active.as_str() {
                "project" => (
                    "defaults for every session in this checkout".to_string(),
                    project_summary.clone(),
                ),
                "aoe" => (
                    "defaults for every session on this machine".to_string(),
                    aoe_summary.clone(),
                ),
                _ => (
                    "a row's badge says where the tag comes from".to_string(),
                    format!("{} target(s)", snap.resolved.len()),
                ),
            };
            match active.as_str() {
                "project" => pane_blocks.extend(level_blocks(
                    crate::levels::PROJECT,
                    &path,
                    &project_edits,
                    &families,
                    &default_entries,
                )),
                "aoe" => pane_blocks.extend(level_blocks(
                    crate::levels::AOE,
                    "",
                    &aoe_edits,
                    &families,
                    &default_entries,
                )),
                _ => pane_blocks.append(&mut session_blocks),
            }
            let _ = self.call_host(
                "ui.state.set",
                serde_json::json!({
                    "slot": "pane",
                    "id": "llm_tags",
                    "session_id": id,
                    "payload": {
                        "title": "LLM Tags",
                        "default_location": "right",
                        "blocks": pane_blocks,
                        "footer": {
                            "text": footer.0,
                            "value": footer.1,
                        },
                    }
                }),
            );

            page_sessions.push(serde_json::json!({
                "id": id,
                "title": title,
                "tool": tool,
                "targets": snap.resolved.iter().map(|item| serde_json::json!({
                    "name": item.header,
                    "env_var": item.env_var,
                    "value": unpublished(&item.value, &edits.token),
                })).collect::<Vec<_>>(),
                "tags": snap.settings.tags.iter().filter_map(|raw| {
                    let tag = resolver::Tag::parse(raw).ok()?;
                    let origin = if session_keys.contains(&tag.key) {
                        "session"
                    } else if project_keys.contains(&tag.key) {
                        "project"
                    } else if aoe_keys.contains(&tag.key) {
                        "aoe"
                    } else {
                        "config"
                    };
                    Some(serde_json::json!({
                        "key": tag.key,
                        "value": tag.value,
                        "origin": origin,
                    }))
                }).collect::<Vec<_>>(),
                "unset": edits.unset,
                // The JSON viewer's two halves: the effective map is what the
                // targets will render, and each level's own map is what that
                // level's file says. A `null` means "hidden here", not "unset".
                "effective": snap.tags.iter().map(|tag| {
                    (tag.key.clone(), tag.value.clone())
                }).collect::<serde_json::Map<String, Value>>(),
                "own": values_object(&edits.tags, &edits.unset),
                "levels": {
                    "aoe": level_values("", &aoe_edits),
                    "project": if path.is_empty() {
                        Value::Null
                    } else {
                        level_values(&path, &project_edits)
                    },
                },
                // Never the token itself: the page is a read-only surface for a
                // value the host keeps, so it learns that one is set and nothing
                // about what it says. The CLI (`tag --show`) is where it is read.
                "token_set": !edits.token.is_empty(),
                // The activity is edited in the pane and in the JSON viewer, so
                // the page is told the family but never the token.
                "activity": activity.map(|now| serde_json::json!({
                    "key": now.key,
                    "label": now.label,
                    "icon": now.icon,
                    "tone": now.tone,
                })),
                "path": match &app_dir {
                    Ok(app_dir) => crate::session_tags::path(app_dir, &id)
                        .display()
                        .to_string(),
                    Err(_) => String::new(),
                },
            }));
        }

        // The page's dropdown is fed from the same catalogue the `references`
        // verb prints, so a reference can never be insertable-but-unresolvable.
        let references: Vec<serde_json::Value> = resolver::reference_catalog()
            .iter()
            .map(|item| {
                serde_json::json!({
                    "name": item.name,
                    "reference": format!("{{{}}}", item.name),
                    "group": item.group,
                    "description": item.description,
                })
            })
            .collect();
        let presets: Vec<serde_json::Value> = crate::session_tags::PRESETS
            .iter()
            .map(|(id, tags)| {
                serde_json::json!({
                    "id": id,
                    "label": match *id {
                        "git" => "Add standard git information",
                        other => other,
                    },
                    "tags": tags,
                })
            })
            .collect();
        let intent: Vec<String> = harness_extension_val
            .as_ref()
            .and_then(|value| value.as_array())
            .map(|names| {
                names
                    .iter()
                    .filter_map(|name| name.as_str())
                    .map(|name| name.trim().to_ascii_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        let extensions = extension_rows(&adapters, &intent, app_dir.as_deref().ok());
        let _ = self.call_host(
            "ui.state.set",
            serde_json::json!({
                "slot": "page",
                "id": "tags",
                "payload": {
                    "sessions": page_sessions,
                    "references": references,
                    "presets": presets,
                    "extensions": extensions,
                    "page": EDITOR_PAGE,
                }
            }),
        );
    }
}

/// The worker methods a pane or page may fire. The host forwards a block's
/// `method` verbatim, so these are whole names — not the trailing segment that
/// names a host command (`plugin.<id>.status`).
pub const EDITS: [&str; 17] = [
    // Not an edit: it only moves the one pane between its three tabs, and goes
    // through the same fire-and-publish path so a click re-renders.
    "pane.tab",
    "tag.set",
    "tag.remove",
    "tag.restore",
    "tag.preset",
    "tag.git",
    "tag.level.set",
    "tag.level.unset",
    "tag.level.restore",
    "tag.level.replace",
    "tag.level.reset",
    "tag.level.git",
    "token.set",
    "token.clear",
    "tags.reset",
    // Machine-wide, not session-wide: the page is where a user reads the
    // disclosure and approves it, and the host merges a `session_id` into every
    // page action, which these two simply ignore.
    "extension.enable",
    "extension.disable",
];

/// The one pane's tabs, left to right: this session, this project's defaults and
/// the machine-wide ones. The order is the layout, so it is spelled once.
pub const TABS: [&str; 3] = ["session", "project", "aoe"];

/// What this level says about the activity, if anything. `Some(Some(key))` is
/// an entry here, `Some(None)` is a hide here, and `None` inherits.
fn activity_own(tags: &[String], unset: &[String]) -> Option<Option<String>> {
    if unset.iter().any(|key| key == crate::activity::KEY) {
        return Some(None);
    }
    tags.iter()
        .filter_map(|raw| resolver::Tag::parse(raw).ok())
        .find(|tag| tag.key == crate::activity::KEY)
        .map(|tag| Some(tag.value.as_str().unwrap_or_default().to_string()))
}

fn level_params(level: &str, project_path: &str, extra: serde_json::Value) -> serde_json::Value {
    let mut params = serde_json::json!({"level": level});
    if level == crate::levels::PROJECT {
        params["project_path"] = serde_json::json!(project_path);
    }
    if let Some(extra) = extra.as_object() {
        for (key, value) in extra {
            params[key] = value.clone();
        }
    }
    params
}

/// The tab strip at the top of the pane, laid out left to right.
///
/// The host's pane vocabulary has no tab kind, so a tab is a button carrying a
/// `method`: an `action` block, with the open one drawn in the host's `primary`
/// variant. A `row` per tab would stack them down the pane, and a tab strip is
/// the one thing that must not be a list.
///
/// `columns` is the only horizontal kind, and the host draws it as a grid of at
/// most two, so three tabs are a two-wide grid holding a two-wide grid: the
/// buttons land at the left edge, a quarter and a half of the pane. Buttons are
/// as wide as their label, not their cell, so the third one is not stretched to
/// the half the grid gives it.
///
/// Each button's tooltip carries what that level holds, so a closed tab still
/// answers "what have I set here"; the footer repeats it for the open one. A
/// session with no project has no project tab, so the button is left out rather
/// than shown as a dead end.
fn tab_blocks(active: &str, has_project: bool, tabs: &[(&str, &str, &str)]) -> serde_json::Value {
    let buttons: Vec<serde_json::Value> = tabs
        .iter()
        .filter(|(tab, ..)| *tab != crate::levels::PROJECT || has_project)
        .map(|(tab, label, summary)| {
            let open = *tab == active;
            let mut button = serde_json::json!({
                "kind": "action",
                "label": label,
                "method": "pane.tab",
                "params": {"tab": tab},
                "tooltip": if open {
                    format!("showing now \u{00b7} {summary}")
                } else {
                    format!("show this level \u{00b7} {summary}")
                },
            });
            if open {
                button["variant"] = serde_json::json!("primary");
            }
            button
        })
        .collect();
    let cells: Vec<serde_json::Value> = buttons
        .chunks(2)
        .map(|pair| serde_json::json!({"kind": "columns", "children": pair}))
        .collect();
    serde_json::json!({"kind": "columns", "children": cells})
}

/// One level's whole tab. The session level has its own rows; a project or AoE
/// tab turns the entries in `<app dir>/tags.toml` into rows and adds the two
/// behaviours that are not tags: the default activity and the git sample.
fn level_blocks(
    level: &str,
    project_path: &str,
    edits: &crate::levels::EditSet,
    families: &crate::activity::Families,
    default_entries: &[String],
) -> Vec<serde_json::Value> {
    let mut rows: Vec<serde_json::Value> = Vec::new();
    for raw in &edits.tags {
        let Ok(tag) = resolver::Tag::parse(raw) else {
            continue;
        };
        rows.push(serde_json::json!({
            "kind": "row",
            "label": tag.key,
            "value": match &tag.value {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            },
            "mono": true,
            "badges": [{"text": if level == crate::levels::PROJECT { "project" } else { "AoE" }, "tone": "info"}],
            "tooltip": "hide this entry here; it stays in the file and can be restored",
            "method": "tag.level.unset",
            "params": level_params(level, project_path, serde_json::json!({"key": tag.key})),
        }));
    }
    for key in &edits.unset {
        rows.push(serde_json::json!({
            "kind": "row",
            "label": key,
            "value": "(hidden)",
            "mono": true,
            "badges": [{"text": "hidden", "tone": "warn"}],
            "tooltip": "let the inherited value show again here",
            "method": "tag.level.restore",
            "params": level_params(level, project_path, serde_json::json!({"key": key})),
        }));
    }
    if edits.tags.is_empty() && edits.unset.is_empty() {
        rows.push(serde_json::json!({
            "kind": "row",
            "label": "No tag overrides here: everything is inherited",
            "mono": true,
        }));
    }

    let own_activity = activity_own(&edits.tags, &edits.unset);
    let activity_value = match &own_activity {
        Some(Some(key)) => families
            .find(key)
            .map(|found| format!("{} (set here)", found.label))
            .unwrap_or_else(|| format!("{key} (set here)")),
        Some(None) => "none here".to_string(),
        None => "inherited".to_string(),
    };
    let mut activity_rows: Vec<serde_json::Value> = families
        .chosen()
        .activities
        .iter()
        .map(|candidate| {
            let selected = own_activity
                .as_ref()
                .is_some_and(|now| now.as_deref() == Some(candidate.key.as_str()));
            serde_json::json!({
                "kind": "row",
                "label": candidate.label,
                "icon": candidate.icon,
                "tone": candidate.tone,
                "mono": true,
                "selected": selected,
                "badges": [{"text": crate::activity::KEY, "tone": "neutral"}],
                "tooltip": "make this the default activity at this level",
                "method": "tag.level.set",
                "params": level_params(
                    level,
                    project_path,
                    serde_json::json!({"key": crate::activity::KEY, "value": candidate.key}),
                ),
            })
        })
        .collect();
    activity_rows.push(serde_json::json!({
        "kind": "row",
        "label": "Inherit from below",
        "icon": "corner-up-left",
        "mono": true,
        "selected": own_activity.is_none(),
        "tooltip": "stop overriding the activity at this level",
        "method": "tag.level.restore",
        "params": level_params(level, project_path, serde_json::json!({"key": crate::activity::KEY})),
    }));
    activity_rows.push(serde_json::json!({
        "kind": "row",
        "label": "No activity here",
        "icon": "circle-slash",
        "mono": true,
        "tone": "warn",
        "selected": own_activity == Some(None),
        "tooltip": "hide the inherited activity for everything at this level",
        "method": "tag.level.unset",
        "params": level_params(level, project_path, serde_json::json!({"key": crate::activity::KEY})),
    }));

    let git_keys = crate::levels::git_tag_keys();
    let own_git = !git_keys.is_empty()
        && git_keys.iter().all(|key| {
            edits
                .tags
                .iter()
                .filter_map(|raw| resolver::Tag::parse(raw).ok())
                .any(|tag| &tag.key == key)
        });
    let git_on = edits.git_tags == Some(true) || (edits.git_tags.is_none() && own_git);
    let git_value = if git_on {
        "on here"
    } else if edits.git_tags == Some(false) {
        "off here"
    } else {
        "inherited"
    };
    let git_row = serde_json::json!({
        "kind": "row",
        "label": "Standard git information",
        "icon": "git-branch",
        "value": git_value,
        "mono": true,
        "selected": git_on,
        "badges": [{"text": "preset", "tone": "neutral"}],
        "tooltip": "include or hide vcs:git, git.branch, git.commit and git.tag at this level",
        "method": "tag.level.git",
        "params": level_params(level, project_path, serde_json::json!({"enabled": !git_on})),
    });

    let mut blocks = vec![serde_json::json!({"kind": "heading", "text": "Tags"})];
    blocks.extend(rows);
    blocks.push(serde_json::json!({"kind": "divider"}));
    blocks.push(serde_json::json!({
        "kind": "section",
        "title": "Default activity",
        "value": activity_value,
        "collapsible": true,
        "collapsed": false,
        "children": activity_rows,
    }));
    blocks.push(serde_json::json!({"kind": "divider"}));
    blocks.push(serde_json::json!({"kind": "heading", "text": "Options"}));
    blocks.push(git_row);

    let entries: Vec<String> = default_entries.iter().take(20).cloned().collect();
    blocks.push(serde_json::json!({
        "kind": "row",
        "label": if level == crate::levels::PROJECT {
            "Use this session's tags as this project's default"
        } else {
            "Use this session's tags for every session"
        },
        "icon": "copy",
        "tooltip": "copy this session's current entries into this level",
        "method": "tag.level.replace",
        "params": level_params(level, project_path, serde_json::json!({"entries": entries})),
    }));
    blocks.push(serde_json::json!({
        "kind": "row",
        "label": if level == crate::levels::PROJECT {
            "Clear this project's overrides"
        } else {
            "Clear the AoE defaults"
        },
        "icon": "rotate-ccw",
        "tone": "danger",
        "tooltip": "remove this level's entries and hides; the settings are untouched",
        "method": "tag.level.reset",
        "params": level_params(level, project_path, serde_json::json!({})),
    }));
    blocks.push(serde_json::json!({"kind": "divider"}));
    blocks.push(serde_json::json!({
        "kind": "action",
        "label": "Edit tags as JSON",
        "href": EDITOR_PAGE,
    }));
    blocks.push(serde_json::json!({
        "kind": "note",
        "text": if level == crate::levels::PROJECT {
            "This tab is the default for every session in this checkout; the session tab can still override it."
        } else {
            "This tab is the default for every session on this machine; a project or a session can still override it."
        },
        "tone": "neutral",
    }));
    blocks
}

/// A level's editable values: its explicit entries plus `null` for a key it
/// hides. Injected behaviours such as the git sample are deliberately absent —
/// they are switches, and showing them as four editable keys would make the
/// JSON and the switch disagree.
/// The keys a level owns. A git switch owns the sample's keys even though it
/// stores one boolean rather than four entries, so an origin badge can still
/// say "project" or "AoE" for them.
fn owned_keys(edits: &crate::levels::EditSet) -> Vec<String> {
    let mut keys = edits.keys();
    if edits.git_tags == Some(true) {
        keys.extend(crate::levels::git_tag_keys());
    }
    keys
}

fn values_object(tags: &[String], unset: &[String]) -> serde_json::Map<String, Value> {
    let mut values = serde_json::Map::new();
    for raw in tags {
        if let Ok(tag) = resolver::Tag::parse(raw) {
            values.insert(tag.key, tag.value);
        }
    }
    for key in unset {
        values.insert(key.clone(), Value::Null);
    }
    values
}

fn level_values(path: &str, edits: &crate::levels::EditSet) -> Value {
    serde_json::json!({
        "path": path,
        "values": values_object(&edits.tags, &edits.unset),
        "git_tags": edits.git_tags,
    })
}

/// The editor page's route: the dashboard serves it from the manifest's
/// `entry`, so a relative link is the only portable spelling.
const EDITOR_PAGE: &str = "/plugins/dev.oozut.llm-tags/tags";

/// A value for a pane row: long enough to recognise, short enough not to push
/// the payload past its 64 KiB cap on a wide session list.
/// What a surface may say about one rendered value.
///
/// A session's literal token stands in for the rendered payload, so the target
/// row that shows it *is* the token. A surface never carries it: `ui.state.set`
/// payloads are kept by the host and rendered to everyone who can see the
/// session, so the value stays in the session file and the surface names it.
/// `aoe-llm-tags tag --show` and the `status` command remain the read-back, for
/// the operator who already knows it.
fn unpublished(value: &str, token: &str) -> String {
    if !token.is_empty() && value == token {
        TOKEN_NOT_SHOWN.to_string()
    } else {
        value.to_string()
    }
}

/// What a surface shows where the session's token would be.
const TOKEN_NOT_SHOWN: &str = "(the session's token, not shown here)";

fn elide(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let head: String = value.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// The tag entry a pane/page edit names.
///
/// A page sends `key` and `value` apart; a pane row sends a whole entry. A value
/// is written with `:=` and JSON so a value containing a colon (`https://…`) or a
/// leading space survives the round trip unambiguously.
fn tag_entry(params: &serde_json::Value) -> Option<String> {
    if let Some(tag) = params.get("tag").and_then(serde_json::Value::as_str) {
        return Some(tag.to_string());
    }
    let key = params.get("key").and_then(serde_json::Value::as_str)?;
    match params.get("value") {
        None | Some(serde_json::Value::Null) => Some(key.to_string()),
        Some(serde_json::Value::String(text)) => Some(format!(
            "{key}:={}",
            serde_json::Value::String(text.clone())
        )),
        Some(other) => Some(format!("{key}:={other}")),
    }
}

/// One request line → one response document (`None` for a notification).
pub fn handle_line(line: &str, env: &Env) -> Option<Value> {
    let request: Value = serde_json::from_str(line).ok()?;
    let id = request.get("id")?.clone();
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // The host maps command ids to `plugin.<id>.<command>`, so dispatch on the
    // trailing segment and either form works.
    let command = method.rsplit('.').next().unwrap_or_default();
    Some(match command {
        "status" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": status(env),
        }),
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": METHOD_NOT_FOUND,
                "message": format!("unknown method {method:?}"),
            },
        }),
    })
}

/// What this install would send right now, for the `status` command.
/// What the page shows for each harness that could take an extension: whether it
/// was asked for, whether it was approved, and the exact file and wording that
/// approval covers. The page never decides anything — it echoes the digest back.
fn extension_rows(
    adapters: &[crate::adapters::Adapter],
    intent: &[String],
    app_dir: Option<&std::path::Path>,
) -> Vec<Value> {
    let Some(app_dir) = app_dir else {
        return Vec::new();
    };
    let (receipts, _) = crate::harness_extension::receipts(app_dir);
    adapters
        .iter()
        .filter(|adapter| {
            crate::harness_extension::carrier(adapter).is_some()
                || intent.iter().any(|name| name == &adapter.agent)
        })
        .map(|adapter| {
            let harness = adapter.agent.clone();
            let requested = intent.iter().any(|name| name == &harness);
            let receipt = receipts.get(&harness);
            let carrier = crate::harness_extension::carrier(adapter);
            let sha256 = carrier
                .and_then(|carrier| crate::harness_extension::bundle_sha(adapter, carrier).ok());
            // One verdict, the same one the launch and `doctor` use; the page only
            // renders it, so the three can never disagree.
            let verdict = crate::harness_extension::plan(
                adapter,
                intent,
                app_dir,
                "",
                crate::harness_extension::home().as_deref(),
            );
            let (state, detail) = match &verdict {
                Ok(_) => ("ok".to_string(), String::new()),
                Err(refusal) => (refusal.reason().to_string(), refusal.detail()),
            };
            let disclosure = match (&verdict, &sha256) {
                (Ok(plan), _) => crate::harness_extension::disclosure(plan, ""),
                // Not allowed (or not asked for yet), so the page shows the same
                // text the approval would cover if the user goes ahead.
                _ => crate::harness_extension::proposal(
                    adapter,
                    app_dir,
                    "",
                    crate::harness_extension::home().as_deref(),
                )
                .map(|plan| {
                    crate::harness_extension::disclosure(&plan, "")
                        .lines()
                        .skip(3)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),
            };
            serde_json::json!({
                "harness": harness,
                "requested": requested,
                "approved": receipt.is_some(),
                "state": state,
                "detail": detail,
                "sha256": sha256,
                // What the approval covers, spelled the way the disclosure
                // spells it, and where it lands: a session copy or the user's
                // own config.
                "scope": carrier.map(|carrier| carrier.name()).unwrap_or("none"),
                "files": carrier
                    .map(|carrier| {
                        crate::harness_extension::bundle(adapter, carrier)
                            .parts()
                            .map(|placement| placement.display.clone())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
                "disclosure_version": crate::harness_extension::DISCLOSURE_VERSION,
                "accepted_at": receipt.map(|receipt| receipt.accepted_at.clone()),
                "disclosure": disclosure,
            })
        })
        .collect()
}

pub fn status(env: &Env) -> Value {
    // Same resolution as the hook: the worker inherits the daemon's environment,
    // so AOE_* fallbacks matter here exactly as they do at session start.
    let flags = Flags::from_given(&Given::default(), env);
    let snap = resolver::snapshot(&flags, env, None);
    let headers: serde_json::Map<String, Value> = snap
        .resolved
        .iter()
        .map(|item| (item.header.clone(), Value::String(item.value.clone())))
        .collect();
    let session = if snap.facts.id.is_empty() {
        "(none)".to_string()
    } else {
        snap.facts.id.clone()
    };
    json!({
        "ok": true,
        "message": format!("{} target(s) for session {session}", headers.len()),
        "session": session,
        "headers": headers,
        "warnings": snap.warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(value: Value) -> String {
        value.to_string()
    }

    #[test]
    fn status_answers_with_the_resolved_headers() {
        let env = Env::new([("HOME", "/tmp"), ("AOE_SESSION_ID", "abcd1234")]);
        let response = handle_line(
            &line(json!({"jsonrpc": "2.0", "id": 1, "method": "plugin.dev.oozut.llm-tags.status"})),
            &env,
        )
        .expect("a response");
        assert_eq!(response["id"], 1);
        assert_eq!(response["result"]["ok"], true);
        // The default target sends one JSON object, so a consumer reads a field.
        let raw = response["result"]["headers"][resolver::DEFAULT_HEADER]
            .as_str()
            .expect("a header value");
        let payload: Value = serde_json::from_str(raw).expect("the value is JSON");
        assert_eq!(payload["session"], "S-abcd");
        assert_eq!(payload["session_id"], "abcd1234");
        assert_eq!(payload["tool"], "unknown");
    }

    #[test]
    fn an_unknown_method_is_method_not_found() {
        let env = Env::default();
        let response = handle_line(
            &line(json!({"jsonrpc": "2.0", "id": 7, "method": "plugin.dev.oozut.llm-tags.nope"})),
            &env,
        )
        .expect("a response");
        assert_eq!(response["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn a_notification_is_never_answered() {
        let env = Env::default();
        assert!(handle_line(
            &line(json!({"jsonrpc": "2.0", "method": "session.status.changed"})),
            &env
        )
        .is_none());
    }

    #[test]
    fn garbage_is_ignored() {
        assert!(handle_line("not json", &Env::default()).is_none());
    }

    #[test]
    fn an_edit_names_a_tag_the_way_the_settings_would() {
        // A page sends key and value apart; the value is written as JSON so a
        // colon in it (`https://…`) does not turn into a second separator.
        assert_eq!(
            tag_entry(&json!({"key": "team", "value": "platform"})).as_deref(),
            Some(r#"team:="platform""#)
        );
        assert_eq!(
            tag_entry(&json!({"key": "url", "value": "https://example.test/x"})).as_deref(),
            Some(r#"url:="https://example.test/x""#)
        );
        assert_eq!(
            tag_entry(&json!({"key": "platform"})).as_deref(),
            Some("platform")
        );
        assert_eq!(
            tag_entry(&json!({"key": "cost", "value": 1.5})).as_deref(),
            Some("cost:=1.5")
        );
        // A whole entry from a pane row passes through the same parser the
        // settings use, so there is only ever one grammar.
        assert_eq!(
            tag_entry(&json!({"tag": "epic:checkout"})).as_deref(),
            Some("epic:checkout")
        );
        assert_eq!(tag_entry(&json!({"value": "orphan"})), None);
        for params in [
            json!({"key": "team", "value": "platform"}),
            json!({"key": "url", "value": "https://example.test/x"}),
            json!({"key": "cost", "value": 1.5}),
            json!({"tag": "epic:checkout"}),
            json!({"key": "marker"}),
        ] {
            let entry = tag_entry(&params).expect("an entry");
            assert!(resolver::Tag::parse(&entry).is_ok(), "{entry}");
        }
    }

    #[test]
    fn a_pane_row_does_not_grow_past_the_payload_cap() {
        let short = "a".repeat(10);
        assert_eq!(elide(&short, 80), short);
        let long = "a".repeat(200);
        let cut = elide(&long, 80);
        assert_eq!(cut.chars().count(), 80);
        assert!(cut.ends_with('…'), "{cut}");
        // A multi-byte value is cut by characters, never mid-character.
        let wide = "é".repeat(100);
        assert_eq!(elide(&wide, 10).chars().count(), 10);
    }

    #[test]
    fn status_and_hook_share_one_resolver() {
        let env = Env::new([("HOME", "/tmp"), ("AOE_SESSION_ID", "abcd1234")]);
        let flags = Flags::from_given(&Given::default(), &env);
        let snap = resolver::snapshot(&flags, &env, None);
        let response = handle_line(
            &line(json!({"jsonrpc": "2.0", "id": 9, "method": "plugin.dev.oozut.llm-tags.status"})),
            &env,
        )
        .expect("a response");
        let headers = response["result"]["headers"].as_object().expect("headers");
        assert_eq!(headers.len(), snap.resolved.len());
        for item in &snap.resolved {
            assert_eq!(
                headers.get(&item.header).and_then(Value::as_str),
                Some(item.value.as_str())
            );
        }
    }
}

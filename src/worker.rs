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
        next_id: 1_000_000,
    };
    while host.process_one() {}
    0
}

struct Host {
    env: Env,
    rx: std::sync::mpsc::Receiver<String>,
    stdout: std::sync::Mutex<std::io::Stdout>,
    pending: std::collections::HashMap<i64, std::sync::mpsc::Sender<serde_json::Value>>,
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

    /// Push every surface the plugin renders: the row badge, the session pane
    /// and the editor page.
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
            // The badge shows the first rendered value — the token's stand-in for a
            // target with no template included, which is why it is worked out here,
            // after the session's own file has been read.
            let text = snap
                .resolved
                .first()
                .map(|item| unpublished(&item.value, &edits.token))
                .unwrap_or_default();
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
            let project_keys = project_edits.keys();
            let aoe_keys = aoe_edits.keys();
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
            let mut session_rows = tag_rows;
            // The sample set as one switch: the same write the CLI's `--git` and
            // the page's old tick box made, kept where the session is edited.
            let git_entries = crate::session_tags::preset("git").unwrap_or(&[]);
            let git_keys: Vec<&str> = git_entries
                .iter()
                .filter_map(|entry| entry.split(':').next())
                .collect();
            let git_applied = !git_keys.is_empty()
                && git_keys
                    .iter()
                    .all(|key| snap.tags.iter().any(|tag| tag.key == *key));
            session_rows.push(serde_json::json!({
                "kind": "row",
                "label": "Standard git information",
                "icon": "git-branch",
                "value": if git_applied { "on" } else { "off" },
                "mono": true,
                "selected": git_applied,
                "badges": [{"text": "preset", "tone": "neutral"}],
                "tooltip": if git_applied {
                    "remove the sample git entries from this session"
                } else {
                    "write vcs:git, git.branch, git.commit and git.tag for this session"
                },
                "method": "tag.preset",
                "params": {"preset": "git", "applied": !git_applied},
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
            // moment it is copied.
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

            let mut blocks: Vec<serde_json::Value> = vec![serde_json::json!({
                "kind": "heading",
                "text": "LLM Tags",
            })];
            blocks.append(&mut target_rows);
            blocks.push(serde_json::json!({"kind": "divider"}));
            // Three levels, most specific first. The session section is open
            // because that is what a pane is about; the two above it are one
            // click away and say how many overrides they own.
            blocks.push(level_section(
                "This session",
                None,
                "",
                &crate::levels::EditSet::default(),
                0,
                &[],
                session_rows,
            ));
            // Changing the activity from inside the session, not only from the
            // dashboard page: each row is a `tag.set`, and the current one is
            // marked so a human can see what the agent last declared. A terminal
            // UI renders these as text and tone, so the label carries the meaning
            // and the icon is decoration.
            let activity_rows: Vec<serde_json::Value> = families
                .chosen()
                .activities
                .iter()
                .map(|candidate| {
                    let current = activity.is_some_and(|now| now.key == candidate.key);
                    serde_json::json!({
                        "kind": "row",
                        "label": candidate.label,
                        "icon": candidate.icon,
                        "tone": candidate.tone,
                        "mono": true,
                        "selected": current,
                        "badges": [{"text": crate::activity::KEY, "tone": "neutral"}],
                        "tooltip": if current {
                            "this is what the session is doing now"
                        } else {
                            "set this session's activity"
                        },
                        "method": "tag.set",
                        "params": {"key": crate::activity::KEY, "value": candidate.key},
                    })
                })
                .collect();
            // Where the words come from, and what else this machine defines. The
            // page no longer renders an activity card, so this row is the only
            // place a person sees the family a session is pinned to without
            // opening the project form or the terminal.
            let others = families
                .names()
                .into_iter()
                .filter(|name| *name != families.chosen)
                .collect::<Vec<_>>();
            let mut activity_children = activity_rows;
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
            blocks.push(serde_json::json!({
                "kind": "section",
                "title": "Activity",
                "value": activity.map(|now| now.label.clone()).unwrap_or_else(|| "not set".to_string()),
                "collapsible": true,
                "collapsed": true,
                "children": activity_children,
            }));
            // A project section edits `<app dir>/tags.toml` for this repository;
            // an AoE section edits the same file for every session. Neither
            // touches the plugin's settings, so the host's config keeps what the
            // user wrote there.
            blocks.push(level_section(
                "This project",
                Some(crate::levels::PROJECT),
                &path,
                &project_edits,
                snap.settings.tags.len(),
                &default_entries,
                Vec::new(),
            ));
            blocks.push(level_section(
                "AoE — every session",
                Some(crate::levels::AOE),
                "",
                &aoe_edits,
                snap.settings.tags.len(),
                &default_entries,
                Vec::new(),
            ));
            if !edits.token.is_empty() {
                // The value never reaches a surface: `ui.state.set` payloads are
                // kept by the host and rendered for anyone who can see the
                // session, so "a token is set" is all a surface is told.
                blocks.push(serde_json::json!({
                    "kind": "note",
                    "text": "a token override is set for this session (value not shown here)",
                    "tone": "info",
                }));
            }
            blocks.push(serde_json::json!({
                "kind": "note",
                "text": "Tag definitions stay in the plugin settings or the repository's tags file; \
                         these panels choose where each one applies.",
            }));
            blocks.push(serde_json::json!({
                "kind": "action",
                "label": "Session details (token, harness)",
                "href": EDITOR_PAGE,
            }));

            // The row badge is where the activity is visible at a glance: one item
            // per tag, the activity first and with its own icon, then the rest with
            // the generic tag icon, then a count. An icon is presentation, so a
            // terminal UI gets the same items as text and tone.
            let mut badges: Vec<serde_json::Value> = Vec::new();
            if let Some(activity) = activity {
                badges.push(serde_json::json!({
                    "text": activity.label,
                    "icon": activity.icon,
                    "tone": activity.tone,
                    "tooltip": format!("activity: {}", activity.key),
                    "href": EDITOR_PAGE,
                }));
            }
            let others: Vec<_> = snap
                .tags
                .iter()
                // A tag the vocabulary did not recognise is not hidden: it is
                // reported above, and shown here as the plain tag it is, so a typo
                // is visible rather than silently dropped from the row.
                .filter(|tag| tag.key != crate::activity::KEY || activity.is_none())
                .collect();
            for tag in others.iter().take(3) {
                badges.push(serde_json::json!({
                    "text": tag.key,
                    "icon": "tag",
                    "tone": "neutral",
                    "tooltip": format!("{}: {}", tag.key, match &tag.value {
                        serde_json::Value::String(text) => text.clone(),
                        other => other.to_string(),
                    }),
                    "href": EDITOR_PAGE,
                }));
            }
            if others.len() > 3 {
                badges.push(serde_json::json!({
                    "text": format!("+{}", others.len() - 3),
                    "icon": "tags",
                    "tone": "neutral",
                    "tooltip": "the rest of this session's tags",
                    "href": EDITOR_PAGE,
                }));
            }
            let badge_payload = if badges.is_empty() {
                // Nothing to break down: the rendered value is still the honest
                // answer to "what is this session sending".
                serde_json::json!({
                    "text": text,
                    "icon": "tags",
                    "tooltip": "open the LLM Tags panel",
                    "href": EDITOR_PAGE,
                })
            } else {
                serde_json::json!({ "items": badges })
            };
            let _ = self.call_host(
                "ui.state.set",
                serde_json::json!({
                    "slot": "row-badge",
                    "id": "llm_tags_badge",
                    "session_id": id,
                    "payload": badge_payload,
                }),
            );
            let _ = self.call_host(
                "ui.state.set",
                serde_json::json!({
                    "slot": "pane",
                    "id": "llm_tags_pane",
                    "session_id": id,
                    "payload": {
                        "title": "LLM Tags",
                        "icon": "tags",
                        "default_location": "right",
                        "blocks": blocks,
                        "footer": {
                            "text": "a row's badge says where the tag comes from",
                            "value": format!("{} target(s)", snap.resolved.len()),
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
                    let origin = if session_keys.contains(&tag.key) { "session" } else { "config" };
                    Some(serde_json::json!({
                        "key": tag.key,
                        "value": tag.value,
                        "origin": origin,
                    }))
                }).collect::<Vec<_>>(),
                "unset": edits.unset,
                // Never the token itself: the page is a read-only surface for a
                // value the host keeps, so it learns that one is set and nothing
                // about what it says. The CLI (`tag --show`) is where it is read.
                "token_set": !edits.token.is_empty(),
                // The activity is edited in the pane (the page holds session
                // details only), so the page is not told the vocabulary.
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
pub const EDITS: [&str; 13] = [
    "tag.set",
    "tag.remove",
    "tag.restore",
    "tag.preset",
    "tag.level.unset",
    "tag.level.restore",
    "tag.level.replace",
    "tag.level.reset",
    "token.set",
    "token.clear",
    "tags.reset",
    // Machine-wide, not session-wide: the page is where a user reads the
    // disclosure and approves it, and the host merges a `session_id` into every
    // page action, which these two simply ignore.
    "extension.enable",
    "extension.disable",
];

/// One level's section in the pane. The session level passes its rows in; the
/// project and AoE levels have no rows of their own until this function turns
/// their file entries into them.
///
/// A level's own entry is hidden with `tag.level.unset`, which keeps the entry in
/// the file so `tag.level.restore` can bring it back. Nothing here writes the
/// plugin's settings: an entry is an overlay, and "clear" removes the overlay.
fn level_section(
    title: &str,
    level: Option<&str>,
    project_path: &str,
    edits: &crate::levels::EditSet,
    inherited: usize,
    default_entries: &[String],
    children: Vec<serde_json::Value>,
) -> serde_json::Value {
    let Some(level) = level else {
        return serde_json::json!({
            "kind": "section",
            "title": title,
            "value": format!("{} row(s)", children.len()),
            "children": children,
        });
    };
    let mut rows = children;
    let write = |method: &str, params: serde_json::Value| serde_json::json!({"method": method, "params": params});
    for entry in &edits.tags {
        let (key, value) = match entry.split_once(':') {
            Some((key, value)) => (key, value.trim_start_matches('=')),
            None => (entry.as_str(), "true"),
        };
        let mut row = serde_json::json!({
            "kind": "row",
            "label": key,
            "value": value,
            "mono": true,
            "badges": [{"text": if level == crate::levels::PROJECT { "project" } else { "AoE" }, "tone": "info"}],
            "tooltip": "hide this entry here; it stays in the file and can be restored",
            "method": "tag.level.unset",
            "params": {"level": level, "key": key},
        });
        if level == crate::levels::PROJECT {
            row["params"]["project_path"] = serde_json::json!(project_path);
        }
        rows.push(row);
    }
    for key in &edits.unset {
        let mut row = serde_json::json!({
            "kind": "row",
            "label": key,
            "value": "(hidden)",
            "mono": true,
            "badges": [{"text": "hidden", "tone": "warn"}],
            "tooltip": "let the inherited value show again here",
            "method": "tag.level.restore",
            "params": {"level": level, "key": key},
        });
        if level == crate::levels::PROJECT {
            row["params"]["project_path"] = serde_json::json!(project_path);
        }
        rows.push(row);
    }
    if edits.is_empty() {
        rows.push(serde_json::json!({
            "kind": "row",
            "label": "No overrides here: everything is inherited",
            "mono": true,
        }));
    }
    let entries: Vec<String> = default_entries.iter().take(20).cloned().collect();
    let mut replace = serde_json::json!({
        "kind": "row",
        "label": if level == crate::levels::PROJECT {
            "Use this session's tags as this project's default"
        } else {
            "Use this session's tags for every session"
        },
        "icon": "copy",
        "tooltip": "copy this session's current entries into this level",
        "method": "tag.level.replace",
        "params": {"level": level, "entries": entries},
    });
    if level == crate::levels::PROJECT {
        replace["params"]["project_path"] = serde_json::json!(project_path);
    }
    rows.push(replace);
    let mut clear = serde_json::json!({
        "kind": "row",
        "label": if level == crate::levels::PROJECT {
            "Clear this project's overrides"
        } else {
            "Clear the AoE overrides"
        },
        "icon": "rotate-ccw",
        "tone": "danger",
        "tooltip": "remove this level's entries and hides; the settings are untouched",
        "method": "tag.level.reset",
        "params": {"level": level},
    });
    if level == crate::levels::PROJECT {
        clear["params"]["project_path"] = serde_json::json!(project_path);
    }
    rows.push(clear);
    let _ = write;
    serde_json::json!({
        "kind": "section",
        "title": title,
        "value": if edits.is_empty() {
            format!("inherits {inherited} tag(s)")
        } else {
            format!("{} override(s), {} hidden", edits.tags.len(), edits.unset.len())
        },
        "collapsible": true,
        // The session is what the pane is about, so the level nearest to it is
        // open; the AoE default starts folded away.
        "collapsed": level == crate::levels::AOE,
        "children": rows,
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

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

    fn handle_notification(&mut self, method: &str, _params: &serde_json::Value) {
        let command = method.rsplit('.').next().unwrap_or_default();
        // Publish badges on any host event; failures are best-effort.
        let _ = command;
        self.publish_badges();
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

    fn publish_badges(&mut self) {
        let sessions = match self.call_host("sessions.list", serde_json::json!({})) {
            Ok(value) => value,
            Err(_) => return,
        };
        let sessions = sessions
            .get("sessions")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();

        let headers_val = self
            .call_host("config.get", serde_json::json!({"key": "headers"}))
            .ok()
            .and_then(|v| v.get("value").cloned());
        let feature_tag_val = self
            .call_host("config.get", serde_json::json!({"key": "feature_tag"}))
            .ok()
            .and_then(|v| v.get("value").cloned());
        let session_tagging_val = self
            .call_host("config.get", serde_json::json!({"key": "session_tagging"}))
            .ok()
            .and_then(|v| v.get("value").cloned());
        let rules_val = self
            .call_host("config.get", serde_json::json!({"key": "rules"}))
            .ok()
            .and_then(|v| v.get("value").cloned());

        let mut table = toml::Table::new();
        if let Some(value) = headers_val {
            if let Some(converted) = json_to_toml(&value) {
                table.insert("headers".to_string(), converted);
            }
        }
        if let Some(value) = feature_tag_val {
            if let Some(converted) = json_to_toml(&value) {
                table.insert("feature_tag".to_string(), converted);
            }
        }
        if let Some(value) = session_tagging_val {
            if let Some(converted) = json_to_toml(&value) {
                table.insert("session_tagging".to_string(), converted);
            }
        }
        if let Some(value) = rules_val {
            if let Some(converted) = json_to_toml(&value) {
                table.insert("rules".to_string(), converted);
            }
        }

        let flags = Flags::from_given(&Given::default(), &self.env);

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
            let facts_override = resolver::SessionFacts {
                id: id.clone(),
                title,
                tool,
                path,
                profile: "default".to_string(),
                group: String::new(),
                branch: String::new(),
            };

            // snapshot_with_table ignores the runner; pass None.
            let snap = resolver::snapshot_with_table(
                &flags,
                &self.env,
                None,
                Some(table.clone()),
                Some(facts_override),
            );
            let text = snap
                .resolved
                .first()
                .map(|item| item.value.clone())
                .unwrap_or_default();
            let body = if snap.resolved.is_empty() {
                "No headers resolved.".to_string()
            } else {
                snap.resolved
                    .iter()
                    .map(|item| format!("{}: {}", item.header, item.value))
                    .collect::<Vec<_>>()
                    .join("\n")
            };

            let _ = self.call_host(
                "ui.state.set",
                serde_json::json!({
                    "slot": "row-badge",
                    "id": "llm_tags_badge",
                    "session_id": id,
                    "payload": {
                        "text": text,
                    }
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
                        "body": body,
                    }
                }),
            );
        }
    }
}

fn json_to_toml(value: &serde_json::Value) -> Option<toml::Value> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::Bool(v) => Some(toml::Value::Boolean(*v)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(toml::Value::Integer(i))
            } else {
                n.as_f64().map(toml::Value::Float)
            }
        }
        serde_json::Value::String(s) => Some(toml::Value::String(s.clone())),
        serde_json::Value::Array(items) => {
            let mut out = Vec::new();
            for item in items {
                out.push(json_to_toml(item)?);
            }
            Some(toml::Value::Array(out))
        }
        serde_json::Value::Object(map) => {
            let mut table = toml::Table::new();
            for (key, val) in map {
                if let Some(converted) = json_to_toml(val) {
                    table.insert(key.clone(), converted);
                }
            }
            Some(toml::Value::Table(table))
        }
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
        "message": format!("{} header(s) for session {session}", headers.len()),
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
        assert_eq!(
            response["result"]["headers"][resolver::DEFAULT_HEADER],
            "aoe,feature:,session:S-abcd"
        );
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

//! A throwaway config tree, a fake `aoe`, and a harness config — no ambient state.
//!
//! Every variable the plugin reads is set explicitly and the process environment
//! is cleared first, so a test can never pass because the developer happened to
//! be inside a tagged aoe session.

#![cfg(unix)]
// Each integration test compiles this module separately; unused helpers are expected.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub const PLUGIN_ID: &str = "dev.oozut.llm-tags";
pub const CLI: &str = env!("CARGO_BIN_EXE_aoe-llm-tags");

pub const SESSION_JSON: &str = r#"{"id":"cf11af87da7e4bf0","title":"Lithuanians","path":"/var/lib/aoe/android-keyboard-worktrees/lithuanians","group":"","tool":"pi-proxy","profile":"main"}"#;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub struct Sandbox {
    pub root: PathBuf,
    pub env: BTreeMap<String, String>,
}

impl Sandbox {
    /// A temp tree with `HOME`, a config dir, and a fake `aoe` on `AOE_BIN`.
    pub fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("llm-tags-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("home")).expect("home");
        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), root.join("home").display().to_string());
        env.insert(
            "XDG_CONFIG_HOME".to_string(),
            root.join("config").display().to_string(),
        );
        env.insert(
            "PATH".to_string(),
            "/usr/local/bin:/usr/bin:/bin".to_string(),
        );
        let mut sandbox = Self { root, env };
        let aoe = sandbox.fake_aoe(SESSION_JSON);
        sandbox
            .env
            .insert("AOE_BIN".to_string(), aoe.display().to_string());
        sandbox
            .env
            .insert("AOE_SESSION_ID".to_string(), "cf11af87da7e4bf0".to_string());
        sandbox
            .env
            .insert("AOE_PROFILE".to_string(), "main".to_string());
        sandbox
            .env
            .insert("AOE_TOOL".to_string(), "pi-proxy".to_string());
        sandbox.write_config(&settings_config("", ""));
        sandbox
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// `$XDG_CONFIG_HOME/agent-of-empires/config.toml`, or the profile override.
    pub fn config_path(&self, profile: &str) -> PathBuf {
        let app = self.root.join("config").join("agent-of-empires");
        if profile.is_empty() {
            app.join("config.toml")
        } else {
            app.join("profiles").join(profile).join("config.toml")
        }
    }

    pub fn write_config(&self, body: &str) {
        self.write_profile_config("", body);
    }

    pub fn write_profile_config(&self, profile: &str, body: &str) {
        let path = self.config_path(profile);
        fs::create_dir_all(path.parent().unwrap()).expect("config dir");
        fs::write(path, body).expect("config");
    }

    /// `$HOME/.pi/agent/models.json` — where the pi adapter writes.
    pub fn pi_models_path(&self) -> PathBuf {
        self.path("home/.pi/agent/models.json")
    }

    pub fn write_pi_models(&self, body: &str) {
        let path = self.pi_models_path();
        self.write_file(&path, body);
    }

    pub fn read_pi_models(&self) -> serde_json::Value {
        let body = fs::read_to_string(self.pi_models_path()).expect("models.json");
        serde_json::from_str(&body).expect("valid JSON")
    }

    pub fn write_file(&self, path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).expect("parent");
        fs::write(path, body).expect("file");
    }

    pub fn write_executable(&self, path: &Path, body: &str) {
        self.write_file(path, body);
        let mut mode = fs::metadata(path).expect("metadata").permissions();
        mode.set_mode(0o755);
        fs::set_permissions(path, mode).expect("chmod");
    }

    /// A real executable answering `session show` and `list`, like the host does.
    pub fn fake_aoe(&self, session_json: &str) -> PathBuf {
        let path = self
            .root
            .join("bin")
            .join(format!("aoe-{}", COUNTER.fetch_add(1, Ordering::SeqCst)));
        self.fake_aoe_at(&path, session_json);
        path
    }

    pub fn fake_aoe_at(&self, path: &Path, session_json: &str) {
        self.write_executable(
            path,
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n  session) echo '{session_json}' ;;\n  list) echo '[]' ;;\n  *) echo '{{}}' ;;\nesac\n"
            ),
        );
    }

    /// A pi config shaped the way this host's launcher requires it.
    pub fn pi_models(&self, headers: &[(&str, &str)]) -> PathBuf {
        let path = self.root.join("home/.pi/agent/models.json");
        let headers = headers
            .iter()
            .map(|(key, value)| format!("{key:?}: {value:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        self.write_file(
            &path,
            &format!(
                r#"{{"providers": {{"litellm": {{"baseUrl": "http://127.0.0.1:8787/v1", "api": "openai-completions", "apiKey": "$LITELLM_API_KEY", "headers": {{{headers}}}, "models": [{{"id": "deepseek-v4-flash"}}]}}}}}}"#
            ),
        );
        path
    }

    /// A plugin tree shaped like an install, so the hook line names this CLI.
    pub fn hook_command(&self) -> String {
        let cli = self.root.join("plugin/bin/aoe-llm-tags");
        self.write_executable(&cli, "#!/bin/sh\nexit 0\n");
        llm_tags::setup::hook_command(&cli, self.env.get("AOE_BIN").expect("AOE_BIN"))
    }

    /// A fully wired install: both hook keys, and the header mapped in pi.
    pub fn wire(&self) {
        let command = self.hook_command();
        self.write_config(&settings_config(
            "",
            &format!("before_session = ['{command}']\nbefore_start = ['{command}']\n"),
        ));
        self.pi_models(&[("X-AoE-Tags", "$AOE_LLM_TAGS")]);
    }

    pub fn run(&self, args: &[&str]) -> Run {
        self.run_with(args, &[])
    }

    pub fn run_with(&self, args: &[&str], extra: &[(&str, &str)]) -> Run {
        let mut command = std::process::Command::new(CLI);
        command.env_clear().args(args);
        for (key, value) in &self.env {
            command.env(key, value);
        }
        for (key, value) in extra {
            command.env(key, value);
        }
        let output = command.output().expect("spawn the plugin CLI");
        Run {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    pub fn doctor(&self, args: &[&str]) -> Run {
        let mut argv = vec!["doctor"];
        argv.extend_from_slice(args);
        self.run(&argv)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug)]
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    pub fn lines(&self) -> Vec<&str> {
        self.stdout.lines().collect()
    }

    pub fn assert_ok(&self) -> &Self {
        assert_eq!(self.code, 0, "exit {}\nstderr: {}", self.code, self.stderr);
        self
    }

    pub fn assert_code(&self, code: i32) -> &Self {
        assert_eq!(self.code, code, "stderr: {}", self.stderr);
        self
    }

    pub fn assert_contains(&self, needle: &str) -> &Self {
        assert!(
            self.stdout.contains(needle) || self.stderr.contains(needle),
            "missing {needle:?} in\nstdout: {}\nstderr: {}",
            self.stdout,
            self.stderr
        );
        self
    }
}

pub fn settings_config(settings: &str, hooks: &str) -> String {
    let mut body = format!(
        "[plugins.\"{PLUGIN_ID}\".settings]\n{}\n",
        settings.trim_end()
    );
    if !hooks.trim().is_empty() {
        body.push_str(&format!("[host_hooks]\n{}\n", hooks.trim_end()));
    }
    body
}

pub const TWO_HEADERS: &str = "headers = [\n  { header = \"X-AoE-Tags\", template = \"a\", env_var = \"AOE_LLM_TAGS\" },\n  { header = \"X-AoE-Session\", template = \"{session_id}\", env_var = \"AOE_HDR_X_AOE_SESSION\" },\n]\n";

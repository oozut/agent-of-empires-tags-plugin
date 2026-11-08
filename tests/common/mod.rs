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

pub const SESSION_ID: &str = "cf11af87da7e4bf0";

/// `aoe session show`'s row for the fixture session. `__PROJECT__` is the
/// sandbox's own project directory: git facts are only read from a checkout, so a
/// fixture naming a path this machine happens to have could not test them.
pub const SESSION_JSON: &str = r#"{"id":"cf11af87da7e4bf0","title":"Lithuanians","path":"__PROJECT__","group":"","tool":"pi-proxy","profile":"main"}"#;

/// The default target sends the whole tag set as one JSON object. Keys are
/// sorted (serde_json's default map), so this is the exact header value for the
/// fixture session — a change in the payload shape fails here first.
pub const DEFAULT_PAYLOAD: &str = r#"{"profile":"main","project":"__PROJECT_NAME__","project_path":"__PROJECT__","session":"S-cf11","session_id":"cf11af87da7e4bf0","tool":"pi-proxy"}"#;

/// The same payload when `aoe` cannot be reached: no project row, so only the
/// fields the hook environment carries are present.
pub const DEFAULT_PAYLOAD_NO_AOE: &str =
    r#"{"profile":"main","session":"S-cf11","session_id":"cf11af87da7e4bf0","tool":"pi-proxy"}"#;

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
        // The fixture session sits in `<root>/project`, a checkout: an empty
        // `.git` is a checkout too, which is exactly what the git gate asks for.
        fs::create_dir_all(sandbox.project().join(".git")).expect("project checkout");
        let aoe = sandbox.fake_aoe(&sandbox.session_json());
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

    /// The fixture session's project: a real directory inside the sandbox, made a
    /// checkout by [`Sandbox::new`].
    pub fn project(&self) -> PathBuf {
        self.root.join("project")
    }

    /// The `aoe session show` row naming [`Sandbox::project`].
    pub fn session_json(&self) -> String {
        SESSION_JSON.replace("__PROJECT__", &self.project().display().to_string())
    }

    /// The `AOE_LLM_TAGS` value the default target produces for this sandbox.
    pub fn default_payload(&self) -> String {
        DEFAULT_PAYLOAD
            .replace("__PROJECT__", &self.project().display().to_string())
            .replace("__PROJECT_NAME__", "project")
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

    /// Replace the fake session this sandbox reports: the session row beats the
    /// environment, so a test that needs another harness says so here.
    pub fn session(&mut self, id: &str, tool: &str) {
        let json = self
            .session_json()
            .replace(SESSION_ID, id)
            .replace("\"tool\":\"pi-proxy\"", &format!("\"tool\":\"{tool}\""));
        let path = self.fake_aoe(&json);
        self.env
            .insert("AOE_BIN".to_string(), path.display().to_string());
        self.env
            .insert("AOE_SESSION_ID".to_string(), id.to_string());
        self.env.insert("AOE_TOOL".to_string(), tool.to_string());
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
                // `list` answers with the same row in an array, the way the host does: the
                // substrate check needs a live-id set, and an empty list would mean "aoe did
                // not answer" rather than "no sessions".
                "#!/bin/sh\ncase \"$1\" in\n  session) echo '{session_json}' ;;\n  list) echo '[{session_json}]' ;;\n  *) echo '{{}}' ;;\nesac\n"
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
        self.hook_command_for("before_session")
    }

    /// The line a real install carries under `key`: the two keys declare
    /// different substrates (`setup::Substrate::for_key`).
    pub fn hook_command_for(&self, key: &str) -> String {
        let cli = self.root.join("plugin/bin/aoe-llm-tags");
        self.write_executable(&cli, "#!/bin/sh\nexit 0\n");
        llm_tags::setup::hook_command(
            &cli,
            self.env.get("AOE_BIN").expect("AOE_BIN"),
            llm_tags::setup::Substrate::for_key(key),
        )
    }

    /// A fully wired install: both hook keys, and the header mapped in pi.
    pub fn wire(&self) {
        self.write_config(&settings_config(
            "",
            &format!(
                "before_session = ['{}']\nbefore_start = ['{}']\n",
                self.hook_command_for("before_session"),
                self.hook_command_for("before_start")
            ),
        ));
        self.pi_models(&[("X-AoE-Tags", "$AOE_LLM_TAGS")]);
    }

    pub fn run(&self, args: &[&str]) -> Run {
        self.run_with(args, &[])
    }

    pub fn run_with(&self, args: &[&str], extra: &[(&str, &str)]) -> Run {
        self.run_bin(std::path::Path::new(CLI), args, extra)
    }

    /// Run from `dir`, so a command that falls back to a relative path writes
    /// where the test can see it.
    pub fn run_within(&self, dir: &Path, args: &[&str], extra: &[(&str, &str)]) -> Run {
        self.run_bin_in(dir, std::path::Path::new(CLI), args, extra)
    }

    /// Run this CLI through a copy placed in `<root>/plugin/bin`, so the plugin
    /// root — and therefore the adapter table it reads — is the one the test
    /// wrote. `plugin_root()` is the parent of the executable's directory.
    pub fn run_installed(&self, args: &[&str]) -> Run {
        let bin = self.root.join("plugin/bin");
        fs::create_dir_all(&bin).expect("bin");
        let cli = bin.join("aoe-llm-tags");
        fs::copy(CLI, &cli).expect("copy the CLI");
        self.run_bin(&cli, args, &[])
    }

    fn run_bin(&self, cli: &std::path::Path, args: &[&str], extra: &[(&str, &str)]) -> Run {
        self.run_bin_in(Path::new("."), cli, args, extra)
    }

    fn run_bin_in(
        &self,
        dir: &Path,
        cli: &std::path::Path,
        args: &[&str],
        extra: &[(&str, &str)],
    ) -> Run {
        let mut command = std::process::Command::new(cli);
        command.env_clear().current_dir(dir).args(args);
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

/// Approve any harness by name, with the digest its disclosure printed: the two
/// steps a user takes, in the order they take them.
pub fn approve_any(sandbox: &Sandbox, harness: &str) -> Run {
    let printed = sandbox.run(&["extension", "enable", harness, "--print"]);
    printed.assert_ok();
    let digest = printed
        .stdout
        .lines()
        .find_map(|line| line.split("--accept ").nth(1))
        .map(str::trim)
        .expect("the printed approve command")
        .to_string();
    let run = sandbox.run(&["extension", "enable", harness, "--accept", &digest]);
    run.assert_ok();
    run
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

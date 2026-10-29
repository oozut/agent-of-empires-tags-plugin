//! Where the plugin's settings live, and how a profile overrides them.
//!
//! aoe stores plugin settings as an opaque table under
//! `[plugins."dev.oozut.llm-tags".settings]` in `<app_dir>/config.toml`
//! (`settings: toml::Table` in the host's session config) and keeps them while
//! the plugin is disabled. A profile adds `<app_dir>/profiles/<name>/config.toml`
//! whose *top level* is the sparse override tree, and the host merges it onto the
//! global config as a JSON merge: object keys recurse, arrays replace. This
//! module reproduces exactly that for the one section the plugin owns, so the
//! hook and any UI cannot disagree about a value.
//!
//! Failure behaviour is the hook's, not a library's: a file that cannot be read
//! or parsed is a warning and an empty table, never an error.

use std::path::{Path, PathBuf};

use crate::env::Env;

pub const PLUGIN_ID: &str = "dev.oozut.llm-tags";

/// Release-build app dir name (`APP_DIR_NAME_XDG` in the host's session module).
/// Debug builds use `agent-of-empires-dev`; a hook cannot tell which build
/// launched it, so it reads the release dir, which is what deployments run.
pub const APP_DIR_NAME: &str = "agent-of-empires";

const GLOBAL_CONFIG: &str = "config.toml";

/// The aoe app dir: `$XDG_CONFIG_HOME` when absolute, else `~/.config`.
///
/// Mirrors `xdg_config_base()` + `get_app_dir_path()` for a Linux release build.
/// No aoe env var overrides this, so neither do we.
pub fn app_dir(env: &Env) -> PathBuf {
    let base = match env.get("XDG_CONFIG_HOME") {
        Some(xdg) if Path::new(xdg).is_absolute() => PathBuf::from(xdg),
        _ => {
            let home = env
                .home()
                .map(str::to_string)
                .or_else(|| std::env::var("HOME").ok())
                .unwrap_or_default();
            Path::new(&home).join(".config")
        }
    };
    base.join(APP_DIR_NAME)
}

pub fn global_config_path(root: &Path) -> PathBuf {
    root.join(GLOBAL_CONFIG)
}

pub fn profile_config_path(root: &Path, profile: &str) -> Option<PathBuf> {
    if profile.is_empty() {
        return None;
    }
    Some(root.join("profiles").join(profile).join(GLOBAL_CONFIG))
}

/// The effective config table (global + profile override) and where it came from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawConfig {
    pub table: toml::Table,
    pub warnings: Vec<String>,
    /// Files that parsed.
    pub sources: Vec<PathBuf>,
    /// Files that existed at all; a broken file is present but not a source, and
    /// `doctor` needs both, because "no file" and "file without a hook" are
    /// different failures.
    pub present: Vec<PathBuf>,
}

/// The merged `[plugins.<id>.settings]` table, plus how we got it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawSettings {
    pub table: toml::Table,
    pub warnings: Vec<String>,
    pub sources: Vec<PathBuf>,
}

pub fn load_config(profile: &str, root: Option<&Path>, env: &Env) -> RawConfig {
    let root = root.map(Path::to_path_buf).unwrap_or_else(|| app_dir(env));
    let mut out = RawConfig::default();

    let path = global_config_path(&root);
    let loaded = read_table(&path, &mut out.warnings);
    if path.exists() {
        out.present.push(path.clone());
    }
    if let Some(table) = loaded {
        out.sources.push(path);
        out.table = table;
    }

    if let Some(profile_path) = profile_config_path(&root, profile) {
        let overrides = read_table(&profile_path, &mut out.warnings);
        if profile_path.exists() {
            out.present.push(profile_path.clone());
        }
        if let Some(overrides) = overrides {
            out.sources.push(profile_path);
            out.table = deep_merge(&out.table, &overrides);
        }
    }

    out
}

pub fn load(profile: &str, root: Option<&Path>, env: &Env) -> RawSettings {
    let config = load_config(profile, root, env);
    RawSettings {
        table: plugin_table(&config.table),
        warnings: config.warnings,
        sources: config.sources,
    }
}

/// Read one TOML file as a table; `None` when it is absent or unusable.
fn read_table(path: &Path, warnings: &mut Vec<String>) -> Option<toml::Table> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            warnings.push(format!(
                "{}: cannot be read ({err}); ignoring it",
                path.display()
            ));
            return None;
        }
    };
    match raw.parse::<toml::Table>() {
        Ok(table) => Some(table),
        Err(err) => {
            warnings.push(format!(
                "{}: malformed TOML ({err}); ignoring it",
                path.display()
            ));
            None
        }
    }
}

/// JSON-merge semantics: keys recurse, scalars **and arrays** replace.
pub fn deep_merge(base: &toml::Table, over: &toml::Table) -> toml::Table {
    let mut merged = base.clone();
    for (key, value) in over {
        let current = merged.get(key);
        match (value, current) {
            (toml::Value::Table(next), Some(toml::Value::Table(prev))) => {
                merged.insert(key.clone(), toml::Value::Table(deep_merge(prev, next)));
            }
            _ => {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
    merged
}

/// `[plugins.<PLUGIN_ID>.settings]` as a plain table (empty when absent).
pub fn plugin_table(config: &toml::Table) -> toml::Table {
    config
        .get("plugins")
        .and_then(toml::Value::as_table)
        .and_then(|plugins| plugins.get(PLUGIN_ID))
        .and_then(toml::Value::as_table)
        .and_then(|entry| entry.get("settings"))
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default()
}

/// The effective `[host_hooks]` entries for `before_session`/`before_start`.
pub fn host_hook_entries<'a>(config: &'a toml::Table, key: &str) -> Vec<&'a str> {
    let Some(hooks) = config.get("host_hooks").and_then(toml::Value::as_table) else {
        return Vec::new();
    };
    match hooks.get(key) {
        Some(toml::Value::String(entry)) => vec![entry.as_str()],
        Some(toml::Value::Array(entries)) => entries
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(pairs: &[(&str, &str)]) -> Env {
        Env::new(pairs.iter().copied())
    }

    #[test]
    fn xdg_config_home_wins_when_absolute() {
        let env = env_with(&[("HOME", "/home/x"), ("XDG_CONFIG_HOME", "/tmp/cfg")]);
        assert_eq!(app_dir(&env), PathBuf::from("/tmp/cfg/agent-of-empires"));
    }

    #[test]
    fn relative_xdg_is_ignored() {
        let env = env_with(&[("HOME", "/home/x"), ("XDG_CONFIG_HOME", "relative/cfg")]);
        assert_eq!(
            app_dir(&env),
            PathBuf::from("/home/x/.config/agent-of-empires")
        );
    }

    #[test]
    fn missing_home_does_not_raise() {
        assert!(!app_dir(&env_with(&[])).as_os_str().is_empty());
    }

    #[test]
    fn objects_merge_recursively() {
        let base: toml::Table = "a.b = 1\na.c = 2".parse().unwrap();
        let over: toml::Table = "a.c = 3".parse().unwrap();
        let merged = deep_merge(&base, &over);
        let a = merged["a"].as_table().unwrap();
        assert_eq!(a["b"].as_integer(), Some(1));
        assert_eq!(a["c"].as_integer(), Some(3));
    }

    #[test]
    fn arrays_replace() {
        let base: toml::Table = "headers = [{ header = \"X-G\" }]".parse().unwrap();
        let over: toml::Table = "headers = [{ header = \"X-P\" }]".parse().unwrap();
        let merged = deep_merge(&base, &over);
        assert_eq!(merged["headers"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn unknown_plugin_is_ignored() {
        assert!(plugin_table(&toml::Table::new()).is_empty());
        let table: toml::Table = "[plugins.\"other.plugin\".settings]\na = 1"
            .parse()
            .unwrap();
        assert!(plugin_table(&table).is_empty());
    }

    #[test]
    fn host_hook_entries_accept_a_string_or_an_array() {
        let table: toml::Table = "[host_hooks]\nbefore_session = [\"a\"]\nbefore_start = \"b\""
            .parse()
            .unwrap();
        assert_eq!(host_hook_entries(&table, "before_session"), vec!["a"]);
        assert_eq!(host_hook_entries(&table, "before_start"), vec!["b"]);
        assert!(host_hook_entries(&table, "other").is_empty());
        assert!(host_hook_entries(&toml::Table::new(), "before_session").is_empty());
    }

    #[test]
    fn missing_files_are_not_warnings() {
        let dir = tempdir();
        let env = Env::new([("HOME", dir.path.to_str().unwrap())]);
        let raw = load("", None, &env);
        assert!(raw.table.is_empty());
        assert!(raw.warnings.is_empty());
        assert!(load_config("", None, &env).present.is_empty());
    }

    #[test]
    fn malformed_global_keeps_the_profile() {
        let dir = tempdir();
        let env = Env::new([("HOME", dir.path.to_str().unwrap())]);
        let global = global_config_path(&app_dir(&env));
        std::fs::create_dir_all(global.parent().unwrap()).unwrap();
        std::fs::write(global, "not = = toml\n").unwrap();
        let profile = profile_config_path(&app_dir(&env), "main").unwrap();
        std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
        std::fs::write(
            &profile,
            format!("[plugins.\"{PLUGIN_ID}\".settings]\nfeature_tag = \"profile\"\n"),
        )
        .unwrap();

        let raw = load("main", None, &env);
        assert_eq!(raw.table["feature_tag"].as_str(), Some("profile"));
        assert!(raw.warnings.iter().any(|w| w.contains("malformed TOML")));
    }

    struct TempDir {
        path: PathBuf,
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn tempdir() -> TempDir {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("llm-tags-settings-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }
}

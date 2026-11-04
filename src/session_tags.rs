//! A session's own edits: the one place a user can change a tag or the token
//! without touching the global config.
//!
//! The invariant this module exists to keep is the plugin's oldest one: **no
//! global config edit, ever**. A user who wants `team:core` on one session and
//! `team:platform` on another does not get a config file with both and a rule to
//! choose; they get a per-session override, stored inside the session's own
//! directory next to the harness copies:
//!
//! ```text
//! <app dir>/sessions/<session id>/overrides.toml
//! ```
//!
//! Three keys, one grammar — the same tag grammar the settings use:
//!
//! ```toml
//! tags = ["team:platform", "epic:checkout"]  # add or replace, key by key
//! unset = ["cost"]                           # drop an inherited tag
//! token = '{"team":"platform"}'              # a literal value for this session
//! ```
//!
//! `overlay::prune` already deletes a session's directory once the host no
//! longer lists it, so these edits cannot outlive their session either.

use std::path::{Path, PathBuf};

use crate::overlay::sanitize;
use crate::resolver::{Settings, Tag};
use crate::settings::APP_DIR_NAME;

pub const FILE: &str = "overrides.toml";

/// The bare ones: a session's tag edits and its literal token.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    /// Entries this session adds or replaces, in tag grammar.
    pub tags: Vec<String>,
    /// Keys this session drops even though the settings carry them.
    pub unset: Vec<String>,
    /// A literal value standing in for what the targets would render.
    pub token: String,
}

impl Overrides {
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty() && self.unset.is_empty() && self.token.is_empty()
    }

    /// The payload fields this session sets, in the order they were added.
    pub fn entries(&self) -> Vec<(String, serde_json::Value)> {
        self.tags
            .iter()
            .filter_map(|raw| Tag::parse(raw).ok())
            .map(|tag| (tag.key, tag.value))
            .collect()
    }
}

/// The sample tag sets a user can apply in one step.
///
/// A preset is ordinary tag grammar, spelled once in the resolver and applied
/// here, so the editor's tick box, the pane's action and `tag --git` all write the
/// same entries — and a user can still edit or drop any of them afterwards.
pub const PRESETS: [(&str, &[&str]); 1] = [("git", &crate::resolver::GIT_PRESET)];

/// The entries of a preset, or `None` for an unknown id.
pub fn preset(id: &str) -> Option<&'static [&'static str]> {
    PRESETS
        .iter()
        .find(|(name, _)| *name == id)
        .map(|(_, tags)| *tags)
}

/// Add every entry of a preset, key by key, exactly as `set_tag` would.
pub fn apply_preset(app_dir: &Path, session_id: &str, id: &str) -> Result<Overrides, String> {
    let entries = preset(id).ok_or_else(|| format!("unknown preset {id:?}"))?;
    for entry in entries {
        set_tag(app_dir, session_id, entry)?;
    }
    Ok(load(app_dir, session_id).0)
}

/// Remove every key a preset sets. Unticking the box is one action, not four.
pub fn remove_preset(app_dir: &Path, session_id: &str, id: &str) -> Result<Overrides, String> {
    let entries = preset(id).ok_or_else(|| format!("unknown preset {id:?}"))?;
    for entry in entries {
        if let Ok(tag) = Tag::parse(entry) {
            unset_tag(app_dir, session_id, &tag.key)?;
        }
    }
    Ok(load(app_dir, session_id).0)
}

/// `<app dir>/sessions/<session id>/overrides.toml`.
pub fn path(app_dir: &Path, session_id: &str) -> PathBuf {
    app_dir
        .join("sessions")
        .join(sanitize(session_id))
        .join(FILE)
}

/// Read a session's edits. A file that cannot be read or parsed is a warning and
/// no edits, never an error: a hook that fails is worse than a tag that is not
/// applied, and the user can always see what the file says.
pub fn load(app_dir: &Path, session_id: &str) -> (Overrides, Vec<String>) {
    let mut warnings = Vec::new();
    let path = path(app_dir, session_id);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return (Overrides::default(), warnings)
        }
        Err(err) => {
            warnings.push(format!("overrides: cannot read {}: {err}", path.display()));
            return (Overrides::default(), warnings);
        }
    };
    let table: toml::Table = match raw.parse() {
        Ok(table) => table,
        Err(err) => {
            warnings.push(format!(
                "overrides: {} is not valid TOML ({err}); ignoring it",
                path.display()
            ));
            return (Overrides::default(), warnings);
        }
    };
    let list = |key: &str| -> Vec<String> {
        table
            .get(key)
            .and_then(toml::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut overrides = Overrides {
        tags: list("tags"),
        unset: list("unset"),
        token: table
            .get("token")
            .and_then(toml::Value::as_str)
            .unwrap_or_default()
            .to_string(),
    };
    // A tag that does not parse would be dropped later with a message about
    // "settings", which is the wrong file; report it here instead.
    for raw in std::mem::take(&mut overrides.tags) {
        match Tag::parse(&raw) {
            Ok(_) => overrides.tags.push(raw),
            Err(message) => warnings.push(format!(
                "overrides: {message}; dropping it (file {})",
                path.display()
            )),
        }
    }
    (overrides, warnings)
}

/// Write a session's edits atomically, or delete the file when there are none.
///
/// The rename is what makes a concurrent hook safe: a reader sees the old file
/// or the new one, never a half-written table.
pub fn save(app_dir: &Path, session_id: &str, overrides: &Overrides) -> Result<(), String> {
    let path = path(app_dir, session_id);
    if overrides.is_empty() {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(format!("cannot remove {}: {err}", path.display())),
        };
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)
        .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let mut text = String::from(
        "# Per-session LLM Tags edits, written by the plugin's own UI and `tag` command.\n\
         # This file belongs to one session: the global settings are never edited.\n",
    );
    let quote = |items: &[String]| -> String {
        items
            .iter()
            .map(|item| format!("{:?}", item))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !overrides.tags.is_empty() {
        text.push_str(&format!("tags = [{}]\n", quote(&overrides.tags)));
    }
    if !overrides.unset.is_empty() {
        text.push_str(&format!("unset = [{}]\n", quote(&overrides.unset)));
    }
    if !overrides.token.is_empty() {
        text.push_str(&format!("token = {:?}\n", overrides.token));
    }
    let temp = path.with_extension("toml.tmp");
    std::fs::write(&temp, text).map_err(|err| format!("cannot write {}: {err}", temp.display()))?;
    std::fs::rename(&temp, &path)
        .map_err(|err| format!("cannot replace {}: {err}", path.display()))?;
    Ok(())
}

/// Add or replace one tag for a session. The entry is the same grammar a
/// setting uses, so `team:platform` and `epic:=42` mean what they mean anywhere.
pub fn set_tag(app_dir: &Path, session_id: &str, entry: &str) -> Result<Overrides, String> {
    let tag = Tag::parse(entry)?;
    let (mut overrides, _) = load(app_dir, session_id);
    overrides.tags.retain(|raw| match Tag::parse(raw) {
        Ok(existing) => existing.key != tag.key,
        Err(_) => true,
    });
    overrides.unset.retain(|key| key != &tag.key);
    overrides.tags.push(entry.trim().to_string());
    save(app_dir, session_id, &overrides)?;
    Ok(overrides)
}

/// Drop a tag for this session — whether it came from the settings or from this
/// session's own file. Dropping a session entry removes it; dropping an inherited
/// one records the key in `unset`, which is still a per-session edit.
pub fn unset_tag(app_dir: &Path, session_id: &str, key: &str) -> Result<Overrides, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("a tag key is required".to_string());
    }
    let (mut overrides, _) = load(app_dir, session_id);
    overrides.tags.retain(|raw| match Tag::parse(raw) {
        Ok(existing) => existing.key != key,
        Err(_) => true,
    });
    if !overrides.unset.iter().any(|listed| listed == key) {
        overrides.unset.push(key.to_string());
    }
    save(app_dir, session_id, &overrides)?;
    Ok(overrides)
}

/// Replace what every template-less target renders with a literal string.
/// An empty value clears the override, which is the same as not having one.
pub fn set_token(app_dir: &Path, session_id: &str, token: &str) -> Result<Overrides, String> {
    let (mut overrides, _) = load(app_dir, session_id);
    overrides.token = token.to_string();
    save(app_dir, session_id, &overrides)?;
    Ok(overrides)
}

/// Forget every edit this session has.
pub fn reset(app_dir: &Path, session_id: &str) -> Result<(), String> {
    save(app_dir, session_id, &Overrides::default())
}

/// Fold a session's edits into the settings a render is about to use.
///
/// The order is what makes an edit predictable: drop the keys the session
/// `unset`, then replace same-key entries, then append the session's own. The
/// token needs no merge — it stands in for the whole rendered payload.
pub fn apply(settings: &mut Settings, app_dir: &Path, session_id: &str) {
    let (overrides, warnings) = load(app_dir, session_id);
    settings.warnings.extend(warnings);
    if overrides.is_empty() {
        return;
    }
    if !overrides.unset.is_empty() {
        settings.tags.retain(|raw| match Tag::parse(raw) {
            Ok(tag) => !overrides.unset.iter().any(|key| key == &tag.key),
            Err(_) => true,
        });
    }
    let replaced: Vec<String> = overrides
        .tags
        .iter()
        .filter_map(|raw| Tag::parse(raw).ok())
        .map(|tag| tag.key)
        .collect();
    if !replaced.is_empty() {
        settings.tags.retain(|raw| match Tag::parse(raw) {
            Ok(tag) => !replaced.contains(&tag.key),
            Err(_) => true,
        });
    }
    settings.tags.extend(overrides.tags.iter().cloned());
    if !overrides.token.is_empty() {
        settings.token = overrides.token.clone();
    }
}

/// The app dir's name, re-exported so callers do not have to reach into
/// `settings` for the one constant they need.
pub fn app_dir_name() -> &'static str {
    APP_DIR_NAME
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Env;
    use crate::resolver::{default_targets, parse_tags, payload, payload_json, SessionFacts};

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "session-tags-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    fn facts() -> SessionFacts {
        SessionFacts {
            id: "cf11af87da7e4bf0".to_string(),
            title: "Lithuanians".to_string(),
            path: "/w/lithuanians".to_string(),
            group: String::new(),
            tool: "pi".to_string(),
            profile: "main".to_string(),
            branch: String::new(),
        }
    }

    fn settings(tags: &[&str]) -> Settings {
        let _ = Env::current();
        Settings {
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            targets: default_targets(),
            rules: Vec::new(),
            session_tagging: "short".to_string(),
            feature_tag: None,
            token: String::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn an_edit_lands_in_the_sessions_own_file_and_nowhere_else() {
        let app = temp();
        let edits = set_tag(&app, "cf11af87da7e4bf0", "team:platform").expect("set");
        assert_eq!(edits.tags, vec!["team:platform"]);
        let path = path(&app, "cf11af87da7e4bf0");
        assert!(
            path.ends_with("sessions/cf11af87da7e4bf0/overrides.toml"),
            "{path:?}"
        );
        let text = std::fs::read_to_string(&path).expect("file");
        assert!(text.contains("team:platform"), "{text}");
        assert!(text.contains("global settings are never edited"), "{text}");
        // A different session reads nothing.
        let (other, _) = load(&app, "0123456789abcdef");
        assert!(other.is_empty());
        let _ = std::fs::remove_dir_all(&app);
    }

    #[test]
    fn one_key_at_a_time_replace_unset_and_re_add() {
        let app = temp();
        let session = "cf11af87da7e4bf0";
        set_tag(&app, session, "team:core").expect("set");
        set_tag(&app, session, "team:platform").expect("replace");
        let (edits, _) = load(&app, session);
        assert_eq!(edits.tags, vec!["team:platform"]);

        unset_tag(&app, session, "team").expect("unset");
        let (edits, _) = load(&app, session);
        assert!(edits.tags.is_empty(), "{edits:?}");
        assert_eq!(edits.unset, vec!["team"]);

        // Re-adding clears the tombstone, or the edit would silently do nothing.
        set_tag(&app, session, "team:platform").expect("re-add");
        let (edits, _) = load(&app, session);
        assert_eq!(edits.tags, vec!["team:platform"]);
        assert!(edits.unset.is_empty(), "{edits:?}");
        let _ = std::fs::remove_dir_all(&app);
    }

    #[test]
    fn an_entry_that_does_not_parse_is_refused_before_it_is_written() {
        let app = temp();
        let err = set_tag(&app, "cf11af87da7e4bf0", "=core").expect_err("refused");
        assert!(err.contains('='), "{err}");
        let err = set_tag(&app, "cf11af87da7e4bf0", ":core").expect_err("refused");
        assert!(err.contains("no key"), "{err}");
        assert!(!path(&app, "cf11af87da7e4bf0").exists());
        let _ = std::fs::remove_dir_all(&app);
    }

    #[test]
    fn the_token_stands_in_for_what_the_targets_render() {
        let app = temp();
        let session = "cf11af87da7e4bf0";
        set_token(&app, session, "{\"hand\":\"written\"}").expect("token");
        let (edits, _) = load(&app, session);
        assert_eq!(edits.token, "{\"hand\":\"written\"}");
        set_token(&app, session, "").expect("clear");
        assert!(
            !path(&app, session).exists(),
            "an empty override is no file"
        );
        let _ = std::fs::remove_dir_all(&app);
    }

    #[test]
    fn a_broken_file_is_a_warning_and_no_edits() {
        let app = temp();
        let session = "cf11af87da7e4bf0";
        let path = path(&app, session);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "tags = [ this is not toml").unwrap();
        let (edits, warnings) = load(&app, session);
        assert!(edits.is_empty());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("not valid TOML"), "{warnings:?}");

        std::fs::write(&path, "tags = [\"=nope\"]\n").unwrap();
        let (edits, warnings) = load(&app, session);
        assert!(edits.tags.is_empty(), "{edits:?}");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("overrides"), "{warnings:?}");
        let _ = std::fs::remove_dir_all(&app);
    }

    #[test]
    fn the_edits_outrank_the_config_and_the_config_is_still_there() {
        let app = temp();
        let session = "cf11af87da7e4bf0";
        let mut configured = settings(&["team:core", "cost:1"]);
        apply(&mut configured, &app, session);
        assert_eq!(configured.tags, vec!["team:core", "cost:1"]);
        assert!(configured.warnings.is_empty(), "{:?}", configured.warnings);

        set_tag(&app, session, "team:platform").expect("set");
        unset_tag(&app, session, "cost").expect("unset");
        let mut configured = settings(&["team:core", "cost:1"]);
        apply(&mut configured, &app, session);
        assert_eq!(configured.tags, vec!["team:platform"]);

        // The payload a consumer would see, from the same resolver a hook uses.
        let (tags, _) = parse_tags(&configured.tags);
        let (built, warnings) = payload(&configured, &facts(), &tags, "");
        assert!(warnings.is_empty(), "{warnings:?}");
        let object: serde_json::Value = serde_json::from_str(&payload_json(&built)).expect("json");
        assert_eq!(object["team"], "platform");
        assert!(object.get("cost").is_none(), "{object}");
        let _ = std::fs::remove_dir_all(&app);
    }

    #[test]
    fn resetting_forgets_everything_and_deletes_the_file() {
        let app = temp();
        let session = "cf11af87da7e4bf0";
        set_tag(&app, session, "team:core").expect("set");
        set_token(&app, session, "literal").expect("token");
        reset(&app, session).expect("reset");
        assert!(!path(&app, session).exists());
        let (edits, _) = load(&app, session);
        assert!(edits.is_empty());
        let _ = std::fs::remove_dir_all(&app);
    }

    #[test]
    fn the_git_preset_writes_the_standard_entries_and_unticking_clears_them() {
        let app = temp();
        let session = "cf11af87da7e4bf0";
        let edits = apply_preset(&app, session, "git").expect("apply");
        assert_eq!(edits.tags, crate::resolver::GIT_PRESET.to_vec());
        // Ordinary tag grammar, so every entry parses and carries a reference value.
        for raw in &edits.tags {
            let tag = Tag::parse(raw).unwrap_or_else(|err| panic!("{raw}: {err}"));
            assert!(!tag.key.is_empty());
        }
        assert!(load(&app, session).0.unset.is_empty());

        let edits = remove_preset(&app, session, "git").expect("remove");
        assert!(edits.tags.is_empty(), "{edits:?}");
        assert_eq!(edits.unset.len(), crate::resolver::GIT_PRESET.len());
        // The file is gone only when nothing else remains: the tombstones are edits.
        assert!(path(&app, session).exists());
        let mut configured = settings(&[]);
        apply(&mut configured, &app, session);
        assert!(configured.tags.is_empty(), "{:?}", configured.tags);

        let unknown = apply_preset(&app, session, "nope").expect_err("refused");
        assert!(unknown.contains("unknown preset"), "{unknown}");
        let _ = std::fs::remove_dir_all(&app);
    }

    #[test]
    fn the_entries_helper_keeps_the_grammar_it_was_given() {
        let app = temp();
        let session = "cf11af87da7e4bf0";
        set_tag(&app, session, "epic:=42").expect("set");
        set_tag(&app, session, "platform").expect("set");
        let (edits, _) = load(&app, session);
        assert_eq!(
            edits.entries(),
            vec![
                ("epic".to_string(), serde_json::json!(42)),
                ("platform".to_string(), serde_json::json!(true)),
            ]
        );
        let _ = std::fs::remove_dir_all(&app);
    }
}

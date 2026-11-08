//! Tags set above one session: a project's defaults and the AoE-wide defaults.
//!
//! A session's own edits live in [`crate::session_tags`]. This module is the
//! layer above it, with the same grammar and the same invariant: **the plugin's
//! settings are never edited**. A user who wants `team:core` for every session in
//! `billing` writes a project entry; one who wants it everywhere writes an AoE
//! entry. Both are stored in this plugin's own file, so the host's `config.toml`
//! stays what the user made it:
//!
//! ```text
//! <app dir>/tags.toml
//! ```
//!
//! ```toml
//! [aoe]
//! tags = ["team:core"]       # add or replace, key by key
//! unset = ["cost"]           # drop a tag the settings carry
//!
//! [[project]]
//! path = "/var/lib/aoe/billing"
//! tags = ["feature:checkout"]
//! unset = ["team"]           # this project does not want the AoE team tag
//! ```
//!
//! Precedence, most specific first: session edits, project, the repository's
//! `tags_file`, AoE, the settings' own `tags`, then matching rules. The two
//! daemon-written levels are overlays, not replacement lists: an entry here
//! replaces a same-key inherited tag, and `unset` drops one without erasing it
//! from the settings. That is what lets a panel say "drop this here" and "restore
//! it" without the user retyping the value.

use std::path::{Path, PathBuf};

use crate::private;
use crate::resolver::{Settings, Tag};

pub const FILE: &str = "tags.toml";

/// The two levels above a session. `session` is handled by `session_tags`.
pub const AOE: &str = "aoe";
pub const PROJECT: &str = "project";

/// One level's overlay: entries to add or replace, and keys to hide.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditSet {
    pub tags: Vec<String>,
    pub unset: Vec<String>,
}

impl EditSet {
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty() && self.unset.is_empty()
    }

    /// The keys this level adds or replaces, in file order.
    pub fn keys(&self) -> Vec<String> {
        self.tags
            .iter()
            .filter_map(|raw| Tag::parse(raw).ok())
            .map(|tag| tag.key)
            .collect()
    }

    fn set(&mut self, entry: &str) -> Result<(), String> {
        let tag = Tag::parse(entry)?;
        self.tags.retain(|raw| match Tag::parse(raw) {
            Ok(existing) => existing.key != tag.key,
            Err(_) => true,
        });
        self.unset.retain(|key| key != &tag.key);
        self.tags.push(entry.trim().to_string());
        Ok(())
    }

    fn unset(&mut self, key: &str) -> Result<(), String> {
        let key = key.trim();
        if key.is_empty() {
            return Err("a tag key is required".to_string());
        }
        // Hiding is not deleting: a level's own entry stays in the file, so
        // `restore` brings it back and the panel can say what will return.
        if !self.unset.iter().any(|listed| listed == key) {
            self.unset.push(key.to_string());
        }
        Ok(())
    }

    fn restore(&mut self, key: &str) {
        self.unset.retain(|listed| listed != key);
    }

    fn clear(&mut self) {
        self.tags.clear();
        self.unset.clear();
    }
}

/// One project's overlay, keyed by the project path the host reports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Project {
    pub path: String,
    pub edits: EditSet,
}

/// Everything stored in `<app dir>/tags.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Levels {
    pub aoe: EditSet,
    pub projects: Vec<Project>,
}

impl Levels {
    /// The project entry for `path`, or an empty overlay. The comparison trims a
    /// trailing slash so a host that reports `/repo/` finds `/repo`.
    pub fn project(&self, path: &str) -> &EditSet {
        let wanted = path.trim().trim_end_matches('/');
        self.projects
            .iter()
            .find(|project| project.path.trim_end_matches('/') == wanted)
            .map(|project| &project.edits)
            .unwrap_or(EMPTY)
    }

    fn project_mut(&mut self, path: &str) -> &mut EditSet {
        let wanted = path.trim().trim_end_matches('/').to_string();
        if let Some(index) = self
            .projects
            .iter()
            .position(|project| project.path.trim_end_matches('/') == wanted)
        {
            return &mut self.projects[index].edits;
        }
        self.projects.push(Project {
            path: wanted,
            edits: EditSet::default(),
        });
        let index = self.projects.len() - 1;
        &mut self.projects[index].edits
    }
}

static EMPTY: &EditSet = &EditSet {
    tags: Vec::new(),
    unset: Vec::new(),
};

pub fn is_level(level: &str) -> bool {
    level == AOE || level == PROJECT
}

/// `<app dir>/tags.toml`.
pub fn path(app_dir: &Path) -> PathBuf {
    app_dir.join(FILE)
}

/// Read both levels. A missing file is no overrides; a broken one is a warning
/// and no overrides, never an error — a hook that fails is worse than a tag that
/// is not applied, and `doctor` shows the warning.
pub fn load(app_dir: &Path) -> (Levels, Vec<String>) {
    let mut warnings = Vec::new();
    let path = path(app_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return (Levels::default(), warnings)
        }
        Err(err) => {
            warnings.push(format!(
                "levels: {} cannot be read ({err}); ignoring it",
                path.display()
            ));
            return (Levels::default(), warnings);
        }
    };
    let table: toml::Table = match raw.parse() {
        Ok(table) => table,
        Err(err) => {
            warnings.push(format!(
                "levels: {} is malformed TOML ({err}); ignoring it",
                path.display()
            ));
            return (Levels::default(), warnings);
        }
    };

    let mut levels = Levels::default();
    if let Some(value) = table.get("aoe") {
        if let Some(aoe) = value.as_table() {
            levels.aoe = edits_from(aoe, "aoe", &mut warnings);
        } else {
            warnings.push("levels: `aoe` is not a table; ignoring it".to_string());
        }
    }
    if let Some(value) = table.get("project") {
        let Some(rows) = value.as_array() else {
            warnings.push("levels: `project` is not an array of tables; ignoring it".to_string());
            return (levels, warnings);
        };
        for (index, row) in rows.iter().enumerate() {
            let Some(row) = row.as_table() else {
                warnings.push(format!(
                    "levels: project #{} is not a table; ignoring it",
                    index + 1
                ));
                continue;
            };
            let path = row
                .get("path")
                .and_then(toml::Value::as_str)
                .unwrap_or_default()
                .trim()
                .trim_end_matches('/')
                .to_string();
            if path.is_empty() {
                warnings.push(format!(
                    "levels: project #{} has no path; ignoring it",
                    index + 1
                ));
                continue;
            }
            let edits = edits_from(row, &format!("project {path}"), &mut warnings);
            if !edits.is_empty() {
                levels.projects.push(Project { path, edits });
            }
        }
    }
    (levels, warnings)
}

fn edits_from(table: &toml::Table, where_: &str, warnings: &mut Vec<String>) -> EditSet {
    let mut edits = EditSet::default();
    if let Some(value) = table.get("tags") {
        for raw in string_list(value, where_, "tags", warnings) {
            match Tag::parse(&raw) {
                Ok(tag) => {
                    // One entry per key, as everywhere else: a later row replaces
                    // an earlier one instead of sending both to the renderer.
                    edits.tags.retain(|existing| match Tag::parse(existing) {
                        Ok(existing) => existing.key != tag.key,
                        Err(_) => true,
                    });
                    edits.tags.push(raw);
                }
                Err(message) => warnings.push(format!(
                    "levels: {where_}: {message}; dropping it (file {FILE})"
                )),
            }
        }
    }
    if let Some(value) = table.get("unset") {
        for key in string_list(value, where_, "unset", warnings) {
            let key = key.trim();
            if !key.is_empty() && !edits.unset.iter().any(|listed| listed == key) {
                edits.unset.push(key.to_string());
            }
        }
    }
    edits
}

fn string_list(
    value: &toml::Value,
    where_: &str,
    field: &str,
    warnings: &mut Vec<String>,
) -> Vec<String> {
    let Some(items) = value.as_array() else {
        warnings.push(format!(
            "levels: {where_}: `{field}` is not a list; ignoring it"
        ));
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| match item.as_str() {
            Some(text) => Some(text.trim().to_string()),
            None => {
                warnings.push(format!(
                    "levels: {where_}: a `{field}` entry is not a string; dropping it"
                ));
                None
            }
        })
        .collect()
}

/// Write both levels atomically, or delete the file when neither has anything.
pub fn save(app_dir: &Path, levels: &Levels) -> Result<(), String> {
    let path = path(app_dir);
    let aoe = levels.aoe.clone();
    let projects: Vec<&Project> = levels
        .projects
        .iter()
        .filter(|project| !project.edits.is_empty())
        .collect();
    if aoe.is_empty() && projects.is_empty() {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(format!("cannot remove {}: {err}", path.display())),
        };
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    private::create_dir(dir).map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    let quote = |items: &[String]| -> String {
        items
            .iter()
            .map(|item| format!("{:?}", item))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut text = String::from(
        "# Tags above one session, written by the LLM Tags panel in the aoe dashboard.\n\
         # The plugin's settings are never edited: an entry here replaces a same-key\n\
         # inherited tag, and `unset` hides one without erasing it from the settings.\n",
    );
    if !aoe.is_empty() {
        text.push_str("\n[aoe]\n");
        if !aoe.tags.is_empty() {
            text.push_str(&format!("tags = [{}]\n", quote(&aoe.tags)));
        }
        if !aoe.unset.is_empty() {
            text.push_str(&format!("unset = [{}]\n", quote(&aoe.unset)));
        }
    }
    for project in projects {
        text.push_str("\n[[project]]\n");
        text.push_str(&format!("path = {:?}\n", project.path));
        if !project.edits.tags.is_empty() {
            text.push_str(&format!("tags = [{}]\n", quote(&project.edits.tags)));
        }
        if !project.edits.unset.is_empty() {
            text.push_str(&format!("unset = [{}]\n", quote(&project.edits.unset)));
        }
    }
    private::write_atomic(&path, &text)
        .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    Ok(())
}

/// Add or replace one entry at a level. `project` is required for `project`.
pub fn set_tag(app_dir: &Path, level: &str, project: &str, entry: &str) -> Result<Levels, String> {
    let (mut levels, _) = load(app_dir);
    match level {
        AOE => levels.aoe.set(entry)?,
        PROJECT => {
            if project.trim().is_empty() {
                return Err("a project level edit needs a project path".to_string());
            }
            levels.project_mut(project).set(entry)?;
        }
        other => return Err(format!("unknown tag level {other:?}")),
    }
    save(app_dir, &levels)?;
    Ok(levels)
}

/// Hide one key at a level, whether it came from below or from this level.
pub fn unset_tag(app_dir: &Path, level: &str, project: &str, key: &str) -> Result<Levels, String> {
    let (mut levels, _) = load(app_dir);
    match level {
        AOE => levels.aoe.unset(key)?,
        PROJECT => {
            if project.trim().is_empty() {
                return Err("a project level edit needs a project path".to_string());
            }
            levels.project_mut(project).unset(key)?;
        }
        other => return Err(format!("unknown tag level {other:?}")),
    }
    save(app_dir, &levels)?;
    Ok(levels)
}

/// Undo an `unset` at a level: let the inherited value show through again.
pub fn restore(app_dir: &Path, level: &str, project: &str, key: &str) -> Result<Levels, String> {
    let (mut levels, _) = load(app_dir);
    match level {
        AOE => levels.aoe.restore(key),
        PROJECT => {
            if project.trim().is_empty() {
                return Err("a project level edit needs a project path".to_string());
            }
            levels.project_mut(project).restore(key);
        }
        other => return Err(format!("unknown tag level {other:?}")),
    }
    save(app_dir, &levels)?;
    Ok(levels)
}

/// Replace a level's whole entry list. Used by "use this session's tags as the
/// defaults": what is chosen is what the level will carry, so every key it names
/// also stops being hidden.
pub fn replace(
    app_dir: &Path,
    level: &str,
    project: &str,
    entries: &[String],
) -> Result<Levels, String> {
    let mut set = EditSet::default();
    for entry in entries {
        set.set(entry)?;
    }
    let (mut levels, _) = load(app_dir);
    match level {
        AOE => levels.aoe = set,
        PROJECT => {
            if project.trim().is_empty() {
                return Err("a project level edit needs a project path".to_string());
            }
            *levels.project_mut(project) = set;
        }
        other => return Err(format!("unknown tag level {other:?}")),
    }
    save(app_dir, &levels)?;
    Ok(levels)
}

/// Forget one level's overlay. A project with nothing left is removed entirely.
pub fn reset(app_dir: &Path, level: &str, project: &str) -> Result<Levels, String> {
    let (mut levels, _) = load(app_dir);
    match level {
        AOE => levels.aoe.clear(),
        PROJECT => {
            let wanted = project.trim().trim_end_matches('/');
            if wanted.is_empty() {
                return Err("a project level edit needs a project path".to_string());
            }
            levels
                .projects
                .retain(|entry| entry.path.trim_end_matches('/') != wanted);
        }
        other => return Err(format!("unknown tag level {other:?}")),
    }
    save(app_dir, &levels)?;
    Ok(levels)
}

/// The edit set for a level, or an empty one.
pub fn edits_for<'a>(levels: &'a Levels, level: &str, project: &str) -> &'a EditSet {
    match level {
        AOE => &levels.aoe,
        PROJECT => levels.project(project),
        _ => EMPTY,
    }
}

/// Fold the AoE overlay into the settings, before the repository's file is read.
pub fn apply_aoe(settings: &mut Settings, edits: &EditSet) {
    apply(settings, edits);
}

/// Fold a project overlay in after the repository's file, so the more specific
/// project choice wins over the checkout's declaration.
pub fn apply_project(settings: &mut Settings, edits: &EditSet) {
    apply(settings, edits);
}

fn apply(settings: &mut Settings, edits: &EditSet) {
    if edits.is_empty() {
        return;
    }
    if !edits.unset.is_empty() {
        settings.tags.retain(|raw| match Tag::parse(raw) {
            Ok(tag) => !edits.unset.iter().any(|key| key == &tag.key),
            Err(_) => true,
        });
    }
    let replaced = edits.keys();
    if !replaced.is_empty() {
        settings.tags.retain(|raw| match Tag::parse(raw) {
            Ok(tag) => !replaced.contains(&tag.key),
            Err(_) => true,
        });
    }
    // A level's own entries go in front: they are more specific than everything
    // below, and the renderer spends its bounded budget in list order. They are
    // not session edits, so they stay inside `TAG_CAP`.
    let mut merged = edits.tags.clone();
    merged.append(&mut settings.tags);
    settings.tags = merged;
    // `unset` is a hide, not a delete, so it also covers this level's own entry:
    // hiding a project tag and restoring it later must not lose its value.
    if !edits.unset.is_empty() {
        settings.tags.retain(|raw| match Tag::parse(raw) {
            Ok(tag) => !edits.unset.iter().any(|key| key == &tag.key),
            Err(_) => true,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::Settings;

    struct TempDir {
        path: PathBuf,
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn temp() -> (TempDir, PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("llm-tags-levels-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        let app = path.join("agent-of-empires");
        std::fs::create_dir_all(&app).expect("app dir");
        (TempDir { path }, app)
    }

    fn settings(tags: &[&str]) -> Settings {
        let mut table = toml::Table::new();
        table.insert(
            "tags".to_string(),
            toml::Value::Array(
                tags.iter()
                    .map(|tag| toml::Value::String((*tag).to_string()))
                    .collect(),
            ),
        );
        Settings::from_table(&table, Vec::new())
    }

    fn keys(settings: &Settings) -> Vec<String> {
        settings.tags.clone()
    }

    #[test]
    fn a_project_round_trips_and_leaves_the_settings_alone() {
        let (_dir, app) = temp();
        set_tag(&app, PROJECT, "/repo/a", "team:core").expect("project tag");
        set_tag(&app, PROJECT, "/repo/a", "feature:=42").expect("project json");
        unset_tag(&app, PROJECT, "/repo/a", "cost").expect("project unset");
        let (levels, warnings) = load(&app);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            levels.project("/repo/a"),
            &EditSet {
                tags: vec!["team:core".to_string(), "feature:=42".to_string()],
                unset: vec!["cost".to_string()],
            }
        );
        assert!(levels.aoe.is_empty());
    }

    #[test]
    fn a_trailing_slash_still_finds_the_project() {
        let (_dir, app) = temp();
        set_tag(&app, PROJECT, "/repo/a", "team:core").expect("project tag");
        let (levels, _) = load(&app);
        assert_eq!(levels.project("/repo/a/").tags, vec!["team:core"]);
    }

    #[test]
    fn aoe_wins_over_config_and_project_wins_over_aoe() {
        let (_dir, app) = temp();
        set_tag(&app, AOE, "", "team:core").expect("aoe");
        set_tag(&app, PROJECT, "/repo/a", "team:platform").expect("project");
        let (levels, _) = load(&app);

        let mut base = settings(&["team:config", "keep:yes"]);
        apply_aoe(&mut base, &levels.aoe);
        assert_eq!(keys(&base), vec!["team:core", "keep:yes"]);
        apply_project(&mut base, levels.project("/repo/a"));
        assert_eq!(keys(&base), vec!["team:platform", "keep:yes"]);
    }

    #[test]
    fn unset_hides_an_inherited_tag_and_restore_brings_it_back() {
        let (_dir, app) = temp();
        set_tag(&app, PROJECT, "/repo/a", "team:core").expect("project tag");
        unset_tag(&app, PROJECT, "/repo/a", "team").expect("project unset");
        let (levels, _) = load(&app);
        let mut base = settings(&["team:config", "keep:yes"]);
        apply_aoe(&mut base, &levels.aoe);
        apply_project(&mut base, levels.project("/repo/a"));
        assert_eq!(keys(&base), vec!["keep:yes"]);

        restore(&app, PROJECT, "/repo/a", "team").expect("restore");
        let (levels, _) = load(&app);
        let mut base = settings(&["team:config", "keep:yes"]);
        apply_project(&mut base, levels.project("/repo/a"));
        assert_eq!(keys(&base), vec!["team:core", "keep:yes"]);
    }

    #[test]
    fn replace_sets_the_whole_list_and_clears_hidden_keys() {
        let (_dir, app) = temp();
        unset_tag(&app, AOE, "", "old").expect("unset");
        replace(
            &app,
            AOE,
            "",
            &["team:core".to_string(), "feature:billing".to_string()],
        )
        .expect("replace");
        let (levels, _) = load(&app);
        assert_eq!(levels.aoe.tags, vec!["team:core", "feature:billing"]);
        assert!(levels.aoe.unset.is_empty());
    }

    #[test]
    fn resetting_a_project_removes_its_row() {
        let (_dir, app) = temp();
        set_tag(&app, PROJECT, "/repo/a", "team:core").expect("project tag");
        set_tag(&app, PROJECT, "/repo/b", "team:core").expect("project tag");
        reset(&app, PROJECT, "/repo/a").expect("reset");
        let (levels, _) = load(&app);
        assert!(levels.project("/repo/a").is_empty());
        assert_eq!(levels.project("/repo/b").tags, vec!["team:core"]);
    }

    #[test]
    fn malformed_rows_warn_and_are_dropped() {
        let (_dir, app) = temp();
        std::fs::write(
            path(&app),
            "[aoe]\ntags = [\"ok:yes\", \"bad key\", 7]\nunset = [\"gone\"]\n\n[[project]]\ntags = [\"x:y\"]\n",
        )
        .expect("write");
        let (levels, warnings) = load(&app);
        assert_eq!(levels.aoe.tags, vec!["ok:yes"]);
        assert_eq!(levels.aoe.unset, vec!["gone"]);
        assert!(levels.projects.is_empty());
        assert!(
            warnings.iter().any(|w| w.contains("bad key")),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("not a string")),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("no path")),
            "{warnings:?}"
        );
    }

    #[test]
    fn an_unknown_level_is_an_error() {
        let (_dir, app) = temp();
        assert!(set_tag(&app, "session", "", "team:core").is_err());
        assert!(edits_for(&Levels::default(), "session", "/repo").is_empty());
    }
}

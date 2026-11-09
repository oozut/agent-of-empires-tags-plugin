//! Activity families: what a session is *doing*, and when to say so.
//!
//! A tag says what a session *is* — `feature:billing`, `team:core`. Those are
//! stable for a launch, so the launch hook can deliver them. An activity changes
//! while the session runs (plan, code, test, debug…), which is exactly what the
//! harness extension exists to carry (`plan/m5.md`).
//!
//! The value set is a **family**: a named list of activities, each with a label,
//! a lucide icon, a tone and — the part that makes the family useful to an agent
//! — a `when` sentence saying what has to be true to move into it ("when starting
//! to implement the feature, move the activity to coding"). The sentence is
//! advisory: nothing in this module decides that a transition happened, because a
//! guess about the work is a claim sent downstream. A later release can add a
//! machine-readable trigger beside `when` without changing the file shape.
//!
//! Families come from four places, each more specific than the last: the built-in
//! `default` family, the plugin settings (global and profile), the project's
//! plugin settings, and a TOML file inside the repository. A same-named family
//! from a more specific source replaces the whole definition — the repo file wins
//! a conflict, and everything the repo does not name still comes from the UI or
//! the settings. The family a session *starts* in is a per-project setting, so a
//! repository's sessions begin in the family the user chose for that project.
//!
//! **Definitions are launch-scoped.** The hook pins the resolved family into
//! `<app_dir>/sessions/<id>/activity.toml`, and every mid-session read — the
//! `activity` verb and the pane — reads that pin. Editing a family
//! therefore changes the *next* session, never one already running; only the
//! current value changes while a session runs. `plan/parameters.md` §5b.
//!
//! Presentation only. The tag itself is `activity` = one value of the session's
//! family, and that is what travels in the payload; the icon never does. A
//! terminal UI shows text and tone, so [`Activity::label`] is what a badge falls
//! back to.

use std::path::{Path, PathBuf};

use crate::resolver::Tag;

/// The reserved tag key. A session has at most one activity.
pub const KEY: &str = "activity";

/// The pinned family, inside the session's own directory.
pub const FILE: &str = "activity.toml";

/// The family every untroubled session gets.
pub const DEFAULT_FAMILY: &str = "default";

/// Where a repository declares its families, relative to the project root. It is
/// the `activity_file` setting's default; an empty setting means "no repo file".
pub const DEFAULT_FILE: &str = ".aoe/activity.toml";

/// The tones a surface knows how to render.
pub const TONES: [&str; 5] = ["neutral", "info", "success", "warn", "danger"];

/// A `when` sentence is a sentence, not a document.
pub const WHEN_MAX_CHARS: usize = 400;
const LABEL_MAX_CHARS: usize = 48;
const NAME_MAX_CHARS: usize = 64;

/// One activity: its tag value, how it is rendered, and when to move into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    /// The value half of `activity:<this>`.
    pub key: String,
    /// What a badge says where an icon cannot render.
    pub label: String,
    /// A lucide kebab-case name, the only icon vocabulary the host renders.
    pub icon: String,
    /// One of [`TONES`].
    pub tone: String,
    /// The transition requirement in prose: what has to be true to enter it.
    pub when: String,
}

/// A named set of activities: one session's whole vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Family {
    pub name: String,
    pub activities: Vec<Activity>,
}

/// The families this launch resolved, which one it chose, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Families {
    pub list: Vec<Family>,
    /// The name of the family this session uses. Always names a family in
    /// `list`: an unknown choice is a warning and [`DEFAULT_FAMILY`].
    pub chosen: String,
    /// Where the chosen family's definition came from (`built-in`, `settings`,
    /// `repo .aoe/activity.toml`), for `activity --show` and the pane's note.
    pub source: String,
    pub warnings: Vec<String>,
}

impl Families {
    /// The session's family. `list` always holds [`DEFAULT_FAMILY`], so this is
    /// total.
    pub fn chosen(&self) -> &Family {
        self.list
            .iter()
            .find(|family| family.name == self.chosen)
            .unwrap_or_else(|| self.list.first().expect("a family list is never empty"))
    }

    pub fn names(&self) -> Vec<&str> {
        self.list
            .iter()
            .map(|family| family.name.as_str())
            .collect()
    }

    pub fn find(&self, value: &str) -> Option<&Activity> {
        find(self.chosen(), value)
    }

    pub fn legend(&self) -> Vec<serde_json::Value> {
        legend(self.chosen())
    }

    /// The values this session may be set to, on one line, for a message that
    /// tells an agent what it *can* say after it said something else.
    pub fn names_in_use(&self) -> String {
        self.chosen()
            .activities
            .iter()
            .map(|activity| activity.key.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Whether this launch has to pin anything.
    ///
    /// The built-in family is compiled in, so a session that resolved nothing but
    /// the built-in has nothing to pin: leaving it unpinned keeps "a launch that
    /// carries nothing writes nothing" true. A chosen family, a second family, or
    /// a repo declaration is a decision only this launch knows, and it is pinned so
    /// editing it later cannot relabel a running session.
    pub fn needs_pin(&self) -> bool {
        self.source != "built-in" || self.chosen != DEFAULT_FAMILY || self.list.len() > 1
    }

    /// The pin this launch writes into the session's directory.
    pub fn to_toml(&self) -> String {
        let mut text = String::from(
            "# The activity family this session was launched with, written by the plugin.\n\
             # Editing the definitions affects the next session, not this one.\n",
        );
        text.push_str(&toml_string("family", &self.chosen));
        text.push_str(&toml_string("source", &self.source));
        text.push('\n');
        for activity in &self.chosen().activities {
            text.push_str("[[activities]]\n");
            text.push_str(&toml_string("key", &activity.key));
            text.push_str(&toml_string("label", &activity.label));
            text.push_str(&toml_string("icon", &activity.icon));
            text.push_str(&toml_string("tone", &activity.tone));
            text.push_str(&toml_string("when", &activity.when));
            text.push('\n');
        }
        text
    }

    /// Read a pin back. `None` when the file says nothing usable, so the caller
    /// can fall back to the live resolution instead of showing an empty family.
    pub fn from_pin(raw: &str) -> (Option<Families>, Vec<String>) {
        let mut warnings = Vec::new();
        let table = match raw.parse::<toml::Table>() {
            Ok(table) => table,
            Err(err) => {
                warnings.push(format!(
                    "activity: the pinned family is not valid TOML ({err})"
                ));
                return (None, warnings);
            }
        };
        let name = match table.get("family").and_then(toml::Value::as_str) {
            Some(name) if !name.trim().is_empty() => name.trim().to_string(),
            _ => {
                warnings.push("activity: the pinned family names no family".to_string());
                return (None, warnings);
            }
        };
        let rows = table
            .get("activities")
            .and_then(toml::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let activities = parse_rows(&rows, &name, "the pinned family", &mut warnings);
        let activities: Vec<Activity> = activities
            .into_iter()
            .flat_map(|family| family.activities)
            .collect();
        if activities.is_empty() {
            warnings.push("activity: the pinned family has no activities".to_string());
            return (None, warnings);
        }
        let source = table
            .get("source")
            .and_then(toml::Value::as_str)
            .unwrap_or("pinned at launch")
            .to_string();
        (
            Some(Families {
                list: vec![Family {
                    name: name.clone(),
                    activities,
                }],
                chosen: name,
                source,
                warnings: Vec::new(),
            }),
            warnings,
        )
    }
}

/// The built-in family: the nine phases of the work, and the sentence that says
/// when each one starts. A user defines their own to replace it.
pub fn builtin() -> Family {
    let activity = |key: &str, label: &str, icon: &str, tone: &str, when: &str| Activity {
        key: key.to_string(),
        label: label.to_string(),
        icon: icon.to_string(),
        tone: tone.to_string(),
        when: when.to_string(),
    };
    Family {
        name: DEFAULT_FAMILY.to_string(),
        activities: vec![
            activity(
                "explore",
                "exploring",
                "compass",
                "neutral",
                "when reading the codebase, grepping, or mapping a subsystem",
            ),
            activity(
                "plan",
                "planning",
                "map",
                "info",
                "when agreeing a design, writing a plan or a task list",
            ),
            activity(
                "code",
                "coding",
                "code",
                "neutral",
                "when starting to implement or edit source files",
            ),
            activity(
                "test",
                "testing",
                "flask-conical",
                "warn",
                "when writing or running tests, or the build",
            ),
            activity(
                "debug",
                "debugging",
                "bug",
                "danger",
                "when chasing a failure to its cause",
            ),
            activity(
                "review",
                "reviewing",
                "git-pull-request",
                "info",
                "when reading a diff for correctness, or reviewing a branch",
            ),
            activity(
                "docs",
                "writing docs",
                "book-open",
                "neutral",
                "when writing a README, docs, a changelog or comments",
            ),
            activity(
                "ops",
                "ops",
                "terminal",
                "warn",
                "when touching dependencies, packaging, CI, deploys or migrations",
            ),
            activity(
                "idle",
                "idle",
                "pause",
                "neutral",
                "when waiting on the user, or nothing is in flight",
            ),
        ],
    }
}

/// Resolve this launch's families from the settings a session already resolved.
///
/// The project's own plugin settings arrive through the same settings table (the
/// resolver folds the registry into it), so this function never reads a file: it
/// is the one place the precedence order lives, and every surface calls it.
pub fn resolve(settings: &crate::resolver::Settings) -> Families {
    let mut warnings = Vec::new();
    let mut families: Vec<(String, Family)> = vec![(DEFAULT_FAMILY.to_string(), builtin())];
    let mut sources: Vec<String> = vec!["built-in".to_string()];

    // A more specific source replaces a same-named family whole; it does not
    // extend it. That is what "the repo file wins a conflict" means in practice,
    // and it is the only rule a reader has to remember.
    let mut replace = |family: Family, source: String| {
        let name = family.name.clone();
        match families.iter().position(|(existing, _)| *existing == name) {
            Some(index) => {
                families[index] = (name, family);
                sources[index] = source;
            }
            None => {
                families.push((name, family));
                sources.push(source);
            }
        }
    };
    // The settings table: global and profile merged, plus the project's own rows
    // when the resolver could read the registry.
    if !settings.activity_families.is_empty() {
        for family in parse_rows(&settings.activity_families, "", "settings", &mut warnings) {
            replace(family, "settings".to_string());
        }
    }

    // The repository's file is the most specific declaration: a same-named family
    // is replaced whole, so "the repo wins a conflict" needs no field-by-field
    // merge to explain.
    let mut repo_default = None;
    if let Some(table) = &settings.activity_repo {
        let origin = format!("repo {}", settings.activity_file.trim());
        let (parsed, declared) = parse_repo(table, &mut warnings);
        for family in parsed {
            replace(family, origin.clone());
        }
        repo_default = declared;
    }

    // The starting family: the repo's own `default` first, then the per-project
    // (or global/profile) `activity_family`, then the built-in name.
    let configured = settings.activity_family.trim().to_string();
    let mut pick = |candidate: &str, origin: &str| -> Option<String> {
        if families.iter().any(|(name, _)| name == candidate) {
            Some(candidate.to_string())
        } else {
            warnings.push(format!(
                "activity: {origin} names the family {candidate:?}, which nothing defines; \
                 using another"
            ));
            None
        }
    };
    let chosen = repo_default
        .as_deref()
        .and_then(|name| pick(name, "the repo file's default"))
        .or_else(|| {
            (!configured.is_empty())
                .then(|| pick(&configured, "activity_family"))
                .flatten()
        })
        .unwrap_or_else(|| DEFAULT_FAMILY.to_string());
    let source = families
        .iter()
        .position(|(name, _)| *name == chosen)
        .map(|index| sources[index].clone())
        .unwrap_or_else(|| "built-in".to_string());

    Families {
        list: families.into_iter().map(|(_, family)| family).collect(),
        chosen,
        source,
        warnings,
    }
}

/// The activity a value names, accepting `test`, `activity:test`, or a value
/// with stray case/whitespace — all three mean the same badge.
pub fn find<'a>(family: &'a Family, value: &str) -> Option<&'a Activity> {
    let value = value.trim().to_ascii_lowercase();
    let value = value.strip_prefix("activity:").unwrap_or(&value);
    // The key is canonical, but a person (and an agent copying the word out of
    // the badge) says "testing", so the label is accepted too. It is never
    // stored: the tag is always `activity:<key>`.
    family
        .activities
        .iter()
        .find(|activity| activity.key == value || activity.label.to_ascii_lowercase() == value)
}

/// This session's activity: the last `activity` tag wins, because a session's
/// own `overrides.toml` is applied after the inherited tags.
pub fn of<'a>(families: &'a Families, tags: &[Tag]) -> Option<&'a Activity> {
    tags.iter()
        .rev()
        .filter(|tag| tag.key == KEY)
        .find_map(|tag| match &tag.value {
            serde_json::Value::String(value) => families.find(value),
            // A bare `activity` marker is not an activity: which one would it be?
            _ => None,
        })
}

/// Activities a tag set asks for but this family does not have.
///
/// Reported once per launch rather than rendered: a value outside the family is
/// not given a guessed icon, so a typo (or a value from another family) stays
/// visible instead of becoming a fourth spelling of "testing".
pub fn unknown(families: &Families, tags: &[Tag]) -> Vec<String> {
    tags.iter()
        .filter(|tag| tag.key == KEY)
        .filter_map(|tag| match &tag.value {
            serde_json::Value::String(value) if families.find(value).is_none() => {
                Some(value.clone())
            }
            _ => None,
        })
        .collect()
}

/// One family as the pane offers it: a row per activity, icon and tone included.
pub fn legend(family: &Family) -> Vec<serde_json::Value> {
    family
        .activities
        .iter()
        .map(|activity| {
            serde_json::json!({
                "key": activity.key,
                "label": activity.label,
                "icon": activity.icon,
                "tone": activity.tone,
                "when": activity.when,
            })
        })
        .collect()
}

/// `<app dir>/sessions/<session id>/activity.toml`.
pub fn path(app_dir: &Path, session_id: &str) -> PathBuf {
    app_dir
        .join("sessions")
        .join(crate::overlay::sanitize(session_id))
        .join(FILE)
}

/// Pin the family this launch resolved, so a mid-session read sees the
/// definitions the session started with.
pub fn pin(app_dir: &Path, session_id: &str, families: &Families) -> Result<(), String> {
    let path = path(app_dir, session_id);
    let dir = path.parent().unwrap_or(Path::new("."));
    crate::private::create_dir(dir)
        .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
    crate::private::write_atomic(&path, &families.to_toml())
        .map_err(|err| format!("cannot write {}: {err}", path.display()))
}

/// The family this session was launched with, if the launch pinned one.
pub fn pinned(app_dir: &Path, session_id: &str) -> (Option<Families>, Vec<String>) {
    let path = path(app_dir, session_id);
    match std::fs::read_to_string(&path) {
        Ok(raw) => Families::from_pin(&raw),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (None, Vec::new()),
        Err(err) => (
            None,
            vec![format!("activity: cannot read {}: {err}", path.display())],
        ),
    }
}

/// The family a mid-session read must use: the pin when there is one, the live
/// resolution otherwise (a session that predates pinning, or one whose pin could
/// not be written).
pub fn for_session(app_dir: &Path, session_id: &str, live: &Families) -> (Families, Vec<String>) {
    match pinned(app_dir, session_id) {
        (Some(pinned), warnings) => (pinned, warnings),
        (None, warnings) => (live.clone(), warnings),
    }
}

// --- parsing ---------------------------------------------------------------

/// A family name a settings row or a repo file may use.
pub fn valid_family_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && name.chars().count() <= NAME_MAX_CHARS
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

/// An activity key. Lowercase kebab on purpose: the value is a tag value, and a
/// key that changes spelling would make the badge an accident of typing.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.chars().count() <= NAME_MAX_CHARS
        && key
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        && key
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphanumeric())
        && key
            .chars()
            .last()
            .is_some_and(|ch| ch.is_ascii_alphanumeric())
}

/// Rows in the flat shape — the settings object list, a repo file's
/// `[[activities]]`, and the pin:
///
/// ```toml
/// [[activities]]
/// family = "work"      # absent when the caller names the family
/// value = "code"       # `key` is accepted too, for the pin
/// label = "coding"
/// icon = "code"
/// tone = "neutral"
/// when = "when starting to implement the feature"
/// ```
fn parse_rows(
    rows: &[toml::Value],
    family: &str,
    origin: &str,
    warnings: &mut Vec<String>,
) -> Vec<Family> {
    let mut out: Vec<Family> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let position = index + 1;
        let Some(row) = row.as_table() else {
            warnings.push(format!(
                "activity: {origin} row {position} is not a table; skipping it"
            ));
            continue;
        };
        let named = string_field(row, "family", origin, position, warnings);
        let name = named.as_deref().unwrap_or(family).trim().to_string();
        let name = if name.is_empty() {
            warnings.push(format!(
                "activity: {origin} row {position} names no family; skipping it"
            ));
            continue;
        } else if !valid_family_name(&name) {
            warnings.push(format!(
                "activity: {origin} family {name:?} must be letters, digits, `.`, `_` or `-` \
                 (≤{NAME_MAX_CHARS}); skipping row {position}"
            ));
            continue;
        } else {
            name
        };
        let key = string_field(row, "value", origin, position, warnings)
            .or_else(|| string_field(row, "key", origin, position, warnings))
            .unwrap_or_default()
            .trim()
            .to_string();
        if !valid_key(&key) {
            warnings.push(format!(
                "activity: {origin} row {position} has no usable value (lowercase letters, \
                 digits and `-`, ≤{NAME_MAX_CHARS}); skipping it"
            ));
            continue;
        }
        let label = string_field(row, "label", origin, position, warnings)
            .map(|label| label.trim().to_string())
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| key.clone());
        let label = crate::resolver::truncate_chars(&label, LABEL_MAX_CHARS);
        let icon = string_field(row, "icon", origin, position, warnings)
            .map(|icon| icon.trim().to_string())
            .filter(|icon| !icon.is_empty())
            .unwrap_or_else(|| "tag".to_string());
        let tone = match string_field(row, "tone", origin, position, warnings) {
            None => "neutral".to_string(),
            Some(tone) if TONES.contains(&tone.trim()) => tone.trim().to_string(),
            Some(tone) => {
                warnings.push(format!(
                    "activity: {origin} row {position} tone {tone:?} is not one of {}; \
                     using neutral",
                    TONES.join(", ")
                ));
                "neutral".to_string()
            }
        };
        let when = string_field(row, "when", origin, position, warnings)
            .map(|when| when.trim().to_string())
            .unwrap_or_default();
        let when = crate::resolver::truncate_chars(&when, WHEN_MAX_CHARS);
        let activity = Activity {
            key,
            label,
            icon,
            tone,
            when,
        };

        match out.iter_mut().find(|existing| existing.name == name) {
            Some(existing) => match existing
                .activities
                .iter_mut()
                .find(|existing| existing.key == activity.key)
            {
                Some(current) => *current = activity,
                None => existing.activities.push(activity),
            },
            None => out.push(Family {
                name,
                activities: vec![activity],
            }),
        }
    }
    out
}

fn string_field(
    row: &toml::Table,
    key: &str,
    origin: &str,
    position: usize,
    warnings: &mut Vec<String>,
) -> Option<String> {
    match row.get(key) {
        None => None,
        Some(toml::Value::String(text)) => Some(text.clone()),
        Some(_) => {
            warnings.push(format!(
                "activity: {origin} row {position} field {key:?} is not a string; ignoring it"
            ));
            None
        }
    }
}

/// The repo file's own shape, which is meant to be written by hand:
///
/// ```toml
/// default = "work"
///
/// [families.work.activities.code]
/// label = "coding"
/// icon = "code"
/// tone = "neutral"
/// when = "when starting to implement the feature"
/// ```
///
/// A flat `[[activities]]` array is accepted too, because the settings UI and the
/// pin use it and one grammar fewer is one surprise fewer.
fn parse_repo(table: &toml::Table, warnings: &mut Vec<String>) -> (Vec<Family>, Option<String>) {
    let mut families: Vec<Family> = Vec::new();
    if let Some(nested) = table.get("families") {
        match nested.as_table() {
            None => {
                warnings.push("activity: the repo file's `families` is not a table".to_string())
            }
            Some(nested) => {
                for (name, entry) in nested {
                    let name = name.trim();
                    let Some(entry) = entry.as_table() else {
                        warnings.push(format!(
                            "activity: repo family {name:?} is not a table; skipping it"
                        ));
                        continue;
                    };
                    if !valid_family_name(name) {
                        warnings.push(format!(
                            "activity: repo family {name:?} must be letters, digits, `.`, `_` or \
                             `-` (≤{NAME_MAX_CHARS}); skipping it"
                        ));
                        continue;
                    }
                    let mut rows: Vec<toml::Value> = Vec::new();
                    match entry.get("activities") {
                        Some(toml::Value::Table(activities)) => {
                            for (key, fields) in activities {
                                let mut row = fields.as_table().cloned().unwrap_or_default();
                                row.insert("value".to_string(), toml::Value::String(key.clone()));
                                rows.push(toml::Value::Table(row));
                            }
                        }
                        Some(toml::Value::Array(rows_in)) => rows.extend(rows_in.iter().cloned()),
                        Some(_) => warnings.push(format!(
                            "activity: repo family {name:?} has an `activities` value that is \
                             neither a table nor an array; skipping it"
                        )),
                        None => warnings.push(format!(
                            "activity: repo family {name:?} lists no activities; skipping it"
                        )),
                    }
                    let parsed = parse_rows(&rows, name, "the repo file", warnings);
                    if let Some(family) = parsed.into_iter().next() {
                        families.push(family);
                    }
                }
            }
        }
    }
    if let Some(rows) = table.get("activities").and_then(toml::Value::as_array) {
        for family in parse_rows(rows, "", "the repo file", warnings) {
            match families
                .iter_mut()
                .find(|existing| existing.name == family.name)
            {
                Some(existing) => existing.activities.extend(family.activities),
                None => families.push(family),
            }
        }
    }
    let default = table
        .get("default")
        .or_else(|| table.get("family"))
        .and_then(toml::Value::as_str)
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty());
    (families, default)
}

/// One TOML string assignment, so the pin cannot be broken by a value holding a
/// quote or a newline.
fn toml_string(key: &str, value: &str) -> String {
    format!("{key} = {:?}\n", value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::Settings;

    fn settings(rows: &str) -> Settings {
        let table: toml::Table = rows.parse().expect("toml");
        Settings::from_table(&table, Vec::new())
    }

    fn tag(value: &str) -> Tag {
        Tag {
            key: KEY.to_string(),
            value: serde_json::json!(value),
        }
    }

    #[test]
    fn the_builtin_family_names_every_value_and_says_when_it_starts() {
        let family = builtin();
        assert_eq!(family.name, DEFAULT_FAMILY);
        assert_eq!(family.activities.len(), 9);
        for activity in &family.activities {
            assert!(valid_key(&activity.key), "{}", activity.key);
            assert!(!activity.label.is_empty());
            assert!(
                !activity
                    .icon
                    .chars()
                    .any(|c| c.is_ascii_uppercase() || c == '_'),
                "{} is not a lucide kebab-case name",
                activity.icon
            );
            assert!(TONES.contains(&activity.tone.as_str()), "{}", activity.tone);
            assert!(
                !activity.when.is_empty(),
                "{} has no transition text",
                activity.key
            );
        }
    }

    #[test]
    fn a_setting_rows_family_replaces_the_builtin_one() {
        let resolved = resolve(&settings(
            r#"
            [[activity_families]]
            family = "work"
            value = "write"
            label = "writing"
            icon = "pen-line"
            tone = "info"
            when = "when starting to implement the feature"
            "#,
        ));
        assert_eq!(resolved.chosen, DEFAULT_FAMILY);
        assert_eq!(resolved.names(), vec!["default", "work"]);
        let work = resolved.list.iter().find(|f| f.name == "work").unwrap();
        assert_eq!(work.activities.len(), 1);
        assert_eq!(
            work.activities[0].when,
            "when starting to implement the feature"
        );
        // The built-in family is untouched: a new family is added, not merged in.
        assert_eq!(resolved.list[0].activities.len(), 9);
    }

    #[test]
    fn the_chosen_family_comes_from_the_repo_then_the_setting_then_the_default() {
        let mut s = settings(
            r#"
            activity_family = "research"
            [[activity_families]]
            family = "research"
            value = "read"
            label = "reading"
            "#,
        );
        let resolved = resolve(&s);
        assert_eq!(resolved.chosen, "research");
        assert_eq!(resolved.source, "settings");

        // The repo file names its own default, and wins the choice.
        s.activity_file = ".aoe/activity.toml".to_string();
        s.activity_repo = Some(
            r#"
            default = "work"
            [families.work.activities.code]
            label = "coding"
            when = "when implementing"
            "#
            .parse()
            .unwrap(),
        );
        let resolved = resolve(&s);
        assert_eq!(resolved.chosen, "work");
        assert!(resolved.source.starts_with("repo "), "{}", resolved.source);
        assert_eq!(resolved.names(), vec!["default", "research", "work"]);
    }

    #[test]
    fn a_repo_family_replaces_a_same_named_setting_family_whole() {
        let mut s = settings(
            r#"
            [[activity_families]]
            family = "work"
            value = "write"
            label = "writing"
            "#,
        );
        s.activity_repo = Some(
            r#"
            [families.work.activities.code]
            label = "coding"
            "#
            .parse()
            .unwrap(),
        );
        let resolved = resolve(&s);
        let work = resolved.list.iter().find(|f| f.name == "work").unwrap();
        // `write` is gone: the repo's definition replaced the family, it did not
        // extend it.
        assert_eq!(work.activities.len(), 1);
        assert_eq!(work.activities[0].key, "code");
    }

    #[test]
    fn an_unknown_choice_is_a_warning_and_the_default_family() {
        let resolved = resolve(&settings("activity_family = \"nope\""));
        assert_eq!(resolved.chosen, DEFAULT_FAMILY);
        assert!(
            resolved.warnings.iter().any(|w| w.contains("nope")),
            "{:?}",
            resolved.warnings
        );
    }

    #[test]
    fn a_row_with_a_bad_value_or_tone_is_skipped_or_defaulted() {
        let resolved = resolve(&settings(
            r#"
            [[activity_families]]
            family = "work"
            value = "Code!"
            [[activity_families]]
            family = "work"
            value = "code"
            tone = "loud"
            [[activity_families]]
            family = "not a family"
            value = "code"
            "#,
        ));
        let work = resolved.list.iter().find(|f| f.name == "work").unwrap();
        assert_eq!(work.activities.len(), 1, "{:?}", work.activities);
        assert_eq!(work.activities[0].tone, "neutral");
        assert_eq!(work.activities[0].label, "code");
        assert_eq!(resolved.warnings.len(), 3, "{:?}", resolved.warnings);
    }

    #[test]
    fn case_around_a_value_does_not_make_a_new_activity() {
        let families = Families {
            list: vec![builtin()],
            chosen: DEFAULT_FAMILY.to_string(),
            source: "built-in".to_string(),
            warnings: Vec::new(),
        };
        assert_eq!(families.find("TEST").map(|a| a.key.as_str()), Some("test"));
        assert_eq!(
            families.find("  activity:Debug ").map(|a| a.key.as_str()),
            Some("debug")
        );
        // The label is a convenience for a human or an agent quoting the badge;
        // the tag it produces is still the key.
        assert_eq!(
            families.find("Testing").map(|a| a.key.as_str()),
            Some("test")
        );
        assert_eq!(
            families.find("writing docs").map(|a| a.key.as_str()),
            Some("docs")
        );
        assert_eq!(families.find("testing!"), None);
    }

    #[test]
    fn the_last_activity_wins_and_a_value_from_another_family_is_reported() {
        let families = Families {
            list: vec![builtin()],
            chosen: DEFAULT_FAMILY.to_string(),
            source: "built-in".to_string(),
            warnings: Vec::new(),
        };
        let tags = vec![tag("plan"), tag("code"), tag("shipping")];
        assert_eq!(of(&families, &tags).map(|a| a.key.as_str()), Some("code"));
        assert_eq!(unknown(&families, &tags), vec!["shipping".to_string()]);
        assert_eq!(of(&families, &[]), None);
    }

    #[test]
    fn a_bare_marker_is_not_an_activity() {
        let families = Families {
            list: vec![builtin()],
            chosen: DEFAULT_FAMILY.to_string(),
            source: "built-in".to_string(),
            warnings: Vec::new(),
        };
        let tags = vec![Tag {
            key: KEY.to_string(),
            value: serde_json::Value::Bool(true),
        }];
        assert_eq!(of(&families, &tags), None);
        assert!(
            unknown(&families, &tags).is_empty(),
            "a marker is not a typo either"
        );
    }

    #[test]
    fn the_pin_round_trips_through_toml() {
        let families = Families {
            list: vec![Family {
                name: "work".to_string(),
                activities: vec![Activity {
                    key: "code".to_string(),
                    label: "coding".to_string(),
                    icon: "code".to_string(),
                    tone: "neutral".to_string(),
                    when: "when starting to implement \"the feature\"\nand the plan is agreed"
                        .to_string(),
                }],
            }],
            chosen: "work".to_string(),
            source: "repo .aoe/activity.toml".to_string(),
            warnings: Vec::new(),
        };
        let text = families.to_toml();
        let (back, warnings) = Families::from_pin(&text);
        let back = back.expect("a pin reads back");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(back.chosen, "work");
        assert_eq!(back.list[0].activities, families.list[0].activities);
        assert_eq!(back.source, "repo .aoe/activity.toml");
    }

    #[test]
    fn a_pin_with_no_activities_is_not_a_family() {
        let (back, warnings) = Families::from_pin("family = \"work\"\n");
        assert!(back.is_none());
        assert!(
            warnings.iter().any(|w| w.contains("no activities")),
            "{warnings:?}"
        );
        let (back, warnings) = Families::from_pin("not = = toml");
        assert!(back.is_none());
        assert!(!warnings.is_empty());
    }

    #[test]
    fn the_pin_is_read_from_the_session_directory_and_the_live_family_is_the_fallback() {
        let dir = std::env::temp_dir().join(format!(
            "activity-pin-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let session = "cf11af87da7e4bf0";
        let live = Families {
            list: vec![builtin()],
            chosen: DEFAULT_FAMILY.to_string(),
            source: "built-in".to_string(),
            warnings: Vec::new(),
        };
        // No pin yet: the live resolution is used.
        let (family, warnings) = for_session(&dir, session, &live);
        assert_eq!(family.chosen, DEFAULT_FAMILY);
        assert!(warnings.is_empty());

        let pinned = Families {
            list: vec![Family {
                name: "work".to_string(),
                activities: vec![Activity {
                    key: "code".to_string(),
                    label: "coding".to_string(),
                    icon: "code".to_string(),
                    tone: "neutral".to_string(),
                    when: "when implementing".to_string(),
                }],
            }],
            chosen: "work".to_string(),
            source: "settings".to_string(),
            warnings: Vec::new(),
        };
        pin(&dir, session, &pinned).expect("pin");
        let (family, warnings) = for_session(&dir, session, &live);
        assert_eq!(family.chosen, "work");
        assert!(warnings.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

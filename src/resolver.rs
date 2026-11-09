//! The one resolver: session facts → fields → one value per enabled header.
//!
//! Every verb (`hook`, `value`, `setup`, `doctor`) calls this module, so they
//! cannot drift. The only outside dependency is a subprocess call to
//! `aoe session show --json` / `aoe list --json`, always behind an injectable
//! runner and a hard timeout: the tests need no aoe, no network, no daemon.
//!
//! Resolution order per field is fixed by the plan (`plan/m0.md` task 2): the
//! session row first, then the flags the hook filled from `AOE_*`, then
//! defaults. The row is a superset of the hook environment and the only source
//! of `group`/`title` for structured host sessions.

use std::collections::BTreeMap;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::env::Env;
use crate::settings;

// --- constants: plan/parameters.md §4, §5, §7 -------------------------------

/// The budget for *inherited* tags in one header. A session's own edits are
/// added on top and are never capped (plan/parameters.md §5).
pub const TAG_CAP: usize = 5;
/// A tag carries a whole `key:=value` pair now, so the ceiling is the string in
/// settings, not the delivered value (`VALUE_MAX_BYTES` still clamps that).
pub const TAG_MAX_CHARS: usize = 96;
/// A `tags_file` is a small declaration, not a data dump; the cap keeps a hook
/// from reading a repository file it has no business reading, and it is applied
/// to the *read* (`take`), not to whatever `metadata` claims the size is.
pub const TAGS_FILE_MAX_BYTES: u64 = 64 * 1024;
/// A string from a `tags_file` is one field of one header, not a document:
/// longer than this it is trimmed. A list or an object is left whole — it is a
/// structure rather than text, and the file cap already bounds it.
pub const TAGS_FILE_STRING_MAX_CHARS: usize = 256;
pub const VALUE_MAX_BYTES: usize = 800;
pub const SHORT_ID_PREFIX: &str = "S-";
pub const SHORT_ID_CHARS: usize = 4;
pub const ENV_PREFIX: &str = "AOE_HDR_";
pub const ENV_PREFIX_QUERY: &str = "AOE_QRY_";
pub const ENV_PREFIX_BODY: &str = "AOE_BODY_";
pub const DEFAULT_HEADER: &str = "X-AoE-Tags";
pub const DEFAULT_SESSION_TAGGING: &str = "short_id";
pub const DEFAULT_TOOL: &str = "unknown";
pub const AOE_FALLBACK: &str = "/usr/local/bin/aoe";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

pub const SESSION_TAGGING_MODES: [&str; 3] = ["none", "short_id", "full_id"];
pub const MATCH_KINDS_M0: [&str; 2] = ["project", "manual"];
pub const MATCH_KINDS_M1: [&str; 3] = ["group", "tool", "title_regex"];

/// Hook environment → flag fallback. The hook line passes these explicitly; the
/// direct command form (`!aoe-llm-tags value`) relies on the variables aoe
/// already injects into the agent process.
pub const ENV_FALLBACKS: [(&str, &str); 6] = [
    ("session", "AOE_SESSION_ID"),
    ("profile", "AOE_PROFILE"),
    ("tool", "AOE_TOOL"),
    ("project", "AOE_PROJECT_PATH"),
    ("group", "AOE_GROUP_PATH"),
    ("title", "AOE_SESSION_TITLE"),
];

/// A `aoe` call that could not be made, failed, or printed something else.
#[derive(Debug, thiserror::Error)]
pub enum AoeError {
    #[error("`{bin} {verb}` timed out after {timeout}s")]
    TimedOut {
        bin: String,
        verb: String,
        timeout: f64,
    },
    #[error("`{bin}` could not run: {source}")]
    Spawn {
        bin: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`{bin} {verb}` exited {code}{detail}")]
    Failed {
        bin: String,
        verb: String,
        code: i32,
        detail: String,
    },
    #[error("`{bin} {verb}` printed invalid JSON: {detail}")]
    InvalidJson {
        bin: String,
        verb: String,
        detail: String,
    },
}

/// Where the harness should put the value: a request header (default), a query
/// parameter on the provider URL, or a JSON field in the request body.
///
/// Only a header can carry a per-session value on every harness: pi interpolates
/// `$VAR` in `headers` values, not in `baseUrl` or `samplingParams`. A `query` or
/// `body` entry therefore needs a `literal` on such a harness, and the CLI says so
/// instead of writing a `$VAR` that would arrive verbatim.
pub const KINDS: [&str; 3] = ["header", "query", "body"];
pub const DEFAULT_KIND: &str = "header";

/// How a target encodes the tag set.
///
/// `json` is the default: the tag set goes out as one object, so a consumer reads
/// a field (`tags["team"]`) instead of splitting a string and guessing where a
/// value starts. `csv` is the M0 flat form, kept for consumers that already parse
/// it and for values a JSON object would not carry (a bare header, a shell line).
pub const FORMATS: [&str; 2] = ["json", "csv"];
pub const DEFAULT_FORMAT: &str = "json";

/// One place the tag set is delivered to: a request header, a query parameter on
/// the provider URL, or a JSON field in the request body.
///
/// The tag set is resolved once (`tags`); a target only chooses `where` and
/// `which slice of it` — `{tags}` for the whole set, `{feature}`/`{session}`/…
/// for one field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub template: String,
    pub env_var: String,
    pub enabled: bool,
    /// `header`, `query` or `body`.
    pub kind: String,
    /// Query-parameter or body-field name; empty means "use `name`".
    pub param: String,
    /// Value written verbatim when the target cannot interpolate `$VAR`.
    pub literal: String,
    /// `json` (the tag set as an object) or `csv` (the flat tag list).
    pub format: String,
}

impl Target {
    pub fn new(name: &str, template: &str, env_var: &str) -> Self {
        Self {
            name: name.to_string(),
            template: template.to_string(),
            env_var: env_var.to_string(),
            enabled: true,
            kind: DEFAULT_KIND.to_string(),
            param: String::new(),
            literal: String::new(),
            format: DEFAULT_FORMAT.to_string(),
        }
    }

    pub fn with_format(mut self, format: &str) -> Self {
        self.format = format.to_string();
        self
    }

    /// True when this target sends the tag set as one JSON object.
    pub fn renders_payload(&self) -> bool {
        self.template.trim().is_empty() && self.format != "csv"
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    pub fn with_kind(mut self, kind: &str, param: &str) -> Self {
        self.kind = kind.to_string();
        self.param = param.to_string();
        self
    }

    pub fn with_literal(mut self, literal: &str) -> Self {
        self.literal = literal.to_string();
        self
    }

    /// The query-parameter or body-field name this target writes.
    pub fn param_name(&self) -> &str {
        if self.param.is_empty() {
            &self.name
        } else {
            &self.param
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub tag: String,
    pub match_kind: String,
    pub match_value: String,
    pub priority: i32,
}

impl Rule {
    pub fn new(tag: &str, match_kind: &str, match_value: &str) -> Self {
        Self {
            tag: tag.to_string(),
            match_kind: match_kind.to_string(),
            match_value: match_value.to_string(),
            priority: 0,
        }
    }

    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }
}

pub fn default_targets() -> Vec<Target> {
    vec![
        // No template: the default target sends the whole tag set as JSON, so a
        // consumer can read `session`, `team`, `feature` as fields.
        Target::new(DEFAULT_HEADER, "", "AOE_LLM_TAGS"),
        Target::new("X-AoE-Session", "{session_id}", "AOE_HDR_X_AOE_SESSION").disabled(),
    ]
}

/// Validated settings. Malformed rows are dropped with a warning, never fatal.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Tags every session carries, before any rule matches. After
    /// [`crate::session_tags::apply`] this starts with the session's own edits,
    /// then the repo file's pairs, then this list.
    pub tags: Vec<String>,
    /// How many leading [`Settings::tags`] entries are the session's own edits.
    /// They are exempt from [`TAG_CAP`]: an operator who types a tag for one
    /// session gets it even when the inherited set already fills the header.
    pub manual_tag_count: usize,
    /// A JSON file inside the session's project whose key/value pairs become
    /// tags; empty means "no file". The pair set is merged into `tags` when the
    /// snapshot is built, because only then is the project path known.
    pub tags_file: String,
    /// Where the tag set is delivered.
    pub targets: Vec<Target>,
    pub rules: Vec<Rule>,
    pub session_tagging: String,
    /// The operator's feature: one standard tag entry, not a second taxonomy.
    pub feature_tag: Option<Tag>,
    /// Per-session only: a literal standing in for what a template-less target
    /// would render. Never read from a settings file — see [`crate::session_tags`].
    pub token: String,
    /// Harnesses the user allows a harness-side extension in (`harness_extension`).
    /// Intent only: `src/harness_extension.rs` also requires a receipt for the
    /// exact file, so listing a harness here asks for the disclosure; it does not
    /// approve one.
    pub harness_extension: Vec<String>,
    /// The activity family a session starts in (`activity_family`). Per project,
    /// so the project's registry entry can replace it before this table is
    /// parsed; empty means "the built-in `default` family".
    pub activity_family: String,
    /// Activity rows from the settings UI (`activity_families`), in the flat
    /// `{family, value, label, icon, tone, when}` shape. Parsed and merged by
    /// [`crate::activity::resolve`], which owns the precedence order.
    pub activity_families: Vec<toml::Value>,
    /// A project-relative TOML file a repository declares its families in.
    pub activity_file: String,
    /// That file, once the project path is known; `None` means "not read".
    pub activity_repo: Option<toml::Table>,
    pub warnings: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            tags: Vec::new(),
            manual_tag_count: 0,
            tags_file: String::new(),
            targets: default_targets(),
            rules: Vec::new(),
            session_tagging: DEFAULT_SESSION_TAGGING.to_string(),
            feature_tag: None,
            token: String::new(),
            harness_extension: Vec::new(),
            activity_family: String::new(),
            activity_families: Vec::new(),
            activity_file: crate::activity::DEFAULT_FILE.to_string(),
            activity_repo: None,
            warnings: Vec::new(),
        }
    }
}

impl Settings {
    pub fn from_table(table: &toml::Table, warnings: Vec<String>) -> Self {
        let mut warns = warnings;
        let tags = parse_static_tags(table.get("tags"), &mut warns);
        let tags_file = parse_tags_file(table.get("tags_file"), &mut warns);
        let targets = parse_targets(table, &mut warns);
        let rules = parse_rules(table.get("rules"), &mut warns);

        let mut tagging = table
            .get("session_tagging")
            .map(|value| match value.as_str() {
                Some(text) => text.to_string(),
                None => render_toml_scalar(value),
            })
            .unwrap_or_else(|| DEFAULT_SESSION_TAGGING.to_string());
        if !SESSION_TAGGING_MODES.contains(&tagging.as_str()) {
            warns.push(format!(
                "settings: session_tagging = {tagging:?} is not one of {}; using {DEFAULT_SESSION_TAGGING}",
                SESSION_TAGGING_MODES.join(", ")
            ));
            tagging = DEFAULT_SESSION_TAGGING.to_string();
        }

        let feature_tag = parse_feature_tag(table.get("feature_tag"), &mut warns);

        Self {
            tags,
            manual_tag_count: 0,
            tags_file,
            targets,
            rules,
            session_tagging: tagging,
            feature_tag,
            token: String::new(),
            harness_extension: parse_name_list(
                table.get("harness_extension"),
                "harness_extension",
                &mut warns,
            ),
            activity_family: parse_activity_family(table.get("activity_family"), &mut warns),
            activity_families: parse_activity_families(table.get("activity_families"), &mut warns),
            activity_file: parse_activity_file(table.get("activity_file"), &mut warns),
            activity_repo: None,
            warnings: warns,
        }
    }
}

/// Per-field fallbacks; `None` means "not given", empty means "given empty".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Given {
    pub session: Option<String>,
    pub profile: Option<String>,
    pub tool: Option<String>,
    pub project: Option<String>,
    pub group: Option<String>,
    pub title: Option<String>,
    pub adapter: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub session: String,
    pub profile: String,
    pub tool: String,
    pub project: String,
    pub group: String,
    pub title: String,
    pub adapter: String,
}

impl Flags {
    pub fn from_given(given: &Given, env: &Env) -> Self {
        let mut flags = Flags::default();
        for (field, variable) in ENV_FALLBACKS {
            let value = given_value(given, field)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .or_else(|| env.get(variable).map(str::to_string))
                .unwrap_or_default();
            match field {
                "session" => flags.session = value,
                "profile" => flags.profile = value,
                "tool" => flags.tool = value,
                "project" => flags.project = value,
                "group" => flags.group = value,
                "title" => flags.title = value,
                _ => {}
            }
        }
        flags.adapter = given.adapter.clone().unwrap_or_default();
        flags
    }
}

fn given_value<'a>(given: &'a Given, field: &str) -> Option<&'a str> {
    match field {
        "session" => given.session.as_deref(),
        "profile" => given.profile.as_deref(),
        "tool" => given.tool.as_deref(),
        "project" => given.project.as_deref(),
        "group" => given.group.as_deref(),
        "title" => given.title.as_deref(),
        _ => None,
    }
}

/// What the session is, already resolved. Empty string = genuinely absent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionFacts {
    pub id: String,
    pub title: String,
    pub path: String,
    pub group: String,
    pub tool: String,
    pub profile: String,
    pub branch: String,
}

/// One configured tag, parsed once.
///
/// A tag is a key/value pair, not a word: `team:core` is `"team": "core"`.
/// Three shapes are accepted, so a value can grow past a plain string without
/// inventing a new settings syntax:
///
/// * `key:value`  — a string value
/// * `key:=json`  — any JSON value (`cost:=1.5`, `scope:=["a","b"]`)
/// * `key`        — `true`, for a tag that is just a marker
#[derive(Clone, Debug, PartialEq)]
pub struct Tag {
    pub key: String,
    pub value: Value,
}

impl Tag {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("the tag is empty".to_string());
        }
        let (key, rest) = match raw.split_once(':') {
            Some((key, rest)) => (key.trim(), Some(rest)),
            None => (raw, None),
        };
        if key.is_empty() {
            return Err(format!("tag {raw:?} has no key"));
        }
        if key.chars().any(|ch| ch.is_whitespace() || ch == '=') {
            return Err(format!("tag key {key:?} has spaces or `=` in it"));
        }
        let value = match rest {
            None => Value::Bool(true),
            Some(rest) if rest.starts_with('=') => {
                let text = rest[1..].trim();
                serde_json::from_str::<Value>(text)
                    .map_err(|err| format!("tag {raw:?} has invalid JSON after `:=` ({err})"))?
            }
            Some(rest) => Value::String(rest.to_string()),
        };
        Ok(Self {
            key: key.to_string(),
            value,
        })
    }

    /// The tag as one `csv` field, for the `csv` format and `{tags}`.
    ///
    /// A value is free text (`{"team":"core,platform"}` is a perfectly good entry
    /// from a repo's `tags_file`), so a field that would otherwise split the list
    /// is quoted and its own quotes doubled: `"team:core,platform"`. A consumer
    /// that splits on the comma alone still sees two fields — a format with an
    /// escape cannot save a parser that ignores it, and `json` is the format for
    /// those — but a CSV parser now reads the list the way it was written.
    pub fn csv(&self) -> String {
        let field = match &self.value {
            Value::Bool(true) => self.key.clone(),
            Value::String(text) => format!("{}:{text}", self.key),
            other => format!("{}:{other}", self.key),
        };
        if field
            .chars()
            .any(|ch| matches!(ch, ',' | '"' | '\n' | '\r'))
        {
            format!("\"{}\"", field.replace('"', "\"\""))
        } else {
            field
        }
    }

    /// The settings spelling of this same tag, so a value read from a JSON file
    /// can travel through the code that parses settings tags without losing its
    /// type: `team:core`, `platform` for a `true` marker, `cost:=1.5` otherwise.
    /// A string that starts with `=` takes the JSON form, because the plain form
    /// would read back as an `:=` entry and fail to parse.
    pub fn entry(&self) -> String {
        match &self.value {
            Value::String(text) if !text.starts_with('=') => {
                format!("{}:{text}", self.key)
            }
            Value::Bool(true) => self.key.clone(),
            other => format!("{}:={other}", self.key),
        }
    }
}

/// The JSON object every target can carry: session fields first, then tags.
pub type Payload = Vec<(String, Value)>;

/// The tag list, parsed. A tag that does not parse is reported and dropped, so
/// one typo never costs a session its tags.
pub fn parse_tags(raw: &[String]) -> (Vec<Tag>, Vec<String>) {
    let mut tags = Vec::new();
    let mut warnings = Vec::new();
    for row in raw {
        match Tag::parse(row) {
            Ok(tag) => tags.push(tag),
            Err(message) => warnings.push(format!("settings: {message}; dropping it")),
        }
    }
    (tags, warnings)
}

/// Read the JSON tags file a settings `tags_file` names, inside the session's
/// project. Every top-level key/value pair becomes a tag, with its JSON type
/// kept (`{"cost": 1.5}` is `cost:=1.5`, not the string `"1.5"`).
///
/// Failure is a warning and no tags, never an error: a hook that fails is worse
/// than a tag that is not applied, and `doctor` surfaces the warning.
pub fn load_tags_file(project_path: &str, configured: &str) -> (Vec<Tag>, Vec<String>) {
    let mut warnings = Vec::new();
    let configured = configured.trim();
    if configured.is_empty() {
        return (Vec::new(), warnings);
    }
    if project_path.trim().is_empty() {
        warnings.push(format!(
            "settings: tags_file {configured:?} is set, but this session has no project path; \
             no tags loaded"
        ));
        return (Vec::new(), warnings);
    }

    let resolved = match resolve_project_file(
        project_path,
        configured,
        "tags_file",
        "no tags loaded",
        &mut warnings,
    ) {
        Some(resolved) => resolved,
        None => return (Vec::new(), warnings),
    };
    let path = Path::new(project_path).join(configured);

    // Read through the cap instead of trusting `metadata().len()`: a growing file
    // reports the length it had a moment ago, and this is the read that must be
    // bounded. `take` stops one byte past the cap, which is what "too large" is.
    let raw = match read_capped(&resolved, TAGS_FILE_MAX_BYTES) {
        Ok(bytes) if bytes.len() as u64 > TAGS_FILE_MAX_BYTES => {
            warnings.push(format!(
                "settings: tags_file {} is larger than {TAGS_FILE_MAX_BYTES} bytes; no tags loaded",
                path.display()
            ));
            return (Vec::new(), warnings);
        }
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(raw) => raw,
            Err(_) => {
                warnings.push(format!(
                    "settings: tags_file {} is not valid UTF-8; no tags loaded",
                    path.display()
                ));
                return (Vec::new(), warnings);
            }
        },
        Err(err) => {
            warnings.push(format!(
                "settings: tags_file {}: cannot be read ({err}); no tags loaded",
                path.display()
            ));
            return (Vec::new(), warnings);
        }
    };
    let value = match serde_json::from_str::<Value>(&raw) {
        Ok(value) => value,
        Err(err) => {
            warnings.push(format!(
                "settings: tags_file {} is not valid JSON ({err}); no tags loaded",
                path.display()
            ));
            return (Vec::new(), warnings);
        }
    };
    let Some(map) = value.as_object() else {
        warnings.push(format!(
            "settings: tags_file {} must be a JSON object of key/value pairs; no tags loaded",
            path.display()
        ));
        return (Vec::new(), warnings);
    };

    let mut tags = Vec::new();
    for (key, value) in map {
        let readable = valid_tag(key) && !key.contains(':') && !key.contains('=');
        if !readable {
            warnings.push(format!(
                "settings: tags_file {} key {key:?} is not a valid tag key \
                 (starts [a-z0-9], printable, no `:` or `=`, ≤{TAG_MAX_CHARS} chars); skipping it",
                path.display()
            ));
            continue;
        }
        tags.push(Tag {
            key: key.clone(),
            value: sanitize_file_value(value.clone(), &path, key, &mut warnings),
        });
    }
    (tags, warnings)
}

/// Resolve `<project>/<configured>` to a file this launch may read.
///
/// The path comes from a setting and the file from a repository, so both are
/// untrusted: the file must stay inside the project (a symlink is how a relative
/// path leaves the tree without ever containing `..`, so the canonical target is
/// checked), it must be a regular file, and the cap is applied to the read rather
/// than to what `metadata` claims. `None` means the caller loads nothing;
/// `what`/`tail` only shape the warning a user reads.
fn resolve_project_file(
    project_path: &str,
    configured: &str,
    what: &str,
    tail: &str,
    warnings: &mut Vec<String>,
) -> Option<PathBuf> {
    let relative = Path::new(configured);
    let escapes = relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        });
    if escapes {
        warnings.push(format!(
            "settings: {what} {configured:?} must be a relative path inside the project \
             ({project_path}); ignoring it"
        ));
        return None;
    }

    let path = Path::new(project_path).join(relative);

    // Existence first: a file that is not there costs no open and no read.
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {
            warnings.push(format!(
                "settings: {what} {} does not exist in {}; {tail}",
                configured,
                Path::new(project_path).display()
            ));
            return None;
        }
        Err(err) => {
            warnings.push(format!(
                "settings: {what} {}: cannot be read ({err}); {tail}",
                path.display()
            ));
            return None;
        }
    }

    let root = match std::fs::canonicalize(project_path) {
        Ok(root) => root,
        Err(err) => {
            warnings.push(format!(
                "settings: {what} {}: the project {} cannot be resolved ({err}); {tail}",
                configured,
                Path::new(project_path).display()
            ));
            return None;
        }
    };
    let resolved = match std::fs::canonicalize(&path) {
        Ok(resolved) => resolved,
        Err(err) => {
            warnings.push(format!(
                "settings: {what} {}: cannot be resolved ({err}); {tail}",
                path.display()
            ));
            return None;
        }
    };
    if !resolved.starts_with(&root) {
        warnings.push(format!(
            "settings: {what} {} resolves to {}, which is not inside the project {}; {tail}",
            configured,
            resolved.display(),
            root.display()
        ));
        return None;
    }
    match std::fs::metadata(&resolved) {
        Ok(meta) if meta.is_file() => Some(resolved),
        Ok(_) => {
            warnings.push(format!(
                "settings: {what} {} is not a regular file; {tail}",
                path.display()
            ));
            None
        }
        Err(err) => {
            warnings.push(format!(
                "settings: {what} {}: cannot be read ({err}); {tail}",
                path.display()
            ));
            None
        }
    }
}

/// Read the TOML activity file a settings `activity_file` names, inside the
/// session's project.
///
/// The file is optional in every repository — the built-in family and the
/// settings are enough — so a missing file is silent, unlike a missing
/// `tags_file`. A malformed file, one that leaves the project, or one over the
/// cap is a warning and no families, never an error: a launch must not fail over
/// a declaration.
pub fn load_activity_file(
    project_path: &str,
    configured: &str,
) -> (Option<toml::Table>, Vec<String>) {
    let mut warnings = Vec::new();
    let configured = configured.trim();
    if configured.is_empty() || project_path.trim().is_empty() {
        return (None, warnings);
    }
    let path = Path::new(project_path).join(configured);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        // Optional: a repository that declares nothing costs no warning.
        Err(err) if err.kind() == ErrorKind::NotFound => return (None, warnings),
        Err(_) => {}
    }
    let resolved = match resolve_project_file(
        project_path,
        configured,
        "activity_file",
        "no activity families loaded",
        &mut warnings,
    ) {
        Some(resolved) => resolved,
        None => return (None, warnings),
    };
    let raw = match read_capped(&resolved, TAGS_FILE_MAX_BYTES) {
        Ok(bytes) if bytes.len() as u64 > TAGS_FILE_MAX_BYTES => {
            warnings.push(format!(
                "settings: activity_file {} is larger than {TAGS_FILE_MAX_BYTES} bytes; \
                 no activity families loaded",
                path.display()
            ));
            return (None, warnings);
        }
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(raw) => raw,
            Err(_) => {
                warnings.push(format!(
                    "settings: activity_file {} is not valid UTF-8; no activity families loaded",
                    path.display()
                ));
                return (None, warnings);
            }
        },
        Err(err) => {
            warnings.push(format!(
                "settings: activity_file {}: cannot be read ({err}); no activity families loaded",
                path.display()
            ));
            return (None, warnings);
        }
    };
    match raw.parse::<toml::Table>() {
        Ok(table) => (Some(table), warnings),
        Err(err) => {
            warnings.push(format!(
                "settings: activity_file {} is not valid TOML ({err}); \
                 no activity families loaded",
                path.display()
            ));
            (None, warnings)
        }
    }
}

/// Read at most one byte past `limit`, so the caller can tell "exactly at the
/// cap" from "over it" without reading the rest. The file is already known to be
/// a regular file inside the project; the cap is still applied here because this
/// is the call that would block or grow.
fn read_capped(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.by_ref().take(limit + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Clean one value from the repository's file before it can reach a header.
///
/// The file is repository content, so it is sanitised here and not only by the
/// resolver's clamp later: every string is passed through the same sanitiser the
/// resolver uses — control characters dropped and surrounding whitespace trimmed
/// — at every depth (a newline in a value is what splits a `csv` list or a
/// `KEY=VALUE` line), and a top-level *string* is additionally trimmed to
/// [`TAGS_FILE_STRING_MAX_CHARS`]. A list or an object keeps its type and its
/// length — it is a structure, not text, and the file cap already bounds it.
fn sanitize_file_value(value: Value, path: &Path, key: &str, warnings: &mut Vec<String>) -> Value {
    let mut value = sanitize_json_strings(value);
    if let Value::String(text) = &value {
        let trimmed = truncate_chars(text, TAGS_FILE_STRING_MAX_CHARS);
        if trimmed != *text {
            warnings.push(format!(
                "settings: tags_file {} key {key:?} is longer than \
                 {TAGS_FILE_STRING_MAX_CHARS} characters; trimmed",
                path.display()
            ));
            value = Value::String(trimmed);
        }
    }
    value
}

/// The same sanitiser the resolver applies to a rendered value, at every depth:
/// a nested string reaches the header inside the payload JSON just as a top-level
/// one does.
fn sanitize_json_strings(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(sanitize_value(&text)),
        Value::Array(items) => Value::Array(items.into_iter().map(sanitize_json_strings).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (sanitize_value(&key), sanitize_json_strings(value)))
                .collect(),
        ),
        other => other,
    }
}

/// The first `limit` characters — a character count, like [`TAG_MAX_CHARS`], so a
/// multi-byte value is never cut mid-character.
pub fn truncate_chars(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => text[..end].to_string(),
        None => text.to_string(),
    }
}

/// Fields [`payload`] derives from the session. A tag or a `feature_tag` naming
/// one of these is dropped rather than allowed to rewrite the session's identity.
const SESSION_FIELDS: [&str; 8] = [
    "session",
    "session_id",
    "tool",
    "profile",
    "group",
    "project",
    "project_path",
    "branch",
];

/// The sample "standard git information" set: what a consumer usually wants in
/// order to attribute a request to a revision.
///
/// It is ordinary tag grammar — four entries with `{git.xxx}` references as
/// values — so it needs no new setting, and a user can drop or edit any entry
/// afterwards. Ticking "Add standard git information" in the editor and
/// `aoe-llm-tags tag --git` write exactly this list, from this one constant.
pub const GIT_PRESET: [&str; 4] = [
    "vcs:git",
    "git.branch:{git.branch}",
    "git.commit:{git.commit}",
    "git.tag:{git.tag}",
];

/// One entry in the reference catalogue the editor's dropdown and the
/// `references` verb list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reference {
    /// Without braces: `session` is written `{session}`.
    pub name: &'static str,
    /// The heading the dropdown groups it under.
    pub group: &'static str,
    /// One line, in the settings' voice.
    pub description: &'static str,
}

/// The session and tag-shape references. The git ones come from
/// [`crate::gitinfo::REFERENCES`], so there is one source for each.
pub const SESSION_REFERENCES: [Reference; 15] = [
    Reference {
        name: "session",
        group: "Session",
        description: "Short session id (`S-9f3a`), or the full id when `session_tagging` says so.",
    },
    Reference {
        name: "session_id",
        group: "Session",
        description: "Full session id.",
    },
    Reference {
        name: "feature",
        group: "Session",
        description: "The operator's feature: a per-project `feature_tag`, or a `feature:` tag.",
    },
    Reference {
        name: "tool",
        group: "Session",
        description: "The harness the session runs (pi, claude, codex, opencode).",
    },
    Reference {
        name: "profile",
        group: "Session",
        description: "The aoe profile the session was launched with.",
    },
    Reference {
        name: "group",
        group: "Session",
        description: "The session's group name, as written.",
    },
    Reference {
        name: "group_slug",
        group: "Session",
        description: "The group name slugged for use as a dimension.",
    },
    Reference {
        name: "project",
        group: "Session",
        description: "The project path (the payload's `project` is its slug).",
    },
    Reference {
        name: "project_slug",
        group: "Session",
        description: "The project directory name, slugged: `lithuanians`.",
    },
    Reference {
        name: "branch",
        group: "Session",
        description:
            "The branch the session manager reports; `{git.branch}` reads it from git itself.",
    },
    Reference {
        name: "tags",
        group: "Tags",
        description: "The tag set as a flat list: `team:core,feature:billing`.",
    },
    Reference {
        name: "tags_csv",
        group: "Tags",
        description: "The same flat list, under the name older templates use.",
    },
    Reference {
        name: "tags_json",
        group: "Tags",
        description: "The tags alone, as one JSON object.",
    },
    Reference {
        name: "payload_json",
        group: "Tags",
        description: "The whole payload — session fields and tags — as JSON.",
    },
    Reference {
        name: "tags_b64",
        group: "Tags",
        description: "The payload, base64url-encoded, for a harness that splits on commas.",
    },
];

/// Everything a template or a tag value may name, session fields first, then git.
pub fn reference_catalog() -> Vec<Reference> {
    let mut all = SESSION_REFERENCES.to_vec();
    all.extend(crate::gitinfo::REFERENCES.iter().map(|item| Reference {
        name: item.name,
        group: "Git",
        description: item.description,
    }));
    all
}

/// The operator's feature, spelled as one standard tag entry.
///
/// `feature_tag` is a single tag, not a second taxonomy, so it accepts the shapes
/// a `tags` entry does — plus the bare word older settings files hold:
///
/// * `feature:billing` — a normal entry, with any key
/// * `billing` — the same as `feature:billing` (the key was implied, so it stays implied)
/// * `{ key = "epic", value = "checkout" }` — an explicit pair, the shape a
///   per-project settings form stores when it keeps the key and value apart
/// * `["feature:billing"]` — a one-entry list, the shape a `tags` field has
///
/// The result is a [`Tag`], so every level that can set this — the global config,
/// a profile, a project — is parsed, validated and delivered by the same code as
/// every other tag.
pub fn parse_feature_tag(value: Option<&toml::Value>, warnings: &mut Vec<String>) -> Option<Tag> {
    match value? {
        toml::Value::String(raw) => feature_from_entry(raw, warnings),
        toml::Value::Array(items) => {
            let mut parsed: Option<Tag> = None;
            for item in items {
                let tag = match item {
                    toml::Value::String(raw) => feature_from_entry(raw, warnings),
                    other => feature_from_pair(other, warnings),
                };
                if tag.is_none() {
                    continue;
                }
                if parsed.is_some() {
                    warnings.push(
                        "settings: feature_tag is a list; only its first entry is used".to_string(),
                    );
                    break;
                }
                parsed = tag;
            }
            parsed
        }
        other => feature_from_pair(other, warnings),
    }
}

fn feature_from_entry(raw: &str, warnings: &mut Vec<String>) -> Option<Tag> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if !valid_tag(raw) {
        warnings.push(format!(
            "settings: feature_tag {raw:?} is not a valid tag \
             (starts [a-z0-9], printable, ≤{TAG_MAX_CHARS} chars); ignoring it"
        ));
        return None;
    }
    if raw.contains(':') {
        return match Tag::parse(raw) {
            Ok(tag) => Some(tag),
            Err(message) => {
                warnings.push(format!("settings: {message}; ignoring feature_tag"));
                None
            }
        };
    }
    Some(Tag {
        key: "feature".to_string(),
        value: Value::String(raw.to_string()),
    })
}

fn feature_from_pair(value: &toml::Value, warnings: &mut Vec<String>) -> Option<Tag> {
    let table = match value {
        toml::Value::Table(table) => table,
        other => {
            warnings.push(format!(
                "settings: feature_tag {other:?} is not a tag entry \
                 (`key:value`, a bare word, or {{ key = …, value = … }}); ignoring it"
            ));
            return None;
        }
    };
    let key = table
        .get("key")
        .and_then(toml::Value::as_str)
        .unwrap_or("feature")
        .trim()
        .to_string();
    if !valid_tag(&key) {
        warnings.push(format!(
            "settings: feature_tag key {key:?} is not a valid tag key; ignoring it"
        ));
        return None;
    }
    let Some(raw) = table.get("value") else {
        // No value is a marker, the same as a bare entry in `tags`.
        return Some(Tag {
            key,
            value: Value::Bool(true),
        });
    };
    match serde_json::to_value(raw) {
        Ok(value) => Some(Tag { key, value }),
        Err(err) => {
            warnings.push(format!(
                "settings: feature_tag value {raw:?} is not representable ({err}); ignoring it"
            ));
            None
        }
    }
}

/// `{feature}` is the operator's feature, not merely the first tag: the setting
/// wins, else the first `feature:<name>` rule, else nothing.
///
/// The setting only answers `{feature}` when it is *the* `feature` entry. A pair
/// with another key (`epic:checkout`) renames the field rather than leaving the
/// payload's `feature` field and `{feature}` pointing at different values.
pub fn feature_of(settings: &Settings, tags: &[Tag]) -> String {
    if let Some(tag) = &settings.feature_tag {
        if tag.key == "feature" {
            return match &tag.value {
                Value::String(name) => name.clone(),
                other => other.to_string(),
            };
        }
    }
    tags.iter()
        .find_map(|tag| {
            if tag.key != "feature" {
                return None;
            }
            match &tag.value {
                Value::String(name) if !name.is_empty() => Some(name.clone()),
                _ => None,
            }
        })
        .unwrap_or_default()
}

/// The tag set as one object, without the session fields.
pub fn tags_json(tags: &[Tag]) -> String {
    let mut map = serde_json::Map::new();
    for tag in tags {
        map.insert(tag.key.clone(), tag.value.clone());
    }
    Value::Object(map).to_string()
}

/// Base64url without padding: the same bytes as the input, spelled with a
/// 64-symbol alphabet that cannot contain a comma.
///
/// A harness that splits one environment variable into several headers on
/// `,(?=name:)` — `GEMINI_CLI_CUSTOM_HEADERS` does — cannot carry a JSON payload
/// verbatim, because `{"a":1,"b":2}` looks like two headers. `{tags_b64}` is
/// the whole payload in a spelling no parser can mistake for a second header,
/// and a consumer decodes it with a two-line base64url call.
pub fn base64url(input: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(triple >> 6) as usize & 0x3f] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[triple as usize & 0x3f] as char);
        }
    }
    out
}

/// Fields that are *built from* the tags, so a tag value cannot reference them
/// without asking for a value that does not exist yet. `feature` is among them
/// because a `feature:` tag is one of the two things it can come from.
const TAG_DERIVED_FIELDS: [&str; 6] = [
    "feature",
    "tags",
    "tags_csv",
    "tags_json",
    "payload_json",
    "tags_b64",
];

/// Expand `{reference}` inside a tag's value, recursively for JSON values.
///
/// A tag is a key and a value, and the value may be a reference:
/// `git.branch:{git.branch}` makes the branch part of the tag set itself, so a
/// JSON consumer reads `tags["git.branch"]` instead of the operator writing a
/// template for one target. The session fields and the git facts are available;
/// the tag-derived fields are not, and saying so is better than an empty tag that
/// looks like a real answer.
fn expand_tag_value(
    value: &Value,
    fields: &BTreeMap<String, String>,
    key: &str,
    warnings: &mut Vec<String>,
) -> Value {
    match value {
        Value::String(text) => {
            let (rendered, missing) = render_template(text, fields);
            for name in missing {
                if TAG_DERIVED_FIELDS.contains(&name.as_str()) {
                    warnings.push(format!(
                        "settings: tag {key:?} cannot reference {{{name}}}: that field is \
                         built from the tags themselves; it renders empty"
                    ));
                } else {
                    warnings.push(format!(
                        "settings: tag {key:?} uses unknown reference {{{name}}}; it renders empty"
                    ));
                }
            }
            Value::String(rendered)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| expand_tag_value(item, fields, key, warnings))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(name, item)| (name.clone(), expand_tag_value(item, fields, key, warnings)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The git references this launch names: enabled target templates, the tag values
/// and the feature tag, each once. Empty means no `git` process is worth starting.
pub fn wanted_git_references(settings: &Settings, tags: &[Tag]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for header in enabled_targets(settings) {
        note_git_references(referenced_names(&header.template), &mut names);
    }
    for tag in tags {
        note_git_references(referenced_names_in_value(&tag.value), &mut names);
    }
    if let Some(tag) = &settings.feature_tag {
        note_git_references(referenced_names_in_value(&tag.value), &mut names);
    }
    names
}

/// Collect the git references a template or a tag value names, once each.
fn note_git_references(names: Vec<String>, into: &mut Vec<String>) {
    for name in names {
        if crate::gitinfo::is_reference(&name) && !into.contains(&name) {
            into.push(name);
        }
    }
}

/// Build what a consumer receives: the session fields every target needs, then
/// the tags. Tags with the same key as a field are reported and dropped rather
/// than allowed to rewrite the session's identity.
pub fn payload(
    settings: &Settings,
    facts: &SessionFacts,
    tags: &[Tag],
    branch: &str,
) -> (Payload, Vec<String>) {
    let mut warnings = Vec::new();
    let mut payload: Payload = Vec::new();
    let mut push = |key: &str, value: String| {
        if !value.is_empty() {
            payload.push((key.to_string(), Value::String(value)));
        }
    };
    push("session", short_id(&facts.id, &settings.session_tagging));
    push("session_id", facts.id.clone());
    push("tool", facts.tool.clone());
    push("profile", facts.profile.clone());
    push("group", facts.group.clone());
    let project = project_slug(&facts.path);
    push(
        "project",
        if project.is_empty() {
            facts.path.clone()
        } else {
            project
        },
    );
    push("project_path", facts.path.clone());
    push("branch", branch.to_string());
    for tag in tags {
        if payload.iter().any(|(key, _)| *key == tag.key) {
            warnings.push(format!(
                "settings: tag {:?} names a session field; the field wins, so the tag is dropped",
                tag.key
            ));
            continue;
        }
        payload.push((tag.key.clone(), tag.value.clone()));
    }
    // The operator's feature is one more tag, delivered like any other, and
    // written last so it can override a `feature:<name>` entry from `tags`. It
    // cannot rewrite a session field, for the same reason a tag cannot.
    if let Some(tag) = &settings.feature_tag {
        if SESSION_FIELDS.contains(&tag.key.as_str()) {
            warnings.push(format!(
                "settings: feature_tag {:?} names a session field; the field wins, so it is dropped",
                tag.key
            ));
        } else {
            match payload.iter_mut().find(|(key, _)| *key == tag.key) {
                Some((_, value)) => *value = tag.value.clone(),
                None => payload.push((tag.key.clone(), tag.value.clone())),
            }
        }
    }
    (payload, warnings)
}

/// The payload as a compact JSON object — what a `json` target sends.
pub fn payload_json(payload: &Payload) -> String {
    let mut map = serde_json::Map::new();
    for (key, value) in payload {
        map.insert(key.clone(), value.clone());
    }
    Value::Object(map).to_string()
}

/// The value one target sends: its own template when it has one, else the
/// configured format. Unknown fields in a template are returned for reporting.
pub fn target_value(
    target: &Target,
    payload: &Payload,
    fields: &BTreeMap<String, String>,
) -> (String, Vec<String>) {
    if !target.template.trim().is_empty() {
        return render_template(&target.template, fields);
    }
    if target.format == "csv" {
        return (fields.get("tags").cloned().unwrap_or_default(), Vec::new());
    }
    (payload_json(payload), Vec::new())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedHeader {
    pub header: String,
    pub env_var: String,
    pub value: String,
}

/// One resolution pass: what the session is, what settings applied, what came out.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub facts: SessionFacts,
    pub settings: Settings,
    /// The activity families this launch resolved, and the one it starts in. The
    /// hook pins it into the session directory; a mid-session read prefers the
    /// pin (`crate::activity::for_session`).
    pub families: crate::activity::Families,
    /// The tags as they are sent: matched, parsed, and with every `{git.xxx}`
    /// reference in a value already expanded. `settings.tags` still holds the
    /// entries as stored, which is what an editor has to show.
    pub tags: Vec<Tag>,
    pub resolved: Vec<ResolvedHeader>,
    pub warnings: Vec<String>,
    pub raw: toml::Table,
}

/// An injectable `aoe` call. The tests supply one; production supplies
/// [`default_runner`].
pub type Runner<'a> = &'a dyn Fn(&[String]) -> Result<Value, AoeError>;

// --- aoe subprocess ---------------------------------------------------------

/// `AOE_BIN` → `which aoe` → the documented fallback (parameters §2).
pub fn aoe_bin(env: &Env) -> String {
    env.non_empty("AOE_BIN")
        .map(str::to_string)
        .or_else(|| which_aoe(env))
        .unwrap_or_else(|| AOE_FALLBACK.to_string())
}

fn which_aoe(env: &Env) -> Option<String> {
    let path = match env.non_empty("PATH") {
        Some(path) => path.to_string(),
        None => std::env::var("PATH").unwrap_or_default(),
    };
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = Path::new(dir).join("aoe");
        if is_executable(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

pub fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// How much of a child's output is kept. A lookup that answers with more than
/// this is not answering the question that was asked, and pulling it into memory
/// without a bound is how one wedged subprocess takes the launch down with it.
pub const OUTPUT_MAX_BYTES: usize = 4 * 1024 * 1024;

/// What the drains get to hand over once the child is gone.
///
/// A child that exits closes its own pipes, so a successful lookup spends none of
/// this. It is only ever paid when a *grandchild* still holds the write end open —
/// the case that used to hold the launch past its budget, because the reader
/// thread could not finish and the caller joined it.
const DRAIN_GRACE: Duration = Duration::from_millis(250);

/// Read one pipe to the end, keeping at most [`OUTPUT_MAX_BYTES`] of it.
///
/// Past the cap the bytes are read and dropped rather than left in the pipe: a
/// child blocked on a full pipe never exits, and the wait loop would then spend
/// the whole budget on a child that is only waiting for us.
fn drain(mut stream: impl Read, done: mpsc::Sender<Vec<u8>>) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8 * 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                if buf.len() < OUTPUT_MAX_BYTES {
                    let room = OUTPUT_MAX_BYTES - buf.len();
                    buf.extend_from_slice(&chunk[..read.min(room)]);
                }
            }
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    let _ = done.send(buf);
}

/// Run one read-only subprocess and return its stdout as text. Never blocks long.
///
/// Shared by the `aoe` query and the `git` facts: both are read-only lookups the
/// launch can survive without, so both are bounded the same way and both report
/// a failure as [`AoeError`] rather than panicking. `verb` is only the label a
/// warning prints (`aoe session show`, `git rev-parse`).
pub fn run_text(
    bin: &str,
    verb: &str,
    args: &[String],
    timeout: Duration,
) -> Result<String, AoeError> {
    run_text_clean(bin, verb, args, &[], timeout)
}

/// [`run_text`], removing `drop` from the child's environment.
///
/// The launch environment is the host's, not the command's: a `GIT_DIR` inherited
/// from it would send `git -C <session>` to a repository the session does not own,
/// whatever the git facts checked before spawning.
pub fn run_text_clean(
    bin: &str,
    verb: &str,
    args: &[String],
    drop: &[&str],
    timeout: Duration,
) -> Result<String, AoeError> {
    let mut command = std::process::Command::new(bin);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for name in drop {
        command.env_remove(name);
    }
    let mut child = command.spawn().map_err(|source| AoeError::Spawn {
        bin: bin.to_string(),
        source,
    })?;

    // Both pipes are drained on their own threads: `aoe list --json` can exceed
    // the pipe buffer, and a child blocked on a full pipe never exits, so reading
    // only after `try_wait` would turn a slow query into a guaranteed timeout.
    //
    // The threads are detached and report through a channel, not joined: joining
    // a reader whose pipe a grandchild still holds is exactly the wait the budget
    // exists to avoid, and it made the timeout advisory rather than real.
    let (out_tx, out_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();
    if let Some(stream) = child.stdout.take() {
        std::thread::spawn(move || drain(stream, out_tx));
    }
    if let Some(stream) = child.stderr.take() {
        std::thread::spawn(move || drain(stream, err_tx));
    }

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AoeError::Spawn {
                    bin: bin.to_string(),
                    source: err,
                });
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            timed_out = true;
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    // The decision is made at the deadline; the drains only get a grace period to
    // hand over what they have already read. A reader still blocked when it runs
    // out is left behind — it exits by itself if the pipe ever closes, and its
    // buffer is bounded either way.
    let drain_deadline = Instant::now() + DRAIN_GRACE;
    let out_buf = out_rx
        .recv_timeout(drain_deadline.saturating_duration_since(Instant::now()))
        .unwrap_or_default();
    let err_buf = err_rx
        .recv_timeout(drain_deadline.saturating_duration_since(Instant::now()))
        .unwrap_or_default();

    if timed_out || status.is_none() {
        return Err(AoeError::TimedOut {
            bin: bin.to_string(),
            verb: verb.to_string(),
            timeout: timeout.as_secs_f64(),
        });
    }
    let status = status.unwrap();
    if !status.success() {
        let detail = String::from_utf8_lossy(&err_buf)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        return Err(AoeError::Failed {
            bin: bin.to_string(),
            verb: verb.to_string(),
            code: status.code().unwrap_or(-1),
            detail: if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            },
        });
    }
    Ok(String::from_utf8_lossy(&out_buf).into_owned())
}

/// Run one read-only `aoe` command and parse its JSON. Never blocks long.
pub fn aoe_json(args: &[String], env: &Env, timeout: Duration) -> Result<Value, AoeError> {
    let bin = aoe_bin(env);
    let verb = args.first().cloned().unwrap_or_default();
    let text = run_text(&bin, &verb, args, timeout)?;
    serde_json::from_str(&text).map_err(|err| AoeError::InvalidJson {
        bin,
        verb,
        detail: err.to_string(),
    })
}

pub fn default_runner<'a>(
    env: &'a Env,
    timeout: Duration,
) -> impl Fn(&[String]) -> Result<Value, AoeError> + 'a {
    move |args| aoe_json(args, env, timeout)
}

// --- session facts ----------------------------------------------------------

/// Row → flags → defaults. A failed lookup is a warning, never an error.
pub fn resolve_session(
    flags: &Flags,
    runner: Option<Runner<'_>>,
    env: &Env,
    timeout: Duration,
) -> (SessionFacts, Vec<String>) {
    let mut warnings = Vec::new();
    let mut row: Option<Value> = None;
    let owned;
    let runner: Runner<'_> = match runner {
        Some(runner) => runner,
        None => {
            owned = default_runner(env, timeout);
            &owned
        }
    };

    if !flags.session.is_empty() {
        let mut argv = vec![
            "session".to_string(),
            "show".to_string(),
            flags.session.clone(),
        ];
        if !flags.profile.is_empty() {
            argv.push("-p".to_string());
            argv.push(flags.profile.clone());
        }
        argv.push("--json".to_string());

        match runner(&argv) {
            Ok(Value::Object(map)) => row = Some(Value::Object(map)),
            Ok(_) => warnings.push(
                "`aoe session show` did not return a JSON object; using the hook environment"
                    .to_string(),
            ),
            Err(err) => warnings.push(format!(
                "{err}; falling back to the hook environment (group/title will be empty)"
            )),
        }
    }

    let row = row.as_ref().and_then(Value::as_object);
    let pick = |key: &str| -> String {
        row.and_then(|map| map.get(key))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .unwrap_or_default()
            .to_string()
    };

    let facts = SessionFacts {
        id: first(&[pick("id"), flags.session.clone()]),
        title: first(&[pick("title"), flags.title.clone()]),
        path: first(&[pick("path"), flags.project.clone()]),
        group: first(&[pick("group"), flags.group.clone()]),
        tool: first(&[pick("tool"), flags.tool.clone(), DEFAULT_TOOL.to_string()]),
        profile: first(&[pick("profile"), flags.profile.clone()]),
        branch: String::new(),
    };
    (facts, warnings)
}

/// `{branch}` needs `aoe list --json`; ask only when a template wants it.
pub fn resolve_branch(
    facts: &SessionFacts,
    runner: Option<Runner<'_>>,
    env: &Env,
    timeout: Duration,
) -> (String, Vec<String>) {
    if facts.id.is_empty() {
        return (String::new(), Vec::new());
    }
    let owned;
    let runner: Runner<'_> = match runner {
        Some(runner) => runner,
        None => {
            owned = default_runner(env, timeout);
            &owned
        }
    };
    let argv = vec!["list".to_string(), "--json".to_string()];
    let rows = match runner(&argv) {
        Ok(rows) => rows,
        Err(err) => {
            return (
                String::new(),
                vec![format!("{err}; {{branch}} renders empty")],
            )
        }
    };
    let Some(rows) = rows.as_array() else {
        return (
            String::new(),
            vec!["`aoe list --json` did not return a list; {branch} renders empty".to_string()],
        );
    };
    for row in rows {
        let Some(map) = row.as_object() else { continue };
        if map.get("id").and_then(Value::as_str) != Some(facts.id.as_str()) {
            continue;
        }
        if let Some(branch) = map
            .get("worktree")
            .and_then(Value::as_object)
            .and_then(|worktree| worktree.get("branch"))
            .and_then(Value::as_str)
        {
            return (branch.to_string(), Vec::new());
        }
    }
    (String::new(), Vec::new())
}

// --- fields and rendering ---------------------------------------------------

/// `short_id` → `S-` + the first 4 characters; derivable from `aoe list`.
pub fn short_id(session_id: &str, mode: &str) -> String {
    if session_id.is_empty() || mode == "none" {
        return String::new();
    }
    if mode == "full_id" {
        return session_id.to_string();
    }
    let prefix: String = session_id.chars().take(SHORT_ID_CHARS).collect();
    format!("{SHORT_ID_PREFIX}{prefix}")
}

pub fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut pending_sep = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_sep && !out.is_empty() {
                out.push('-');
            }
            pending_sep = false;
            out.push(ch.to_ascii_lowercase());
        } else {
            pending_sep = true;
        }
    }
    out
}

pub fn project_slug(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    Path::new(path)
        .file_name()
        .map(|name| slug(&name.to_string_lossy()))
        .unwrap_or_default()
}

/// `X-AoE-Session` → `AOE_HDR_X_AOE_SESSION` (parameters §3).
pub fn derive_env_var(header: &str) -> String {
    derive_env_var_for(DEFAULT_KIND, header)
}

/// The minted variable name for one target; `AOE_HDR_`, `AOE_QRY_`, `AOE_BODY_`.
pub fn derive_env_var_for(kind: &str, name: &str) -> String {
    let mut out = String::from(match kind {
        "query" => ENV_PREFIX_QUERY,
        "body" => ENV_PREFIX_BODY,
        _ => ENV_PREFIX,
    });
    let mut pending = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending && !out.ends_with('_') {
                out.push('_');
            }
            pending = false;
            out.push(ch.to_ascii_uppercase());
        } else {
            pending = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

pub fn valid_tag(tag: &str) -> bool {
    let mut chars = tag.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    // A value may be JSON (`cost:=1.5`, `scope:=["api","ui"]`), so any printable
    // non-space character is allowed after the key. Quotes and backslashes stay
    // out: they make a value unreadable in a header and unquotable in `csv`.
    if !chars.all(|ch| ch.is_ascii_graphic() && !matches!(ch, '"' | '\\' | '\'')) {
        return false;
    }
    tag.chars().count() <= TAG_MAX_CHARS
}

pub fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
        && name.len() <= 64
}

/// An environment variable name: a letter or `_`, then letters, digits or `_`.
///
/// The hook prints `NAME=value` lines and the mapping line for a `custom_headers_env`
/// harness is a shell command that reads `$NAME`, so a name is not free text: a
/// `;` or a newline in it would turn one exported variable into a command.
pub fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// Every matching rule contributes; one tag per key, then cap the inherited tags
/// at 5 (parameters §5). A session's own edits are always carried, so the total
/// may be `TAG_CAP + settings.manual_tag_count`.
pub fn matched_tags(settings: &Settings, facts: &SessionFacts) -> Vec<String> {
    let slugged = project_slug(&facts.path);
    // Stable sort by priority (descending); equal priorities keep list order.
    let mut ordered: Vec<&Rule> = settings.rules.iter().collect();
    ordered.sort_by_key(|a| std::cmp::Reverse(a.priority));
    // Settings `tags` come first — a session edit, then the repo file, then the
    // global list — so the budget is spent in that priority order. A rule adds a
    // dimension; it never rewrites a key the operator already set, even under a
    // different value.
    let cap = TAG_CAP + settings.manual_tag_count;
    let mut tags: Vec<String> = settings.tags.clone();
    tags.truncate(cap);
    let mut keys: Vec<String> = tags
        .iter()
        .filter_map(|raw| Tag::parse(raw).ok())
        .map(|tag| tag.key)
        .collect();
    for rule in ordered {
        if tags.len() == cap {
            break;
        }
        if !rule_matches(rule, facts, &slugged) {
            continue;
        }
        let Ok(tag) = Tag::parse(&rule.tag) else {
            // `parse_rules` already warned when this rule was loaded.
            continue;
        };
        if keys.contains(&tag.key) {
            continue;
        }
        keys.push(tag.key);
        tags.push(rule.tag.clone());
    }
    tags
}

/// M0 kinds only: `project` and `manual` (parameters §3).
///
/// `project` compares against the project slug (basename) or the full path;
/// `manual` is an operator-written unconditional tag, which is what M0 can
/// honour before manual tags are assignable in the UI (M3-T06).
pub fn rule_matches(rule: &Rule, facts: &SessionFacts, slugged_project: &str) -> bool {
    match rule.match_kind.as_str() {
        "manual" => true,
        "project" => {
            if rule.match_value.is_empty() {
                return false;
            }
            let slugged = if slugged_project.is_empty() {
                project_slug(&facts.path)
            } else {
                slugged_project.to_string()
            };
            rule.match_value == slugged || rule.match_value == facts.path
        }
        "group" => {
            if rule.match_value.is_empty() {
                return false;
            }
            rule.match_value == facts.group || rule.match_value == slug(&facts.group)
        }
        "tool" => {
            if rule.match_value.is_empty() {
                return false;
            }
            rule.match_value == facts.tool
        }
        "title_regex" => {
            if rule.match_value.is_empty() {
                return false;
            }
            regex::Regex::new(&rule.match_value)
                .map(|re| re.is_match(&facts.title))
                .unwrap_or(false)
        }
        _ => false,
    }
}

/// The fields that describe the session alone: no tag and no payload depends on
/// them, so they are what a **tag value** may reference (`git.branch:{git.branch}`).
pub fn base_fields(
    settings: &Settings,
    facts: &SessionFacts,
    branch: &str,
) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    fields.insert(
        "session".to_string(),
        short_id(&facts.id, &settings.session_tagging),
    );
    fields.insert("session_id".to_string(), facts.id.clone());
    fields.insert(
        "tool".to_string(),
        if facts.tool.is_empty() {
            DEFAULT_TOOL.to_string()
        } else {
            facts.tool.clone()
        },
    );
    fields.insert("profile".to_string(), facts.profile.clone());
    fields.insert("group".to_string(), facts.group.clone());
    fields.insert("group_slug".to_string(), slug(&facts.group));
    fields.insert("project".to_string(), facts.path.clone());
    fields.insert("project_slug".to_string(), project_slug(&facts.path));
    fields.insert("branch".to_string(), branch.to_string());
    fields
}

/// Every field a template may name: the session's own, the tag/payload shapes and
/// the `{git.xxx}` references resolved for this launch.
pub fn build_fields(
    settings: &Settings,
    facts: &SessionFacts,
    tags: &[Tag],
    payload: &Payload,
    branch: &str,
    git: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut fields = base_fields(settings, facts, branch);
    let csv: Vec<String> = tags.iter().map(Tag::csv).collect();
    fields.insert("feature".to_string(), feature_of(settings, tags));
    fields.insert("tags".to_string(), csv.join(","));
    fields.insert("tags_csv".to_string(), csv.join(","));
    fields.insert("tags_json".to_string(), tags_json(tags));
    fields.insert("payload_json".to_string(), payload_json(payload));
    fields.insert("tags_b64".to_string(), base64url(&payload_json(payload)));
    for (name, value) in git {
        fields.insert(name.clone(), value.clone());
    }
    fields
}

/// A template field name: lower snake case, or a dotted reference (`git.branch`).
///
/// The dot is the only punctuation allowed, so `{team:core}` and `{1,2}` are left
/// as literal text rather than half-parsed.
pub fn is_field_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '.')
}

/// Every well-formed `{name}` in `text`, once each and in order.
///
/// This is how the resolver learns which `{git.xxx}` facts to read: nothing is
/// resolved speculatively, and a template that names no git fact starts no
/// subprocess. It uses the same grammar as [`render_template`], so a name that
/// would not be substituted is not collected either.
pub fn referenced_names(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'{' {
            let step = text[index..].chars().next().map_or(1, char::len_utf8);
            index += step;
            continue;
        }
        let Some(close) = text[index..].find('}') else {
            break;
        };
        let name = &text[index + 1..index + close];
        if is_field_name(name) && !names.iter().any(|seen| seen == name) {
            names.push(name.to_string());
        }
        index += close + 1;
    }
    names
}

/// The `{name}` references inside a tag's value, including nested JSON values, so
/// `meta:={"branch":"{git.branch}"}` is discovered the same way `key:{git.branch}` is.
pub fn referenced_names_in_value(value: &Value) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut collect = |text: &str| {
        for name in referenced_names(text) {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    };
    fn walk<F: FnMut(&str)>(value: &Value, collect: &mut F) {
        match value {
            Value::String(text) => collect(text),
            Value::Array(items) => {
                for item in items {
                    walk(item, collect);
                }
            }
            Value::Object(map) => {
                for item in map.values() {
                    walk(item, collect);
                }
            }
            _ => {}
        }
    }
    walk(value, &mut collect);
    names
}

/// Substitute `{field}`; unknown fields render empty and are reported.
pub fn render_template(template: &str, fields: &BTreeMap<String, String>) -> (String, Vec<String>) {
    let mut out = String::new();
    let mut missing = Vec::new();
    let bytes = template.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'{' {
            let next = template[index..].chars().next().unwrap_or_default();
            out.push(next);
            index += next.len_utf8();
            continue;
        }
        let Some(close) = template[index..].find('}') else {
            out.push('{');
            index += 1;
            continue;
        };
        let name = &template[index + 1..index + close];
        if !is_field_name(name) {
            out.push('{');
            index += 1;
            continue;
        }
        match fields.get(name) {
            Some(value) => out.push_str(value),
            None => {
                missing.push(name.to_string());
            }
        }
        index += close + 1;
    }
    (out, missing)
}

/// One printable line: aoe parses stdout as KEY=VALUE and does not trim.
///
/// Newlines would truncate the variable; other control characters have no
/// business in a header value. Non-ASCII stays — §5 allows it when the operator
/// writes it into a template.
pub fn sanitize_value(value: &str) -> String {
    value
        .chars()
        .filter(|ch| *ch >= ' ' && *ch != '\x7f')
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn clamp_bytes(value: &str, limit: usize) -> (String, bool) {
    if value.len() <= limit {
        return (value.to_string(), false);
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

/// The headers this launch will emit or map: enabled, in settings order.
pub fn enabled_targets(settings: &Settings) -> Vec<&Target> {
    settings
        .targets
        .iter()
        .filter(|header| header.enabled)
        .collect()
}

pub fn snapshot(flags: &Flags, env: &Env, runner: Option<Runner<'_>>) -> Snapshot {
    snapshot_with_table(flags, env, runner, None, None)
}

pub fn snapshot_with_table(
    flags: &Flags,
    env: &Env,
    runner: Option<Runner<'_>>,
    table_override: Option<toml::Table>,
    facts_override: Option<SessionFacts>,
) -> Snapshot {
    let started = Instant::now();
    let (mut facts, session_warnings) = match facts_override {
        Some(facts) => (facts, Vec::new()),
        None => resolve_session(flags, runner, env, DEFAULT_TIMEOUT),
    };
    let elapsed = started.elapsed();
    let profile = if facts.profile.is_empty() {
        flags.profile.clone()
    } else {
        facts.profile.clone()
    };
    let from_worker = table_override.is_some();
    let (mut raw_table, raw_warnings) = match table_override {
        Some(table) => (table, Vec::new()),
        None => {
            let raw = settings::load(&profile, None, env);
            (raw.table, raw.warnings)
        }
    };
    let mut warnings = session_warnings;
    warnings.extend(raw_warnings.iter().cloned());
    // `activity_family` and `activity_families` are `per_project`: the host keeps
    // the project's copy in `projects.json`, and a launch happens in a bare shell
    // with no host RPC, so the registry is read and folded into the same settings
    // table here. A worker passes its own table (and asks the host instead), so
    // this only runs for a launch, `value`, `doctor` or the `activity` verb.
    if !from_worker {
        if let Ok(app_dir) = settings::app_dir(env) {
            let (project, project_warnings) =
                crate::registry::project_settings(&app_dir, &profile, &facts.path);
            warnings.extend(project_warnings);
            for (key, value) in project {
                if let Some(converted) = settings::json_to_toml(&value) {
                    raw_table.insert(key, converted);
                }
            }
        }
    }
    let mut configured = Settings::from_table(&raw_table, warnings);
    // Two overlays live in the plugin's own app dir, one level above the session:
    // the AoE-wide default and the project's default. They are read here, once,
    // and folded in without touching the settings' file. `tags.toml` is written
    // by the dashboard's LLM Tags panel; the session layer below is written by
    // that panel (for one session) and the `tag` command.
    let levels = match settings::app_dir(env) {
        Ok(app_dir) => {
            let (levels, level_warnings) = crate::levels::load(&app_dir);
            configured.warnings.extend(level_warnings);
            Some((app_dir, levels))
        }
        Err(message) => {
            configured.warnings.push(format!(
                "levels: {message}; no project or AoE tags are read"
            ));
            None
        }
    };
    // The AoE default sits under the repository's file: a checkout's declaration
    // is more specific than an operator-wide default.
    if let Some((_, levels)) = &levels {
        crate::levels::apply_aoe(&mut configured, &levels.aoe);
    }
    // A repo declares its own tags in a JSON file named by `tags_file`. They are
    // merged before the session's edits, so a session can still replace or drop
    // one, and they sit in front of the settings `tags` because the repository is
    // the more specific declaration.
    apply_tags_file(&mut configured, &facts.path);
    // A project's panel entry is more specific than the checkout, and less
    // specific than the session's own edits applied just below.
    if let Some((_, levels)) = &levels {
        crate::levels::apply_project(&mut configured, levels.project(&facts.path));
    }
    // The repository's activity families are read at launch like its tags: the
    // most specific declaration, so editing one applies to the next session and
    // never to a running one. They are their own vocabulary rather than tags, so
    // the level overlays above neither extend nor replace them.
    apply_activity_file(&mut configured, &facts.path);
    // A session's own edits (the UI page and the `tag` command write them) are
    // applied here, once, so the hook, `status`, `doctor` and the worker all see
    // the same tags. They live in the session's directory, never the config, and
    // `apply` puts them in front of everything inherited: a tag typed for this
    // session is the most specific thing there is.
    if !facts.id.is_empty() {
        match &levels {
            Some((app_dir, _)) => {
                crate::session_tags::apply(&mut configured, app_dir, &facts.id);
            }
            None => configured
                .warnings
                .push("overrides: no app dir; no session edits are read".to_string()),
        }
    }
    warn_if_tags_overflow(&mut configured);

    let remaining = DEFAULT_TIMEOUT.saturating_sub(elapsed);
    let (resolved, render_warnings, tags) =
        render_headers(&configured, &mut facts, runner, env, remaining);
    let mut all_warnings = configured.warnings.clone();
    all_warnings.extend(render_warnings);
    // One resolution, every surface: the hook (which pins it), `activity --show`,
    // `doctor` and the pane all read this field.
    let families = crate::activity::resolve(&configured);
    all_warnings.extend(families.warnings.iter().cloned());

    Snapshot {
        facts,
        settings: configured,
        tags,
        resolved,
        families,
        warnings: all_warnings,
        raw: raw_table,
    }
}

pub fn render_headers(
    settings: &Settings,
    facts: &mut SessionFacts,
    runner: Option<Runner<'_>>,
    env: &Env,
    timeout: Duration,
) -> (Vec<ResolvedHeader>, Vec<String>, Vec<Tag>) {
    let mut warnings = Vec::new();
    let enabled = enabled_targets(settings);

    let mut branch = String::new();
    let needs_branch = !facts.id.is_empty()
        && enabled
            .iter()
            .any(|h| h.template.contains("{branch}") || h.renders_payload());
    if needs_branch {
        if timeout.is_zero() {
            warnings.push(
                "session lookup used the whole 2s budget; {branch} renders empty".to_string(),
            );
        } else {
            let (resolved_branch, branch_warnings) = resolve_branch(facts, runner, env, timeout);
            branch = resolved_branch;
            warnings.extend(branch_warnings);
        }
    }
    facts.branch = branch.clone();

    let matched = matched_tags(settings, facts);
    let (mut tags, tag_warnings) = parse_tags(&matched);
    warnings.extend(tag_warnings);

    // Which git facts does this launch name? Templates, tag values and the
    // feature tag are scanned together, so `git.branch:{git.branch}` reads git
    // once and a launch that mentions no git fact starts no subprocess at all.
    let git_names = wanted_git_references(settings, &tags);
    let wanted: Vec<&str> = git_names.iter().map(String::as_str).collect();
    let (git_fields, git_warnings) = if wanted.is_empty() {
        (BTreeMap::new(), Vec::new())
    } else {
        crate::gitinfo::resolve(&facts.path, &wanted, None, env, timeout)
    };
    warnings.extend(git_warnings);

    // Expand tag values before the payload exists: a tag carries the value it
    // names, so `{tags_json}` and a JSON consumer see `main`, not `{git.branch}`.
    let mut expandable = base_fields(settings, facts, &branch);
    expandable.extend(git_fields.clone());
    for tag in &mut tags {
        tag.value = expand_tag_value(&tag.value, &expandable, &tag.key, &mut warnings);
    }
    let mut settings = settings.clone();
    if let Some(tag) = &mut settings.feature_tag {
        tag.value = expand_tag_value(&tag.value, &expandable, &tag.key, &mut warnings);
    }

    let (payload, payload_warnings) = payload(&settings, facts, &tags, &branch);
    warnings.extend(payload_warnings);
    let fields = build_fields(&settings, facts, &tags, &payload, &branch, &git_fields);
    let configured_token = settings.token.clone();

    let mut resolved = Vec::new();
    for header in enabled {
        // A header target is checked as a header name; a query/body target as a
        // parameter name (its own `name` when `param` is empty).
        let is_header = header.kind == DEFAULT_KIND;
        if is_header && !valid_header_name(&header.name) {
            warnings.push(format!(
                "settings: {:?} is not a valid HTTP header name; skipping it",
                header.name
            ));
            continue;
        }
        if !is_header && !valid_param_name(header.param_name()) {
            warnings.push(format!(
                "settings: {:?} is not a valid query/body parameter name; skipping it",
                header.param_name()
            ));
            continue;
        }
        let (mut value, missing) = target_value(header, &payload, &fields);
        // A literal token is the session's hand-written value: it replaces the
        // payload a template-less target would render, and leaves a target that
        // names its own field (`{session_id}`) alone.
        if !configured_token.is_empty() && header.template.trim().is_empty() {
            value = configured_token.clone();
        }
        if !missing.is_empty() {
            let mut unique: Vec<String> = Vec::new();
            for name in &missing {
                if !unique.contains(name) {
                    unique.push(name.clone());
                }
            }
            let rendered: Vec<String> = unique.iter().map(|name| format!("{{{name}}}")).collect();
            warnings.push(format!(
                "settings: template {:?} for {} uses unknown field(s) {}; they render empty",
                header.template,
                header.name,
                rendered.join(", ")
            ));
        }
        let value = sanitize_value(&value);
        let (value, truncated) = clamp_bytes(&value, VALUE_MAX_BYTES);
        if truncated {
            warnings.push(format!(
                "settings: {} renders more than {VALUE_MAX_BYTES} bytes; truncated",
                header.name
            ));
        }
        let env_var = if header.env_var.is_empty() {
            derive_env_var_for(&header.kind, &header.name)
        } else {
            header.env_var.clone()
        };
        if !valid_env_name(&env_var) {
            warnings.push(format!(
                "settings: {env_var:?} is not a valid environment name; skipping it"
            ));
            continue;
        }
        resolved.push(ResolvedHeader {
            header: header.name.clone(),
            env_var,
            value,
        });
    }

    (resolved, warnings, tags)
}

/// The hook's stdout: exactly `NAME=value` per enabled header, nothing else.
pub fn hook_lines(resolved: &[ResolvedHeader]) -> Vec<String> {
    resolved
        .iter()
        .map(|item| format!("{}={}", item.env_var, item.value))
        .collect()
}

// --- settings normalisation -------------------------------------------------

fn parse_targets(table: &toml::Table, warnings: &mut Vec<String>) -> Vec<Target> {
    // `targets` is the M1 shape; `headers` is the M0 name, read as a header target.
    let legacy = table.get("targets").is_none();
    let rows = if legacy {
        table.get("headers")
    } else {
        table.get("targets")
    };
    let Some(value) = rows else {
        return default_targets();
    };
    let Some(rows) = value.as_array() else {
        warnings.push(format!(
            "settings: {key} is not an array of tables; using the built-in target set",
            key = if legacy { "headers" } else { "targets" }
        ));
        return default_targets();
    };
    let label = if legacy { "headers" } else { "targets" };
    let mut targets = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let index = index + 1;
        let Some(row) = row.as_table() else {
            warnings.push(format!(
                "settings: {label}[{index}] is not a table; skipping it"
            ));
            continue;
        };
        let Some(name) = row
            .get("name")
            .or_else(|| row.get("header"))
            .and_then(toml::Value::as_str)
            .filter(|name| !name.is_empty())
        else {
            warnings.push(format!(
                "settings: {label}[{index}] has no name; skipping it"
            ));
            continue;
        };
        let kind_row = row.get("kind").or_else(|| row.get("delivery"));
        let kind = match kind_row {
            None => {
                // An M0 row named `X-AoE-*` is a header; anything else with no
                // kind keeps the header default too, and says so if it looks odd.
                DEFAULT_KIND.to_string()
            }
            Some(toml::Value::String(text)) if KINDS.contains(&text.as_str()) => text.clone(),
            Some(toml::Value::String(text)) => {
                warnings.push(format!(
                    "settings: {label}[{index}] ({name}) kind {text:?} is not one of {}; \
                     using {DEFAULT_KIND}",
                    KINDS.join(", ")
                ));
                DEFAULT_KIND.to_string()
            }
            Some(_) => {
                warnings.push(format!(
                    "settings: {label}[{index}] ({name}) kind is not a string; using {DEFAULT_KIND}"
                ));
                DEFAULT_KIND.to_string()
            }
        };
        let template = match row.get("template") {
            None => String::new(),
            Some(toml::Value::String(text)) => text.clone(),
            Some(_) => {
                warnings.push(format!(
                    "settings: {label}[{index}] ({name}) template is not a string; skipping it"
                ));
                continue;
            }
        };
        let enabled = match row.get("enabled") {
            None => true,
            Some(toml::Value::Boolean(flag)) => *flag,
            Some(_) => {
                warnings.push(format!(
                    "settings: {label}[{index}] ({name}) enabled is not a boolean; \
                     treating it as true"
                ));
                true
            }
        };
        let env_var = match row.get("env_var") {
            None => derive_env_var_for(&kind, name),
            Some(toml::Value::String(text)) => text.clone(),
            Some(_) => {
                warnings.push(format!(
                    "settings: {label}[{index}] ({name}) env_var is not a string; skipping it"
                ));
                continue;
            }
        };
        // A name is carried into `NAME=value` lines and into the shell command a
        // guarded harness runs, so it is checked where it is accepted rather than
        // only where it is used.
        if !valid_env_name(&env_var) {
            warnings.push(format!(
                "settings: {label}[{index}] ({name}) env_var {env_var:?} is not a valid \
                 environment variable name (a letter or `_`, then letters, digits or `_`); \
                 skipping it"
            ));
            continue;
        }
        let param = match row.get("param") {
            None => String::new(),
            Some(toml::Value::String(text)) => text.clone(),
            Some(_) => {
                warnings.push(format!(
                    "settings: {label}[{index}] ({name}) param is not a string; using {name:?}"
                ));
                String::new()
            }
        };
        if kind != DEFAULT_KIND && !valid_param_name(&param_name(name, &param)) {
            warnings.push(format!(
                "settings: {label}[{index}] ({name}) param {:?} is not a valid query/body \
                 parameter name; skipping it",
                param_name(name, &param)
            ));
            continue;
        }
        // A literal used to be written out as it was typed, so a newline in one
        // added an extra `NAME=value` line to the hook's output — a variable the
        // user never set — and a `\r` broke the header list an `env_only` harness
        // reads. It goes through the same cleaning and the same cap as every other
        // value now, at the point where it is accepted.
        let literal = match row.get("literal") {
            None => String::new(),
            Some(toml::Value::String(text)) => {
                let cleaned = sanitize_value(text);
                let (cleaned, truncated) = clamp_bytes(&cleaned, VALUE_MAX_BYTES);
                if cleaned != *text {
                    warnings.push(format!(
                        "settings: {label}[{index}] ({name}) literal {}",
                        if truncated {
                            format!("is longer than {VALUE_MAX_BYTES} bytes; trimmed")
                        } else {
                            "has control characters or surrounding spaces; cleaned up".to_string()
                        }
                    ));
                }
                cleaned
            }
            Some(_) => {
                warnings.push(format!(
                    "settings: {label}[{index}] ({name}) literal is not a string; ignoring it"
                ));
                String::new()
            }
        };
        let format = match row.get("format") {
            None => DEFAULT_FORMAT.to_string(),
            Some(toml::Value::String(text)) if FORMATS.contains(&text.as_str()) => text.clone(),
            Some(toml::Value::String(text)) => {
                warnings.push(format!(
                    "settings: {label}[{index}] ({name}) format {text:?} is not one of {}; \
                     using {DEFAULT_FORMAT}",
                    FORMATS.join(", ")
                ));
                DEFAULT_FORMAT.to_string()
            }
            Some(_) => {
                warnings.push(format!(
                    "settings: {label}[{index}] ({name}) format is not a string; \
                     using {DEFAULT_FORMAT}"
                ));
                DEFAULT_FORMAT.to_string()
            }
        };
        targets.push(Target {
            name: name.to_string(),
            template,
            env_var,
            enabled,
            kind,
            param,
            literal,
            format,
        });
    }
    targets
}

/// Static tags every session carries, merged in front of the rule matches.
fn parse_name_list(
    value: Option<&toml::Value>,
    key: &str,
    warnings: &mut Vec<String>,
) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let Some(rows) = value.as_array() else {
        warnings.push(format!(
            "settings: {key} is not an array of harness names; ignoring it"
        ));
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let Some(name) = row.as_str().map(str::trim).filter(|name| !name.is_empty()) else {
            warnings.push(format!(
                "settings: {key}[{}]={} is not a harness name; skipping it",
                index + 1,
                render_toml_scalar(row)
            ));
            continue;
        };
        let name = name.to_ascii_lowercase();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// `activity_family`: the family a session starts in. A blank string means "the
/// built-in `default` family", which is also what an unusable value means.
fn parse_activity_family(value: Option<&toml::Value>, warnings: &mut Vec<String>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    let Some(name) = value.as_str().map(str::trim) else {
        warnings.push(format!(
            "settings: activity_family={} is not a family name; using the default family",
            render_toml_scalar(value)
        ));
        return String::new();
    };
    if name.is_empty() {
        return String::new();
    }
    if !crate::activity::valid_family_name(name) {
        warnings.push(format!(
            "settings: activity_family {name:?} must be letters, digits, `.`, `_` or `-`; \
             using the default family"
        ));
        return String::new();
    }
    name.to_string()
}

/// `activity_families`: the rows the settings UI writes. Kept as raw TOML values
/// here — [`crate::activity::resolve`] validates them field by field, so a bad row
/// is reported once, in the place that also knows about the repo file.
fn parse_activity_families(
    value: Option<&toml::Value>,
    warnings: &mut Vec<String>,
) -> Vec<toml::Value> {
    let Some(value) = value else {
        return Vec::new();
    };
    let Some(rows) = value.as_array() else {
        warnings.push(
            "settings: activity_families is not an array of activity rows; ignoring it".to_string(),
        );
        return Vec::new();
    };
    rows.clone()
}

/// `activity_file`: where the repository declares its families, relative to the
/// project root. An empty string turns the repo file off for a machine that wants
/// the settings to be the only source.
fn parse_activity_file(value: Option<&toml::Value>, warnings: &mut Vec<String>) -> String {
    let Some(value) = value else {
        return crate::activity::DEFAULT_FILE.to_string();
    };
    let Some(path) = value.as_str().map(str::trim) else {
        warnings.push(format!(
            "settings: activity_file={} is not a path; using {}",
            render_toml_scalar(value),
            crate::activity::DEFAULT_FILE
        ));
        return crate::activity::DEFAULT_FILE.to_string();
    };
    path.to_string()
}

fn parse_static_tags(value: Option<&toml::Value>, warnings: &mut Vec<String>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let Some(rows) = value.as_array() else {
        warnings.push("settings: tags is not an array of strings; ignoring it".to_string());
        return Vec::new();
    };
    let mut tags = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let Some(tag) = row.as_str().filter(|tag| valid_tag(tag)) else {
            warnings.push(format!(
                "settings: tags[{}]={} is not a valid tag \
                 (starts [a-z0-9], printable, ≤{TAG_MAX_CHARS} chars); skipping it",
                index + 1,
                render_toml_scalar(row)
            ));
            continue;
        };
        if !tags.iter().any(|existing| existing == tag) {
            tags.push(tag.to_string());
        }
    }
    tags
}

/// Merge the project's JSON tags file into `settings.tags`, if one is configured.
///
/// The file's keys win over a settings tag with the same key (the repo is more
/// specific than the global list), and the file's pairs come first so they get
/// the bounded budget before a global tag does. Session edits are applied later
/// and therefore still win. A warning, never an error: see [`load_tags_file`].
fn apply_tags_file(settings: &mut Settings, project_path: &str) {
    if settings.tags_file.is_empty() {
        return;
    }
    let (file_tags, warnings) = load_tags_file(project_path, &settings.tags_file);
    settings.warnings.extend(warnings);
    if file_tags.is_empty() {
        return;
    }
    let file_keys: Vec<String> = file_tags.iter().map(|tag| tag.key.clone()).collect();
    settings.tags.retain(|raw| match Tag::parse(raw) {
        Ok(tag) => !file_keys.contains(&tag.key),
        Err(_) => true,
    });
    let mut merged: Vec<String> = file_tags.iter().map(Tag::entry).collect();
    merged.append(&mut settings.tags);
    settings.tags = merged;
}

/// Read the repository's activity families and hand them to the resolver.
///
/// Not merged with the settings here: a same-named family replaces the whole
/// definition at the next level up, and that rule lives in one place,
/// [`crate::activity::resolve`].
fn apply_activity_file(settings: &mut Settings, project_path: &str) {
    let (table, warnings) = load_activity_file(project_path, &settings.activity_file);
    settings.warnings.extend(warnings);
    settings.activity_repo = table;
}

/// One warning when the merged inherited set is over the per-header budget.
///
/// The list is in priority order by the time this runs — the session's own edits,
/// then the repo file, then the settings — so the entries named here are exactly
/// the inherited ones a render will drop. Session edits are exempt: `manual_tag_count`
/// leading entries always travel. Rules can add nothing once the budget is full.
fn warn_if_tags_overflow(settings: &mut Settings) {
    let cap = TAG_CAP + settings.manual_tag_count;
    if settings.tags.len() <= cap {
        return;
    }
    let dropped: Vec<&str> = settings.tags.iter().skip(cap).map(String::as_str).collect();
    let shown = dropped
        .iter()
        .take(5)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    let more = dropped.len().saturating_sub(5);
    settings.warnings.push(format!(
        "settings: {} tags are configured but a header carries {TAG_CAP} inherited tags plus \
         {} session edit(s); dropped {shown}{}",
        settings.tags.len(),
        settings.manual_tag_count,
        if more > 0 {
            format!(", +{more} more")
        } else {
            String::new()
        }
    ));
}

/// A `tags_file` value: one repo-relative path, or nothing. Anything else is a
/// warning and no file, the way every other settings field degrades.
fn parse_tags_file(value: Option<&toml::Value>, warnings: &mut Vec<String>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    match value.as_str() {
        Some(path) => path.trim().to_string(),
        None => {
            warnings.push("settings: tags_file is not a path string; ignoring it".to_string());
            String::new()
        }
    }
}

/// The variable the hook exports for one target: the row's own `env_var`, else
/// the per-kind spelling (`AOE_HDR_`, `AOE_QRY_`, `AOE_BODY_`). One definition so
/// the hook, the overlay writer and `doctor` can never disagree.
pub fn target_env_var(target: &Target) -> String {
    if target.env_var.is_empty() {
        derive_env_var_for(&target.kind, &target.name)
    } else {
        target.env_var.clone()
    }
}

/// The effective query/body parameter name for a settings row.
pub fn param_name(name: &str, param: &str) -> String {
    if param.is_empty() {
        name.to_string()
    } else {
        param.to_string()
    }
}

/// Query/body parameter names: netstring-ish, no `&`, `=`, `#` or spaces.
pub fn valid_param_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 64
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-' | '[' | ']'))
}

fn parse_rules(rows: Option<&toml::Value>, warnings: &mut Vec<String>) -> Vec<Rule> {
    let Some(value) = rows else {
        return Vec::new();
    };
    let Some(rows) = value.as_array() else {
        warnings.push("settings: rules is not an array of tables; ignoring it".to_string());
        return Vec::new();
    };
    let mut rules = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let index = index + 1;
        let Some(row) = row.as_table() else {
            warnings.push(format!(
                "settings: rules[{index}] is not a table; skipping it"
            ));
            continue;
        };
        let tag = row.get("tag").and_then(toml::Value::as_str);
        let Some(tag) = tag.filter(|tag| valid_tag(tag)) else {
            warnings.push(format!(
                "settings: rules[{index}] tag {} is not a valid tag \
                 (starts [a-z0-9], printable, ≤{TAG_MAX_CHARS} chars); skipping it",
                match tag {
                    Some(tag) => format!("{tag:?}"),
                    None => "None".to_string(),
                }
            ));
            continue;
        };
        let kind = match row.get("match_kind") {
            None => "project".to_string(),
            Some(toml::Value::String(text)) => text.clone(),
            Some(_) => {
                warnings.push(format!(
                    "settings: rules[{index}] ({tag}) match_kind is not a string; skipping it"
                ));
                continue;
            }
        };
        if !MATCH_KINDS_M0.contains(&kind.as_str()) && !MATCH_KINDS_M1.contains(&kind.as_str()) {
            warnings.push(format!(
                "settings: rules[{index}] ({tag}) match_kind {kind:?} is unknown; it never matches"
            ));
        }
        let value = match row.get("match_value") {
            None => String::new(),
            Some(toml::Value::String(text)) => text.clone(),
            Some(_) => {
                warnings.push(format!(
                    "settings: rules[{index}] ({tag}) match_value is not a string; skipping it"
                ));
                continue;
            }
        };
        let priority = match row.get("priority") {
            None => 0,
            Some(toml::Value::Integer(n)) => (*n).clamp(-100, 100) as i32,
            Some(_) => {
                warnings.push(format!(
                    "settings: rules[{index}] ({tag}) priority is not an integer; using 0"
                ));
                0
            }
        };
        rules.push(Rule {
            tag: tag.to_string(),
            match_kind: kind,
            match_value: value,
            priority,
        });
    }
    rules
}

// --- small helpers ----------------------------------------------------------

fn first(values: &[String]) -> String {
    values
        .iter()
        .find(|value| !value.is_empty())
        .cloned()
        .unwrap_or_default()
}

fn render_toml_scalar(value: &toml::Value) -> String {
    match value {
        toml::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Where the CLI looks for its adapter table: the plugin root next to `bin/`.
pub fn plugin_root() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            if let Some(root) = bin_dir.parent() {
                return root.to_path_buf();
            }
        }
    }
    PathBuf::from(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(tags: &[&str]) -> Settings {
        Settings {
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            manual_tag_count: 0,
            tags_file: String::new(),
            targets: default_targets(),
            rules: Vec::new(),
            session_tagging: "short".to_string(),
            feature_tag: None,
            token: String::new(),
            harness_extension: Vec::new(),
            activity_family: String::new(),
            activity_families: Vec::new(),
            activity_file: crate::activity::DEFAULT_FILE.to_string(),
            activity_repo: None,
            warnings: Vec::new(),
        }
    }

    fn facts() -> SessionFacts {
        SessionFacts {
            id: "cf11af87da7e4bf0".to_string(),
            title: "Lithuanians".to_string(),
            path: "/var/lib/aoe/work/lithuanians".to_string(),
            group: String::new(),
            tool: "pi".to_string(),
            profile: "main".to_string(),
            branch: String::new(),
        }
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
            std::env::temp_dir().join(format!("llm-tags-resolver-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }

    #[test]
    fn a_tag_is_a_key_and_a_value_not_a_word() {
        assert_eq!(Tag::parse("team:core").unwrap().value, Value::from("core"));
        assert_eq!(Tag::parse("cost:=1.5").unwrap().value, Value::from(1.5));
        assert_eq!(
            Tag::parse("scope:=[\"api\",\"ui\"]").unwrap().value,
            serde_json::json!(["api", "ui"])
        );
        assert_eq!(Tag::parse("platform").unwrap().value, Value::Bool(true));
        // `feature:` keeps an empty value rather than becoming a marker.
        assert_eq!(Tag::parse("feature:").unwrap().value, Value::from(""));
        assert!(Tag::parse("team :=core").is_err());
        assert!(Tag::parse("cost:=not json").is_err());
        assert!(Tag::parse(":core").is_err());
    }

    #[test]
    fn a_tags_file_entry_keeps_its_json_type_round_tripping_settings() {
        for raw in [
            "team:core",
            "platform",
            "cost:=1.5",
            "ported:=true",
            "matrix:=[1,2]",
            "meta:={\"k\":1}",
        ] {
            let tag = Tag::parse(raw).unwrap();
            let entry = tag.entry();
            assert_eq!(Tag::parse(&entry).unwrap(), tag, "{raw:?} -> {entry:?}");
        }
        // A string that starts with `=` takes the JSON form, or it would read
        // back as a broken `:=` entry.
        let odd = Tag {
            key: "note".to_string(),
            value: Value::from("=x"),
        };
        assert_eq!(Tag::parse(&odd.entry()).unwrap(), odd);
    }

    #[test]
    fn a_tags_file_turns_every_top_level_pair_into_a_typed_tag() {
        let dir = tempdir();
        std::fs::write(
            dir.path.join("tags.json"),
            r#"{"team":"core","cost":1.5,"ported":true,"matrix":[1,2],"meta":{"k":1},"note":null}"#,
        )
        .unwrap();
        let (tags, warnings) = load_tags_file(dir.path.to_str().unwrap(), "tags.json");
        assert!(warnings.is_empty(), "{warnings:?}");
        let pairs: BTreeMap<String, Value> =
            tags.into_iter().map(|tag| (tag.key, tag.value)).collect();
        assert_eq!(
            pairs,
            BTreeMap::from([
                ("cost".to_string(), Value::from(1.5)),
                ("matrix".to_string(), serde_json::json!([1, 2])),
                ("meta".to_string(), serde_json::json!({"k": 1})),
                ("note".to_string(), Value::Null),
                ("ported".to_string(), Value::Bool(true)),
                ("team".to_string(), Value::from("core")),
            ])
        );
    }

    #[test]
    fn a_tags_file_must_stay_inside_the_project() {
        let dir = tempdir();
        for configured in ["/etc/tags.json", "../tags.json", "sub/../../tags.json"] {
            let (tags, warnings) = load_tags_file(dir.path.to_str().unwrap(), configured);
            assert!(tags.is_empty(), "{configured:?} loaded tags");
            assert!(
                warnings[0].contains("inside the project"),
                "{configured:?}: {warnings:?}"
            );
        }
    }

    #[test]
    fn a_broken_tags_file_is_a_warning_not_an_error() {
        let dir = tempdir();
        let project = dir.path.to_str().unwrap();
        let (tags, warnings) = load_tags_file(project, "missing.json");
        assert!(tags.is_empty());
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("does not exist")),
            "{warnings:?}"
        );

        std::fs::write(dir.path.join("bad.json"), "{").unwrap();
        let (_, warnings) = load_tags_file(project, "bad.json");
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("not valid JSON")),
            "{warnings:?}"
        );

        std::fs::write(dir.path.join("list.json"), "[1,2]").unwrap();
        let (_, warnings) = load_tags_file(project, "list.json");
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("JSON object")),
            "{warnings:?}"
        );

        std::fs::write(
            dir.path.join("keys.json"),
            r#"{"Team":"x","bad key":"y","ok":"z"}"#,
        )
        .unwrap();
        let (tags, warnings) = load_tags_file(project, "keys.json");
        assert_eq!(tags.len(), 1, "{tags:?}");
        assert_eq!(tags[0].key, "ok");
        assert_eq!(warnings.len(), 2, "{warnings:?}");
    }

    #[test]
    fn a_tags_file_that_is_not_a_regular_file_is_refused_without_being_opened() {
        let dir = tempdir();
        let project = dir.path.to_str().unwrap();
        // A directory stands in for every non-regular type: the point is that the
        // type is checked before the read, so a FIFO (which would block the hook
        // until a writer appears) or a device never reaches an `open`.
        std::fs::create_dir(dir.path.join("tags.json")).unwrap();
        let (tags, warnings) = load_tags_file(project, "tags.json");
        assert!(tags.is_empty(), "{tags:?}");
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("not a regular file")),
            "{warnings:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_tags_file_symlink_may_not_point_out_of_the_project() {
        let dir = tempdir();
        let project = dir.path.join("project");
        std::fs::create_dir(&project).unwrap();
        let outside = dir.path.join("outside.json");
        std::fs::write(&outside, r#"{"token":"exfiltrated"}"#).unwrap();

        std::os::unix::fs::symlink(&outside, project.join("tags.json")).unwrap();
        let (tags, warnings) = load_tags_file(project.to_str().unwrap(), "tags.json");
        assert!(tags.is_empty(), "{tags:?}");
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("not inside the project")),
            "{warnings:?}"
        );

        // A link that stays inside the project is still a file inside the project.
        std::fs::write(project.join("real.json"), r#"{"token":"ours"}"#).unwrap();
        std::fs::remove_file(project.join("tags.json")).unwrap();
        std::os::unix::fs::symlink(project.join("real.json"), project.join("tags.json")).unwrap();
        let (tags, warnings) = load_tags_file(project.to_str().unwrap(), "tags.json");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(tags.len(), 1, "{tags:?}");
    }

    #[test]
    fn a_tags_file_over_the_cap_is_refused_by_the_read_not_by_its_metadata() {
        let dir = tempdir();
        let filler = "x".repeat(TAGS_FILE_MAX_BYTES as usize);
        std::fs::write(
            dir.path.join("tags.json"),
            format!(r#"{{"note":"{filler}"}}"#),
        )
        .unwrap();
        let (tags, warnings) = load_tags_file(dir.path.to_str().unwrap(), "tags.json");
        assert!(tags.is_empty(), "{tags:?}");
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("is larger than")),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_tags_file_string_is_trimmed_but_a_list_or_object_keeps_its_shape() {
        let dir = tempdir();
        let long = "y".repeat(TAGS_FILE_STRING_MAX_CHARS * 2);
        std::fs::write(
            dir.path.join("tags.json"),
            serde_json::json!({
                "note": long.clone(),
                "matrix": [long.clone(), long.clone()],
                "meta": { "k": long.clone() },
            })
            .to_string(),
        )
        .unwrap();
        let (tags, warnings) = load_tags_file(dir.path.to_str().unwrap(), "tags.json");
        let pairs: BTreeMap<String, Value> =
            tags.into_iter().map(|tag| (tag.key, tag.value)).collect();

        assert_eq!(
            pairs["note"],
            Value::from("y".repeat(TAGS_FILE_STRING_MAX_CHARS)),
            "a string is trimmed to the cap"
        );
        assert_eq!(
            pairs["matrix"],
            serde_json::json!([long.clone(), long.clone()]),
            "a list is a structure, not text, and keeps its length"
        );
        assert_eq!(
            pairs["meta"],
            serde_json::json!({ "k": long.clone() }),
            "an object keeps its length too"
        );
        assert_eq!(
            warnings.len(),
            1,
            "one warning, for the trimmed string: {warnings:?}"
        );
        assert!(warnings[0].contains("trimmed"), "{warnings:?}");
    }

    #[test]
    fn a_tags_file_value_loses_its_control_characters_at_every_depth() {
        let dir = tempdir();
        std::fs::write(
            dir.path.join("tags.json"),
            serde_json::json!({
                "note": "a\nb\tc\u{0}d\u{7f}e",
                "meta": { "k": "x\ny" },
                "matrix": ["p\rq"],
            })
            .to_string(),
        )
        .unwrap();
        let (tags, warnings) = load_tags_file(dir.path.to_str().unwrap(), "tags.json");
        assert!(warnings.is_empty(), "{warnings:?}");
        let pairs: BTreeMap<String, Value> =
            tags.into_iter().map(|tag| (tag.key, tag.value)).collect();
        assert_eq!(pairs["note"], Value::from("abcde"));
        assert_eq!(pairs["meta"], serde_json::json!({ "k": "xy" }));
        assert_eq!(pairs["matrix"], serde_json::json!(["pq"]));
    }

    #[test]
    fn the_repo_tags_file_wins_over_a_same_key_settings_tag() {
        let dir = tempdir();
        std::fs::write(dir.path.join("tags.json"), r#"{"team":"repo","cost":1.5}"#).unwrap();
        let table: toml::Table =
            "tags = [\"team:global\", \"feature:billing\"]\ntags_file = \"tags.json\""
                .parse()
                .unwrap();
        let env = Env::new([("HOME", dir.path.to_str().unwrap())]);
        let facts = SessionFacts {
            id: String::new(),
            path: dir.path.display().to_string(),
            ..facts()
        };
        let snap = snapshot_with_table(&Flags::default(), &env, None, Some(table), Some(facts));
        assert!(snap.warnings.is_empty(), "{:?}", snap.warnings);
        // File tags come first and replace the same key; the settings tag that
        // does not collide is still carried after them. (The JSON object is a
        // sorted map, so its pairs arrive in key order.)
        assert_eq!(
            snap.settings.tags,
            vec!["cost:=1.5", "team:repo", "feature:billing"]
        );
        let payload: Value = serde_json::from_str(&snap.resolved[0].value).unwrap();
        assert_eq!(payload["team"], "repo");
        assert_eq!(payload["cost"], 1.5);
        assert_eq!(payload["feature"], "billing");
    }

    #[test]
    fn a_session_edit_outranks_the_repo_file_and_keeps_the_budget() {
        let dir = tempdir();
        std::fs::write(
            dir.path.join("tags.json"),
            r#"{"alpha":"1","beta":"2","gamma":"3","delta":"4","epsilon":"5"}"#,
        )
        .unwrap();
        let table: toml::Table = "tags = [\"global:one\"]\ntags_file = \"tags.json\""
            .parse()
            .unwrap();
        let env = Env::new([("HOME", dir.path.to_str().unwrap())]);
        let facts = SessionFacts {
            id: "cf11af87da7e4bf0".to_string(),
            path: dir.path.display().to_string(),
            ..facts()
        };
        let app_dir = crate::settings::app_dir(&env).expect("app dir");
        crate::session_tags::set_tag(&app_dir, &facts.id, "hand:written").expect("session edit");
        let snap = snapshot_with_table(&Flags::default(), &env, None, Some(table), Some(facts));

        // Seven configured tags, five carried: the manual one is first, then the
        // repo file's pairs, and the global tag is the one that falls off.
        assert_eq!(snap.settings.tags[0], "hand:written");
        let payload: Value = serde_json::from_str(&snap.resolved[0].value).unwrap();
        assert_eq!(payload["hand"], "written", "{payload}");
        assert!(payload.get("alpha").is_some(), "{payload}");
        assert!(payload.get("global").is_none(), "{payload}");
        assert!(
            snap.warnings
                .iter()
                .any(|warning| warning.contains("dropped")),
            "{:?}",
            snap.warnings
        );
    }

    #[test]
    fn the_overflow_warning_names_what_the_render_will_drop() {
        let mut configured = settings(&["a:1", "b:2", "c:3", "d:4", "e:5", "f:6"]);
        warn_if_tags_overflow(&mut configured);
        assert_eq!(configured.warnings.len(), 1, "{:?}", configured.warnings);
        assert!(
            configured.warnings[0].contains("dropped f:6"),
            "{:?}",
            configured.warnings
        );
    }

    #[test]
    fn a_manual_tag_is_not_counted_against_the_inherited_budget() {
        let mut configured = settings(&["hand:written", "a:1", "b:2", "c:3", "d:4", "e:5", "f:6"]);
        configured.manual_tag_count = 1;
        let carried = matched_tags(&configured, &facts());
        assert_eq!(carried.len(), TAG_CAP + 1, "{carried:?}");
        assert!(carried.contains(&"hand:written".to_string()), "{carried:?}");
        assert!(!carried.contains(&"f:6".to_string()), "{carried:?}");
    }

    #[test]
    fn the_overflow_warning_never_names_a_session_edit() {
        let mut configured = settings(&["hand:written", "a:1", "b:2", "c:3", "d:4", "e:5", "f:6"]);
        configured.manual_tag_count = 1;
        warn_if_tags_overflow(&mut configured);
        assert_eq!(configured.warnings.len(), 1, "{:?}", configured.warnings);
        assert!(
            configured.warnings[0].contains("dropped f:6"),
            "{:?}",
            configured.warnings
        );
        assert!(
            !configured.warnings[0].contains("hand:written"),
            "{:?}",
            configured.warnings
        );
    }

    #[test]
    fn a_rule_cannot_replace_a_manually_set_key() {
        let mut configured = settings(&["team:manual"]);
        configured.rules = vec![Rule::new("team:rule", "manual", "")];
        assert_eq!(matched_tags(&configured, &facts()), vec!["team:manual"]);
    }

    #[test]
    fn rules_fill_the_remaining_budget_behind_the_static_tags() {
        let mut configured = settings(&["a:1", "b:2", "c:3", "d:4"]);
        configured.rules = vec![
            Rule::new("e:5", "manual", "").with_priority(1),
            Rule::new("f:6", "manual", ""),
        ];
        assert_eq!(
            matched_tags(&configured, &facts()),
            vec!["a:1", "b:2", "c:3", "d:4", "e:5"]
        );
    }

    fn feature_settings(raw: &str) -> Settings {
        let mut warnings = Vec::new();
        let tag = parse_feature_tag(Some(&toml::Value::String(raw.to_string())), &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        Settings {
            tags: Vec::new(),
            manual_tag_count: 0,
            tags_file: String::new(),
            targets: default_targets(),
            rules: Vec::new(),
            session_tagging: "short".to_string(),
            feature_tag: tag,
            token: String::new(),
            harness_extension: Vec::new(),
            activity_family: String::new(),
            activity_families: Vec::new(),
            activity_file: crate::activity::DEFAULT_FILE.to_string(),
            activity_repo: None,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn a_feature_tag_is_an_ordinary_tag_entry() {
        // The spelling older settings files hold: a bare word means the `feature` key.
        let bare = Some(Tag {
            key: "feature".to_string(),
            value: Value::from("billing"),
        });
        assert_eq!(feature_settings("billing").feature_tag, bare);
        // The standard spelling: a key and a value, exactly like an entry in `tags`.
        assert_eq!(feature_settings("feature:billing").feature_tag, bare);
        assert_eq!(
            feature_settings("feature:=42").feature_tag,
            Some(Tag {
                key: "feature".to_string(),
                value: Value::from(42),
            })
        );
        assert_eq!(
            feature_settings("epic:checkout").feature_tag,
            Some(Tag {
                key: "epic".to_string(),
                value: Value::from("checkout"),
            })
        );
        assert_eq!(feature_settings("").feature_tag, None);
    }

    #[test]
    fn a_feature_tag_may_be_a_stored_pair_or_a_one_entry_list() {
        let pair: toml::Value = toml::from_str("key = 'epic'\nvalue = 'checkout'\n").unwrap();
        let mut warnings = Vec::new();
        assert_eq!(
            parse_feature_tag(Some(&pair), &mut warnings),
            Some(Tag {
                key: "epic".to_string(),
                value: Value::from("checkout"),
            })
        );
        assert!(warnings.is_empty(), "{warnings:?}");

        // A pair with no value is a marker, like a bare `tags` entry.
        let marker: toml::Value = toml::from_str("key = 'platform'\n").unwrap();
        assert_eq!(
            parse_feature_tag(Some(&marker), &mut warnings),
            Some(Tag {
                key: "platform".to_string(),
                value: Value::Bool(true),
            })
        );

        let list = toml::Value::Array(vec![
            toml::Value::String("feature:billing".into()),
            toml::Value::String("feature:checkout".into()),
        ]);
        let mut warnings = Vec::new();
        assert_eq!(
            parse_feature_tag(Some(&list), &mut warnings)
                .map(|tag| tag.value)
                .and_then(|value| value.as_str().map(str::to_string)),
            Some("billing".to_string())
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");

        // A shape that is not a tag at all is reported, never silently swallowed.
        let mut warnings = Vec::new();
        assert_eq!(
            parse_feature_tag(Some(&toml::Value::Integer(3)), &mut warnings),
            None
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn the_feature_entry_travels_like_a_tag_and_keeps_the_features_agreement() {
        let (tags, _) = parse_tags(&["team:core".into(), "feature:billing".into()]);
        let (own, warnings) = payload(&feature_settings("feature:checkout"), &facts(), &tags, "");
        assert!(warnings.is_empty(), "{warnings:?}");
        let object: Value = serde_json::from_str(&payload_json(&own)).unwrap();
        assert_eq!(object["team"], "core");
        // The setting wins over the `feature:` tag, in the payload and in `{feature}`.
        assert_eq!(object["feature"], "checkout");

        // A pair under another key is that field — and `{feature}` keeps reading
        // the `feature` entry, so the two never disagree.
        let (epic_payload, warnings) =
            payload(&feature_settings("epic:checkout"), &facts(), &tags, "");
        assert!(warnings.is_empty(), "{warnings:?}");
        let object: Value = serde_json::from_str(&payload_json(&epic_payload)).unwrap();
        assert_eq!(object["epic"], "checkout");
        assert_eq!(object["feature"], "billing");
        assert_eq!(
            feature_of(&feature_settings("epic:checkout"), &tags),
            "billing"
        );

        // And it cannot rewrite a session field, for the same reason a tag cannot.
        let (_, warnings) = payload(&feature_settings("session:mine"), &facts(), &tags, "");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn the_payload_carries_session_fields_then_tags() {
        let (tags, warnings) = parse_tags(&["team:core".into(), "feature:billing".into()]);
        assert!(warnings.is_empty(), "{warnings:?}");
        let (payload, warnings) = payload(&settings(&[]), &facts(), &tags, "main");
        assert!(warnings.is_empty(), "{warnings:?}");
        let json = payload_json(&payload);
        let object: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(object["session"], "S-cf11");
        assert_eq!(object["session_id"], "cf11af87da7e4bf0");
        assert_eq!(object["tool"], "pi");
        assert_eq!(object["project"], "lithuanians");
        assert_eq!(object["team"], "core");
        assert_eq!(object["feature"], "billing");
        // A field that has no value is absent, not empty.
        assert!(object.get("group").is_none());
    }

    #[test]
    fn a_tag_cannot_rewrite_a_session_field() {
        let (tags, _) = parse_tags(&["session:mine".into()]);
        let (payload, warnings) = payload(&settings(&[]), &facts(), &tags, "");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let object: Value = serde_json::from_str(&payload_json(&payload)).unwrap();
        assert_eq!(object["session"], "S-cf11");
    }

    #[test]
    fn the_default_target_sends_the_json_object_and_csv_stays_flat() {
        let (tags, _) = parse_tags(&["team:core".into()]);
        let (payload, _) = payload(&settings(&[]), &facts(), &tags, "");
        let fields = build_fields(
            &settings(&[]),
            &facts(),
            &tags,
            &payload,
            "",
            &BTreeMap::new(),
        );
        let json_target = &default_targets()[0];
        assert!(json_target.renders_payload());
        let (value, missing) = target_value(json_target, &payload, &fields);
        assert!(missing.is_empty());
        assert_eq!(
            serde_json::from_str::<Value>(&value).unwrap()["team"],
            "core"
        );

        let mut csv_target = Target::new("X-Flat", "", "AOE_HDR_X_FLAT").with_format("csv");
        let (flat, _) = target_value(&csv_target, &payload, &fields);
        assert_eq!(flat, "team:core");
        // A template still wins over the format, so an old config is unchanged.
        csv_target.template = "aoe,{tags},session:{session}".to_string();
        let (templated, _) = target_value(&csv_target, &payload, &fields);
        assert_eq!(templated, "aoe,team:core,session:S-cf11");
    }

    #[test]
    fn a_template_can_ask_for_the_payload_as_json() {
        let (tags, _) = parse_tags(&["team:core".into(), "weight:=2".into()]);
        let (payload, _) = payload(&settings(&[]), &facts(), &tags, "");
        let fields = build_fields(
            &settings(&[]),
            &facts(),
            &tags,
            &payload,
            "",
            &BTreeMap::new(),
        );
        let target = Target::new("X-Both", "{payload_json}", "AOE_HDR_X_BOTH");
        let (value, _) = target_value(&target, &payload, &fields);
        let object: Value = serde_json::from_str(&value).unwrap();
        assert_eq!(object["weight"], 2);
        assert_eq!(object["tags_json"], Value::Null);
        // `{tags_json}` is the tag map alone, without the session fields.
        let only_tags: Value = serde_json::from_str(&fields["tags_json"]).unwrap();
        assert_eq!(only_tags, serde_json::json!({"team": "core", "weight": 2}));
    }

    #[test]
    fn a_template_field_may_hold_digits_and_a_brace_that_is_not_a_field_stays() {
        let mut fields = BTreeMap::new();
        fields.insert("tags_b64".to_string(), "eyJhIjoxfQ".to_string());
        let (out, missing) = render_template("x {tags_b64} y", &fields);
        assert_eq!(out, "x eyJhIjoxfQ y");
        assert!(missing.is_empty(), "{missing:?}");

        let (out, missing) = render_template("{payload_json}", &fields);
        assert_eq!(out, "");
        assert_eq!(missing, vec!["payload_json".to_string()]);

        // `{Name}` is a header line, not one of our fields: leave it alone.
        let (out, missing) = render_template("{X-Aoe}", &fields);
        assert_eq!(out, "{X-Aoe}");
        assert!(missing.is_empty(), "{missing:?}");
    }

    #[test]
    fn an_env_var_that_is_not_a_name_is_refused_where_it_is_written() {
        // The hook prints `NAME=value` and the `custom_headers_env` line of a
        // guarded harness runs a shell command that reads `$NAME`, so a `;` or a
        // `$(` here is a command, not a variable name.
        for bad in ["AOE; reboot", "$(id)", "AOE HDR", "9AOE", ""] {
            let table: toml::Table = format!(
                r#"
                [[targets]]
                name = "X-AoE-Tags"
                env_var = {bad:?}
                template = "{{team}}"

                [[targets]]
                name = "X-AoE-Session"
                env_var = "AOE_HDR_X_AOE_SESSION"
                template = "{{session_id}}"
                "#
            )
            .parse()
            .unwrap();
            let mut warnings = Vec::new();
            let parsed = Settings::from_table(&table, std::mem::take(&mut warnings));
            assert_eq!(
                parsed.targets.len(),
                1,
                "{bad:?} was kept: {:?}",
                parsed.targets
            );
            assert_eq!(parsed.targets[0].env_var, "AOE_HDR_X_AOE_SESSION");
            warnings.extend(parsed.warnings);
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning.contains("is not a valid environment variable name")),
                "{bad:?}: {warnings:?}"
            );
        }
    }

    #[test]
    fn a_literal_with_a_newline_is_cleaned_up_before_it_can_add_a_line() {
        // Written out as typed, a newline added a second `NAME=value` line to the
        // hook's output — a variable the user never set — and a `\r` broke the
        // header list an `env_only` harness reads.
        let table: toml::Table = r#"
            [[targets]]
            name = "X-AoE-Tags"
            literal = "  a\r\nb  "
        "#
        .parse()
        .unwrap();
        let warnings = Vec::new();
        let parsed = Settings::from_table(&table, warnings);
        assert_eq!(parsed.targets[0].literal, "ab");
        assert!(
            parsed
                .warnings
                .iter()
                .any(|warning| warning.contains("cleaned up")),
            "{:?}",
            parsed.warnings
        );
    }

    #[test]
    fn a_literal_is_capped_like_every_other_value() {
        let long = "a".repeat(VALUE_MAX_BYTES + 40);
        let table: toml::Table =
            format!("[[targets]]\nname = \"X-AoE-Tags\"\nliteral = \"{long}\"\n")
                .parse()
                .unwrap();
        let parsed = Settings::from_table(&table, Vec::new());
        assert_eq!(parsed.targets[0].literal.len(), VALUE_MAX_BYTES);
        assert!(
            parsed
                .warnings
                .iter()
                .any(|warning| warning.contains("is longer than")),
            "{:?}",
            parsed.warnings
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_lookup_that_floods_stdout_is_read_up_to_the_cap_and_no_further() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir();
        let script = dir.path.join("flood");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nhead -c {} /dev/zero | tr '\\0' 'a'\n",
                OUTPUT_MAX_BYTES * 2
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let out = run_text(
            script.to_str().unwrap(),
            "flood",
            &[],
            Duration::from_secs(10),
        )
        .expect("the command succeeds");
        assert_eq!(
            out.len(),
            OUTPUT_MAX_BYTES,
            "the cap bounds what one lookup can cost"
        );
    }

    #[test]
    fn a_comma_in_a_csv_field_is_quoted_so_it_cannot_forge_a_pair() {
        // `csv` is the format a consumer splits on `,`: an unquoted comma in a
        // value would arrive as a second key it never sent.
        let unforged = Tag::parse("team:x,org:evil").unwrap();
        assert_eq!(unforged.csv(), "\"team:x,org:evil\"");
        let quoted = Tag::parse("note:he said \"hi\"").unwrap();
        assert_eq!(quoted.csv(), "\"note:he said \"\"hi\"\"\"");
        // Nothing to escape: no quotes, so the common case stays plain.
        assert_eq!(Tag::parse("team:core").unwrap().csv(), "team:core");
        assert_eq!(Tag::parse("platform").unwrap().csv(), "platform");
    }
}

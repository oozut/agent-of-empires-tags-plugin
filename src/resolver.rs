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
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::env::Env;
use crate::settings;

// --- constants: plan/parameters.md §4, §5, §7 -------------------------------

pub const TAG_CAP: usize = 5;
/// A tag carries a whole `key:=value` pair now, so the ceiling is the string in
/// settings, not the delivered value (`VALUE_MAX_BYTES` still clamps that).
pub const TAG_MAX_CHARS: usize = 96;
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
    /// Tags every session carries, before any rule matches.
    pub tags: Vec<String>,
    /// Where the tag set is delivered.
    pub targets: Vec<Target>,
    pub rules: Vec<Rule>,
    pub session_tagging: String,
    pub feature_tag: String,
    pub warnings: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            tags: Vec::new(),
            targets: default_targets(),
            rules: Vec::new(),
            session_tagging: DEFAULT_SESSION_TAGGING.to_string(),
            feature_tag: String::new(),
            warnings: Vec::new(),
        }
    }
}

impl Settings {
    pub fn from_table(table: &toml::Table, warnings: Vec<String>) -> Self {
        let mut warns = warnings;
        let tags = parse_static_tags(table.get("tags"), &mut warns);
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

        let mut feature_tag = match table.get("feature_tag") {
            None => String::new(),
            Some(toml::Value::String(text)) => text.clone(),
            Some(_) => {
                warns.push("settings: feature_tag is not a string; ignoring it".to_string());
                String::new()
            }
        };
        if !feature_tag.is_empty() && !valid_tag(&feature_tag) {
            warns.push(format!(
                "settings: feature_tag {feature_tag:?} is not a valid tag \
                 (starts [a-z0-9], printable, ≤{TAG_MAX_CHARS} chars); ignoring it"
            ));
            feature_tag.clear();
        }

        Self {
            tags,
            targets,
            rules,
            session_tagging: tagging,
            feature_tag,
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

    /// The tag as written in settings, for the `csv` format and `{tags}`.
    pub fn csv(&self) -> String {
        match &self.value {
            Value::Bool(true) => self.key.clone(),
            Value::String(text) => format!("{}:{text}", self.key),
            other => format!("{}:{other}", self.key),
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

/// `{feature}` is the operator's feature, not merely the first tag: the setting
/// wins, else the first `feature:<name>` rule, else nothing.
pub fn feature_of(settings: &Settings, tags: &[Tag]) -> String {
    if !settings.feature_tag.is_empty() {
        return settings.feature_tag.clone();
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
    // `feature` is derived from the tags, so it is written last: the setting may
    // override the `feature:<name>` tag, and the tag still is a tag.
    let feature = feature_of(settings, tags);
    if !feature.is_empty() {
        match payload.iter_mut().find(|(key, _)| key == "feature") {
            Some((_, value)) => *value = Value::String(feature),
            None => payload.push(("feature".to_string(), Value::String(feature))),
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

/// Run one read-only `aoe` command and parse its JSON. Never blocks long.
pub fn aoe_json(args: &[String], env: &Env, timeout: Duration) -> Result<Value, AoeError> {
    let bin = aoe_bin(env);
    let verb = args.first().cloned().unwrap_or_default();

    let mut command = std::process::Command::new(&bin);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().map_err(|source| AoeError::Spawn {
        bin: bin.clone(),
        source,
    })?;

    // Both pipes are drained on their own threads: `aoe list --json` can exceed
    // the pipe buffer, and a child blocked on a full pipe never exits, so reading
    // only after `try_wait` would turn a slow query into a guaranteed timeout.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let deadline = Instant::now() + timeout;
    let (status, timed_out, out_buf, err_buf) = std::thread::scope(|scope| {
        let drain = |mut stream: std::process::ChildStdout| {
            scope.spawn(move || {
                let mut buf = Vec::new();
                let _ = stream.read_to_end(&mut buf);
                buf
            })
        };
        let out_handle = stdout.map(drain);
        let err_handle = stderr.map(|mut stream: std::process::ChildStderr| {
            scope.spawn(move || {
                let mut buf = Vec::new();
                let _ = stream.read_to_end(&mut buf);
                buf
            })
        });
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {}
                Err(err) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return (Err(err), false, Vec::new(), Vec::new());
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
        let out_buf = out_handle
            .map(|handle| handle.join().unwrap_or_default())
            .unwrap_or_default();
        let err_buf = err_handle
            .map(|handle| handle.join().unwrap_or_default())
            .unwrap_or_default();
        (Ok(status), timed_out, out_buf, err_buf)
    });
    let status = status.map_err(|source| AoeError::Spawn {
        bin: bin.clone(),
        source,
    })?;

    if timed_out || status.is_none() {
        return Err(AoeError::TimedOut {
            bin,
            verb,
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
            bin,
            verb,
            code: status.code().unwrap_or(-1),
            detail: if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            },
        });
    }
    serde_json::from_slice(&out_buf).map_err(|err| AoeError::InvalidJson {
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

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// Every matching rule contributes; dedupe, then cap at 5 (parameters §5).
pub fn matched_tags(settings: &Settings, facts: &SessionFacts) -> Vec<String> {
    let slugged = project_slug(&facts.path);
    // Stable sort by priority (descending); equal priorities keep list order.
    let mut ordered: Vec<&Rule> = settings.rules.iter().collect();
    ordered.sort_by_key(|a| std::cmp::Reverse(a.priority));
    // Settings `tags` come first: the operator lists what every session carries,
    // and rules add to that set. A rule never displaces a static tag.
    let mut tags: Vec<String> = settings.tags.clone();
    tags.truncate(TAG_CAP);
    for rule in ordered {
        if tags.contains(&rule.tag) {
            continue;
        }
        if rule_matches(rule, facts, &slugged) {
            tags.push(rule.tag.clone());
        }
        if tags.len() == TAG_CAP {
            break;
        }
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

pub fn build_fields(
    settings: &Settings,
    facts: &SessionFacts,
    tags: &[Tag],
    payload: &Payload,
    branch: &str,
) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    let csv: Vec<String> = tags.iter().map(Tag::csv).collect();
    fields.insert(
        "session".to_string(),
        short_id(&facts.id, &settings.session_tagging),
    );
    fields.insert("session_id".to_string(), facts.id.clone());
    fields.insert("feature".to_string(), feature_of(settings, tags));
    fields.insert("tags".to_string(), csv.join(","));
    fields.insert("tags_csv".to_string(), csv.join(","));
    fields.insert("tags_json".to_string(), tags_json(tags));
    fields.insert("payload_json".to_string(), payload_json(payload));
    fields.insert("tags_b64".to_string(), base64url(&payload_json(payload)));
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
        // Field names are lower_snake: `tags_b64` has digits and is still a field.
        let is_field = !name.is_empty()
            && name
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_');
        if !is_field {
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
    let (raw_table, raw_warnings) = match table_override {
        Some(table) => (table, Vec::new()),
        None => {
            let raw = settings::load(&profile, None, env);
            (raw.table, raw.warnings)
        }
    };
    let mut warnings = session_warnings;
    warnings.extend(raw_warnings.iter().cloned());
    let configured = Settings::from_table(&raw_table, warnings);

    let remaining = DEFAULT_TIMEOUT.saturating_sub(elapsed);
    let (resolved, render_warnings) =
        render_headers(&configured, &mut facts, runner, env, remaining);
    let mut all_warnings = configured.warnings.clone();
    all_warnings.extend(render_warnings);

    Snapshot {
        facts,
        settings: configured,
        resolved,
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
) -> (Vec<ResolvedHeader>, Vec<String>) {
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

    let tags = matched_tags(settings, facts);
    let (tags, tag_warnings) = parse_tags(&tags);
    warnings.extend(tag_warnings);
    let (payload, payload_warnings) = payload(settings, facts, &tags, &branch);
    warnings.extend(payload_warnings);
    let fields = build_fields(settings, facts, &tags, &payload, &branch);

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
        let (value, missing) = target_value(header, &payload, &fields);
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

    (resolved, warnings)
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
        let literal = match row.get("literal") {
            None => String::new(),
            Some(toml::Value::String(text)) => text.clone(),
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
            targets: default_targets(),
            rules: Vec::new(),
            session_tagging: "short".to_string(),
            feature_tag: String::new(),
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
        let fields = build_fields(&settings(&[]), &facts(), &tags, &payload, "");
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
        let fields = build_fields(&settings(&[]), &facts(), &tags, &payload, "");
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
}

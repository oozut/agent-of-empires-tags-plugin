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
pub const TAG_MAX_CHARS: usize = 32;
pub const VALUE_MAX_BYTES: usize = 800;
pub const SHORT_ID_PREFIX: &str = "S-";
pub const SHORT_ID_CHARS: usize = 4;
pub const ENV_PREFIX: &str = "AOE_HDR_";
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub template: String,
    pub env_var: String,
    pub enabled: bool,
}

impl Header {
    pub fn new(name: &str, template: &str, env_var: &str) -> Self {
        Self {
            name: name.to_string(),
            template: template.to_string(),
            env_var: env_var.to_string(),
            enabled: true,
        }
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub tag: String,
    pub match_kind: String,
    pub match_value: String,
}

impl Rule {
    pub fn new(tag: &str, match_kind: &str, match_value: &str) -> Self {
        Self {
            tag: tag.to_string(),
            match_kind: match_kind.to_string(),
            match_value: match_value.to_string(),
        }
    }
}

pub fn default_headers() -> Vec<Header> {
    vec![
        Header::new(
            DEFAULT_HEADER,
            "aoe,feature:{feature},session:{session}",
            "AOE_LLM_TAGS",
        ),
        Header::new("X-AoE-Session", "{session_id}", "AOE_HDR_X_AOE_SESSION").disabled(),
    ]
}

/// Validated settings. Malformed rows are dropped with a warning, never fatal.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub headers: Vec<Header>,
    pub rules: Vec<Rule>,
    pub session_tagging: String,
    pub feature_tag: String,
    pub warnings: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            headers: default_headers(),
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
        let headers = parse_headers(table.get("headers"), &mut warns);
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
                 ([a-z0-9][a-z0-9._:-]*, ≤{TAG_MAX_CHARS} chars); ignoring it"
            ));
            feature_tag.clear();
        }

        Self {
            headers,
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
    let mut out = String::from(ENV_PREFIX);
    let mut pending = false;
    for ch in header.chars() {
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
    if !chars.all(|ch| {
        ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | ':' | '-')
    }) {
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
    let mut tags: Vec<String> = Vec::new();
    for rule in &settings.rules {
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
        _ => false,
    }
}

pub fn build_fields(
    settings: &Settings,
    facts: &SessionFacts,
    tags: &[String],
    branch: &str,
) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    let feature = tags
        .first()
        .cloned()
        .unwrap_or_else(|| settings.feature_tag.clone());
    fields.insert(
        "session".to_string(),
        short_id(&facts.id, &settings.session_tagging),
    );
    fields.insert("session_id".to_string(), facts.id.clone());
    fields.insert("feature".to_string(), feature);
    fields.insert("tags".to_string(), tags.join(","));
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
        let is_field =
            !name.is_empty() && name.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_');
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
pub fn enabled_headers(settings: &Settings) -> Vec<&Header> {
    settings
        .headers
        .iter()
        .filter(|header| header.enabled)
        .collect()
}

pub fn snapshot(flags: &Flags, env: &Env, runner: Option<Runner<'_>>) -> Snapshot {
    let started = Instant::now();
    let (mut facts, session_warnings) = resolve_session(flags, runner, env, DEFAULT_TIMEOUT);
    let elapsed = started.elapsed();
    let profile = if facts.profile.is_empty() {
        flags.profile.clone()
    } else {
        facts.profile.clone()
    };
    let raw = settings::load(&profile, None, env);
    let mut warnings = session_warnings;
    warnings.extend(raw.warnings.iter().cloned());
    let configured = Settings::from_table(&raw.table, warnings);

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
        raw: raw.table,
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
    let enabled = enabled_headers(settings);

    let mut branch = String::new();
    let needs_branch =
        !facts.id.is_empty() && enabled.iter().any(|h| h.template.contains("{branch}"));
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
    let fields = build_fields(settings, facts, &tags, &branch);

    let mut resolved = Vec::new();
    for header in enabled {
        if !valid_header_name(&header.name) {
            warnings.push(format!(
                "settings: {:?} is not a valid HTTP header name; skipping it",
                header.name
            ));
            continue;
        }
        let (value, missing) = render_template(&header.template, &fields);
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
            derive_env_var(&header.name)
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

fn parse_headers(rows: Option<&toml::Value>, warnings: &mut Vec<String>) -> Vec<Header> {
    let Some(value) = rows else {
        return default_headers();
    };
    let Some(rows) = value.as_array() else {
        warnings.push(
            "settings: headers is not an array of tables; using the built-in header set"
                .to_string(),
        );
        return default_headers();
    };
    let mut headers = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let index = index + 1;
        let Some(row) = row.as_table() else {
            warnings.push(format!(
                "settings: headers[{index}] is not a table; skipping it"
            ));
            continue;
        };
        let Some(name) = row.get("header").and_then(toml::Value::as_str) else {
            warnings.push(format!(
                "settings: headers[{index}] has no header name; skipping it"
            ));
            continue;
        };
        if name.is_empty() {
            warnings.push(format!(
                "settings: headers[{index}] has no header name; skipping it"
            ));
            continue;
        }
        let template = match row.get("template") {
            None => String::new(),
            Some(toml::Value::String(text)) => text.clone(),
            Some(_) => {
                warnings.push(format!(
                    "settings: headers[{index}] ({name}) template is not a string; skipping it"
                ));
                continue;
            }
        };
        let enabled = match row.get("enabled") {
            None => true,
            Some(toml::Value::Boolean(flag)) => *flag,
            Some(_) => {
                warnings.push(format!(
                    "settings: headers[{index}] ({name}) enabled is not a boolean; treating it as true"
                ));
                true
            }
        };
        let env_var = match row.get("env_var") {
            None => derive_env_var(name),
            Some(toml::Value::String(text)) => text.clone(),
            Some(_) => {
                warnings.push(format!(
                    "settings: headers[{index}] ({name}) env_var is not a string; skipping it"
                ));
                continue;
            }
        };
        headers.push(Header {
            name: name.to_string(),
            template,
            env_var,
            enabled,
        });
    }
    headers
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
                 ([a-z0-9][a-z0-9._:-]*, ≤{TAG_MAX_CHARS} chars); skipping it",
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
        if !MATCH_KINDS_M0.contains(&kind.as_str()) {
            if MATCH_KINDS_M1.contains(&kind.as_str()) {
                warnings.push(format!(
                    "settings: rules[{index}] ({tag}) match_kind {kind:?} is not evaluated until M1; \
                     it never matches yet"
                ));
            } else {
                warnings.push(format!(
                    "settings: rules[{index}] ({tag}) match_kind {kind:?} is unknown; it never matches"
                ));
            }
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
        rules.push(Rule {
            tag: tag.to_string(),
            match_kind: kind,
            match_value: value,
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

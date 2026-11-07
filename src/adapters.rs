//! Harness adapter data (`adapters.toml`): where a harness keeps its mapping.
//!
//! The table is data, not code, so a user can fix or add a row without a plugin
//! release (`docs/harness-adapters.md`). This module only finds a row, resolves
//! its config path, and renders its mapping line; it never writes anything.
//!
//! Which system finally reads the header is not an adapter fact
//! (`docs/header-contract.md`), so nothing here mentions a vendor product.

use std::path::{Path, PathBuf};

use crate::env::Env;
use crate::resolver::{plugin_root, Target};

pub const ADAPTERS_FILENAME: &str = "adapters.toml";

/// The copy compiled into the binary: an install without the file still works,
/// while a file in the plugin tree overrides this one (so a user can fix a row
/// without a plugin release).
pub const EMBEDDED: &str = include_str!("../adapters.toml");

/// One harness row, straight from the table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Adapter {
    pub agent: String,
    pub label: String,
    pub mechanism: String,
    pub tools: Vec<String>,
    pub config_env: String,
    pub config_paths: Vec<String>,
    pub config_format: String,
    pub mapping_format: String,
    /// Delivery points where this harness expands an env reference (`$VAR`).
    /// Anything not listed needs a literal value on that entry.
    pub interpolates: Vec<String>,
    /// How this harness spells an env reference: `{env_var}` is substituted.
    pub env_ref: String,
    /// What a mapping carries once the variable behind it is gone.
    ///
    /// Some harnesses fail the whole request when a header names a variable they
    /// cannot resolve (pi: `resolveHeadersOrThrow`). A mapping the user pasted
    /// survives the plugin, so a bare `$VAR` there breaks every launch once the
    /// hook is gone — including launches the plugin never sees, like a harness
    /// started by hand. When this is set, the shipped value is a command that
    /// prints the variable when it exists and this payload otherwise, so a
    /// leftover mapping degrades to "nothing known" instead of failing.
    pub guard_fallback: String,
    /// Delivery kinds this harness can actually carry (`header`, `query`, `body`).
    pub supports: Vec<String>,
    /// How the harness is pointed at a per-session config: `env_only`,
    /// `overlay_file`, `overlay_dir` or `none` (`docs/harness-adapters.md`).
    pub write_mode: String,
    /// The variable that points the harness at the session artifact.
    pub overlay_env: String,
    /// Our file inside the artifact (defaults to the source file's name).
    pub overlay_file: String,
    /// Which writer builds that file (`src/writer.rs`).
    pub writer: String,
    /// For `env_only` rows: the harness's own variable that carries headers.
    pub native_env: String,
    /// How one target renders into that variable (`{header}`, `{param}`, `{value}`).
    pub native_format: String,
    /// What separates two targets inside that variable: a newline for claude,
    /// a comma for gemini, whose parser only splits on commas.
    pub native_sep: String,
    /// aoe installs its own hooks into this agent's config file, which pins that
    /// file's path for the launch: aoe refuses any `before_session` hook that
    /// moves it, so this harness can only be served through a reference the
    /// user's own config already carries.
    pub aoe_hooks: bool,
    /// This harness splits its header variable on `,(?=name:)`, so a value that
    /// contains such a comma is addressed as a second header: the harness either
    /// sends a corrupted value or refuses to send anything at all. Gemini's
    /// `GEMINI_CLI_CUSTOM_HEADERS` parser does exactly this.
    pub splits_on_commas: bool,
    /// Artefact this harness loads *inside* the agent process, relative to the
    /// plugin root (`harness/pi.ts`). Empty means this harness has no in-process
    /// hook and stays launch-scope only — never guessed.
    pub extension_artifact: String,
    /// Directory inside the session config copy the artefact goes into
    /// (`extensions`). Defaults to `extensions` when the row declares an artefact.
    pub extension_subdir: String,
    /// Our file's name inside that directory.
    pub extension_filename: String,
    /// The variable the hook exports so the extension finds `live.json`.
    pub extension_env: String,
    pub status: String,
    pub notes: String,
}

/// Read every adapter row: the file in the plugin tree, else the built-in copy.
///
/// A broken file is a warning, never an error: `doctor` reports it and the rest
/// of the install keeps working.
pub fn load(root: Option<&Path>) -> (Vec<Adapter>, Vec<String>) {
    let path = root.unwrap_or(&plugin_root()).join(ADAPTERS_FILENAME);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => EMBEDDED.to_string(),
        Err(err) => {
            return (
                Vec::new(),
                vec![format!("{} could not be read: {err}", path.display())],
            )
        }
    };
    parse(&raw)
        .map_err(|err| format!("{} could not be read: {err}", path.display()))
        .map_or_else(
            |warning| (Vec::new(), vec![warning]),
            |table| rows_from(table, &path),
        )
}

fn rows_from(table: toml::Table, path: &Path) -> (Vec<Adapter>, Vec<String>) {
    let Some(toml::Value::Array(rows)) = table.get("adapter") else {
        return (
            Vec::new(),
            vec![format!(
                "{} has no [[adapter]] rows (plugins file: {})",
                path.display(),
                path.display()
            )],
        );
    };
    let mut warnings = Vec::new();
    let mut adapters: Vec<Adapter> = rows.iter().filter_map(parse_row).collect();
    // aoe installs hooks into some agents' own config files and refuses a launch
    // where `before_session` moves that path. Those rows cannot use a per-session
    // copy at all: fall back to the reference channel and say so.
    for adapter in &mut adapters {
        let Some(reason) = overlay_refused(adapter) else {
            continue;
        };
        warnings.push(reason);
        adapter.write_mode = MODE_NONE.to_string();
        adapter.supports.retain(|kind| kind == "header");
    }
    (adapters, warnings)
}

/// Why a per-session copy cannot be used for this agent, if aoe owns its hooks.
pub fn overlay_refused(adapter: &Adapter) -> Option<String> {
    // `env_only` moves nothing: it exports values and the harness's own config
    // reads them, which aoe's hook-path check never sees.
    let moves_config = matches!(adapter.write_mode.as_str(), "overlay_file" | "overlay_dir");
    if !adapter.aoe_hooks || !moves_config {
        return None;
    }
    Some(format!(
        "adapters: {} declares {}, but aoe installs its hooks and refuses a launch whose \
         hook path moves, so this agent is served through the reference its config already \
         carries (header targets only)",
        adapter.agent, adapter.write_mode
    ))
}

fn parse(raw: &str) -> Result<toml::Table, toml::de::Error> {
    raw.parse::<toml::Table>()
}

fn parse_row(item: &toml::Value) -> Option<Adapter> {
    let row = item.as_table()?;
    let agent = row.get("agent").and_then(toml::Value::as_str)?;
    if agent.is_empty() {
        return None;
    }
    let text = |key: &str| {
        row.get(key)
            .and_then(toml::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let flag = |key: &str| row.get(key).and_then(toml::Value::as_bool).unwrap_or(false);
    let list = |key: &str| -> Vec<String> {
        row.get(key)
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
    let label = text("label");
    let tools = {
        let tools = list("tools");
        if tools.is_empty() {
            vec![agent.to_string()]
        } else {
            tools
        }
    };
    Some(Adapter {
        agent: agent.to_string(),
        label: if label.is_empty() {
            agent.to_string()
        } else {
            label
        },
        mechanism: or_default(text("mechanism"), "unknown"),
        tools,
        config_env: text("config_env"),
        config_paths: list("config_paths"),
        config_format: or_default(text("config_format"), "none"),
        mapping_format: text("mapping_format"),
        interpolates: list("interpolates"),
        env_ref: or_default(text("env_ref"), "${env_var}"),
        guard_fallback: text("guard_fallback"),
        write_mode: or_default(text("write_mode"), "none"),
        overlay_env: text("overlay_env"),
        overlay_file: text("overlay_file"),
        writer: text("writer"),
        native_env: text("native_env"),
        native_format: text("native_format"),
        native_sep: or_default(text("native_sep"), "\n"),
        aoe_hooks: flag("aoe_hooks"),
        splits_on_commas: flag("splits_on_commas"),
        extension_artifact: text("extension_artifact"),
        extension_subdir: text("extension_subdir"),
        extension_filename: text("extension_filename"),
        extension_env: text("extension_env"),
        supports: {
            // `supports` absent means the default (a header); `supports = []` means
            // "no channel at all" and must stay empty, or a row that deliberately
            // declares nothing would claim a header delivery it cannot make.
            match row.get("supports").and_then(toml::Value::as_array) {
                Some(values) => values
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .map(str::to_string)
                    .collect(),
                None => vec!["header".to_string()],
            }
        },
        status: or_default(text("status"), "needs_research"),
        notes: text("notes"),
    })
}

fn or_default(value: String, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value
    }
}

/// Match an `--adapter` value: the row's agent id, its label, or one of its tools.
pub fn find<'a>(adapters: &'a [Adapter], name: &str) -> Option<&'a Adapter> {
    let wanted = name.trim().to_lowercase();
    if wanted.is_empty() {
        return None;
    }
    adapters
        .iter()
        .find(|adapter| {
            wanted == adapter.agent.to_lowercase() || wanted == adapter.label.to_lowercase()
        })
        .or_else(|| {
            adapters.iter().find(|adapter| {
                adapter
                    .tools
                    .iter()
                    .any(|tool| tool.to_lowercase() == wanted)
            })
        })
}

/// Match an aoe `tool` value: exact tool, then adapter id, then `pi-*` → `pi`.
pub fn match_tool<'a>(adapters: &'a [Adapter], tool: &str) -> Option<&'a Adapter> {
    let wanted = tool.trim().to_lowercase();
    if wanted.is_empty() {
        return None;
    }
    find(adapters, &wanted).or_else(|| {
        adapters
            .iter()
            .find(|adapter| wanted.starts_with(&format!("{}-", adapter.agent.to_lowercase())))
    })
}

/// Expand `~` and `$VAR` from `env` (never the ambient process).
///
/// `None` when a variable the path needs is unset: a half-expanded path is not a
/// path, and guessing one would write to the wrong file.
pub fn expand_path(path: &str, env: &Env) -> Option<PathBuf> {
    if path.is_empty() {
        return None;
    }
    let mut expanded = String::new();
    let mut rest = path;
    if let Some(stripped) = rest.strip_prefix("~/") {
        let home = match env.home() {
            Some(home) => home.to_string(),
            None => std::env::var("HOME").ok()?,
        };
        expanded.push_str(&home);
        expanded.push('/');
        rest = stripped;
    } else if rest == "~" {
        return Some(PathBuf::from(env.home()?));
    }

    let bytes = rest.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'$' {
            let ch = rest[index..].chars().next()?;
            expanded.push(ch);
            index += ch.len_utf8();
            continue;
        }
        let mut cursor = index + 1;
        let braced = bytes.get(cursor) == Some(&b'{');
        if braced {
            cursor += 1;
        }
        let start = cursor;
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'_')
        {
            cursor += 1;
        }
        if start == cursor {
            expanded.push('$');
            index += 1;
            continue;
        }
        let name = &rest[start..cursor];
        if braced {
            if bytes.get(cursor) != Some(&b'}') {
                return None;
            }
            cursor += 1;
        }
        expanded.push_str(env.non_empty(name)?);
        index = cursor;
    }
    Some(PathBuf::from(expanded))
}

/// The first `config_paths` entry that resolves — the write/check target.
///
/// Preference order is adapter data on purpose: a launcher that regenerates a
/// directory must list its *source* file first, so the generated copy is never
/// the target (`docs/harness-adapters.md`).
pub fn config_path(adapter: &Adapter, env: &Env) -> (Option<PathBuf>, Vec<String>) {
    for candidate in &adapter.config_paths {
        if let Some(resolved) = expand_path(candidate, env) {
            return (Some(resolved), Vec::new());
        }
    }
    if adapter.config_paths.is_empty() {
        return (None, Vec::new());
    }
    (
        None,
        vec![format!(
            "adapter {}: none of its config paths resolve ({})",
            adapter.agent,
            adapter.config_paths.join(", ")
        )],
    )
}

/// Can this harness carry a value of `kind` at all?
pub const MODE_NONE: &str = "none";

pub fn supports(adapter: &Adapter, kind: &str) -> bool {
    adapter.supports.iter().any(|listed| listed == kind)
}

/// Does this harness expand `$VAR` at `delivery` (`header`, `query`, `body`)?
pub fn interpolates(adapter: &Adapter, delivery: &str) -> bool {
    adapter.interpolates.iter().any(|point| point == delivery)
}

/// Would this value be read as *two* headers by a comma-splitting parser?
///
/// Gemini's `parseCustomHeaders` splits on `,(?=\s*[^,:]+:)`, so
/// `{"team":"core","feature":"billing"}` becomes `X-AoE-Tags: {"team":"core"`
/// plus a header named `"feature"`, and the request is dropped entirely
/// (`docs/harness-evidence/gemini.json`). A comma that is not followed by
/// `name:` (`a,b`) survives, so the check is on the value, not on the presence
/// of a comma.
pub fn comma_splits(value: &str) -> bool {
    let bytes = value.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b',' {
            continue;
        }
        let mut rest = value[index + 1..].chars().peekable();
        // `\s*` then one or more characters that are neither `,` nor `:`, then `:`.
        while matches!(rest.peek(), Some(c) if c.is_whitespace()) {
            rest.next();
        }
        // `[^,:]+` is one or more characters, then the colon that ends a name.
        let mut chars = rest.by_ref().skip_while(|c| *c == ',');
        let mut hit_colon = false;
        if let Some(first) = chars.next() {
            if first != ':' {
                hit_colon = chars.take_while(|c| *c != ',').any(|c| c == ':');
            }
        }
        if hit_colon {
            return true;
        }
    }
    false
}

/// The reason a `splits_on_commas` harness cannot carry this value, if any.
pub fn comma_refusal(adapter: &Adapter, name: &str, value: &str) -> Option<String> {
    if !adapter.splits_on_commas || !comma_splits(value) {
        return None;
    }
    Some(
        format!(
            "adapter {} reads {} as a header list split on `,(name:)`, and this value contains ",
            adapter.agent, name
        ) + "one: the harness would truncate it or send no request at all. \
       Use `template = \"{tags_b64}\"` (the same JSON, base64url-encoded so it cannot \
       be split) or a comma-free template such as \
       `\"{project}/{feature}\"`",
    )
}

/// The value the mapping line and the writer put at `delivery`:
/// an env reference where the harness expands it, else the literal.
pub fn delivery_value(adapter: &Adapter, target: &Target, kind: &str) -> Option<String> {
    if interpolates(adapter, kind) {
        if adapter.guard_fallback.is_empty() {
            return Some(env_reference(adapter, &target.env_var));
        }
        return Some(guarded_reference(adapter, target));
    }
    if target.literal.is_empty() {
        return None;
    }
    Some(target.literal.clone())
}

/// How a row spells "read this variable", from its `env_ref`.
///
/// The default is `${env_var}`, and the whole `${env_var}` token is replaced
/// before the bare `{env_var}` one — otherwise `${env_var}` would render as
/// `$NAME`, which is a different thing in a JSON or YAML value: `${NAME}` is the
/// spelling hermes, qwen, droid and openclaw expand, `$NAME` the one pi and
/// gemini take, and codex wants the bare name because `env_http_headers` already
/// means "read a variable".
pub fn env_reference(adapter: &Adapter, env_var: &str) -> String {
    adapter
        .env_ref
        .replace("${env_var}", &format!("${{{env_var}}}"))
        .replace("{env_var}", env_var)
}

/// A value that keeps working when the variable behind it stops existing.
///
/// The harness runs this as a command every time it resolves the value, so the
/// variable still wins while the hook mints it; once the plugin is gone the
/// command is all that is left and it prints `guard_fallback`. It is a command
/// rather than `$VAR` because no shell-free form can stay non-empty when the
/// variable is missing, and a harness that resolves headers strictly reads
/// "missing" as "fail the request" rather than "send no header".
///
/// `${VAR-}` and `printf` are shell builtins, so the command needs nothing from
/// the image it runs in (a sandbox may have no `printenv`, and a missing binary
/// would turn the guard into the very failure it exists to prevent). `[ -n "$v" ]`
/// also covers set-but-empty, which is what a target whose template renders
/// nothing exports — a command that printed nothing is a failure to the harness
/// again.
pub fn guarded_reference(adapter: &Adapter, target: &Target) -> String {
    let name = &target.env_var;
    format!(
        "!v=\"${{{name}-}}\"; [ -n \"$v\" ] && printf %s \"$v\" || printf %s {}",
        adapter.guard_fallback
    )
}

/// One printable mapping line for one header (`None` when the row has no format).
pub fn mapping_line(adapter: &Adapter, target: &Target, env: &Env) -> Option<String> {
    let (path, _) = config_path(adapter, env);
    let path = path?;
    if adapter.mapping_format.is_empty() {
        return None;
    }
    let provider = provider_id(adapter, &path);
    let provider = if provider.is_empty() {
        "<provider-id>".to_string()
    } else {
        provider
    };
    if !supports(adapter, &target.kind) {
        return None;
    }
    let value = delivery_value(adapter, target, &target.kind)?;
    // The line shows the value inside a quoted string in the file the user edits:
    // a JSON row needs the quotes of a command value escaped, so what is printed
    // is what can be pasted.
    let value = if adapter.config_format == "json" {
        json_escape(&value)
    } else {
        value
    };
    let template = match target.kind.as_str() {
        "query" => "{file} → {provider} baseUrl query \"{param}\" = \"{value}\"",
        "body" => "{file} → {provider} models[].samplingParams.\"{param}\" = {value}",
        _ => adapter.mapping_format.as_str(),
    };
    Some(
        template
            .replace("{file}", &path.to_string_lossy())
            .replace("{header}", &target.name)
            .replace("{env_ref}", &env_reference(adapter, &target.env_var))
            .replace("{env_var}", &target.env_var)
            .replace("{param}", target.param_name())
            .replace("{value}", &value)
            .replace("{provider}", &provider),
    )
}

/// Escape the characters that cannot appear raw inside a JSON string.
pub fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// The provider entry to name in a printed line, when the config already says.
///
/// Only a config the user already has can be named; otherwise the line carries
/// `<provider-id>` rather than guessing an id the harness does not use. For pi
/// the entry must have a non-empty `models` list, because a launcher that
/// rebuilds the provider drops a headers-only entry.
pub fn provider_id(adapter: &Adapter, path: &Path) -> String {
    let Ok(data) = read_config(adapter, path) else {
        return String::new();
    };
    let mut rows = providers(adapter, &data);
    if adapter.agent == "pi" {
        rows.retain(|row| row.models > 0);
    }
    if rows.len() == 1 {
        rows[0].id.clone()
    } else {
        String::new()
    }
}

/// One provider entry and the header table inside it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Provider {
    pub id: String,
    pub headers: std::collections::BTreeMap<String, String>,
    pub models: usize,
}

/// Parse a harness config file according to its declared format.
pub fn read_config(adapter: &Adapter, path: &Path) -> Result<serde_json::Value, String> {
    let raw = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    match adapter.config_format.as_str() {
        "json" => serde_json::from_str(&raw).map_err(|err| err.to_string()),
        "toml" => {
            let table = raw.parse::<toml::Table>().map_err(|err| err.to_string())?;
            serde_json::to_value(table).map_err(|err| err.to_string())
        }
        other => Err(format!(
            "adapter {} has no readable config format ({other})",
            adapter.agent
        )),
    }
}

/// Every provider entry the adapter's format defines, with its header table.
pub fn providers(adapter: &Adapter, data: &serde_json::Value) -> Vec<Provider> {
    let Some(root) = data.as_object() else {
        return Vec::new();
    };
    let (container, headers_key) = match adapter.agent.as_str() {
        "codex" => ("model_providers", "env_http_headers"),
        "pi" => ("providers", "headers"),
        "opencode" => ("provider", "options"),
        _ => return Vec::new(),
    };
    let Some(Some(entries)) = root.get(container).map(serde_json::Value::as_object) else {
        return Vec::new();
    };

    let mut rows = Vec::new();
    for (id, entry) in entries {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let entry = if adapter.agent == "opencode" {
            entry
                .get("options")
                .and_then(serde_json::Value::as_object)
                .unwrap_or(entry)
        } else {
            entry
        };
        let headers = entry
            .get(headers_key)
            .and_then(serde_json::Value::as_object)
            .map(|headers| {
                headers
                    .iter()
                    .map(|(key, value)| (key.clone(), value_repr(value)))
                    .collect()
            })
            .unwrap_or_default();
        let models = entry
            .get("models")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len)
            .unwrap_or(0);
        rows.push(Provider {
            id: id.clone(),
            headers,
            models,
        });
    }
    rows
}

fn value_repr(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree_rows() -> Vec<Adapter> {
        let (rows, warnings) = load(None);
        assert!(warnings.is_empty(), "{warnings:?}");
        rows
    }

    #[test]
    fn every_row_parses() {
        let rows = worktree_rows();
        assert!(rows.len() >= 10, "{} rows", rows.len());
        assert!(rows.iter().all(|row| !row.agent.is_empty()));
    }

    #[test]
    fn tool_prefixes_and_names_resolve() {
        let rows = worktree_rows();
        assert_eq!(
            match_tool(&rows, "pi-proxy").map(|a| a.agent.as_str()),
            Some("pi")
        );
        assert_eq!(
            match_tool(&rows, "pi").map(|a| a.agent.as_str()),
            Some("pi")
        );
        assert_eq!(
            find(&rows, "claude").map(|a| a.agent.as_str()),
            Some("claude")
        );
        assert!(match_tool(&rows, "totally-unknown").is_none());
        assert!(find(&rows, "").is_none());
    }

    #[test]
    fn unset_variable_makes_a_path_unresolvable() {
        let empty = Env::new([("HOME", "/h")]);
        assert!(expand_path("$NOPE/models.json", &empty).is_none());
        assert_eq!(
            expand_path("~/.pi/agent/models.json", &empty),
            Some(PathBuf::from("/h/.pi/agent/models.json"))
        );
        let cfg = Env::new([("CFG", "/c")]);
        assert_eq!(
            expand_path("$CFG/x.json", &cfg),
            Some(PathBuf::from("/c/x.json"))
        );
    }

    #[test]
    fn a_missing_file_falls_back_to_the_built_in_copy() {
        let (rows, warnings) = load(Some(Path::new("/nonexistent-llm-tags")));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(rows.len(), worktree_rows().len());
    }

    #[test]
    fn a_broken_file_is_a_warning_not_a_panic() {
        let dir = std::env::temp_dir().join(format!("llm-tags-adapters-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(ADAPTERS_FILENAME), "not = = toml\n").unwrap();
        let (rows, warnings) = load(Some(&dir));
        assert!(rows.is_empty());
        assert!(warnings[0].contains("could not be read"), "{warnings:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_overlay_row_for_an_agent_aoe_writes_hooks_for_falls_back_to_a_reference() {
        let dir = std::env::temp_dir().join(format!("llm-tags-hooks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(ADAPTERS_FILENAME),
            r#"
[[adapter]]
agent = "codex"
aoe_hooks = true
write_mode = "overlay_dir"
overlay_env = "CODEX_HOME"
overlay_file = "config.toml"
writer = "codex_toml"
supports = ["header", "query"]
"#,
        )
        .unwrap();
        let (rows, warnings) = load(Some(&dir));
        assert_eq!(rows[0].write_mode, MODE_NONE);
        assert_eq!(rows[0].supports, vec!["header"]);
        assert!(warnings[0].contains("hook path moves"), "{warnings:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_header_list_parser_splits_on_a_comma_before_a_name() {
        // Gemini's parseCustomHeaders splits on `,(?=\s*[^,:]+:)`.
        assert!(comma_splits(r#"{"a":1,"b":2}"#));
        assert!(comma_splits("team:core,feature:billing"));
        assert!(comma_splits("{x},other: 1"));
        // A comma with nothing name-shaped after it survives untouched.
        assert!(!comma_splits("a,b"));
        assert!(!comma_splits("a, b, c"));
        assert!(!comma_splits(r#"{"tags_b64":"eyJhIjoxfQ"}"#));
        assert!(!comma_splits(""));
    }

    #[test]
    fn an_empty_supports_list_is_taken_literally() {
        // `supports = []` is how a row says "no channel"; defaulting it to a
        // header would let `setup` write a delivery the harness drops.
        let sandbox = std::env::temp_dir().join(format!("adapters-{}", std::process::id()));
        let plugin = sandbox.join("plugin");
        std::fs::create_dir_all(plugin.join("bin")).expect("dirs");
        std::fs::write(
            plugin.join("adapters.toml"),
            "[[adapter]]\nagent = \"mute\"\nmechanism = \"none\"\nsupports = []\nstatus = \"documented\"\n\n             [[adapter]]\nagent = \"plain\"\nmechanism = \"config_headers\"\n",
        )
        .expect("table");
        let (rows, _) = load(Some(&plugin));
        let row = |agent: &str| rows.iter().find(|row| row.agent == agent).expect("row");
        assert!(
            row("mute").supports.is_empty(),
            "{:?}",
            row("mute").supports
        );
        assert_eq!(row("plain").supports, vec!["header".to_string()]);
        let _ = std::fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn a_reference_keeps_the_braces_the_row_asked_for() {
        // The default `${env_var}` must render `${NAME}`, not `$NAME`: a JSON or
        // YAML value expands only the braced spelling, and a bare name is not a
        // reference at all. codex wants the bare name, so its row says so.
        let (rows, warnings) = load(None);
        assert!(warnings.is_empty(), "{warnings:?}");
        let row = |agent: &str| rows.iter().find(|row| row.agent == agent).expect("row");
        let openclaw = row("openclaw");
        assert_eq!(openclaw.env_ref, "${env_var}");
        assert_eq!(env_reference(openclaw, "AOE_LLM_TAGS"), "${AOE_LLM_TAGS}");
        let codex = row("codex");
        assert_eq!(env_reference(codex, "AOE_LLM_TAGS"), "AOE_LLM_TAGS");
    }

    #[test]
    fn the_gemini_row_refuses_a_value_its_parser_would_split() {
        let dir = std::env::temp_dir().join(format!("aoe-adapters-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(ADAPTERS_FILENAME),
            r#"
[[adapter]]
agent = "gemini"
mechanism = "custom_headers_env"
mapping_format = 'GEMINI_CLI_CUSTOM_HEADERS="{header}: ${env_var}"'
interpolates = ["header"]
supports = ["header"]
write_mode = "env_only"
native_env = "GEMINI_CLI_CUSTOM_HEADERS"
native_format = "{header}: {value}"
splits_on_commas = true
aoe_hooks = true
status = "verified"
"#,
        )
        .unwrap();
        let (rows, _) = load(Some(&dir));
        let gemini = &rows[0];
        assert!(gemini.splits_on_commas);
        assert!(comma_refusal(gemini, "X-AoE-Tags", r#"{"a":1,"b":2}"#).is_some());
        assert!(comma_refusal(gemini, "X-AoE-Tags", "S-cf11").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

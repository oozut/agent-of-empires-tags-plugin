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
    /// Delivery kinds this harness can actually carry (`header`, `query`, `body`).
    pub supports: Vec<String>,
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
    (
        rows.iter().filter_map(parse_row).collect::<Vec<_>>(),
        Vec::new(),
    )
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
        supports: {
            let listed = list("supports");
            if listed.is_empty() {
                vec!["header".to_string()]
            } else {
                listed
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
pub fn supports(adapter: &Adapter, kind: &str) -> bool {
    adapter.supports.iter().any(|listed| listed == kind)
}

/// Does this harness expand `$VAR` at `delivery` (`header`, `query`, `body`)?
pub fn interpolates(adapter: &Adapter, delivery: &str) -> bool {
    adapter.interpolates.iter().any(|point| point == delivery)
}

/// The value the mapping line and the writer put at `delivery`:
/// an env reference where the harness expands it, else the literal.
pub fn delivery_value(adapter: &Adapter, target: &Target, kind: &str) -> Option<String> {
    if interpolates(adapter, kind) {
        return Some(adapter.env_ref.replace("{env_var}", &target.env_var));
    }
    if target.literal.is_empty() {
        return None;
    }
    Some(target.literal.clone())
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
    let template = match target.kind.as_str() {
        "query" => "{file} → {provider} baseUrl query \"{param}\" = \"{value}\"",
        "body" => "{file} → {provider} models[].samplingParams.\"{param}\" = {value}",
        _ => adapter.mapping_format.as_str(),
    };
    Some(
        template
            .replace("{file}", &path.to_string_lossy())
            .replace("{header}", &target.name)
            .replace("{env_var}", &target.env_var)
            .replace("{param}", target.param_name())
            .replace("{value}", &value)
            .replace("{provider}", &provider),
    )
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
}

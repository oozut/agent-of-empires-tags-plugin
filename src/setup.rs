//! `setup --print` (M0-T05): the exact lines a user pastes, and nothing else.
//!
//! Two host-hook lines (one per `[host_hooks]` key — installing one key only tags
//! one substrate) plus one mapping line per enabled header for the selected
//! harness. `--print` never writes: applying the mapping is a later, explicit
//! action (`docs/harness-adapters.md` §Auto-discovery).
//!
//! Absolute paths everywhere, resolved once: a hook line that needs the user's
//! `PATH` to mean the same thing inside a container is not reproducible.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::adapters::{self, Adapter};
use crate::env::Env;
use crate::resolver::{self, Flags};
use crate::settings;

pub const HOOK_KEYS: [&str; 2] = ["before_session", "before_start"];

/// The CLI file name inside the plugin tree.
pub const CLI_NAME: &str = "aoe-llm-tags";

/// A setup request that cannot be answered honestly (no adapter, no binary).
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("{cli} is missing or not executable; run `aoe plugin update {plugin}`", plugin = settings::PLUGIN_ID)]
    CliMissing { cli: String },
    #[error("the aoe binary {aoe} is not executable; set AOE_BIN to this host's aoe")]
    AoeMissing { aoe: String },
    #[error("no adapter for tool {tool:?}; pass --adapter <agent> (known: {known})")]
    NoAdapter { tool: String, known: String },
    #[error(
        "adapter {agent} is {status} and has no mapping format yet; \
         no static setup can be printed for it"
    )]
    NoMappingFormat { agent: String, status: String },
}

/// One printable mapping line: what to add, where, and for which variable.
#[derive(Clone, Debug, PartialEq)]
pub struct Mapping {
    pub header: String,
    pub env_var: String,
    pub file: String,
    pub line: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub adapter: Option<Adapter>,
    pub cli: PathBuf,
    pub aoe: String,
    pub hooks: Vec<(&'static str, String)>,
    pub mapping: Vec<Mapping>,
    pub warnings: Vec<String>,
}

/// This plugin's `bin/aoe-llm-tags` (absolute, whatever the install location).
///
/// The installed layout puts the CLI at `<plugin>/bin/aoe-llm-tags`, which is
/// also the running executable, so when that derivation misses (a bare binary
/// copied elsewhere) the executable itself is the honest answer.
pub fn cli_path(root: Option<&Path>) -> PathBuf {
    let path = match root {
        Some(root) => root.join("bin").join(CLI_NAME),
        None => {
            let candidate = resolver::plugin_root().join("bin").join(CLI_NAME);
            if candidate.is_file() {
                candidate
            } else {
                std::env::current_exe().unwrap_or(candidate)
            }
        }
    };
    std::fs::canonicalize(&path).unwrap_or(path)
}

/// The one command installed under both `[host_hooks]` keys.
///
/// Every `AOE_*` value the hook reads is passed explicitly, so the command works
/// in the reduced environment structured host sessions get, and `AOE_BIN` pins
/// the binary the plugin resolved instead of whatever `PATH` happens to hold.
pub fn hook_command(cli: &Path, aoe: &str) -> String {
    format!(
        "AOE_BIN=\"{aoe}\" \"{}\" hook \
         --session \"$AOE_SESSION_ID\" \
         --profile \"$AOE_PROFILE\" \
         --tool \"$AOE_TOOL\" \
         --project \"$AOE_PROJECT_PATH\" \
         --group \"$AOE_GROUP_PATH\" \
         --title \"$AOE_SESSION_TITLE\"",
        cli.display()
    )
}

/// Resolve everything `--print` needs, or return the reason it cannot be honest.
///
/// Uses the same snapshot as `hook`, so the mapped headers are exactly the ones
/// this launch would emit; a header the resolver refuses cannot be mapped.
pub fn build(
    flags: &Flags,
    env: &Env,
    root: Option<&Path>,
    adapters: Option<&[Adapter]>,
) -> Result<Plan, SetupError> {
    let cli = cli_path(root);
    let aoe = resolver::aoe_bin(env);
    let mut warnings = Vec::new();

    if !resolver::is_executable(&cli) {
        return Err(SetupError::CliMissing {
            cli: cli.display().to_string(),
        });
    }
    let aoe_path = Path::new(&aoe);
    if !resolver::is_executable(aoe_path) && !aoe_path.is_dir() {
        return Err(SetupError::AoeMissing { aoe });
    }

    let loaded;
    let adapters: &[Adapter] = match adapters {
        Some(adapters) => adapters,
        None => {
            let (rows, adapter_warnings) = adapters::load(root);
            warnings.extend(adapter_warnings);
            loaded = rows;
            &loaded
        }
    };

    let snap = resolver::snapshot(flags, env, None);
    warnings.extend(snap.warnings.iter().cloned());

    let adapter = adapter_for(&snap.facts.tool, flags, adapters).ok_or_else(|| {
        let known = if adapters.is_empty() {
            "none".to_string()
        } else {
            let mut names: Vec<&str> = adapters.iter().map(|row| row.agent.as_str()).collect();
            names.sort_unstable();
            names.join(", ")
        };
        SetupError::NoAdapter {
            tool: snap.facts.tool.clone(),
            known,
        }
    })?;
    if adapter.mapping_format.is_empty() {
        return Err(SetupError::NoMappingFormat {
            agent: adapter.agent.clone(),
            status: adapter.status.clone(),
        });
    }

    let mut mapping = Vec::new();
    for item in &snap.resolved {
        let Some(line) = adapters::mapping_line(adapter, &item.header, &item.env_var, env) else {
            warnings.push(format!(
                "adapter {}: no config path resolves; cannot map {}",
                adapter.agent, item.header
            ));
            continue;
        };
        let (path, _) = adapters::config_path(adapter, env);
        mapping.push(Mapping {
            header: item.header.clone(),
            env_var: item.env_var.clone(),
            file: path
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            line,
        });
    }

    let command = hook_command(&cli, &aoe);
    Ok(Plan {
        adapter: Some(adapter.clone()),
        cli,
        aoe,
        hooks: HOOK_KEYS
            .iter()
            .map(|key| (*key, command.clone()))
            .collect(),
        mapping,
        warnings,
    })
}

/// The printable lines: two hook lines, then one line per enabled header.
pub fn text(plan: &Plan) -> Vec<String> {
    let mut lines: Vec<String> = plan
        .hooks
        .iter()
        .map(|(key, command)| format!("{key} = '{command}'"))
        .collect();
    lines.extend(plan.mapping.iter().map(|item| item.line.clone()));
    lines
}

pub fn as_dict(plan: &Plan) -> serde_json::Value {
    json!({
        "adapter": plan.adapter.as_ref().map(|adapter| adapter.agent.clone()).unwrap_or_default(),
        "aoe": plan.aoe,
        "cli": plan.cli.to_string_lossy(),
        "hooks": plan.hooks.iter().cloned().collect::<std::collections::BTreeMap<_, _>>(),
        "mapping": plan.mapping.iter().map(|item| json!({
            "header": item.header,
            "env_var": item.env_var,
            "file": item.file,
            "line": item.line,
        })).collect::<Vec<_>>(),
        "warnings": plan.warnings,
    })
}

fn adapter_for<'a>(tool: &str, flags: &Flags, adapters: &'a [Adapter]) -> Option<&'a Adapter> {
    if !flags.adapter.is_empty() {
        return adapters::find(adapters, &flags.adapter);
    }
    adapters::match_tool(adapters, tool)
}

/// Apply the pi mapping to the resolved config file (backup + atomic rename).
pub fn apply_cmd(flags: &crate::resolver::Flags, env: &crate::env::Env) -> i32 {
    let plan = match build(flags, env, None, None) {
        Ok(plan) => plan,
        Err(err) => {
            crate::cli::warn(&err.to_string());
            return crate::cli::EXIT_FAIL;
        }
    };
    let Some(adapter) = plan.adapter.clone() else {
        crate::cli::warn("no adapter resolved");
        return crate::cli::EXIT_FAIL;
    };
    if adapter.agent != "pi" {
        crate::cli::warn(&format!(
            "apply is implemented for pi only (adapter {})",
            adapter.agent
        ));
        return crate::cli::EXIT_FAIL;
    }
    let (path, path_warnings) = crate::adapters::config_path(&adapter, env);
    let Some(path) = path else {
        for warning in path_warnings {
            crate::cli::warn(&warning);
        }
        return crate::cli::EXIT_FAIL;
    };
    if let Err(err) = guard_regular_file(&path) {
        crate::cli::warn(&err);
        return crate::cli::EXIT_FAIL;
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) => {
            crate::cli::warn(&format!("{} could not be read: {err}", path.display()));
            return crate::cli::EXIT_FAIL;
        }
    };
    let mut data: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(err) => {
            crate::cli::warn(&format!("{} is not valid JSON: {err}", path.display()));
            return crate::cli::EXIT_FAIL;
        }
    };

    let backup = backup_path(&path);
    if !backup.exists() {
        if let Err(err) = std::fs::copy(&path, &backup) {
            crate::cli::warn(&format!("backup failed: {err}"));
            return crate::cli::EXIT_FAIL;
        }
    }

    // Find the provider entry that carries a non-empty models list.
    let Some(providers) = data.get_mut("providers").and_then(|v| v.as_object_mut()) else {
        crate::cli::warn("no providers object in config");
        return crate::cli::EXIT_FAIL;
    };
    let mut target: Option<String> = None;
    for (name, value) in providers.iter() {
        let models = value
            .get("models")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        if models {
            target = Some(name.clone());
            break;
        }
    }
    let target = match target {
        Some(name) => name,
        None => {
            crate::cli::warn("no provider entry with a non-empty models list");
            return crate::cli::EXIT_FAIL;
        }
    };

    let headers = providers
        .get_mut(&target)
        .and_then(|v| v.as_object_mut())
        .and_then(|obj| {
            if !obj.contains_key("headers") {
                obj.insert("headers".to_string(), serde_json::json!({}));
            }
            obj.get_mut("headers").and_then(|v| v.as_object_mut())
        });
    let Some(headers) = headers else {
        crate::cli::warn("provider headers is not an object");
        return crate::cli::EXIT_FAIL;
    };
    for item in &plan.mapping {
        headers.insert(
            item.header.clone(),
            serde_json::Value::String(format!("${}", item.env_var)),
        );
    }

    let new = match serde_json::to_string_pretty(&data) {
        Ok(text) => text,
        Err(err) => {
            crate::cli::warn(&format!("serialize failed: {err}"));
            return crate::cli::EXIT_FAIL;
        }
    };
    let tmp = path.with_extension("aoe-llm-tags.tmp");
    if let Err(err) = std::fs::write(&tmp, &new) {
        crate::cli::warn(&format!("write failed: {err}"));
        return crate::cli::EXIT_FAIL;
    }
    if let Err(err) = std::fs::rename(&tmp, &path) {
        crate::cli::warn(&format!("rename failed: {err}"));
        return crate::cli::EXIT_FAIL;
    }

    // Re-read and re-parse.
    match std::fs::read_to_string(&path) {
        Ok(reread) => {
            if serde_json::from_str::<serde_json::Value>(&reread).is_err() {
                crate::cli::warn("re-parse failed after write");
                return crate::cli::EXIT_FAIL;
            }
        }
        Err(err) => {
            crate::cli::warn(&format!("re-read failed: {err}"));
            return crate::cli::EXIT_FAIL;
        }
    }

    println!(
        "applied {} header(s) to {}",
        plan.mapping.len(),
        path.display()
    );
    println!("backup: {}", backup.display());
    crate::cli::EXIT_OK
}

pub fn undo_cmd(flags: &crate::resolver::Flags, env: &crate::env::Env) -> i32 {
    let plan = match build(flags, env, None, None) {
        Ok(plan) => plan,
        Err(err) => {
            crate::cli::warn(&err.to_string());
            return crate::cli::EXIT_FAIL;
        }
    };
    let Some(adapter) = plan.adapter.clone() else {
        crate::cli::warn("no adapter resolved");
        return crate::cli::EXIT_FAIL;
    };
    let (path, _) = crate::adapters::config_path(&adapter, env);
    let Some(path) = path else {
        crate::cli::warn("no config path");
        return crate::cli::EXIT_FAIL;
    };
    let backup = backup_path(&path);
    if !backup.exists() {
        crate::cli::warn(&format!("no backup at {}", backup.display()));
        return crate::cli::EXIT_FAIL;
    }
    if let Err(err) = guard_regular_file(&path) {
        crate::cli::warn(&err);
        return crate::cli::EXIT_FAIL;
    }
    // Restore exactly, then remove backup.
    if let Err(err) = std::fs::rename(&backup, &path) {
        crate::cli::warn(&format!("restore failed: {err}"));
        return crate::cli::EXIT_FAIL;
    }
    // Re-read and re-parse.
    match std::fs::read_to_string(&path) {
        Ok(reread) => {
            if adapter.config_format == "json"
                && serde_json::from_str::<serde_json::Value>(&reread).is_err()
            {
                crate::cli::warn("re-parse failed after restore");
                return crate::cli::EXIT_FAIL;
            }
        }
        Err(err) => {
            crate::cli::warn(&format!("re-read failed: {err}"));
            return crate::cli::EXIT_FAIL;
        }
    }
    println!("restored {} from backup", path.display());
    crate::cli::EXIT_OK
}

fn backup_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".aoe-llm-tags.bak");
    path.with_file_name(name)
}

fn guard_regular_file(path: &std::path::Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|err| err.to_string())?;
    if meta.file_type().is_symlink() {
        return Err(format!("{} is a symlink; refusing", path.display()));
    }
    if !meta.is_file() {
        return Err(format!(
            "{} is not a regular file; refusing",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_command_pins_aoe_and_passes_every_variable() {
        let command = hook_command(Path::new("/p/bin/aoe-llm-tags"), "/usr/local/bin/aoe");
        assert!(command.starts_with("AOE_BIN=\"/usr/local/bin/aoe\" \"/p/bin/aoe-llm-tags\" hook"));
        for variable in [
            "AOE_SESSION_ID",
            "AOE_PROFILE",
            "AOE_TOOL",
            "AOE_PROJECT_PATH",
            "AOE_GROUP_PATH",
            "AOE_SESSION_TITLE",
        ] {
            assert!(command.contains(&format!("\"${variable}\"")), "{variable}");
        }
    }
}

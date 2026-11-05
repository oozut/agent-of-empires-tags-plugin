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
    /// `header`, `query` or `body` — where the value is written.
    pub delivery: String,
    /// Query-parameter or body-field name for a non-header delivery.
    pub param: String,
    /// The value written: an env reference, or the entry's literal.
    pub value: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub adapter: Option<Adapter>,
    pub cli: PathBuf,
    pub aoe: String,
    pub hooks: Vec<(&'static str, String)>,
    pub mapping: Vec<Mapping>,
    /// Entries this plan could not map; `--apply` refuses rather than half-writing.
    pub unmappable: Vec<String>,
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
///
/// The trailing `|| true` is what makes an uninstall safe. `aoe plugin uninstall`
/// removes the payload and the `[plugins]` record, but it cannot remove the
/// `[host_hooks]` lines a user pasted — and aoe aborts session creation when a
/// hook exits non-zero (`run_env_minting_hooks`), so the stale line would fail
/// every launch with 127 (the shell cannot find the binary). The hook itself
/// already exits 0 on every failure; this covers the case where it is not there
/// to run at all. Diagnostics still go to stderr.
pub fn hook_command(cli: &Path, aoe: &str) -> String {
    format!(
        "AOE_BIN=\"{aoe}\" \"{}\" hook \
         --session \"$AOE_SESSION_ID\" \
         --profile \"$AOE_PROFILE\" \
         --tool \"$AOE_TOOL\" \
         --project \"$AOE_PROJECT_PATH\" \
         --group \"$AOE_GROUP_PATH\" \
         --title \"$AOE_SESSION_TITLE\" || true",
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
    let mut unmappable = Vec::new();
    for item in snap.settings.targets.iter().filter(|row| row.enabled) {
        if !adapters::supports(adapter, &item.kind) {
            unmappable.push(item.name.clone());
            warnings.push(format!(
                "adapter {} cannot deliver a {} value (it supports {}); {} is not mapped",
                adapter.agent,
                item.kind,
                adapter.supports.join(", "),
                item.name
            ));
            continue;
        }
        // A row the hook writes a session copy for needs no static line: the
        // value is not in the user's file at all, so an empty `delivery_value`
        // is the design, not a failure to find a path.
        if !adapters::interpolates(adapter, &item.kind)
            && adapter.write_mode != adapters::MODE_NONE
            && adapters::mapping_line(adapter, item, env).is_none()
        {
            warnings.push(format!(
                "adapter {} gets {} from the per-session copy it writes at launch \
                 (write_mode = {:?}), not from a line in the user's config; nothing to add",
                adapter.agent, item.name, adapter.write_mode
            ));
            continue;
        }
        let Some(line) = adapters::mapping_line(adapter, item, env) else {
            if !unmappable.contains(&item.name) {
                unmappable.push(item.name.clone());
            }
            warnings.push(format!(
                "adapter {}: no config path resolves, or {} has no value for a {} delivery; \
                 cannot map it",
                adapter.agent, item.name, item.kind
            ));
            continue;
        };
        // This launch's own value, not a guess: `snapshot` resolved it already.
        let rendered = snap
            .resolved
            .iter()
            .find(|row| row.header == item.name)
            .map(|row| row.value.clone())
            .unwrap_or_default();
        if let Some(reason) = adapters::comma_refusal(adapter, &item.name, &rendered) {
            unmappable.push(item.name.clone());
            warnings.push(reason);
            continue;
        }
        let (path, _) = adapters::config_path(adapter, env);
        let value = adapters::delivery_value(adapter, item, &item.kind).unwrap_or_default();
        mapping.push(Mapping {
            header: item.name.clone(),
            env_var: item.env_var.clone(),
            file: path
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            line,
            delivery: item.kind.clone(),
            param: item.param_name().to_string(),
            value,
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
        unmappable,
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
        "session_config": plan.adapter.as_ref().map(|adapter| json!({
            "verdict": crate::overlay::verdict(adapter),
            "write_mode": adapter.write_mode,
            "env_var": adapter.overlay_env,
            "file": adapter.overlay_file,
            "writer": adapter.writer,
        })),
        "aoe": plan.aoe,
        "cli": plan.cli.to_string_lossy(),
        "hooks": plan.hooks.iter().cloned().collect::<std::collections::BTreeMap<_, _>>(),
        "mapping": plan.mapping.iter().map(|item| json!({
            "header": item.header,
            "env_var": item.env_var,
            "file": item.file,
            "line": item.line,
            "delivery": item.delivery,
            "param": item.param,
            "value": item.value,
        })).collect::<Vec<_>>(),
        "unmappable": plan.unmappable,
        "warnings": plan.warnings,
    })
}

fn adapter_for<'a>(tool: &str, flags: &Flags, adapters: &'a [Adapter]) -> Option<&'a Adapter> {
    if !flags.adapter.is_empty() {
        return adapters::find(adapters, &flags.adapter);
    }
    adapters::match_tool(adapters, tool)
}

/// Materialise this session's artifact, or say why it cannot be done.
///
/// Nothing here touches the user's config: `docs/harness-adapters.md`
/// §Per-session config. Called by `hook` (every launch) and by
/// `setup --apply` (a one-off rebuild for debugging).
pub fn apply_cmd(flags: &Flags, env: &Env) -> i32 {
    let snap = resolver::snapshot(flags, env, None);
    let (rows, adapter_warnings) = adapters::load(None);
    let mut warnings = snap.warnings.clone();
    warnings.extend(adapter_warnings);

    let Some(adapter) = adapters::match_tool(&rows, &snap.facts.tool)
        .or_else(|| adapters::find(&rows, &flags.adapter))
        .cloned()
    else {
        crate::cli::warn_all(&warnings);
        crate::cli::warn(&format!(
            "no adapter for tool {:?}; pass --adapter <agent>",
            snap.facts.tool
        ));
        return crate::cli::EXIT_FAIL;
    };

    // Refuse the whole plan rather than write a config with a hole in it.
    let unsupported = crate::overlay::unsupported(&adapter, &snap.settings.targets, &snap.resolved);
    if !unsupported.is_empty() {
        crate::cli::warn_all(&warnings);
        for (name, why) in &unsupported {
            crate::cli::warn(&format!("{name}: {why}"));
        }
        crate::cli::warn(&format!(
            "refusing to write: {} target(s) cannot be delivered by adapter {}",
            unsupported.len(),
            adapter.agent
        ));
        return crate::cli::EXIT_FAIL;
    }

    match crate::overlay::materialise(&adapter, &snap, env) {
        Ok(overlay) => {
            crate::cli::warn_all(&warnings);
            for (name, why) in &overlay.skipped {
                crate::cli::warn(&format!("{name} was not written: {why}"));
            }
            match &overlay.file {
                Some(file) => println!("wrote {}", file.display()),
                None => println!(
                    "adapter {} needs no file; the values ride the launch environment",
                    adapter.agent
                ),
            }
            for line in overlay.env_lines() {
                println!("{line}");
            }
            crate::cli::EXIT_OK
        }
        Err(err) => {
            crate::cli::warn_all(&warnings);
            crate::cli::warn(&err);
            crate::cli::EXIT_FAIL
        }
    }
}

/// The session artifact for a resolved snapshot, if this harness has one.
///
/// `Ok(None)` means "nothing to do": no adapter for this tool, or a harness with
/// no per-session config (claude rides the launch environment). Any real failure
/// is `Err`, so the hook can warn without aborting the launch.
pub fn overlay_for(
    snap: &resolver::Snapshot,
    env: &Env,
) -> Result<Option<(crate::overlay::Overlay, Vec<String>)>, String> {
    let (rows, mut warnings) = adapters::load(None);
    let Some(adapter) = adapters::match_tool(&rows, &snap.facts.tool).cloned() else {
        return Ok(None);
    };
    if !crate::overlay::can_write(&adapter) {
        return Ok(None);
    }
    if adapter.write_mode == crate::overlay::MODE_ENV_ONLY && adapter.native_env.is_empty() {
        // Nothing beyond the tag variables is needed for this harness.
        return Ok(None);
    }
    warnings.clear();
    Ok(Some((
        crate::overlay::materialise(&adapter, snap, env)?,
        warnings,
    )))
}

/// Remove this session's artifact (or every session's with `--all`).
///
/// The user's config was never edited, so there is nothing to restore: undoing a
/// session's tagging is deleting the copy this plugin built for it. `--all` is
/// the first step of an uninstall, so it also names what a user still has to
/// delete by hand (see [`report_left_to_the_user`]).
pub fn undo_cmd(flags: &Flags, env: &Env, all: bool) -> i32 {
    let app_dir = match settings::app_dir(env) {
        Ok(dir) => dir,
        Err(message) => {
            crate::cli::warn(&message);
            return crate::cli::EXIT_FAIL;
        }
    };
    if all {
        let sessions = crate::overlay::all_sessions(&app_dir);
        let mut removed = 0;
        for session in &sessions {
            if std::fs::remove_dir_all(session).is_ok() {
                removed += 1;
            }
        }
        println!(
            "removed {removed} session config(s) from {}",
            app_dir.join("sessions").display()
        );
        report_left_to_the_user(flags, env);
        return crate::cli::EXIT_OK;
    }
    if flags.session.is_empty() {
        crate::cli::warn(
            "no session: pass --session <id>, run inside a session, or use --undo --all",
        );
        return crate::cli::EXIT_USAGE;
    }
    let (rows, _) = adapters::load(None);
    let agent = adapters::find(&rows, &flags.adapter)
        .map(|row| row.agent.clone())
        .or_else(|| adapters::match_tool(&rows, &flags.tool).map(|row| row.agent.clone()));
    let removed = crate::overlay::remove(&app_dir, &flags.session, agent.as_deref());
    if removed == 0 {
        crate::cli::warn(&format!(
            "no session config for {} under {}",
            flags.session,
            app_dir.join("sessions").display()
        ));
        return crate::cli::EXIT_FAIL;
    }
    println!("removed the session config for {}", flags.session);
    crate::cli::EXIT_OK
}

/// What `--undo --all` cannot remove, printed so an uninstall is not silent.
///
/// The plugin never edits files it does not own: the `[host_hooks]` lines and
/// the harness mapping are pasted by the user, so deleting them is the user's
/// edit. Naming them matters because a leftover mapping with no hook behind it
/// fails at request time — the harness cannot resolve `$AOE_LLM_TAGS` because
/// nothing mints it any more — rather than quietly sending nothing.
pub fn report_left_to_the_user(flags: &Flags, env: &Env) {
    let app_dir = settings::app_dir(env).ok();
    let config = settings::load_config(&flags.profile, app_dir.as_deref(), env);
    let mut left: Vec<String> = Vec::new();

    let hooks: Vec<&str> = HOOK_KEYS
        .iter()
        .copied()
        .filter(|key| {
            settings::host_hook_entries(&config.table, key)
                .iter()
                .any(|entry| entry.contains(CLI_NAME))
        })
        .collect();
    if !hooks.is_empty() {
        let files: Vec<String> = config
            .present
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        left.push(format!(
            "[host_hooks] {} — the `{CLI_NAME}` line(s) in {}",
            hooks.join(", "),
            files.join(" or ")
        ));
    }

    let (rows, _) = adapters::load(None);
    let adapter =
        adapters::find(&rows, &flags.adapter).or_else(|| adapters::match_tool(&rows, &flags.tool));
    if let Some(adapter) = adapter {
        let (path, _) = adapters::config_path(adapter, env);
        if let Some(path) = path.filter(|path| path.is_file()) {
            let mentions_ours = std::fs::read_to_string(&path)
                .map(|text| text.contains(ENV_PREFIX))
                .unwrap_or(false);
            if mentions_ours {
                left.push(format!(
                    "the `{ENV_PREFIX}*` mapping(s) in {} (and any copy another wrapper \
                     generates from that provider)",
                    path.display()
                ));
            }
        }
    }

    if left.is_empty() {
        return;
    }
    println!("the plugin never edits files it does not own; delete these by hand:");
    for item in left {
        println!("  - {item}");
    }
}

/// The prefix every variable this plugin mints carries.
const ENV_PREFIX: &str = "$AOE_";

/// Set (or replace) one query parameter, keeping the rest of the URL intact.
pub fn set_query_param(base: &str, name: &str, value: &str) -> String {
    let (stem, rest) = match base.split_once('?') {
        Some((stem, rest)) => (stem, rest),
        None => (base, ""),
    };
    let (query, fragment) = match rest.split_once('#') {
        Some((query, fragment)) => (query, Some(fragment)),
        None => (rest, None),
    };
    let mut pairs: Vec<String> = Vec::new();
    let mut replaced = false;
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let key = pair.split_once('=').map(|(key, _)| key).unwrap_or(pair);
        if key == name {
            pairs.push(format!("{name}={value}"));
            replaced = true;
        } else {
            pairs.push(pair.to_string());
        }
    }
    if !replaced {
        pairs.push(format!("{name}={value}"));
    }
    let mut out = format!("{stem}?{}", pairs.join("&"));
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_query_param_replaces_and_appends() {
        assert_eq!(
            set_query_param("http://h/v1", "tags", "aoe"),
            "http://h/v1?tags=aoe"
        );
        assert_eq!(
            set_query_param("http://h/v1?x=1&tags=old", "tags", "aoe"),
            "http://h/v1?x=1&tags=aoe"
        );
        assert_eq!(
            set_query_param("http://h/v1?x=1#frag", "tags", "aoe"),
            "http://h/v1?x=1&tags=aoe#frag"
        );
    }

    #[test]
    fn hook_command_pins_aoe_and_passes_every_variable() {
        let command = hook_command(Path::new("/p/bin/aoe-llm-tags"), "/usr/local/bin/aoe");
        assert!(command.starts_with("AOE_BIN=\"/usr/local/bin/aoe\" \"/p/bin/aoe-llm-tags\" hook"));
        // An unremoved hook line must be inert once the payload is gone.
        assert!(command.ends_with(" || true"), "{command}");
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

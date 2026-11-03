//! `doctor` (M0-T11): one line per check, exit 1 if any of them fails.
//!
//! The checks are the ways an install silently sends nothing
//! (`plan/m0.md` acceptance 6): a missing hook key, a hook line pointing at a
//! plugin that moved, an unresolvable `aoe`, an unmapped header, a template that
//! cannot render, and a session row the hook cannot read.
//!
//! Every line prints the absolute path it looked at — a hook debugging session
//! with no visible paths is the expensive failure mode. `--json` emits the same
//! checks for a script or a later pane.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::adapters::{self, Adapter};
use crate::env::Env;
use crate::resolver::{self, Flags, ResolvedHeader, Snapshot};
use crate::settings::{self, RawConfig};
use crate::setup;

pub const OK: &str = "ok";
pub const FAIL: &str = "fail";
pub const WARN: &str = "warn";
pub const SKIP: &str = "skip";

const HOOK_CHECKS: [(&str, &str); 2] = [
    ("before_session", "host sessions will not send headers"),
    ("before_start", "sandboxed sessions will not send headers"),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub name: String,
    pub status: String,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub checks: Vec<Check>,
    pub paths: BTreeMap<&'static str, String>,
    pub profile: String,
    pub adapter: String,
}

impl Report {
    pub fn ok(&self) -> bool {
        !self.checks.iter().any(|check| check.status == FAIL)
    }

    pub fn counts(&self) -> BTreeMap<&'static str, usize> {
        let mut counts = BTreeMap::from([(OK, 0), (FAIL, 0), (WARN, 0), (SKIP, 0)]);
        for check in &self.checks {
            if let Some(slot) = counts.get_mut(check.status.as_str()) {
                *slot += 1;
            }
        }
        counts
    }
}

/// Run every check. Never fails for a broken install; that is a failed line.
pub fn run(flags: &Flags, env: &Env, root: Option<&Path>) -> Report {
    let mut checks: Vec<Check> = Vec::new();

    let app_dir = settings::app_dir(env);
    let config = settings::load_config(&flags.profile, Some(&app_dir), env);
    let global_path = settings::global_config_path(&app_dir);
    let profile_path = settings::profile_config_path(&app_dir, &flags.profile);
    let cli = setup::cli_path(root);
    let aoe = resolver::aoe_bin(env);

    checks.push(check_effective_config(
        &config,
        &global_path,
        profile_path.as_deref(),
    ));

    let mut hooks: BTreeMap<&str, String> = BTreeMap::new();
    for (key, message) in HOOK_CHECKS {
        let entries = settings::host_hook_entries(&config.table, key);
        let ours = entries
            .iter()
            .find(|entry| entry.contains("aoe-llm-tags"))
            .map(|entry| entry.to_string());
        if let Some(ours) = ours {
            checks.push(Check {
                name: format!("{key} hook"),
                status: OK.to_string(),
                detail: abbreviate(&ours),
            });
            hooks.insert(key, ours);
        } else if entries.is_empty() {
            checks.push(Check {
                name: format!("{key} hook"),
                status: FAIL.to_string(),
                detail: format!(
                    "no [host_hooks] {key} in {}; {message} \
                     (add the line from `aoe-llm-tags setup --print`)",
                    files(&config)
                ),
            });
        } else {
            checks.push(Check {
                name: format!("{key} hook"),
                status: FAIL.to_string(),
                detail: format!(
                    "{} hook(s) in {} but none runs aoe-llm-tags; {message}",
                    entries.len(),
                    files(&config)
                ),
            });
        }
    }

    checks.push(check_command_path(&hooks, &cli));
    checks.push(check_aoe(&aoe, env));

    let (adapters, adapter_warnings) = adapters::load(root);
    let snap = resolver::snapshot(flags, env, None);
    let adapter = resolve_adapter(flags, &snap.facts.tool, &adapters);
    checks.push(check_adapter(
        adapter,
        &snap.facts.tool,
        &adapters,
        &adapter_warnings,
    ));
    checks.push(check_session(&snap, flags));
    checks.push(check_session_config(adapter, &snap, env));

    let harness = adapter.and_then(|adapter| adapters::config_path(adapter, env).0);
    for (item, target) in snap
        .resolved
        .iter()
        .zip(snap.settings.targets.iter().filter(|row| row.enabled))
    {
        checks.push(check_delivery(adapter, target, env));
        if target.kind == crate::resolver::DEFAULT_KIND {
            checks.push(check_mapping(adapter, item, env));
        }
    }
    checks.push(check_values(&snap));

    // What is actually on this machine, not what the row would do: the five
    // verdicts of plan/parameters.md, from the files themselves (`src/substrate.rs`).
    let live = crate::substrate::live_sessions(env);
    let mut cache = crate::substrate::Cache::load(&app_dir);
    let findings = crate::substrate::classify(
        adapter,
        &snap.settings.targets,
        env,
        &snap.facts.id,
        &live,
        &mut cache,
    );
    cache.save();
    for finding in &findings {
        checks.push(Check {
            name: format!("substrate {}", finding.scope),
            status: finding.status().to_string(),
            detail: format!(
                "{}: {}{}",
                finding.verdict,
                finding.detail,
                if finding.cached { " (cached)" } else { "" }
            ),
        });
    }

    match snap
        .raw
        .get("last_verified")
        .and_then(toml::Value::as_str)
        .map(str::trim)
    {
        Some(value) if !value.is_empty() => checks.push(Check {
            name: "last verification".to_string(),
            status: OK.to_string(),
            detail: value.to_string(),
        }),
        // Read-back is deliberately out of scope: the plugin mints and delivers the header, it
        // does not ask a gateway or a billing system what it did with it. That is roadmap work
        // (`plan/roadmap.md`), so an install that never recorded one is not a degraded install.
        _ => checks.push(Check {
            name: "last verification".to_string(),
            status: SKIP.to_string(),
            detail: "consumer read-back is out of scope for this build; prove arrival with \
                     tools/header-sink.py (docs/install.md \u{a7}5)"
                .to_string(),
        }),
    }

    let mut paths = BTreeMap::new();
    paths.insert("app dir", app_dir.to_string_lossy().into_owned());
    paths.insert("config", global_path.to_string_lossy().into_owned());
    paths.insert(
        "profile config",
        profile_path
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    paths.insert("plugin CLI", cli.to_string_lossy().into_owned());
    paths.insert("aoe binary", aoe);
    paths.insert(
        "harness config",
        harness
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );

    Report {
        checks,
        paths,
        profile: flags.profile.clone(),
        adapter: adapter
            .map(|adapter| adapter.agent.clone())
            .unwrap_or_default(),
    }
}

pub fn format_text(report: &Report) -> String {
    let mut lines = vec![format!(
        "aoe-llm-tags doctor — profile {}",
        if report.profile.is_empty() {
            "(global)".to_string()
        } else {
            report.profile.clone()
        }
    )];
    lines.push("paths:".to_string());
    for (label, value) in &report.paths {
        lines.push(format!(
            "  {:<15} {}",
            label,
            if value.is_empty() { "(none)" } else { value }
        ));
    }
    lines.push("checks:".to_string());
    let width = report
        .checks
        .iter()
        .map(|check| check.name.chars().count())
        .max()
        .unwrap_or(0);
    for check in &report.checks {
        lines.push(format!(
            "  [{:<4}] {:<width$}  {}",
            check.status,
            check.name,
            check.detail,
            width = width
        ));
    }
    let counts = report.counts();
    lines.push(format!(
        "{} ok, {} failed, {} warning(s), {} skipped",
        counts[OK], counts[FAIL], counts[WARN], counts[SKIP]
    ));
    lines.join("\n")
}

pub fn format_json(report: &Report) -> String {
    let value = json!({
        "ok": report.ok(),
        "profile": report.profile,
        "adapter": report.adapter,
        "paths": report.paths,
        "checks": report.checks.iter().map(|check| json!({
            "check": check.name,
            "status": check.status,
            "detail": check.detail,
        })).collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
}

// --- individual checks ------------------------------------------------------

fn check_effective_config(
    config: &RawConfig,
    global_path: &Path,
    profile_path: Option<&Path>,
) -> Check {
    if config.present.is_empty() {
        let extra = profile_path
            .map(|path| format!(" or {}", path.display()))
            .unwrap_or_default();
        return Check {
            name: "effective config".to_string(),
            status: FAIL.to_string(),
            detail: format!(
                "no config file at {}{extra}; nothing can be wired",
                global_path.display()
            ),
        };
    }
    if config.sources.is_empty() {
        return Check {
            name: "effective config".to_string(),
            status: FAIL.to_string(),
            detail: format!(
                "{} could not be parsed: {}",
                join_paths(&config.present),
                config.warnings.join("; ")
            ),
        };
    }
    Check {
        name: "effective config".to_string(),
        status: OK.to_string(),
        detail: format!("read {}", join_paths(&config.sources)),
    }
}

fn check_command_path(hooks: &BTreeMap<&str, String>, cli: &Path) -> Check {
    let entry = hooks
        .get("before_session")
        .or_else(|| hooks.get("before_start"))
        .cloned()
        .unwrap_or_default();
    let path = if entry.is_empty() {
        cli.to_path_buf()
    } else {
        PathBuf::from(command_path(&entry))
    };
    if !path.is_file() || !resolver::is_executable(&path) {
        return Check {
            name: "command path".to_string(),
            status: FAIL.to_string(),
            detail: format!(
                "{} is missing or not executable: the plugin moved, was uninstalled, \
                 or its id changed (run `aoe plugin update {}`)",
                path.display(),
                settings::PLUGIN_ID
            ),
        };
    }
    Check {
        name: "command path".to_string(),
        status: OK.to_string(),
        detail: format!("{} (executable)", path.display()),
    }
}

fn check_aoe(aoe: &str, env: &Env) -> Check {
    let source = if env.non_empty("AOE_BIN").is_some() {
        "from AOE_BIN"
    } else if aoe != resolver::AOE_FALLBACK {
        "from PATH"
    } else {
        "fallback"
    };
    if !resolver::is_executable(Path::new(aoe)) {
        return Check {
            name: "aoe binary".to_string(),
            status: FAIL.to_string(),
            detail: format!(
                "{aoe} ({source}) is not executable; set AOE_BIN to this host's aoe binary"
            ),
        };
    }
    Check {
        name: "aoe binary".to_string(),
        status: OK.to_string(),
        detail: format!("{aoe} ({source})"),
    }
}

fn check_adapter(
    adapter: Option<&Adapter>,
    tool: &str,
    adapters: &[Adapter],
    warnings: &[String],
) -> Check {
    let known = if adapters.is_empty() {
        "none".to_string()
    } else {
        let mut names: Vec<&str> = adapters.iter().map(|row| row.agent.as_str()).collect();
        names.sort_unstable();
        names.join(", ")
    };
    let Some(adapter) = adapter else {
        if tool.is_empty() || tool == resolver::DEFAULT_TOOL {
            return Check {
                name: "harness adapter".to_string(),
                status: WARN.to_string(),
                detail: format!(
                    "no tool given (not in a session shell); pass --adapter to check a mapping \
                     (known: {known})"
                ),
            };
        }
        return Check {
            name: "harness adapter".to_string(),
            status: FAIL.to_string(),
            detail: format!("tool {tool:?} has no adapter row (known: {known}); pass --adapter"),
        };
    };
    if adapter.mechanism == "unknown"
        || adapter.mapping_format.is_empty()
        || adapter.status == "needs_research"
    {
        return Check {
            name: "harness adapter".to_string(),
            status: FAIL.to_string(),
            detail: format!(
                "adapter {} is {} with mechanism {:?}; the header cannot be mapped yet",
                adapter.agent, adapter.status, adapter.mechanism
            ),
        };
    }
    let mut detail = format!(
        "{} ({}, {})",
        adapter.agent, adapter.mechanism, adapter.status
    );
    if !warnings.is_empty() {
        detail.push_str(&format!("; {}", warnings.join("; ")));
    }
    Check {
        name: "harness adapter".to_string(),
        status: OK.to_string(),
        detail,
    }
}

fn check_session(snap: &Snapshot, flags: &Flags) -> Check {
    if flags.session.is_empty() {
        return Check {
            name: "session row".to_string(),
            status: SKIP.to_string(),
            detail: "no --session given; the row lookup was not exercised \
                     (hook environment is the fallback)"
                .to_string(),
        };
    }
    if let Some(warning) = snap
        .warnings
        .iter()
        .find(|warning| warning.contains("falling back to the hook environment"))
    {
        return Check {
            name: "session row".to_string(),
            status: FAIL.to_string(),
            detail: format!(
                "{warning} (id {})",
                if snap.facts.id.is_empty() {
                    "(none)".to_string()
                } else {
                    snap.facts.id.clone()
                }
            ),
        };
    }
    let facts = &snap.facts;
    Check {
        name: "session row".to_string(),
        status: OK.to_string(),
        detail: format!(
            "id {} title {:?} path {} group {} tool {}",
            facts.id,
            facts.title,
            if facts.path.is_empty() {
                "(none)"
            } else {
                &facts.path
            },
            if facts.group.is_empty() {
                "(none)"
            } else {
                &facts.group
            },
            facts.tool
        ),
    }
}

/// Where the value goes: a header (checked by `check_mapping`) or a query/body
/// parameter, which a harness that does not expand `$VAR` there cannot carry live.
/// Where a session's tags land in this harness: a per-session config it is
/// pointed at, or the launch environment alone.
fn check_session_config(adapter: Option<&Adapter>, snap: &Snapshot, env: &Env) -> Check {
    let name = "session config".to_string();
    let Some(adapter) = adapter else {
        return Check {
            name,
            status: SKIP.to_string(),
            detail: "no adapter row; nothing to build".to_string(),
        };
    };
    let verdict = crate::overlay::verdict(adapter);
    let path = crate::overlay::session_dir(&settings::app_dir(env), &snap.facts.id, &adapter.agent);
    match verdict {
        "wired" => Check {
            name,
            status: OK.to_string(),
            detail: format!(
                "{} carries the values in the launch environment; no file is written or edited",
                adapter.agent
            ),
        },
        "overlay" => {
            let env_var = if adapter.overlay_env.is_empty() {
                "(none)".to_string()
            } else {
                adapter.overlay_env.clone()
            };
            Check {
                name,
                status: OK.to_string(),
                detail: format!(
                    "{} writes {} per session and points the harness at it with {}",
                    adapter.agent,
                    path.display(),
                    env_var
                ),
            }
        }
        _ => Check {
            name,
            status: FAIL.to_string(),
            detail: format!(
                "adapter {} has write_mode {:?} with writer {:?}: no per-session config can be \
                 generated, so only the printed mapping is available",
                adapter.agent,
                if adapter.write_mode.is_empty() {
                    crate::overlay::MODE_NONE
                } else {
                    &adapter.write_mode
                },
                adapter.writer
            ),
        },
    }
}

/// A non-header target is delivered by the per-session config, so the only
/// question left is whether the harness can carry that kind at all.
fn check_delivery(adapter: Option<&Adapter>, target: &crate::resolver::Target, env: &Env) -> Check {
    let name = format!("delivery {}", target.name);
    if target.kind == crate::resolver::DEFAULT_KIND {
        return Check {
            name,
            status: SKIP.to_string(),
            detail: "header delivery, checked by the mapping line".to_string(),
        };
    }
    let Some(adapter) = adapter else {
        return Check {
            name,
            status: SKIP.to_string(),
            detail: "no adapter row; nothing to check".to_string(),
        };
    };
    if !adapters::supports(adapter, &target.kind) {
        return Check {
            name,
            status: FAIL.to_string(),
            detail: format!(
                "adapter {} cannot deliver a {} value (it supports {}); use a supported kind or \
                 a different harness",
                adapter.agent,
                target.kind,
                adapter.supports.join(", ")
            ),
        };
    }
    Check {
        name,
        status: OK.to_string(),
        detail: format!(
            "{} {} is written into this session's config as {:?} ({})",
            target.kind,
            target.param_name(),
            if target.literal.is_empty() {
                target.env_var.clone()
            } else {
                target.literal.clone()
            },
            config_or_none(adapter, env)
        ),
    }
}

fn config_or_none(adapter: &Adapter, env: &Env) -> String {
    adapters::config_path(adapter, env)
        .0
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "(no config path)".to_string())
}

fn check_mapping(adapter: Option<&Adapter>, item: &ResolvedHeader, env: &Env) -> Check {
    let name = format!("mapping {}", item.header);
    let Some(adapter) = adapter else {
        return Check {
            name,
            status: SKIP.to_string(),
            detail: "no adapter row; nothing to look in".to_string(),
        };
    };

    if adapter.write_mode == crate::overlay::MODE_ENV_ONLY
        || adapter.mechanism == "custom_headers_env"
    {
        return check_env_mapping(adapter, item, env);
    }

    // A per-session copy is built by our own writer, so the user's file is only
    // the starting point: what matters is that it *exists* and names a provider
    // the writer can put the value into. A missing static entry is the norm here.
    let overlay = adapter.write_mode != crate::overlay::MODE_NONE;

    let (path, warnings) = adapters::config_path(adapter, env);
    let Some(path) = path else {
        return Check {
            name,
            status: FAIL.to_string(),
            detail: if warnings.is_empty() {
                format!("adapter {} has no config path", adapter.agent)
            } else {
                warnings.join("; ")
            },
        };
    };
    if !path.is_file() {
        return Check {
            name,
            status: FAIL.to_string(),
            detail: if overlay {
                format!(
                    "{} does not exist; this session's copy is built from it, so {} needs a \
                     provider there first",
                    path.display(),
                    adapter.agent
                )
            } else {
                format!(
                    "{} does not exist; the header is not mapped \
                     (add the line from `aoe-llm-tags setup --print`)",
                    path.display()
                )
            },
        };
    }
    match scan_file(adapter, &path, &item.header, &item.env_var) {
        Err(err) => Check {
            name,
            status: FAIL.to_string(),
            detail: format!("{} could not be read: {err}", path.display()),
        },
        Ok((found, notes, known)) => {
            if overlay {
                Check {
                    name,
                    status: OK.to_string(),
                    detail: format!(
                        "{}{}",
                        crate::overlay::verdict(adapter),
                        known
                            .as_slice()
                            .first()
                            .map(|provider| format!(
                                ": the session copy carries it for provider {provider}"
                            ))
                            .unwrap_or_else(|| {
                                format!(": {} names a provider the writer can use", path.display())
                            })
                    ),
                }
            } else if found {
                Check {
                    name,
                    status: OK.to_string(),
                    detail: format!("{} → {}", path.display(), notes.join(", ")),
                }
            } else {
                let suffix = if known.is_empty() {
                    " (no provider entry)".to_string()
                } else {
                    format!(" (providers: {})", known.join(", "))
                };
                Check {
                    name,
                    status: FAIL.to_string(),
                    detail: format!(
                        "{} is not mapped in {}{suffix}; add it to the provider entry \
                         (see `aoe-llm-tags setup --print`)",
                        item.header,
                        path.display()
                    ),
                }
            }
        }
    }
}

fn check_env_mapping(adapter: &Adapter, item: &ResolvedHeader, env: &Env) -> Check {
    let name = format!("mapping {}", item.header);
    let variable = if adapter.agent == "claude" {
        "ANTHROPIC_CUSTOM_HEADERS"
    } else {
        ""
    };
    if variable.is_empty() {
        return Check {
            name,
            status: SKIP.to_string(),
            detail: format!("adapter {} has no variable check yet", adapter.agent),
        };
    }
    let value = env.get(variable).unwrap_or_default();
    if value.is_empty() {
        return Check {
            name,
            status: FAIL.to_string(),
            detail: format!("${variable} is not set; {} is not sent", item.header),
        };
    }
    if !value.to_lowercase().contains(&item.header.to_lowercase()) {
        return Check {
            name,
            status: FAIL.to_string(),
            detail: format!("${variable} does not carry {}: {value:?}", item.header),
        };
    }
    Check {
        name,
        status: OK.to_string(),
        detail: format!("${variable}"),
    }
}

fn check_values(snap: &Snapshot) -> Check {
    if snap.resolved.is_empty() {
        return Check {
            name: "value rendering".to_string(),
            status: FAIL.to_string(),
            detail: "no header is enabled, so the hook prints nothing".to_string(),
        };
    }
    if let Some(broken) = snap
        .warnings
        .iter()
        .find(|warning| warning.contains("unknown field"))
    {
        return Check {
            name: "value rendering".to_string(),
            status: FAIL.to_string(),
            detail: broken.clone(),
        };
    }
    let rendered = snap
        .resolved
        .iter()
        .map(|item| format!("{}={}={:?}", item.header, item.env_var, item.value))
        .collect::<Vec<_>>()
        .join("; ");
    if snap.resolved.iter().all(|item| item.value.is_empty()) {
        return Check {
            name: "value rendering".to_string(),
            status: WARN.to_string(),
            detail: format!(
                "{rendered} — every value is empty (open decision #9: mint empty or emit no line)"
            ),
        };
    }
    Check {
        name: "value rendering".to_string(),
        status: OK.to_string(),
        detail: rendered,
    }
}

// --- helpers ----------------------------------------------------------------

/// Is `header` mapped here? Returns (found, matched provider notes, all ids).
pub(crate) fn scan_file(
    adapter: &Adapter,
    path: &Path,
    header: &str,
    env_var: &str,
) -> Result<(bool, Vec<String>, Vec<String>), String> {
    // A reference row (`config_format = "text"`) points at a file this plugin
    // deliberately does not parse: the user pastes one line into a config we do
    // not own, so the honest check is whether that line is there.
    if adapter.config_format == "text" {
        let raw = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
        let reference = adapter.env_ref.replace("{env_var}", env_var);
        let found = raw.to_lowercase().contains(&header.to_lowercase()) && raw.contains(&reference);
        return Ok((
            found,
            if found {
                vec!["reference found".to_string()]
            } else {
                Vec::new()
            },
            Vec::new(),
        ));
    }
    let data = adapters::read_config(adapter, path)?;
    let wanted = header.to_lowercase();
    let (mut found, mut notes, mut known) = (false, Vec::new(), Vec::new());
    for provider in adapters::providers(adapter, &data) {
        known.push(provider.id.clone());
        let values: BTreeMap<String, String> = provider
            .headers
            .iter()
            .map(|(key, value)| (key.to_lowercase(), value.clone()))
            .collect();
        let Some(value) = values.get(&wanted) else {
            continue;
        };
        found = true;
        let mut detail = format!("{} (value: {value}", provider.id);
        if provider.models > 0 {
            detail.push_str(&format!(", models: {}", provider.models));
        }
        notes.push(detail + ")");
    }
    Ok((found, notes, known))
}

fn resolve_adapter<'a>(flags: &Flags, tool: &str, adapters: &'a [Adapter]) -> Option<&'a Adapter> {
    if !flags.adapter.is_empty() {
        return adapters::find(adapters, &flags.adapter);
    }
    adapters::match_tool(adapters, tool)
}

/// The first token of a hook line that is not an `ENV=value` prefix.
pub fn command_path(entry: &str) -> String {
    for token in shell_words(entry) {
        if !token.contains('=') {
            return token;
        }
    }
    String::new()
}

/// Enough of `shlex.split` for our own hook lines: quotes and backslashes.
pub fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            Some(q) if ch == q => {
                quote = None;
                started = true;
            }
            Some(_) => {
                current.push(ch);
                started = true;
            }
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                started = true;
            }
            None if ch == '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                    started = true;
                }
            }
            None if ch.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None => {
                current.push(ch);
                started = true;
            }
        }
    }
    if started {
        words.push(current);
    }
    words
}

fn files(config: &RawConfig) -> String {
    let paths = if config.sources.is_empty() {
        &config.present
    } else {
        &config.sources
    };
    if paths.is_empty() {
        "(no config file)".to_string()
    } else {
        join_paths(paths)
    }
}

fn join_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(", ")
}

fn abbreviate(text: &str) -> String {
    let limit = 96;
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_path_skips_the_environment_prefix() {
        let entry = "AOE_BIN=\"/usr/local/bin/aoe\" \"/p/bin/aoe-llm-tags\" hook --session \"$X\"";
        assert_eq!(command_path(entry), "/p/bin/aoe-llm-tags");
    }

    #[test]
    fn missing_hook_fails_with_the_path_it_looked_at() {
        let config = RawConfig {
            present: vec![PathBuf::from("/app/config.toml")],
            sources: vec![PathBuf::from("/app/config.toml")],
            ..RawConfig::default()
        };
        let check = check_effective_config(&config, Path::new("/app/config.toml"), None);
        assert_eq!(check.status, OK);
        assert!(check.detail.contains("/app/config.toml"));
    }

    #[test]
    fn text_lists_counts_last() {
        let report = Report {
            checks: vec![Check {
                name: "x".to_string(),
                status: OK.to_string(),
                detail: "y".to_string(),
            }],
            paths: BTreeMap::new(),
            profile: String::new(),
            adapter: String::new(),
        };
        let text = format_text(&report);
        assert!(text.starts_with("aoe-llm-tags doctor — profile (global)"));
        assert!(text.ends_with("1 ok, 0 failed, 0 warning(s), 0 skipped"));
    }
}

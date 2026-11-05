//! What is actually on this machine, one verdict per substrate.
//!
//! A hook that exports a variable proves what this plugin *would* send. It does
//! not prove the harness is pointed at it: the user's config may be missing, it
//! may be there without our line, or a copy may be left over from a session that
//! no longer exists (a sandbox, or a daemon that was killed). `doctor` used to
//! print the plan (`overlay::verdict` — what the row *would* do), which reads as
//! "wired" on a machine where nothing is. This module reports what was found,
//! under the five verdicts `plan/parameters.md` fixes:
//!
//! | verdict | means |
//! |---|---|
//! | `wired` | the file exists and every enabled target is in it |
//! | `detected-not-wired` | the file exists, our mapping is not (all of) it |
//! | `sandbox-stale` | a generated copy exists for a session the host no longer has |
//! | `unknown` | nothing to inspect: no adapter row, or no config at any known path |
//! | `unsupported` | the row has no write path at all (`write_mode = "none"`) |
//!
//! The pessimistic verdict wins: a check that cannot be performed reports
//! `unknown`, never `wired` (`plan/m2.md` risks).
//!
//! Scanning is the expensive part (a large `models.json`, a `config.toml` with
//! hundreds of providers), so results are cached by **config path + sha256**
//! (`plan/parameters.md`) under `<app dir>/cache/substrates.json`. A cache entry
//! can only be reused while the digest matches; a hand-edited file is re-scanned
//! because its digest changed, which is also how drift is detected rather than
//! overwritten.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::adapters::{self, Adapter};
use crate::doctor;
use crate::env::Env;
use crate::resolver::Target;
use crate::settings;

pub const WIRED: &str = "wired";
pub const DETECTED_NOT_WIRED: &str = "detected-not-wired";
pub const SANDBOX_STALE: &str = "sandbox-stale";
pub const UNKNOWN: &str = "unknown";
pub const UNSUPPORTED: &str = "unsupported";

/// One substrate, and what was found there.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    /// `harness`, `user`, `session`, `sandbox`.
    pub scope: String,
    /// The file or directory the verdict is about (`""` for a harness verdict).
    pub path: String,
    /// Content digest of the file inspected (`""` when there is none).
    pub sha256: String,
    pub verdict: &'static str,
    /// Whether the scan came from the cache; the verdict is the same either way.
    pub cached: bool,
    pub detail: String,
}

impl Finding {
    fn new(scope: &str, path: impl Into<String>, verdict: &'static str, detail: String) -> Self {
        Self {
            scope: scope.to_string(),
            path: path.into(),
            sha256: String::new(),
            verdict,
            cached: false,
            detail,
        }
    }

    /// Doctor's status for this verdict: only a failed check changes the exit code.
    pub fn status(&self) -> &'static str {
        match self.verdict {
            WIRED => doctor::OK,
            DETECTED_NOT_WIRED | SANDBOX_STALE => doctor::WARN,
            UNSUPPORTED => doctor::FAIL,
            _ => doctor::SKIP,
        }
    }
}

/// What one cache entry remembers about one file.
#[derive(Clone, Debug, Default, PartialEq)]
struct Entry {
    /// The digest of the file, and the targets that were asked about: a settings
    /// change with an untouched file must not reuse the old answer.
    sha256: String,
    wanted: Vec<String>,
    found: Vec<String>,
    missing: Vec<String>,
}

/// The digest cache, loaded once per run and written back if it changed.
pub struct Cache {
    path: PathBuf,
    entries: BTreeMap<String, Entry>,
    dirty: bool,
}

impl Cache {
    /// A cache with nowhere to live: every answer is recomputed and nothing is
    /// written. This is what a host with no app dir gets, rather than a relative
    /// path under whatever directory the process was started in.
    pub fn disabled() -> Self {
        Self {
            path: PathBuf::new(),
            entries: BTreeMap::new(),
            dirty: false,
        }
    }

    /// `<app dir>/cache/substrates.json`; a missing or broken cache is empty, never fatal.
    pub fn load(app_dir: &Path) -> Self {
        let path = app_dir.join("cache").join("substrates.json");
        let entries = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|value| value.get("entries").cloned())
            .and_then(|value| value.as_object().cloned())
            .map(|object| {
                object
                    .into_iter()
                    .filter_map(|(key, value)| {
                        let entry = Entry {
                            sha256: value.get("sha256")?.as_str()?.to_string(),
                            wanted: strings(value.get("wanted")),
                            found: strings(value.get("found")),
                            missing: strings(value.get("missing")),
                        };
                        Some((key, entry))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            path,
            entries,
            dirty: false,
        }
    }

    /// The entry for this path, when both the digest and the asked targets match.
    fn cached(&self, path: &Path, sha256: &str, wanted: &[String]) -> Option<&Entry> {
        self.entries
            .get(&path.to_string_lossy().into_owned())
            .filter(|entry| entry.sha256 == sha256 && entry.wanted == wanted)
    }

    fn store(&mut self, path: &Path, entry: Entry) {
        let key = path.to_string_lossy().into_owned();
        if self.entries.get(&key) != Some(&entry) {
            self.entries.insert(key, entry);
            self.dirty = true;
        }
    }

    /// For the entry whose file disappeared: a stale entry must not outlive it.
    fn forget(&mut self, path: &Path) {
        if self
            .entries
            .remove(&path.to_string_lossy().into_owned())
            .is_some()
        {
            self.dirty = true;
        }
    }

    /// Write the cache back if this run changed it. A failure is not an error:
    /// the cache only saves work, it never decides a verdict.
    pub fn save(&self) {
        if !self.dirty || self.path.as_os_str().is_empty() {
            return;
        }
        let entries: BTreeMap<String, serde_json::Value> = self
            .entries
            .iter()
            .map(|(key, entry)| {
                (
                    key.clone(),
                    serde_json::json!({
                        "sha256": entry.sha256,
                        "wanted": entry.wanted,
                        "found": entry.found,
                        "missing": entry.missing,
                    }),
                )
            })
            .collect();
        let body = serde_json::json!({"version": 1, "entries": entries});
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&self.path, format!("{body}\n"));
    }
}

fn strings(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Classify every substrate for one session.
///
/// `live` is the session ids the host still knows (`aoe list --json`): a
/// generated copy for any other id is stale by definition, which is exactly the
/// sandbox case — start a sandbox from a store that was never wired and the copy
/// it writes names a session the host does not have.
pub fn classify(
    adapter: Option<&Adapter>,
    targets: &[Target],
    env: &Env,
    session_id: &str,
    live: &[String],
    cache: &mut Cache,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let Some(adapter) = adapter else {
        findings.push(Finding::new(
            "harness",
            "",
            UNKNOWN,
            "no adapter row for this harness: the tag only reaches the launch environment \
             (AOE_LLM_TAGS), so nothing can be said about its config"
                .to_string(),
        ));
        findings.extend(stale_copies(env, adapter, live));
        return findings;
    };

    findings.push(harness_finding(adapter, targets));
    findings.push(user_finding(adapter, targets, env, cache));
    if let Some(finding) = session_finding(adapter, env, session_id) {
        findings.push(finding);
    }
    findings.extend(stale_copies(env, Some(adapter), live));
    findings
}

/// Is the row capable of any delivery at all?
fn harness_finding(adapter: &Adapter, targets: &[Target]) -> Finding {
    let enabled: Vec<&str> = targets
        .iter()
        .filter(|target| target.enabled)
        .map(|target| target.kind.as_str())
        .collect();
    // A row with no session copy can still deliver through the reference
    // channel (`codex`, `hermes`, `openclaw`): the user's own config maps the
    // header to a variable this plugin exports, so nothing has to move. Only a
    // row that can do neither is `unsupported` (`kiro`, which has no channel,
    // and `cline`, which cannot be scoped per session).
    let reference_only = targets
        .iter()
        .filter(|target| target.enabled)
        .all(|target| adapters::interpolates(adapter, &target.kind));
    let delivers = !adapter.supports.is_empty()
        && !adapter.mapping_format.is_empty()
        && !enabled.is_empty()
        && enabled
            .iter()
            .all(|kind| adapter.supports.iter().any(|listed| listed == kind));
    if !delivers || (!crate::overlay::can_write(adapter) && !reference_only) {
        let mut why = Vec::new();
        if adapter.mechanism == "none" || adapter.supports.is_empty() {
            why.push("it has no delivery channel".to_string());
        }
        if adapter.mapping_format.is_empty() {
            why.push("it has no mapping format".to_string());
        }
        if !crate::overlay::can_write(adapter) && !reference_only {
            why.push(format!(
                "write_mode {:?} with no `$NAME` interpolation, so there is neither a session \
                 copy nor a reference",
                if adapter.write_mode.is_empty() {
                    crate::overlay::MODE_NONE
                } else {
                    &adapter.write_mode
                }
            ));
        }
        return Finding::new(
            "harness",
            "",
            UNSUPPORTED,
            format!(
                "adapter {}: {} — this session{} will not carry the tag",
                adapter.agent,
                why.join(", "),
                if enabled.is_empty() {
                    "".to_string()
                } else {
                    format!("'s {} target(s)", enabled.len())
                }
            ),
        );
    }
    let carry: Vec<&str> = adapter.supports.iter().map(String::as_str).collect();
    Finding::new(
        "harness",
        "",
        WIRED,
        format!(
            "{} ({}) can carry {}{}",
            adapter.agent,
            adapter.mechanism,
            if carry.is_empty() {
                "nothing".to_string()
            } else {
                carry.join(", ")
            },
            if adapter.aoe_hooks && adapter.write_mode != crate::overlay::MODE_ENV_ONLY {
                " — aoe owns its hook path, so the value rides the reference channel"
            } else {
                ""
            }
        ),
    )
}

/// The user's own config: present, and carrying our mapping or not.
fn user_finding(adapter: &Adapter, targets: &[Target], env: &Env, cache: &mut Cache) -> Finding {
    if adapter.config_format == "none" || adapter.config_paths.is_empty() {
        return Finding::new(
            "user",
            "",
            UNKNOWN,
            format!(
                "adapter {} declares no readable config (config_format {:?}), so there is no file \
                 to inspect",
                adapter.agent, adapter.config_format
            ),
        );
    }
    let Some(path) = adapters::config_path(adapter, env).0 else {
        return Finding::new(
            "user",
            adapter.config_paths.first().cloned().unwrap_or_default(),
            UNKNOWN,
            format!("adapter {}: no configured path exists yet", adapter.agent),
        );
    };
    let wanted: Vec<&Target> = targets.iter().filter(|target| target.enabled).collect();
    let name = |target: &Target| match target.kind.as_str() {
        "header" => target.name.clone(),
        _ => target.param_name().to_string(),
    };
    let label = |target: &Target| format!("{} {}", target.kind, name(target));

    // A path we cannot read is not a pass: say so, pessimistically.
    let digest = match std::fs::read(&path) {
        Ok(bytes) => crate::sha256::hex(&bytes),
        Err(err) => {
            cache.forget(&path);
            return Finding {
                sha256: String::new(),
                ..Finding::new(
                    "user",
                    path.to_string_lossy().into_owned(),
                    UNKNOWN,
                    format!("cannot read {}: {err}", path.display()),
                )
            };
        }
    };

    let labels: Vec<String> = wanted.iter().map(|target| label(target)).collect();
    let (found, missing, cached) = match cache.cached(&path, &digest, &labels) {
        Some(entry) => (entry.found.clone(), entry.missing.clone(), true),
        None => {
            let (mut found, mut missing) = (Vec::new(), Vec::new());
            for target in &wanted {
                match doctor::scan_file(adapter, &path, &name(target), &target.env_var) {
                    Ok((true, _, _)) => found.push(label(target)),
                    // A row this plugin deliberately does not parse (`config_format = "text"`)
                    // can only be searched for the line, which `scan_file` already does.
                    Ok((false, _, _)) => missing.push(label(target)),
                    Err(err) => {
                        cache.forget(&path);
                        return Finding {
                            sha256: digest,
                            ..Finding::new(
                                "user",
                                path.to_string_lossy().into_owned(),
                                DETECTED_NOT_WIRED,
                                format!("{} does not parse: {err}", path.display()),
                            )
                        };
                    }
                }
            }
            cache.store(
                &path,
                Entry {
                    sha256: digest.clone(),
                    wanted: labels.clone(),
                    found: found.clone(),
                    missing: missing.clone(),
                },
            );
            (found, missing, false)
        }
    };

    let verdict = if !wanted.is_empty() && missing.is_empty() {
        WIRED
    } else {
        DETECTED_NOT_WIRED
    };
    let detail = if wanted.is_empty() {
        format!(
            "{} found; no target is enabled, so nothing is expected in it",
            path.display()
        )
    } else if missing.is_empty() {
        format!("{} found; carries {}", path.display(), found.join(", "))
    } else {
        format!(
            "{} found; missing {}{}",
            path.display(),
            missing.join(", "),
            if found.is_empty() {
                String::new()
            } else {
                format!(" (has {})", found.join(", "))
            }
        )
    };
    Finding {
        sha256: digest,
        cached,
        ..Finding::new("user", path.to_string_lossy().into_owned(), verdict, detail)
    }
}

/// This session's generated copy, when one was built.
fn session_finding(adapter: &Adapter, env: &Env, session_id: &str) -> Option<Finding> {
    if !crate::overlay::can_write(adapter) {
        return None;
    }
    let Ok(app_dir) = settings::app_dir(env) else {
        return None;
    };
    let dir = crate::overlay::session_dir(&app_dir, session_id, &adapter.agent);
    if !dir.is_dir() {
        return None;
    }
    let file = dir.join(&adapter.overlay_file);
    let path = if file.is_file() {
        file
    } else {
        return Some(Finding::new(
            "session",
            dir.to_string_lossy().into_owned(),
            DETECTED_NOT_WIRED,
            format!(
                "{} exists but holds no {}; the copy is incomplete",
                dir.display(),
                adapter.overlay_file
            ),
        ));
    };
    let var = if adapter.overlay_env.is_empty() {
        "(no override variable)".to_string()
    } else {
        format!("{}={}", adapter.overlay_env, dir.display())
    };
    Some(Finding::new(
        "session",
        path.to_string_lossy().into_owned(),
        WIRED,
        format!("this session's copy, built at launch; the harness is pointed at it with {var}"),
    ))
}

/// Copies left behind for sessions the host no longer has.
fn stale_copies(env: &Env, _adapter: Option<&Adapter>, live: &[String]) -> Vec<Finding> {
    let Ok(app_dir) = settings::app_dir(env) else {
        return Vec::new();
    };
    let root = app_dir.join("sessions");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    if live.is_empty() {
        // `aoe list` did not answer, so "not live" cannot be established: saying
        // every copy is stale would be a false alarm, and saying nothing is a lie.
        let copies: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        if copies.is_empty() {
            return Vec::new();
        }
        return vec![Finding::new(
            "sandbox",
            root.to_string_lossy().into_owned(),
            UNKNOWN,
            format!(
                "{} generated copy(ies) present but `aoe list` did not answer, so staleness \
                 cannot be decided",
                copies.len()
            ),
        )];
    }
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.path().is_dir() || live.iter().any(|id| crate::overlay::sanitize(id) == name) {
            continue;
        }
        out.push(Finding::new(
            "sandbox",
            entry.path().to_string_lossy().into_owned(),
            SANDBOX_STALE,
            format!(
                "session {name} is not in `aoe list`, so this generated copy belongs to a session \
                 that is gone (a sandbox, or a daemon that was killed); it proves nothing about \
                 the current wiring and the worker will prune it"
            ),
        ));
    }
    out
}

/// The ids the host still knows, from `aoe list --json`.
///
/// An empty list is not "everything is stale": when `aoe` cannot be reached the
/// answer is unknown, and treating every copy as stale would be a false alarm.
/// So the caller passes `live` only when the query succeeded; `prune` in the
/// worker has the same rule.
pub fn live_sessions(env: &Env) -> Vec<String> {
    let value = crate::resolver::aoe_json(
        &["list".to_string(), "--json".to_string()],
        env,
        std::time::Duration::from_secs(5),
    );
    let Ok(value) = value else {
        return Vec::new();
    };
    value
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("id").and_then(serde_json::Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The findings as JSON, for `classify --json` and the pane payload.
pub fn as_json(findings: &[Finding]) -> serde_json::Value {
    serde_json::json!({
        "findings": findings.iter().map(|finding| serde_json::json!({
            "scope": finding.scope,
            "path": finding.path,
            "sha256": finding.sha256,
            "verdict": finding.verdict,
            "status": finding.status(),
            "cached": finding.cached,
            "detail": finding.detail,
        })).collect::<Vec<_>>(),
        "worst": worst(findings),
    })
}

/// The verdict that matters most: `unsupported` > `sandbox-stale` >
/// `detected-not-wired` > `unknown` > `wired`. A UI badge shows one word, so it
/// must be the pessimistic one.
pub fn worst(findings: &[Finding]) -> &'static str {
    let mut worst = WIRED;
    for finding in findings {
        let rank = |verdict: &str| match verdict {
            UNSUPPORTED => 5,
            SANDBOX_STALE => 4,
            DETECTED_NOT_WIRED => 3,
            UNKNOWN => 2,
            _ => 1,
        };
        if rank(finding.verdict) > rank(worst) {
            worst = finding.verdict;
        }
    }
    worst
}

/// `[scope] verdict  path` — one line per finding, for `doctor`.
pub fn format_lines(findings: &[Finding]) -> Vec<String> {
    findings
        .iter()
        .map(|finding| {
            format!(
                "{} {}: {}{}",
                finding.scope,
                finding.verdict,
                finding.detail,
                if finding.cached { " (cached)" } else { "" }
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::Target;

    fn env_at(root: &Path) -> Env {
        Env::new([
            ("HOME", root.join("home").to_string_lossy().into_owned()),
            (
                "XDG_CONFIG_HOME",
                root.join("config").to_string_lossy().into_owned(),
            ),
        ])
    }

    #[test]
    fn a_missing_config_is_unknown_not_wired() {
        let root = std::env::temp_dir().join(format!("substrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).expect("home");
        let env = env_at(&root);
        let (rows, _) = adapters::load(None);
        let pi = rows.iter().find(|row| row.agent == "pi").expect("pi row");
        let targets = vec![Target::new("X-AoE-Tags", "", "AOE_HDR_X_AOE_TAGS")];
        let mut cache = Cache::load(&settings::app_dir(&env).expect("app dir"));
        let findings = classify(Some(pi), &targets, &env, "s1", &[], &mut cache);
        let user = findings
            .iter()
            .find(|finding| finding.scope == "user")
            .expect("user finding");
        assert_eq!(user.verdict, UNKNOWN, "{user:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}

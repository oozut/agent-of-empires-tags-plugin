//! Per-session harness config: the only place this plugin ever writes.
//!
//! The user's own config is never edited. Each session gets its own copy of the
//! harness config under this plugin's app dir, built from the user's file at
//! launch with that session's tag values baked in, and the harness is pointed at
//! it through its own override variable (`PI_CODING_AGENT_DIR`, `CODEX_HOME`,
//! `OPENCODE_CONFIG`, …) which the hook prints next to the tag variables.
//!
//! Three shapes cover every harness (a row's `write_mode` in `adapters.toml`):
//!
//! | mode | session artifact | example |
//! |---|---|---|
//! | `env_only` | none: values ride the launch environment | claude (`ANTHROPIC_CUSTOM_HEADERS`) |
//! | `overlay_file` | one generated file | opencode (`OPENCODE_CONFIG`) |
//! | `overlay_dir` | our file plus links to the user's other entries | pi (`PI_CODING_AGENT_DIR`), codex (`CODEX_HOME`) |
//!
//! A harness that loads code from its config directory (`extensions/*.ts`) can
//! also receive this plugin's own artefact — but only after the user approved
//! that exact file: [`crate::harness_extension`] decides, this module places it.
//! The directory is then a *real* directory whose other entries are links to the
//! user's own, because writing our file through the usual whole-directory link
//! would write into the user's config.
//!
//! What follows from that: two sessions never read the same file, `undo` is
//! "delete the directory", a stale overlay cannot outlive its session (`prune`),
//! and a test can build one in a temp dir and read back exactly what a harness
//! would see — no harness installed, no global state touched.

use std::path::{Path, PathBuf};

use crate::adapters::Adapter;
use crate::env::Env;
use crate::resolver::{self, Snapshot, Target};
use crate::settings;

pub const MODE_ENV_ONLY: &str = "env_only";
pub const MODE_OVERLAY_FILE: &str = "overlay_file";
pub const MODE_OVERLAY_DIR: &str = "overlay_dir";
pub const MODE_NONE: &str = "none";

/// One target resolved for this session: what to write where, with the value.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub kind: String,
    /// Header name, or the query/body parameter name.
    pub name: String,
    pub value: String,
}

/// A materialised session config.
#[derive(Clone, Debug, PartialEq)]
pub struct Overlay {
    /// `<app dir>/sessions/<session id>/<agent>`.
    pub dir: PathBuf,
    /// The file generated inside `dir` (`None` for `env_only`).
    pub file: Option<PathBuf>,
    /// Variables to add to the launch environment (`NAME`, `value`).
    pub env: Vec<(String, String)>,
    /// Targets that could not be placed, with the reason.
    pub skipped: Vec<(String, String)>,
}

impl Overlay {
    /// The `NAME=value` lines the hook prints for this harness.
    pub fn env_lines(&self) -> Vec<String> {
        self.env
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect()
    }
}

/// `<app dir>/sessions/<session id>/<agent>`.
///
/// The agent segment keeps a session that runs one harness now and another later
/// from mixing the two configs.
pub fn session_dir(app_dir: &Path, session_id: &str, agent: &str) -> PathBuf {
    app_dir
        .join("sessions")
        .join(sanitize(session_id))
        .join(sanitize(agent))
}

/// The path is built from a host-supplied id: keep it one boring segment.
pub(crate) fn sanitize(segment: &str) -> String {
    let cleaned: String = segment
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "session".to_string()
    } else {
        cleaned
    }
}

/// Every enabled target with this session's value, in settings order.
///
/// `Snapshot::resolved` has one entry per target that rendered, keyed by the
/// variable the hook would export, so a target that was refused earlier simply
/// has no value here and is reported rather than guessed.
pub fn resolved_targets(snap: &Snapshot) -> (Vec<Resolved>, Vec<(String, String)>) {
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for target in snap.settings.targets.iter().filter(|row| row.enabled) {
        let env_var = resolver::target_env_var(target);
        let Some(found) = snap.resolved.iter().find(|item| item.env_var == env_var) else {
            skipped.push((
                target.name.clone(),
                "no value: the resolver refused it".into(),
            ));
            continue;
        };
        out.push(Resolved {
            kind: target.kind.clone(),
            name: target.param_name().to_string(),
            // A literal is an explicit override; otherwise the rendered template.
            value: if target.literal.is_empty() {
                found.value.clone()
            } else {
                target.literal.clone()
            },
        });
    }
    (out, skipped)
}

/// Build this session's artifact. Idempotent: an existing overlay is rebuilt
/// from the user's config, so a changed tag set never leaves a stale value.
pub fn materialise(adapter: &Adapter, snap: &Snapshot, env: &Env) -> Result<Overlay, String> {
    // No app dir, no copy: the alternative is writing the session's config
    // somewhere relative to the launch directory.
    let app_dir = settings::app_dir(env)?;
    let dir = session_dir(&app_dir, &snap.facts.id, &adapter.agent);
    let (targets, mut skipped) = resolved_targets(snap);

    if adapter.write_mode == MODE_ENV_ONLY {
        let env = native_env(adapter, &targets);
        return Ok(Overlay {
            dir,
            file: None,
            env,
            skipped,
        });
    }
    if !matches!(
        adapter.write_mode.as_str(),
        MODE_OVERLAY_DIR | MODE_OVERLAY_FILE
    ) {
        return Err(format!(
            "adapter {} has write_mode {:?}: this harness has no per-session config to write",
            adapter.agent,
            if adapter.write_mode.is_empty() {
                MODE_NONE
            } else {
                &adapter.write_mode
            }
        ));
    }
    if adapter.overlay_env.is_empty() {
        return Err(format!(
            "adapter {} has no overlay_env: nothing could point the harness at its session config",
            adapter.agent
        ));
    }
    if adapter.writer.is_empty() || adapter.writer == "none" {
        return Err(format!(
            "adapter {} has no writer: its config format is not implemented yet",
            adapter.agent
        ));
    }

    let (source, warnings) = crate::adapters::config_path(adapter, env);
    if let Some(warning) = warnings.first() {
        return Err(warning.clone());
    }
    let source = source.ok_or_else(|| {
        format!(
            "adapter {}: no config path resolves, so there is nothing to build the session config from",
            adapter.agent
        )
    })?;

    let file_name = if adapter.overlay_file.is_empty() {
        source
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("config"))
    } else {
        PathBuf::from(&adapter.overlay_file)
    };
    let generated = dir.join(&file_name);
    let source_dir = source.parent().map(Path::to_path_buf);

    // Build the copy in memory first, and touch the disk only once there is
    // something to put on it: a launch that delivers nothing must not leave a
    // session directory — and its links into the user's config, credentials
    // included — behind for a harness that is never pointed at it.

    // No user file yet: this harness is not configured, so there is nothing to
    // copy and nothing honest to say — stay silent instead of warning at launch.
    if !source.exists() {
        discard(&dir);
        return Ok(Overlay {
            dir,
            file: None,
            env: Vec::new(),
            skipped: Vec::new(),
        });
    }

    // Start from the user's file so providers, models and references survive.
    let raw = match std::fs::read_to_string(&source) {
        Ok(raw) => Some(raw),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(format!("{} could not be read: {err}", source.display())),
    };
    let mut doc = crate::writer::Doc::parse(&adapter.config_format, raw.as_deref())
        .map_err(|err| format!("{} {err}", source.display()))?;

    skipped.extend(crate::writer::apply(
        &adapter.writer,
        &mut doc,
        &targets,
        &source,
    )?);

    // Pointing a harness at a config that carries nothing would hide its real
    // one: an empty plan writes no file and exports no override.
    let placed = targets.len().saturating_sub(skipped.len());
    if placed == 0 {
        discard(&dir);
        return Ok(Overlay {
            dir,
            file: None,
            env: Vec::new(),
            skipped,
        });
    }

    // The one thing that is copied is the harness's own config file, because
    // that is where the tag has to live. Everything else in the session dir is
    // a link to the user's entry, so no other content is duplicated.
    let text = doc.render()?;
    // `0700`: the copy below can carry a provider key, so the directory it lives
    // in is this user's alone (see [`crate::private`]).
    crate::private::create_dir(&dir)
        .map_err(|err| format!("{} could not be created: {err}", dir.display()))?;

    // The extension decision is made before anything is linked: an allowed one
    // needs its directory to stay a real directory, so the whole-directory link
    // must skip it.
    let extension = crate::harness_extension::plan(
        adapter,
        &snap.settings.harness_extension,
        &app_dir,
        &snap.facts.id,
    );
    // Always clear a copy a previous launch left, whichever way the decision
    // went: a revoke takes effect at the next launch, and a stale artefact must
    // never be the thing that runs.
    clear_extension(&dir, adapter)?;

    if adapter.write_mode == MODE_OVERLAY_DIR {
        if let Some(source_dir) = &source_dir {
            let mut skip = Vec::new();
            if let Ok(plan) = &extension {
                skip.push(std::ffi::OsString::from(plan.subdir.clone()));
            }
            link_siblings(source_dir, &dir, &file_name, &skip)?;
        }
    }
    crate::private::write_atomic(&generated, &text)
        .map_err(|err| format!("{} could not be written: {err}", generated.display()))?;

    let mut env = vec![(
        adapter.overlay_env.clone(),
        if adapter.write_mode == MODE_OVERLAY_FILE {
            generated.to_string_lossy().into_owned()
        } else {
            dir.to_string_lossy().into_owned()
        },
    )];

    match &extension {
        Ok(plan) => match place_extension(adapter, plan, &dir, source_dir.as_deref()) {
            Ok(()) => {
                // The live file is what the extension re-reads per request; the
                // values are the ones this launch already resolved, so the
                // extension can never mint a tag of its own.
                match crate::harness_extension::write_live(
                    &app_dir,
                    &snap.facts.id,
                    &snap.resolved,
                    true,
                ) {
                    Ok(live) => {
                        env.push((plan.env_var.clone(), live.to_string_lossy().into_owned()))
                    }
                    Err(why) => skipped.push((plan.file.clone(), why)),
                }
            }
            Err(why) => skipped.push((plan.file.clone(), why)),
        },
        Err(refusal) => {
            // A harness that simply has no hook, or one the user never asked
            // for, is the normal case and says nothing at launch.
            if refusal.worth_a_warning() {
                skipped.push((adapter.agent.clone(), refusal.detail()));
            }
        }
    }

    Ok(Overlay {
        dir: dir.clone(),
        file: Some(generated),
        env,
        skipped,
    })
}

/// The harness's own variable for an `env_only` row: every target rendered into
/// one value, one line per target (`native_format`, default `{header}: {value}`).
fn native_env(adapter: &Adapter, targets: &[Resolved]) -> Vec<(String, String)> {
    if adapter.native_env.is_empty() || targets.is_empty() {
        return Vec::new();
    }
    let format = if adapter.native_format.is_empty() {
        "{header}: {value}"
    } else {
        &adapter.native_format
    };
    let lines: Vec<String> = targets
        .iter()
        .map(|item| {
            format
                .replace("{header}", &item.name)
                .replace("{param}", &item.name)
                .replace("{value}", &item.value)
        })
        .collect();
    vec![(adapter.native_env.clone(), lines.join(&adapter.native_sep))]
}

/// Link every entry of the user's config dir into the session dir, except the
/// file this plugin generates. Writes through a link land in the user's file
/// (`writeFileSync` truncates the target), so auth refresh keeps working.
///
/// A link, not a copy: the user's entry stays the only copy of its content, so
/// credentials, session stores and caches are never duplicated into the app dir.
fn link_siblings(
    source_dir: &Path,
    dir: &Path,
    generated_name: &Path,
    extra_skip: &[std::ffi::OsString],
) -> Result<(), String> {
    let entries = match std::fs::read_dir(source_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(format!("{} could not be read: {err}", source_dir.display())),
    };
    let skip = generated_name.file_name().map(|name| name.to_os_string());
    for entry in entries.flatten() {
        let name = entry.file_name();
        if Some(name.clone()) == skip || extra_skip.contains(&name) {
            continue;
        }
        let link = dir.join(&name);
        if link.symlink_metadata().is_ok() {
            continue;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(entry.path(), &link)
            .map_err(|err| format!("{} could not be linked: {err}", link.display()))?;
    }
    Ok(())
}

/// Put an approved extension in the session's config copy.
///
/// The directory becomes a real directory whose children are links to the
/// user's own entries, so the user's extensions keep loading and writing this
/// plugin's file can never follow a link into the user's config.
fn place_extension(
    adapter: &Adapter,
    plan: &crate::harness_extension::Plan,
    dir: &Path,
    source_dir: Option<&Path>,
) -> Result<(), String> {
    let Some(source) = crate::harness_extension::embedded(adapter) else {
        return Err(format!(
            "{} is not carried by this build",
            plan.artifact.display()
        ));
    };
    let target_dir = dir.join(&plan.subdir);
    if let Ok(meta) = target_dir.symlink_metadata() {
        if meta.file_type().is_symlink() {
            std::fs::remove_file(&target_dir)
                .map_err(|err| format!("{} could not be replaced: {err}", target_dir.display()))?;
        }
    }
    crate::private::create_dir(&target_dir)
        .map_err(|err| format!("{} could not be created: {err}", target_dir.display()))?;
    if let Some(source_dir) = source_dir {
        // One link per entry, not a link to the directory: the user's own files
        // stay the only copy of their content.
        link_siblings(
            &source_dir.join(&plan.subdir),
            &target_dir,
            Path::new(&plan.file),
            &[],
        )?;
    }
    let path = target_dir.join(&plan.file);
    crate::private::write_atomic(&path, source)
        .map_err(|err| format!("{} could not be written: {err}", path.display()))
}

/// Remove a copy a previous launch placed. A directory that stays only because
/// it holds our file goes too; a link to the user's directory is left alone.
fn clear_extension(dir: &Path, adapter: &Adapter) -> Result<(), String> {
    let target = dir.join(crate::harness_extension::subdir(adapter));
    let Ok(meta) = target.symlink_metadata() else {
        return Ok(());
    };
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    let ours = target.join(crate::harness_extension::file_name(adapter));
    if ours.symlink_metadata().is_ok() {
        std::fs::remove_file(&ours)
            .map_err(|err| format!("{} could not be removed: {err}", ours.display()))?;
    }
    // Everything left in there is a link this plugin made, so the directory can
    // go and the ordinary whole-directory link can take its place. A real file
    // (something the user put there) means it stays.
    let only_our_links = std::fs::read_dir(&target)
        .map(|entries| {
            entries.flatten().all(|entry| {
                entry
                    .file_type()
                    .map(|kind| kind.is_symlink())
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if only_our_links {
        let _ = std::fs::remove_dir_all(&target);
    }
    Ok(())
}

/// Write via a temp file in the same directory, then rename: a hook that dies
/// mid-write cannot leave a half config for a harness to read.
/// Remove a session copy this launch will not use: it would carry nothing, or
/// the file it is built from is gone. A copy that is not used is not left on
/// disk either.
fn discard(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// Remove session dirs that belong to no live session. Returns how many went.
///
/// Run from the hook and from `doctor`: an abandoned session cannot leave a
/// config behind for long.
pub fn prune(app_dir: &Path, live: &[String]) -> usize {
    // An empty list is a failed query, not a host with no sessions: pruning on
    // it would delete every session's copy on one bad read.
    if live.is_empty() {
        return 0;
    }
    let entries = match std::fs::read_dir(app_dir.join("sessions")) {
        Ok(entries) => entries,
        Err(_) => return 0,
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if live.iter().any(|id| sanitize(id) == name) {
            continue;
        }
        if remove_generated(&entry.path()) {
            removed += 1;
        }
    }
    removed
}

/// Delete the generated harness copies inside one session dir, and the dir when
/// nothing is left in it.
///
/// `overrides.toml` is not a copy of anything: it is the only place a session's
/// own tags and its literal token exist. A host list that has merely not caught up
/// (a read that failed, a host that was restarted) must not take them with it, so
/// the file stays and the directory stays with it — `setup --undo` is the way to
/// remove an edit, and it says so.
fn remove_generated(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut removed = false;
    for entry in entries.flatten() {
        if entry.file_name() == crate::session_tags::FILE {
            continue;
        }
        let path = entry.path();
        let gone = match entry.file_type() {
            Ok(kind) if kind.is_dir() => std::fs::remove_dir_all(&path).is_ok(),
            Ok(_) => std::fs::remove_file(&path).is_ok(),
            Err(_) => false,
        };
        removed |= gone;
    }
    // Fails while `overrides.toml` is still there, which is the point.
    let _ = std::fs::remove_dir(dir);
    removed
}

/// Remove this session's overlay for one agent, or every agent's.
pub fn remove(app_dir: &Path, session_id: &str, agent: Option<&str>) -> usize {
    let dir = app_dir.join("sessions").join(sanitize(session_id));
    let target = match agent {
        Some(agent) => dir.join(sanitize(agent)),
        None => dir,
    };
    usize::from(std::fs::remove_dir_all(target).is_ok())
}

/// Every session dir on disk, for `doctor` and `setup --undo --all`.
pub fn all_sessions(app_dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(app_dir.join("sessions"))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Can this adapter deliver anything per session at all?
pub fn can_write(adapter: &Adapter) -> bool {
    match adapter.write_mode.as_str() {
        MODE_ENV_ONLY => true,
        MODE_OVERLAY_DIR | MODE_OVERLAY_FILE => {
            !adapter.writer.is_empty()
                && adapter.writer != "none"
                && !adapter.overlay_env.is_empty()
        }
        _ => false,
    }
}

/// The verdict `doctor` and the pane show for one adapter.
pub fn verdict(adapter: &Adapter) -> &'static str {
    if !can_write(adapter) {
        "unsupported"
    } else if adapter.write_mode == MODE_ENV_ONLY {
        "wired"
    } else {
        "overlay"
    }
}

/// The enabled targets a harness cannot carry, named with the reason, known
/// before anything is written.
pub fn unsupported(
    adapter: &Adapter,
    targets: &[Target],
    resolved: &[crate::resolver::ResolvedHeader],
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for target in targets.iter().filter(|target| target.enabled) {
        if !crate::adapters::supports(adapter, &target.kind) {
            out.push((
                target.name.clone(),
                format!(
                    "adapter {} cannot deliver a {} value (it supports {})",
                    adapter.agent,
                    target.kind,
                    adapter.supports.join(", ")
                ),
            ));
            continue;
        }
        // The value this launch would send, not a guess: a harness that splits
        // its header list on `,(name:)` cannot carry it, and writing the mapping
        // anyway would be a lie the far side fails to parse.
        let value = resolved
            .iter()
            .find(|row| row.header == target.name)
            .map(|row| row.value.as_str())
            .unwrap_or_default();
        if let Some(reason) = crate::adapters::comma_refusal(adapter, &target.name, value) {
            out.push((target.name.clone(), reason));
        }
    }
    out
}

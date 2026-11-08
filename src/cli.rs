//! Argument parsing and the M0 verbs: `hook`, `value`, `setup --print`, `doctor`.
//!
//! The user-visible contract for the first two (`plan/parameters.md` §6): stdout
//! is machine-readable only — one `NAME=value` line per enabled header for
//! `hook`, one value for `value` — and diagnostics go to stderr. Exit codes are 0
//! for ok/degraded, 1 for a failing check, 2 for a usage error. A hook must never
//! abort a session start, so an unexpected failure there is still exit 0.
//!
//! `verify` (M2) is named but not implemented: it reports which milestone owns it
//! rather than pretending to be a different error.

use std::io::{IsTerminal, Write};

use clap::{Args, Parser, Subcommand};

use crate::env::Env;
use crate::resolver::{self, Flags, Given};
use crate::{doctor, setup};

pub const PROG: &str = "aoe-llm-tags";
pub const EXIT_OK: i32 = 0;
pub const EXIT_FAIL: i32 = 1;
pub const EXIT_USAGE: i32 = 2;

pub const NOT_IMPLEMENTED: [(&str, &str); 1] = [("verify", "M2 (plan/m2.md)")];

#[derive(Parser, Debug)]
#[command(
    name = PROG,
    version,
    about = "Resolve this session's tag headers (Agent of Empires plugin)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// print one NAME=value per enabled header
    #[command(
        about = "Mint the header variables for this launch. Used by both [host_hooks] keys."
    )]
    Hook(FlagArgs),
    /// print one header's value
    #[command(about = "Print a single header's value, for harnesses that take a command.")]
    Value {
        /// header name (default: X-AoE-Tags)
        #[arg(default_value = resolver::DEFAULT_HEADER)]
        header: String,
        #[command(flatten)]
        flags: FlagArgs,
    },
    /// show how this session's targets are delivered, and build the session copy
    #[command(
        about = "Show the hook lines and one mapping line per enabled target. --apply builds \
                 this session's config; the user's own config is never edited."
    )]
    Setup {
        /// print the hook lines and one mapping line per enabled target
        #[arg(long = "print")]
        print: bool,
        /// show the session config this launch would build, without writing it
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// build this session's config now (the hook does this at launch)
        #[arg(long = "apply")]
        apply: bool,
        /// confirm --apply (nothing is written to the user's config either way)
        #[arg(long = "yes")]
        yes: bool,
        /// remove this session's generated config
        #[arg(long = "undo")]
        undo: bool,
        /// with --undo: remove every session's generated config
        #[arg(long = "all")]
        all: bool,
        /// machine-readable output
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        flags: FlagArgs,
    },
    /// say what is actually on this machine, one verdict per substrate
    #[command(
        about = "Classify the wiring: wired, detected-not-wired, sandbox-stale, unknown or \
                 unsupported — per harness, the user's config, this session's copy and leftovers."
    )]
    Classify {
        /// machine-readable output
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        flags: FlagArgs,
    },
    /// edit this session's tags or token, without touching the global config
    #[command(
        about = "Add, replace or drop a tag for one session, or write a literal token. \
                 The edit lands in the session's own file (sessions/<id>/overrides.toml); \
                 the global settings are never rewritten."
    )]
    Tag {
        /// add or replace this tag (`team:core`, `epic:=42`, or a bare marker)
        #[arg(long = "set", value_name = "KEY:VALUE")]
        set: Option<String>,
        /// drop this key for this session, whether the settings or this file set it
        #[arg(long = "unset", value_name = "KEY")]
        unset: Option<String>,
        /// write a literal value standing in for what the targets render
        #[arg(long = "token", value_name = "VALUE")]
        token: Option<String>,
        /// clear the literal token
        #[arg(long = "clear-token")]
        clear_token: bool,
        /// add the standard git tags as a sample (vcs, branch, commit, tag)
        #[arg(long = "git")]
        git: bool,
        /// forget every edit this session has
        #[arg(long = "reset")]
        reset: bool,
        /// show this session's edits and what it would send now (the default)
        #[arg(long = "show")]
        show: bool,
        /// machine-readable output
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        flags: FlagArgs,
    },
    /// say what this session is doing, in the closed activity vocabulary
    #[command(
        about = "Set this session's `activity` tag (`plan`, `code`, `test`, ...) — what a badge \
                 shows and what a per-request target sends. Use it as work moves between \
                 phases; a session with an approved harness extension sends it on its next \
                 request.",
        after_help = "The vocabulary is closed so every surface renders the same icon and tone. \
                      An unknown name is an error, not a new activity."
    )]
    Activity {
        /// one of explore, plan, code, test, debug, review, docs, ops, idle
        #[arg(value_name = "NAME")]
        name: Option<String>,
        /// drop the activity: this session stops saying what it is doing
        #[arg(long = "clear")]
        clear: bool,
        /// show the current activity and the vocabulary (the default)
        #[arg(long = "show")]
        show: bool,
        /// machine-readable output
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        flags: FlagArgs,
    },
    /// list every reference a template or a tag value may use
    #[command(
        about = "List the {references} this build understands — session fields, tag shapes and \
                 git facts — with this session's value for each. The editor's dropdown is fed \
                 from the same list."
    )]
    References {
        /// machine-readable output
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        flags: FlagArgs,
    },
    /// approve, revoke or report a harness-side extension
    #[command(
        about = "A harness-side extension lets a tag edited while a session runs reach its next \
                 request, instead of the next launch. Enabling one installs a file into every \
                 session's private config copy, so `enable` prints the disclosure first and \
                 records the approval with the file's sha256; nothing is written until then."
    )]
    Extension {
        #[command(subcommand)]
        action: ExtensionCmd,
        #[command(flatten)]
        flags: FlagArgs,
    },
    /// check the wiring; exit 1 if anything would silently send nothing
    #[command(about = "One line per check, with the absolute paths it looked at.")]
    Doctor {
        /// machine-readable output
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        flags: FlagArgs,
    },
}

#[derive(Args, Debug, Default)]
struct FlagArgs {
    /// session id fallback (default: $AOE_SESSION_ID); the session row wins
    #[arg(long)]
    session: Option<String>,
    /// profile fallback (default: $AOE_PROFILE); selects the profile config
    #[arg(long)]
    profile: Option<String>,
    /// tool fallback (default: $AOE_TOOL)
    #[arg(long)]
    tool: Option<String>,
    /// project path fallback (default: $AOE_PROJECT_PATH)
    #[arg(long)]
    project: Option<String>,
    /// group fallback (default: $AOE_GROUP_PATH)
    #[arg(long)]
    group: Option<String>,
    /// title fallback (default: $AOE_SESSION_TITLE)
    #[arg(long)]
    title: Option<String>,
    /// harness adapter row (pi, claude, codex, …) instead of detecting it from the tool
    #[arg(long)]
    adapter: Option<String>,
}

impl FlagArgs {
    fn given(&self) -> Given {
        Given {
            session: self.session.clone(),
            profile: self.profile.clone(),
            tool: self.tool.clone(),
            project: self.project.clone(),
            group: self.group.clone(),
            title: self.title.clone(),
            adapter: self.adapter.clone(),
        }
    }

    fn flags(&self, env: &Env) -> Flags {
        Flags::from_given(&self.given(), env)
    }
}

#[derive(Subcommand, Debug)]
enum ExtensionCmd {
    /// show each harness that can take an extension, and what was approved
    Status {
        /// machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// read the disclosure and approve one harness's extension
    Enable {
        /// the harness id (`pi`) — one approval covers one harness, never a bundle
        harness: String,
        /// the sha256 the disclosure printed; required when stdin is not a terminal
        #[arg(long = "accept", value_name = "SHA256")]
        accept: Option<String>,
        /// print the disclosure and this build's sha256, then stop
        #[arg(long = "print")]
        print: bool,
        /// machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// revoke an approval; the next launch stops installing the file
    Disable {
        /// the harness id (`pi`)
        harness: String,
        /// machine-readable output
        #[arg(long)]
        json: bool,
    },
}

/// Run one command line. Returns the process exit code.
pub fn run(argv: Vec<String>, env: &Env) -> i32 {
    // A named-but-absent verb answers for itself, flags and all: a bare
    // "unrecognized arguments" would read like a typo instead of a future task.
    if let Some(verb) = argv.get(1) {
        if let Some((_, owner)) = NOT_IMPLEMENTED.iter().find(|(name, _)| name == verb) {
            warn(&format!("`{verb}` is not implemented yet; it is {owner}"));
            return EXIT_USAGE;
        }
    }

    let cli = match Cli::try_parse_from(&argv) {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            return EXIT_USAGE;
        }
    };

    match cli.command {
        Command::Hook(flags) => hook(&flags, env),
        Command::Value { header, flags } => value(&header, &flags, env),
        Command::Setup {
            print,
            dry_run,
            apply,
            yes,
            undo,
            all,
            json,
            flags,
        } => setup_cmd(print, dry_run, apply, yes, undo, all, json, &flags, env),
        Command::Classify { json, flags } => classify_cmd(json, &flags, env),
        Command::Tag {
            set,
            unset,
            token,
            clear_token,
            git,
            reset,
            show,
            json,
            flags,
        } => tag_cmd(
            set,
            unset,
            token,
            clear_token,
            git,
            reset,
            show,
            json,
            &flags,
            env,
        ),
        Command::Activity {
            name,
            clear,
            show,
            json,
            flags,
        } => activity_cmd(name, clear, show, json, &flags, env),
        Command::References { json, flags } => references_cmd(json, &flags, env),
        Command::Extension { action, flags } => extension_cmd(action, &flags, env),
        Command::Doctor { json, flags } => doctor_cmd(json, &flags, env),
    }
}

fn hook(args: &FlagArgs, env: &Env) -> i32 {
    // The hook must never abort a session start: any failure is a stderr warning
    // and exit 0, including a panic in a code path we did not anticipate.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let snap = resolver::snapshot(&args.flags(env), env, None);
        let mut lines = resolver::hook_lines(&snap.resolved);
        let mut warnings = snap.warnings.clone();
        // Per-session harness config (`docs/harness-adapters.md`): the values are
        // baked into a copy this session owns, and the harness is pointed at it.
        // A failure here costs the file delivery, never the session.
        match setup::overlay_for(&snap, env) {
            Ok(Some((overlay, overlay_warnings))) => {
                warnings.extend(overlay_warnings);
                for (name, why) in &overlay.skipped {
                    warnings.push(format!(
                        "{name} was not written to the session config: {why}"
                    ));
                }
                lines.extend(overlay.env_lines());
            }
            Ok(None) => {}
            Err(err) => warnings.push(err),
        }
        (lines, warnings)
    }));
    let (lines, warnings) = match outcome {
        Ok(pair) => pair,
        Err(_) => {
            warn("unexpected error: resolver panicked");
            return EXIT_OK;
        }
    };
    warn_all(&warnings);
    let mut stdout = std::io::stdout().lock();
    for line in lines {
        let _ = writeln!(stdout, "{line}");
    }
    let _ = stdout.flush();
    EXIT_OK
}

/// What this session is doing, as one reserved tag.
///
/// The vocabulary lives in [`crate::activity`] and is closed, so this is a thin
/// validated write: it lands in the same per-session file `tag --set` writes, and
/// then refreshes the live file a running session re-reads, so the change reaches
/// the *next request* rather than the next launch. A session without a live file
/// is left alone: the launch did not set up a per-request delivery, and inventing
/// one here would claim something about the session that is not true.
fn activity_cmd(
    name: Option<String>,
    clear: bool,
    show: bool,
    json: bool,
    args: &FlagArgs,
    env: &Env,
) -> i32 {
    let flags = args.flags(env);
    let app_dir = match crate::settings::app_dir(env) {
        Ok(dir) => dir,
        Err(message) => {
            warn(&message);
            return EXIT_FAIL;
        }
    };
    let snap = resolver::snapshot(&flags, env, None);
    warn_all(&snap.warnings);
    let session = snap.facts.id.clone();
    if session.is_empty() {
        warn("`activity` needs a session: pass --session <id> or run inside a session (AOE_SESSION_ID)");
        return EXIT_USAGE;
    }
    let current = crate::activity::of(&snap.tags);

    let write = clear || (name.is_some() && !show);
    if !write {
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "session": session,
                    "activity": current.map(|activity| activity.key),
                    "label": current.map(|activity| activity.label),
                    "icon": current.map(|activity| activity.icon),
                    "tone": current.map(|activity| activity.tone),
                    "vocabulary": crate::activity::legend(),
                })
            );
            return EXIT_OK;
        }
        println!("session {session}");
        match current {
            Some(activity) => println!(
                "  activity {} ({}, icon {})",
                activity.key, activity.label, activity.icon
            ),
            None => println!("  (this session has not said what it is doing)"),
        }
        println!(
            "  {}",
            crate::activity::ALL
                .iter()
                .map(|activity| activity.key)
                .collect::<Vec<_>>()
                .join(", ")
        );
        return EXIT_OK;
    }

    let failure = if clear {
        crate::session_tags::unset_tag(&app_dir, &session, crate::activity::KEY).err()
    } else {
        let requested = name.as_deref().unwrap_or_default();
        match crate::activity::find(requested) {
            Some(activity) => crate::session_tags::set_tag(
                &app_dir,
                &session,
                &format!("{}:{}", crate::activity::KEY, activity.key),
            )
            .err(),
            None => {
                warn(&format!(
                    "`{requested}` is not an activity: {}",
                    crate::activity::ALL
                        .iter()
                        .map(|activity| activity.key)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                return EXIT_USAGE;
            }
        }
    };
    if let Some(err) = failure {
        warn(&err);
        return EXIT_FAIL;
    }

    // Re-resolve so the value written to the live file is the one the resolver
    // produced, not a copy made here: the extension repeats what it reads, and
    // this is the only place that decides what a target carries.
    let after = resolver::snapshot(&flags, env, None);
    let (adapters, adapter_warnings) = crate::adapters::load(None);
    warn_all(&adapter_warnings);
    let refreshed = crate::harness_extension::refresh_live(
        &app_dir,
        &session,
        crate::adapters::match_tool(&adapters, &after.facts.tool),
        &after.settings.harness_extension,
        &after.resolved,
        crate::harness_extension::home().as_deref(),
    );
    let live = refreshed.is_some();
    if let Some(Err(why)) = &refreshed {
        warn(&format!("the live file was not refreshed: {why}"));
    }

    let now = crate::activity::of(&after.tags);
    if json {
        println!(
            "{}",
            serde_json::json!({
                "session": session,
                "activity": now.map(|activity| activity.key),
                "live": live,
                "targets": after
                    .resolved
                    .iter()
                    .map(|item| serde_json::json!({"name": item.header, "value": item.value}))
                    .collect::<Vec<_>>(),
            })
        );
        return EXIT_OK;
    }
    match now {
        Some(activity) => println!("activity {} ({})", activity.key, activity.label),
        None => println!("activity cleared"),
    }
    if live {
        println!("  the next request this session makes carries it");
    } else {
        println!("  the next launch of this session carries it");
    }
    EXIT_OK
}

/// One session's edits: `tag --set`, `--unset`, `--token`, `--reset`, or a read.
///
/// The write goes to `<app dir>/sessions/<id>/overrides.toml` and nowhere else.
/// A session that is not named is a usage error rather than a guess: editing the
/// wrong session's tags is exactly the mistake this verb exists to avoid.
#[allow(clippy::too_many_arguments)]
fn tag_cmd(
    set: Option<String>,
    unset: Option<String>,
    token: Option<String>,
    clear_token: bool,
    git: bool,
    reset: bool,
    show: bool,
    json: bool,
    args: &FlagArgs,
    env: &Env,
) -> i32 {
    let flags = args.flags(env);
    // A session's edits live in the app dir; without one there is nothing to read
    // or write and a guess would land in the current directory.
    let app_dir = match crate::settings::app_dir(env) {
        Ok(dir) => dir,
        Err(message) => {
            warn(&message);
            return EXIT_FAIL;
        }
    };
    let mut session = flags.session.clone();
    let write = set.is_some() || unset.is_some() || token.is_some() || clear_token || git || reset;
    if write && session.is_empty() {
        // The session row is authoritative when there is one; a write into a
        // guess would land in a file no launch reads.
        let facts = resolver::resolve_session(&flags, None, env, std::time::Duration::ZERO);
        session = facts.0.id;
    } else if session.is_empty() && !write {
        session = resolver::resolve_session(&flags, None, env, std::time::Duration::ZERO)
            .0
            .id;
    }
    if session.is_empty() {
        warn("`tag` needs a session: pass --session <id> or run inside a session (AOE_SESSION_ID)");
        return EXIT_USAGE;
    }

    // Each edit is a load-and-replace of the same file, so several flags in one
    // call compose instead of overwriting each other.
    let mut failure = None;
    if reset {
        failure = crate::session_tags::reset(&app_dir, &session).err();
    }
    if failure.is_none() {
        if let Some(entry) = set.as_deref() {
            failure = crate::session_tags::set_tag(&app_dir, &session, entry).err();
        }
    }
    if failure.is_none() {
        if let Some(key) = unset.as_deref() {
            failure = crate::session_tags::unset_tag(&app_dir, &session, key).err();
        }
    }
    if failure.is_none() && git {
        // The sample set, written through the same per-session file: a user who
        // wants standard git information does not hand-write four references.
        failure = crate::session_tags::apply_preset(&app_dir, &session, "git").err();
    }
    if failure.is_none() && (token.is_some() || clear_token) {
        let value = token.as_deref().unwrap_or_default();
        failure = crate::session_tags::set_token(&app_dir, &session, value).err();
    }
    if let Some(err) = failure {
        warn(&err);
        return EXIT_FAIL;
    }

    let (edits, warnings) = crate::session_tags::load(&app_dir, &session);
    warn_all(&warnings);
    // Show the effect, not just the file: the same resolver the hook runs.
    let snap = resolver::snapshot(&flags, env, None);
    warn_all(&snap.warnings);
    if json {
        let targets: Vec<serde_json::Value> = snap
            .resolved
            .iter()
            .map(|item| {
                serde_json::json!({"name": item.header, "env_var": item.env_var, "value": item.value})
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "session": session,
                "path": crate::session_tags::path(&app_dir, &session).display().to_string(),
                "tags": edits.tags,
                "unset": edits.unset,
                "token": edits.token,
                "targets": targets,
            })
        );
        return EXIT_OK;
    }

    println!("session {session}");
    println!(
        "  overrides {}",
        crate::session_tags::path(&app_dir, &session).display()
    );
    if edits.is_empty() {
        println!("  (none: this session sends the settings as they are)");
    }
    for (key, value) in edits.entries() {
        println!("  + {key}: {value}");
    }
    for key in &edits.unset {
        println!("  - {key}");
    }
    if !edits.token.is_empty() {
        println!("  token {}", edits.token);
    }
    let _ = show;
    let mut stdout = std::io::stdout().lock();
    for item in &snap.resolved {
        let _ = writeln!(stdout, "  {} = {}", item.env_var, item.value);
    }
    let _ = stdout.flush();
    EXIT_OK
}

/// Every reference this build understands, with this session's value for each.
///
/// The editor's dropdown is fed from the same catalogue, so the two cannot list
/// different references. Git facts are resolved here on purpose: a listing that
/// shows `{git.branch}` but never what it would render is a promise without a
/// value behind it.
fn references_cmd(json: bool, args: &FlagArgs, env: &Env) -> i32 {
    let flags = args.flags(env);
    let snap = resolver::snapshot(&flags, env, None);
    warn_all(&snap.warnings);
    let (tags, _) = resolver::parse_tags(&snap.settings.tags);
    let (payload, _) = resolver::payload(&snap.settings, &snap.facts, &tags, &snap.facts.branch);
    let git_names: Vec<&str> = crate::gitinfo::REFERENCES
        .iter()
        .map(|item| item.name)
        .collect();
    let (git_fields, git_warnings) = crate::gitinfo::resolve(
        &snap.facts.path,
        &git_names,
        None,
        env,
        resolver::DEFAULT_TIMEOUT,
    );
    warn_all(&git_warnings);
    let fields = resolver::build_fields(
        &snap.settings,
        &snap.facts,
        &tags,
        &payload,
        &snap.facts.branch,
        &git_fields,
    );
    let catalog = resolver::reference_catalog();
    if json {
        let rows: Vec<serde_json::Value> = catalog
            .iter()
            .map(|item| {
                serde_json::json!({
                    "name": item.name,
                    "reference": format!("{{{}}}", item.name),
                    "group": item.group,
                    "description": item.description,
                    "sample": fields.get(item.name).cloned().unwrap_or_default(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "session": snap.facts.id,
                "project": snap.facts.path,
                "references": rows,
            })
        );
        return EXIT_OK;
    }
    println!("References — write one as {{name}} in a target template or a tag value:");
    for item in &catalog {
        let sample = fields
            .get(item.name)
            .map(String::as_str)
            .unwrap_or_default();
        // A sample is one glance, not the value: `{payload_json}` would otherwise
        // push every description off the screen.
        let (sample, cut) = resolver::clamp_bytes(sample, 27);
        let sample = if cut {
            format!("{sample}\u{2026}")
        } else {
            sample
        };
        println!(
            "  {:<16} {:<28} {}",
            format!("{{{}}}", item.name),
            sample,
            item.description
        );
    }
    for (id, tags) in crate::session_tags::PRESETS {
        let flag = match id {
            "git" => "--git".to_string(),
            other => format!("--{other}"),
        };
        println!("\n{id} preset — `{PROG} tag {flag}` writes:");
        for entry in tags {
            println!("  {entry}");
        }
    }
    EXIT_OK
}

fn value(wanted: &str, args: &FlagArgs, env: &Env) -> i32 {
    let snap = resolver::snapshot(&args.flags(env), env, None);
    warn_all(&snap.warnings);
    let found = snap
        .resolved
        .iter()
        .find(|item| item.header.eq_ignore_ascii_case(wanted));
    match found {
        Some(item) => {
            println!("{}", item.value);
        }
        None => {
            let enabled = if snap.resolved.is_empty() {
                "none".to_string()
            } else {
                snap.resolved
                    .iter()
                    .map(|item| item.header.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            warn(&format!(
                "header {wanted:?} is not enabled for this session (enabled: {enabled})"
            ));
        }
    }
    EXIT_OK
}

#[allow(clippy::too_many_arguments)]
fn setup_cmd(
    print: bool,
    dry_run: bool,
    apply: bool,
    yes: bool,
    undo: bool,
    all: bool,
    json: bool,
    args: &FlagArgs,
    env: &Env,
) -> i32 {
    if undo {
        return setup::undo_cmd(&args.flags(env), env, all);
    }
    if apply {
        if !yes {
            // Show the diff and require confirmation.
            match setup::build(&args.flags(env), env, None, None) {
                Err(err) => {
                    warn(&err.to_string());
                    return EXIT_FAIL;
                }
                Ok(plan) => {
                    warn_all(&plan.warnings);
                    for line in setup::text(&plan) {
                        println!("{line}");
                    }
                    warn("rerun with --apply --yes to write");
                    return EXIT_OK;
                }
            }
        }
        return setup::apply_cmd(&args.flags(env), env);
    }
    if !(print || dry_run || json) {
        warn("`setup` only reads unless --apply/--undo; pass --print (or --json)");
        return EXIT_USAGE;
    }
    match setup::build(&args.flags(env), env, None, None) {
        Err(err) => {
            warn(&err.to_string());
            EXIT_FAIL
        }
        Ok(plan) => {
            warn_all(&plan.warnings);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&setup::as_dict(&plan)).unwrap_or_default()
                );
            } else {
                for line in setup::text(&plan) {
                    println!("{line}");
                }
            }
            EXIT_OK
        }
    }
}

/// What is on this machine, per substrate. Exit 0 unless a row cannot deliver at
/// all: a missing config is a fact to report, not a broken install.
fn classify_cmd(json: bool, args: &FlagArgs, env: &Env) -> i32 {
    let snap = resolver::snapshot(&args.flags(env), env, None);
    warn_all(&snap.warnings);
    let (adapters, _) = crate::adapters::load(None);
    let adapter = if args.adapter.is_some() {
        args.adapter
            .as_deref()
            .and_then(|name| crate::adapters::find(&adapters, name))
    } else {
        crate::adapters::match_tool(&adapters, &snap.facts.tool)
    };
    let live = crate::substrate::live_sessions(env);
    let mut cache = match crate::settings::app_dir(env) {
        Ok(app_dir) => crate::substrate::Cache::load(&app_dir),
        Err(_) => crate::substrate::Cache::disabled(),
    };
    let findings = crate::substrate::classify(
        adapter,
        &snap.settings.targets,
        env,
        &snap.facts.id,
        &live,
        &mut cache,
    );
    cache.save();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&crate::substrate::as_json(&findings)).unwrap_or_default()
        );
    } else {
        for line in crate::substrate::format_lines(&findings) {
            println!("{line}");
        }
    }
    if findings
        .iter()
        .any(|finding| finding.verdict == crate::substrate::UNSUPPORTED)
    {
        return EXIT_FAIL;
    }
    EXIT_OK
}

/// `extension status|enable|disable`: the one place an approval is recorded.
///
/// Intent lives in the settings (the user writes it); approval lives in the
/// receipt file. `enable` prints the disclosure first and only writes when it
/// has the digest the user was shown — from `--accept` on a non-tty, from an
/// explicit `yes` on a terminal. Nothing here writes the global settings.
fn extension_cmd(action: ExtensionCmd, args: &FlagArgs, env: &Env) -> i32 {
    let app_dir = match crate::settings::app_dir(env) {
        Ok(dir) => dir,
        Err(message) => {
            warn(&message);
            return EXIT_USAGE;
        }
    };
    let (adapters, warnings) = crate::adapters::load(None);
    warn_all(&warnings);
    match action {
        ExtensionCmd::Status { json } => extension_status(&adapters, &app_dir, args, env, json),
        ExtensionCmd::Enable {
            harness,
            accept,
            print,
            json,
        } => extension_enable(
            &adapters, &app_dir, args, env, &harness, accept, print, json,
        ),
        ExtensionCmd::Disable { harness, json } => {
            let Some(adapter) = crate::adapters::find(&adapters, &harness) else {
                warn(&format!(
                    "no adapter row for {harness:?}; `doctor` lists the harnesses this build knows"
                ));
                return EXIT_USAGE;
            };
            let result = crate::harness_extension::revoke(
                &app_dir,
                crate::harness_extension::home().as_deref(),
                adapter,
            );
            match result {
                Ok(revoked) if !revoked.had_receipt => {
                    warn(&format!("{} had no approval to revoke", adapter.agent));
                    EXIT_OK
                }
                Ok(revoked) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "revoked": adapter.agent,
                                "receipt": crate::harness_extension::receipt_path(&app_dir)
                                    .display().to_string(),
                                "removed": revoked.removed.iter()
                                    .map(|path| path.display().to_string())
                                    .collect::<Vec<_>>(),
                                "kept": revoked.kept.iter()
                                    .map(|(path, why)| serde_json::json!({
                                        "path": path.display().to_string(),
                                        "why": why,
                                    }))
                                    .collect::<Vec<_>>(),
                            })
                        );
                    } else {
                        println!(
                            "revoked: {} ({} removed from {})",
                            adapter.agent,
                            adapter.agent,
                            crate::harness_extension::receipt_path(&app_dir).display()
                        );
                        println!(
                            "a session already running stops sending dynamic tags on its next \
                             request; the next launch installs no extension. The strands of \
                             `{}` in the settings are the user's to remove.",
                            crate::harness_extension::SETTING
                        );
                        // Say what stayed, and why, rather than letting a user find a
                        // directory they expected to be gone.
                        for (path, why) in &revoked.kept {
                            println!("kept: {} ({why})", path.display());
                        }
                    }
                    EXIT_OK
                }
                Err(message) => {
                    warn(&message);
                    EXIT_FAIL
                }
            }
        }
    }
}

fn extension_status(
    adapters: &[crate::adapters::Adapter],
    app_dir: &std::path::Path,
    args: &FlagArgs,
    env: &Env,
    json: bool,
) -> i32 {
    let snap = resolver::snapshot(&args.flags(env), env, None);
    let (receipts, warnings) = crate::harness_extension::receipts(app_dir);
    warn_all(&warnings);
    let mut rows = Vec::new();
    // A requested row with no destination is listed too: the user asked for it,
    // and "nothing was installed" has to have a stated reason.
    for adapter in adapters.iter().filter(|row| {
        crate::harness_extension::carrier(row).is_some()
            || snap
                .settings
                .harness_extension
                .iter()
                .any(|name| name == &row.agent)
    }) {
        let wanted = snap
            .settings
            .harness_extension
            .iter()
            .any(|name| name == &adapter.agent);
        let (state, detail) = match crate::harness_extension::plan(
            adapter,
            &snap.settings.harness_extension,
            app_dir,
            &snap.facts.id,
            crate::harness_extension::home().as_deref(),
        ) {
            Ok(plan) => (
                crate::doctor::OK,
                format!("approved ({})", &plan.sha256[..8]),
            ),
            Err(refusal) => (
                if refusal.worth_a_warning() {
                    crate::doctor::WARN
                } else {
                    crate::doctor::SKIP
                },
                refusal.reason().to_string(),
            ),
        };
        rows.push((adapter, wanted, state, detail));
    }
    if json {
        let value: Vec<serde_json::Value> = rows
            .iter()
            .map(|(adapter, wanted, state, detail)| {
                serde_json::json!({
                    "harness": adapter.agent,
                    "requested": wanted,
                    "state": state,
                    "detail": detail,
                    "artifact": adapter.extension_artifact,
                    "receipt": receipts.get(&adapter.agent).map(|receipt| serde_json::json!({
                        "sha256": receipt.sha256,
                        "accepted_at": receipt.accepted_at,
                        "disclosure_version": receipt.disclosure_version,
                        "tested_harness": receipt.tested_harness,
                    })),
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(value));
        return EXIT_OK;
    }
    if rows.is_empty() {
        println!("no harness in this build loads an in-process extension: every harness keeps to launch-time tags");
        return EXIT_OK;
    }
    for (adapter, wanted, state, detail) in rows {
        println!(
            "{:<10} {:<6} {:<6} {detail}",
            adapter.agent,
            state,
            if wanted { "yes" } else { "no" },
        );
    }
    println!(
        "\ncolumns: harness, state, in `{}`, reason — `aoe-llm-tags extension enable <harness>` prints the disclosure",
        crate::harness_extension::SETTING
    );
    if snap.settings.harness_extension.is_empty() {
        println!(
            "no harness is requested yet: add it to `{}` in settings, then approve it once",
            crate::harness_extension::SETTING
        );
    }
    EXIT_OK
}

#[allow(clippy::too_many_arguments)]
fn extension_enable(
    adapters: &[crate::adapters::Adapter],
    app_dir: &std::path::Path,
    args: &FlagArgs,
    env: &Env,
    harness: &str,
    accept: Option<String>,
    print_only: bool,
    json: bool,
) -> i32 {
    let Some(adapter) = crate::adapters::find(adapters, harness) else {
        warn(&format!(
            "no adapter row for {harness:?}; `aoe-llm-tags extension status` lists the harnesses \
             this build can serve"
        ));
        return EXIT_USAGE;
    };
    let snap = resolver::snapshot(&args.flags(env), env, None);
    let home = crate::harness_extension::home();
    let proposal =
        match crate::harness_extension::proposal(adapter, app_dir, &snap.facts.id, home.as_deref())
        {
            Ok(plan) => plan,
            Err(refusal) => {
                warn(&refusal.detail());
                return EXIT_USAGE;
            }
        };
    let text = crate::harness_extension::disclosure(&proposal, &harness_version(&adapter.agent));
    if print_only {
        println!("{text}");
        println!();
        println!("nothing was written. Approve with:");
        println!(
            "  aoe-llm-tags extension enable {} --accept {}",
            adapter.agent, proposal.sha256
        );
        return EXIT_OK;
    }

    // Whatever the answer, the user sees what they are agreeing to: on a tty the
    // prompt follows it, and `--accept` prints it too, so a scripted approval is
    // still an informed one (and a log keeps the wording it approved).
    println!("{text}");

    let approved = match accept {
        Some(digest) => {
            let digest = digest.trim().to_ascii_lowercase();
            if digest != proposal.sha256 {
                warn(&format!(
                    "the digest you accepted does not match this build's file \
                     (accepted {digest}, this build {})",
                    proposal.sha256
                ));
                warn("nothing was approved");
                return EXIT_FAIL;
            }
            true
        }
        None if std::io::stdin().is_terminal() => {
            print!(
                "\nType `yes` to approve {} for this machine: ",
                adapter.agent
            );
            let _ = std::io::stdout().flush();
            let mut answer = String::new();
            let _ = std::io::stdin().read_line(&mut answer);
            answer.trim().eq_ignore_ascii_case("yes")
        }
        None => {
            println!("{text}");
            warn(
                "stdin is not a terminal, so there is nobody to read the disclosure: re-run with \
                 `--accept <sha256>` above",
            );
            return EXIT_USAGE;
        }
    };
    if !approved {
        warn("not approved; nothing was written");
        return EXIT_OK;
    }

    match crate::harness_extension::accept(
        app_dir,
        home.as_deref(),
        adapter,
        &proposal.sha256,
        &harness_version(&adapter.agent),
    ) {
        Ok(receipt) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "approved": adapter.agent,
                        "sha256": receipt.sha256,
                        "disclosure_version": receipt.disclosure_version,
                        "accepted_at": receipt.accepted_at,
                        "receipt": crate::harness_extension::receipt_path(app_dir).display().to_string(),
                        "installs_on_next_launch": snap.settings.harness_extension
                            .iter().any(|name| name == &adapter.agent),
                    })
                );
            } else {
                println!(
                    "approved: {} (sha256 {}, disclosure v{}), recorded in {}",
                    adapter.agent,
                    &receipt.sha256[..12],
                    receipt.disclosure_version,
                    crate::harness_extension::receipt_path(app_dir).display()
                );
            }
            // Intent is the user's: the plugin has no capability to write the
            // settings, and asking for one to save a line would be a worse trade.
            if !snap
                .settings
                .harness_extension
                .iter()
                .any(|name| name == &adapter.agent)
            {
                warn(&format!(
                    "{} is not in `{}` yet, so nothing will be installed. Add it in the plugin \
                     settings:  {} = [\"{}\"]",
                    adapter.agent,
                    crate::harness_extension::SETTING,
                    crate::harness_extension::SETTING,
                    adapter.agent
                ));
            } else {
                match proposal.carrier {
                    crate::harness_extension::Carrier::Session => println!(
                        "the next launch of a {} session installs it in that session's config copy",
                        adapter.agent
                    ),
                    crate::harness_extension::Carrier::User => {
                        for placement in proposal.parts() {
                            println!("installed: {}", placement.display);
                        }
                    }
                }
            }
            EXIT_OK
        }
        Err(message) => {
            warn(&message);
            EXIT_FAIL
        }
    }
}

/// The harness's own version, for the receipt. Informational only: a harness
/// upgrade never re-asks for approval, and a missing binary is not an error.
fn harness_version(agent: &str) -> String {
    let output = std::process::Command::new(agent).arg("--version").output();
    let Ok(output) = output else {
        return String::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(48)
        .collect()
}

fn doctor_cmd(json: bool, args: &FlagArgs, env: &Env) -> i32 {
    let report = doctor::run(&args.flags(env), env, None);
    println!(
        "{}",
        if json {
            doctor::format_json(&report)
        } else {
            doctor::format_text(&report)
        }
    );
    if report.ok() {
        return EXIT_OK;
    }
    let failed: Vec<&str> = report
        .checks
        .iter()
        .filter(|check| check.status == doctor::FAIL)
        .map(|check| check.name.as_str())
        .collect();
    warn(&format!(
        "{} check(s) failed: {}",
        failed.len(),
        failed.join(", ")
    ));
    EXIT_FAIL
}

pub fn warn(message: &str) {
    eprintln!("{PROG}: {message}");
}

pub(crate) fn warn_all(messages: &[String]) {
    let mut seen: Vec<&str> = Vec::new();
    for message in messages {
        if seen.contains(&message.as_str()) {
            continue;
        }
        seen.push(message);
        warn(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unimplemented_verbs_answer_for_themselves() {
        let env = Env::default();
        assert_eq!(
            run(vec![PROG.to_string(), "verify".to_string()], &env),
            EXIT_USAGE
        );
    }

    #[test]
    fn setup_without_print_is_a_usage_error() {
        let env = Env::default();
        assert_eq!(
            run(vec![PROG.to_string(), "setup".to_string()], &env),
            EXIT_USAGE
        );
    }

    #[test]
    fn an_unknown_verb_is_a_usage_error() {
        let env = Env::default();
        assert_eq!(
            run(vec![PROG.to_string(), "nope".to_string()], &env),
            EXIT_USAGE
        );
    }
}

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

use std::io::Write;

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
    /// print the hook and header-mapping lines to paste
    #[command(
        about = "Print the two [host_hooks] lines and one mapping line per enabled header. \
                 Print-only: nothing is written."
    )]
    Setup {
        /// print the lines (the only mode implemented so far)
        #[arg(long = "print")]
        print: bool,
        /// machine-readable output
        #[arg(long)]
        json: bool,
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
        Command::Setup { print, json, flags } => setup_cmd(print, json, &flags, env),
        Command::Doctor { json, flags } => doctor_cmd(json, &flags, env),
    }
}

fn hook(args: &FlagArgs, env: &Env) -> i32 {
    // The hook must never abort a session start: any failure is a stderr warning
    // and exit 0, including a panic in a code path we did not anticipate.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let snap = resolver::snapshot(&args.flags(env), env, None);
        (resolver::hook_lines(&snap.resolved), snap.warnings)
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

fn setup_cmd(print: bool, json: bool, args: &FlagArgs, env: &Env) -> i32 {
    if !print && !json {
        warn("`setup` is print-only in M0; pass --print (or --json)");
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

fn warn_all(messages: &[String]) {
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

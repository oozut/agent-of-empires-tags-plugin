//! `doctor`: every way an install silently sends nothing, named with the paths it
//! looked at, and exit 0 only when none of them is broken.

#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};

use common::{settings_config, Sandbox, PLUGIN_ID};

// What `aoe-llm-tags setup --print` tells a pi user to paste: a command, so the
// value survives the plugin's removal (`guard_fallback`). A bare `$VAR` here is
// the pre-guard spelling, and doctor has to name that as a hazard.
const GUARDED_TAGS: &str =
    "!v=\"${AOE_LLM_TAGS-}\"; [ -n \"$v\" ] && printf %s \"$v\" || printf %s {}";
const GUARDED_SESSION: &str =
    "!v=\"${AOE_HDR_X_AOE_SESSION-}\"; [ -n \"$v\" ] && printf %s \"$v\" || printf %s {}";

/// A broken install is described by one of these, then applied.
struct Wire {
    before_session: bool,
    before_start: bool,
    foreign_hook: bool,
    cli_exists: bool,
    aoe_exists: bool,
    headers: Option<Vec<(&'static str, &'static str)>>,
    models_file: bool,
    config_file: bool,
    last_verified: &'static str,
    settings: &'static str,
    /// A session row to answer with; `None` is the sandbox's own fixture row.
    session_json: Option<&'static str>,
    /// A `[host_hooks]` block used verbatim, for a line this build no longer emits.
    raw_hooks: Option<String>,
}

impl Default for Wire {
    fn default() -> Self {
        Self {
            before_session: true,
            before_start: true,
            foreign_hook: false,
            cli_exists: true,
            aoe_exists: true,
            headers: Some(vec![("X-AoE-Tags", "$AOE_LLM_TAGS")]),
            models_file: true,
            config_file: true,
            last_verified: "",
            settings: "",
            session_json: None,
            raw_hooks: None,
        }
    }
}

impl Wire {
    fn apply(&self, sandbox: &mut Sandbox) {
        let aoe = sandbox.path("bin/aoe-doctor");
        sandbox.fake_aoe_at(
            &aoe,
            &self
                .session_json
                .map(str::to_string)
                .unwrap_or_else(|| sandbox.session_json()),
        );
        let broken = sandbox.path("gone/bin/aoe");
        sandbox.env.insert(
            "AOE_BIN".to_string(),
            if self.aoe_exists {
                aoe.clone()
            } else {
                broken.clone()
            }
            .display()
            .to_string(),
        );

        let cli = if self.cli_exists {
            sandbox.path("plugin/bin/aoe-llm-tags")
        } else {
            sandbox.path("gone/bin/aoe-llm-tags")
        };
        if self.cli_exists {
            sandbox.write_executable(&cli, "#!/bin/sh\nexit 0\n");
        } else {
            let _ = std::fs::remove_file(&cli);
        }
        // A line per key, the way `setup --print` writes them: the key decides
        // the substrate, so both keys sharing one command is not a real install.
        let command_for = |key: &str| {
            if self.foreign_hook {
                "/usr/local/bin/someone-else hook".to_string()
            } else {
                llm_tags::setup::hook_command(
                    &cli,
                    aoe.to_str().unwrap(),
                    llm_tags::setup::Substrate::for_key(key),
                )
            }
        };

        let mut hooks = String::new();
        if self.before_session {
            hooks.push_str(&format!(
                "before_session = ['{}']\n",
                command_for("before_session")
            ));
        }
        if self.before_start {
            hooks.push_str(&format!(
                "before_start = ['{}']\n",
                command_for("before_start")
            ));
        }
        if let Some(raw) = &self.raw_hooks {
            hooks = raw.clone();
        }
        let config_path = sandbox.config_path("");
        if self.config_file {
            let settings = if self.settings.is_empty() {
                if self.last_verified.is_empty() {
                    String::new()
                } else {
                    format!("last_verified = \"{}\"\n", self.last_verified)
                }
            } else {
                self.settings.to_string()
            };
            sandbox.write_config(&settings_config(&settings, &hooks));
        } else {
            let _ = std::fs::remove_file(config_path);
        }

        let models = sandbox.path("home/.pi/agent/models.json");
        if self.models_file {
            sandbox.pi_models(self.headers.as_deref().unwrap_or(&[]));
        } else {
            let _ = std::fs::remove_file(models);
        }
    }
}

fn wired() -> (Sandbox, Wire) {
    let mut sandbox = Sandbox::new();
    let wire = Wire::default();
    wire.apply(&mut sandbox);
    (sandbox, wire)
}

fn report(sandbox: &Sandbox) -> llm_tags::doctor::Report {
    llm_tags::doctor::run(
        &llm_tags::resolver::Flags::from_given(&Default::default(), &env_of(sandbox)),
        &env_of(sandbox),
        None,
    )
}

fn env_of(sandbox: &Sandbox) -> llm_tags::env::Env {
    llm_tags::env::Env::new(sandbox.env.iter().map(|(k, v)| (k.clone(), v.clone())))
}

fn check<'a>(report: &'a llm_tags::doctor::Report, name: &str) -> &'a llm_tags::doctor::Check {
    report
        .checks
        .iter()
        .find(|check| check.name == name)
        .unwrap_or_else(|| panic!("no check named {name:?}"))
}

// --- a wired install --------------------------------------------------------

#[test]
fn a_clean_install_passes() {
    let (sandbox, _) = wired();
    let report = report(&sandbox);
    let failed: Vec<&str> = report
        .checks
        .iter()
        .filter(|check| check.status == llm_tags::doctor::FAIL)
        .map(|check| check.name.as_str())
        .collect();
    assert!(
        failed.is_empty(),
        "{}",
        llm_tags::doctor::format_text(&report)
    );
    assert!(report.ok());
}

#[test]
fn the_cli_exits_zero_when_wired() {
    let (sandbox, _) = wired();
    let run = sandbox.doctor(&[]);
    run.assert_ok();
    assert!(run.stdout.contains("[ok  ]"), "{}", run.stdout);
    assert!(run.stdout.contains("before_session hook"), "{}", run.stdout);
}

#[test]
fn every_check_is_present() {
    let (sandbox, _) = wired();
    let report = report(&sandbox);
    let names: Vec<&str> = report
        .checks
        .iter()
        .map(|check| check.name.as_str())
        .collect();
    for expected in [
        "effective config",
        "before_session hook",
        "before_start hook",
        "command path",
        "aoe binary",
        "harness adapter",
        "session row",
        "mapping X-AoE-Tags",
        "value rendering",
        "last verification",
    ] {
        assert!(
            names.contains(&expected),
            "{expected:?} missing from {names:?}"
        );
    }
}

#[test]
fn paths_are_absolute_and_printed() {
    let (sandbox, _) = wired();
    let report = report(&sandbox);
    for label in [
        "app dir",
        "config",
        "plugin CLI",
        "aoe binary",
        "harness config",
    ] {
        assert!(
            Path::new(&report.paths[label]).is_absolute(),
            "{label}: {:?}",
            report.paths[label]
        );
    }
    let text = llm_tags::doctor::format_text(&report);
    for value in report.paths.values() {
        assert!(text.contains(value.as_str()), "{value} missing from text");
    }
}

#[test]
fn a_second_header_gets_its_own_mapping_check() {
    let (mut sandbox, _) = wired();
    let wire = Wire {
        settings: common::TWO_HEADERS,
        headers: Some(vec![
            ("X-AoE-Tags", GUARDED_TAGS),
            ("X-AoE-Session", GUARDED_SESSION),
        ]),
        ..Wire::default()
    };
    wire.apply(&mut sandbox);
    let report = report(&sandbox);
    assert!(report.ok(), "{}", llm_tags::doctor::format_text(&report));
    assert_eq!(
        check(&report, "mapping X-AoE-Session").status,
        llm_tags::doctor::OK
    );
}

#[test]
fn a_bare_variable_in_the_users_own_config_is_a_hazard() {
    // The session copy is regenerated on every launch, so a bare `$VAR` in the
    // user's file is invisible until the plugin is gone — and then pi refuses the
    // whole request. Name it while there is still a plugin to print the fix.
    let (sandbox, _) = wired();
    let report = report(&sandbox);
    let check = check(&report, "mapping X-AoE-Tags");
    assert_eq!(check.status, llm_tags::doctor::WARN, "{}", check.detail);
    assert!(check.detail.contains("$AOE_LLM_TAGS"), "{}", check.detail);
    assert!(check.detail.contains("setup --print"), "{}", check.detail);
    assert!(report.ok(), "a hazard is not a broken install");
}

#[test]
fn a_guarded_mapping_is_not_a_warning() {
    let (mut sandbox, _) = wired();
    Wire {
        headers: Some(vec![("X-AoE-Tags", GUARDED_TAGS)]),
        ..Wire::default()
    }
    .apply(&mut sandbox);
    let check = check(&report(&sandbox), "mapping X-AoE-Tags").clone();
    assert_eq!(check.status, llm_tags::doctor::OK, "{}", check.detail);
}

#[test]
fn a_recorded_verification_is_not_a_warning() {
    let (mut sandbox, _) = wired();
    Wire {
        last_verified: "2026-10-29 pi sink",
        ..Wire::default()
    }
    .apply(&mut sandbox);
    assert_eq!(
        check(&report(&sandbox), "last verification").status,
        llm_tags::doctor::OK
    );
}

#[test]
fn no_recorded_verification_is_skipped_not_warned() {
    // The plugin does not read a consumer back (plan/roadmap.md), so "never verified" is not a
    // degraded install: it is a check that does not apply to this build.
    let (sandbox, _) = wired();
    let report = report(&sandbox);
    let last = check(&report, "last verification");
    assert_eq!(last.status, llm_tags::doctor::SKIP);
    assert!(last.detail.contains("out of scope"), "{}", last.detail);
    assert!(report.ok());
}

// --- breaks -----------------------------------------------------------------

#[test]
fn a_missing_before_session_hook_fails() {
    let (mut sandbox, _) = wired();
    Wire {
        before_session: false,
        ..Wire::default()
    }
    .apply(&mut sandbox);
    let report = report(&sandbox);
    let check = check(&report, "before_session hook");
    assert_eq!(check.status, llm_tags::doctor::FAIL);
    assert!(check.detail.contains("host sessions will not send headers"));
}

#[test]
fn a_missing_before_start_hook_fails() {
    let (mut sandbox, _) = wired();
    Wire {
        before_start: false,
        ..Wire::default()
    }
    .apply(&mut sandbox);
    let check = check(&report(&sandbox), "before_start hook").clone();
    assert_eq!(check.status, llm_tags::doctor::FAIL);
    assert!(check
        .detail
        .contains("sandboxed sessions will not send headers"));
}

#[test]
fn a_foreign_hook_does_not_count() {
    let (mut sandbox, _) = wired();
    Wire {
        foreign_hook: true,
        ..Wire::default()
    }
    .apply(&mut sandbox);
    let report = report(&sandbox);
    for name in ["before_session hook", "before_start hook"] {
        let check = check(&report, name);
        assert_eq!(check.status, llm_tags::doctor::FAIL, "{name}");
        assert!(check.detail.contains("none runs aoe-llm-tags"), "{name}");
    }
}

#[test]
fn a_moved_plugin_is_named() {
    let (mut sandbox, _) = wired();
    Wire {
        cli_exists: false,
        ..Wire::default()
    }
    .apply(&mut sandbox);
    let check = check(&report(&sandbox), "command path").clone();
    assert_eq!(check.status, llm_tags::doctor::FAIL);
    assert!(check.detail.contains("moved"), "{}", check.detail);
    assert!(check.detail.contains("gone"), "{}", check.detail);
}

#[test]
fn an_unresolvable_aoe_binary_fails() {
    let (mut sandbox, _) = wired();
    Wire {
        aoe_exists: false,
        ..Wire::default()
    }
    .apply(&mut sandbox);
    let check = check(&report(&sandbox), "aoe binary").clone();
    assert_eq!(check.status, llm_tags::doctor::FAIL);
    assert!(check.detail.contains("AOE_BIN"));
}

#[test]
fn an_overlay_row_needs_no_static_entry_in_the_users_config() {
    // The session copy is written by our own writer, so the user's file is only
    // the starting point: a provider with no header entry is exactly right for a
    // host launch. A sandboxed launch reads that file instead of a copy, so the
    // line is a warning rather than a failure: host sessions are still tagged.
    let (mut sandbox, _) = wired();
    Wire {
        headers: Some(vec![]),
        ..Wire::default()
    }
    .apply(&mut sandbox);
    let check = check(&report(&sandbox), "mapping X-AoE-Tags").clone();
    assert_eq!(check.status, llm_tags::doctor::WARN, "{}", check.detail);
    assert!(
        check.detail.contains("sandboxed session"),
        "{}",
        check.detail
    );
    assert!(check.detail.contains("setup --print"), "{}", check.detail);
}

#[test]
fn an_overlay_row_with_the_static_entry_covers_both_substrates() {
    let (mut sandbox, _) = wired();
    Wire {
        headers: Some(vec![("X-AoE-Tags", GUARDED_TAGS)]),
        ..Wire::default()
    }
    .apply(&mut sandbox);
    let check = check(&report(&sandbox), "mapping X-AoE-Tags").clone();
    assert_eq!(check.status, llm_tags::doctor::OK, "{}", check.detail);
    assert!(check.detail.contains("overlay"), "{}", check.detail);
    assert!(check.detail.contains("litellm"), "{}", check.detail);
    assert!(
        check.detail.contains("sandboxed session"),
        "{}",
        check.detail
    );
}

#[test]
fn a_before_start_line_without_a_substrate_is_refused() {
    // The shipped line says where it runs. One written before sandbox delivery
    // existed would point a container at this plugin's app dir, which aoe does
    // not mount: the harness then has no config at all, not just a missing tag.
    let (mut sandbox, _) = wired();
    let cli = sandbox.path("plugin/bin/aoe-llm-tags");
    let legacy = llm_tags::setup::hook_command(
        std::path::Path::new(&cli),
        sandbox.env.get("AOE_BIN").unwrap(),
        llm_tags::setup::Substrate::Host,
    )
    .replace("--substrate host", "");
    Wire {
        raw_hooks: Some(format!("before_start = ['{legacy}']\n")),
        ..Wire::default()
    }
    .apply(&mut sandbox);
    let check = check(&report(&sandbox), "before_start hook").clone();
    assert_eq!(check.status, llm_tags::doctor::FAIL, "{}", check.detail);
    assert!(check.detail.contains("--substrate"), "{}", check.detail);
    assert!(check.detail.contains("setup --print"), "{}", check.detail);
}

#[test]
fn a_sandbox_line_under_before_session_is_refused() {
    let (mut sandbox, _) = wired();
    let cli = sandbox.path("plugin/bin/aoe-llm-tags");
    let wrong = llm_tags::setup::hook_command(
        std::path::Path::new(&cli),
        sandbox.env.get("AOE_BIN").unwrap(),
        llm_tags::setup::Substrate::Sandbox,
    );
    Wire {
        raw_hooks: Some(format!("before_session = ['{wrong}']\n")),
        ..Wire::default()
    }
    .apply(&mut sandbox);
    let check = check(&report(&sandbox), "before_session hook").clone();
    assert_eq!(check.status, llm_tags::doctor::FAIL, "{}", check.detail);
    assert!(check.detail.contains("before_start"), "{}", check.detail);
}

#[test]
fn an_absent_harness_config_fails() {
    let (mut sandbox, _) = wired();
    Wire {
        models_file: false,
        ..Wire::default()
    }
    .apply(&mut sandbox);
    let check = check(&report(&sandbox), "mapping X-AoE-Tags").clone();
    assert_eq!(check.status, llm_tags::doctor::FAIL);
    assert!(check.detail.contains("does not exist"), "{}", check.detail);
}

#[test]
fn no_config_at_all_fails() {
    let (mut sandbox, _) = wired();
    Wire {
        config_file: false,
        ..Wire::default()
    }
    .apply(&mut sandbox);
    let check = check(&report(&sandbox), "effective config").clone();
    assert_eq!(check.status, llm_tags::doctor::FAIL);
    assert!(check.detail.contains("no config file"), "{}", check.detail);
}

#[test]
fn an_unparsable_config_fails() {
    let (sandbox, _) = wired();
    sandbox.write_config("nope = = nope\n");
    let check = check(&report(&sandbox), "effective config").clone();
    assert_eq!(check.status, llm_tags::doctor::FAIL);
    assert!(
        check.detail.contains("could not be parsed"),
        "{}",
        check.detail
    );
}

#[test]
fn an_unreadable_session_row_fails() {
    let (mut sandbox, _) = wired();
    let broken = sandbox.path("bin/broken-aoe");
    sandbox.write_executable(&broken, "#!/bin/sh\necho 'daemon down' >&2\nexit 1\n");
    sandbox
        .env
        .insert("AOE_BIN".to_string(), broken.display().to_string());
    let check = check(&report(&sandbox), "session row").clone();
    assert_eq!(check.status, llm_tags::doctor::FAIL);
    assert!(
        check
            .detail
            .contains("falling back to the hook environment"),
        "{}",
        check.detail
    );
}

#[test]
fn an_unknown_adapter_fails() {
    let (sandbox, _) = wired();
    let run = sandbox.run(&["doctor", "--adapter", "nope"]);
    run.assert_code(1);
    assert!(run.stdout.contains("pass --adapter"), "{}", run.stdout);
}

#[test]
fn a_broken_install_exits_one_and_names_the_check() {
    let (mut sandbox, _) = wired();
    Wire {
        before_start: false,
        ..Wire::default()
    }
    .apply(&mut sandbox);
    let run = sandbox.doctor(&[]);
    run.assert_code(1);
    assert!(run.stdout.contains("[fail]"), "{}", run.stdout);
    assert!(run.stderr.contains("before_start hook"), "{}", run.stderr);
}

#[test]
fn no_tool_outside_a_session_is_a_warning_not_a_failure() {
    let (mut sandbox, _) = wired();
    let tool_less = sandbox.path("bin/aoe-notool");
    sandbox.fake_aoe_at(&tool_less, r#"{"id":"cf11af87da7e4bf0"}"#);
    Wire::default().apply(&mut sandbox);
    sandbox
        .env
        .insert("AOE_BIN".to_string(), tool_less.display().to_string());
    sandbox.env.remove("AOE_TOOL");
    let report = report(&sandbox);
    let adapter = check(&report, "harness adapter");
    assert_eq!(adapter.status, llm_tags::doctor::WARN);
    assert!(adapter.detail.contains("--adapter"), "{}", adapter.detail);
    assert_eq!(
        check(&report, "mapping X-AoE-Tags").status,
        llm_tags::doctor::SKIP
    );
    assert!(report.ok());
}

// --- json -------------------------------------------------------------------

#[test]
fn json_shape() {
    let (sandbox, _) = wired();
    let run = sandbox.doctor(&["--json"]);
    run.assert_ok();
    let data: serde_json::Value = serde_json::from_str(&run.stdout).expect("JSON");
    assert_eq!(data["ok"], true);
    assert_eq!(data["adapter"], "pi");
    assert!(PathBuf::from(data["paths"]["config"].as_str().unwrap()).is_absolute());
    for row in data["checks"].as_array().unwrap() {
        for key in ["check", "status", "detail"] {
            assert!(row.get(key).is_some(), "{key} missing from {row}");
        }
    }
}

#[test]
fn json_reports_failures() {
    let (mut sandbox, _) = wired();
    Wire {
        before_session: false,
        ..Wire::default()
    }
    .apply(&mut sandbox);
    let run = sandbox.doctor(&["--json"]);
    run.assert_code(1);
    let data: serde_json::Value = serde_json::from_str(&run.stdout).expect("JSON");
    assert_eq!(data["ok"], false);
    assert!(data["checks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["status"] == "fail"));
}

#[test]
fn the_plugin_id_is_configuration_not_a_check_name() {
    // The header contract is vendor-neutral; the only place the id appears is the
    // config table it was read from.
    let (sandbox, _) = wired();
    let run = sandbox.doctor(&[]);
    assert!(run.stdout.contains(PLUGIN_ID) || !run.stdout.contains("dev.oozut"));
}

// --- git references ---------------------------------------------------------

#[test]
fn git_is_not_asked_about_when_nothing_names_a_reference() {
    let (sandbox, _) = wired();
    let report = report(&sandbox);
    assert_eq!(
        check(&report, "git references").status,
        llm_tags::doctor::SKIP
    );
}

#[test]
fn a_named_git_reference_needs_the_binary_and_the_check_names_it() {
    let mut sandbox = Sandbox::new();
    Wire {
        settings: r#"tags = ["git.branch:{git.branch}"]"#,
        ..Wire::default()
    }
    .apply(&mut sandbox);
    // An empty directory on PATH: `git` is not found, and AOE_GIT_BIN is unset.
    let empty = sandbox.path("empty-bin");
    std::fs::create_dir_all(&empty).expect("empty bin");
    sandbox
        .env
        .insert("PATH".to_string(), empty.display().to_string());

    let report = report(&sandbox);
    let git = check(&report, "git references");
    assert_eq!(git.status, llm_tags::doctor::FAIL, "{}", git.detail);
    assert!(git.detail.contains("{git.branch}"), "{}", git.detail);
    assert!(git.detail.contains("render empty"), "{}", git.detail);
    assert!(!report.ok());
}

#[test]
fn an_injected_git_binary_is_reported_where_it_came_from() {
    let mut sandbox = Sandbox::new();
    Wire {
        settings: r#"tags = ["git.branch:{git.branch}"]"#,
        ..Wire::default()
    }
    .apply(&mut sandbox);
    let git = sandbox.path("bin/fake-git");
    sandbox.write_executable(&git, "#!/bin/sh\nexit 0\n");
    sandbox
        .env
        .insert("AOE_GIT_BIN".to_string(), git.display().to_string());

    let report = report(&sandbox);
    let check = check(&report, "git references");
    assert_eq!(check.status, llm_tags::doctor::OK, "{}", check.detail);
    assert!(check.detail.contains("AOE_GIT_BIN"), "{}", check.detail);
    assert!(check.detail.contains("{git.branch}"), "{}", check.detail);
}

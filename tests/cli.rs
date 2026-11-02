//! `hook`, `value` and `setup --print`, run as the user runs them: a real process,
//! a real `aoe` subprocess, stdout and stderr kept apart.

#![cfg(unix)]

mod common;

use std::path::Path;

use common::{settings_config, Run, Sandbox, CLI, PLUGIN_ID, SESSION_JSON};

const DEFAULT_VALUE: &str = "aoe,feature:,session:S-cf11";

fn hook(sandbox: &Sandbox) -> Run {
    sandbox.run(&["hook"])
}

#[test]
fn hook_prints_one_name_value_line() {
    let sandbox = Sandbox::new();
    let run = hook(&sandbox);
    run.assert_ok();
    // NAME is the environment variable: aoe parses hook stdout as KEY=VALUE.
    assert_eq!(run.lines(), vec![format!("AOE_LLM_TAGS={DEFAULT_VALUE}")]);
    assert_eq!(run.stderr, "");
}

#[test]
fn stdout_never_carries_diagnostics() {
    let sandbox = Sandbox::new();
    let run = sandbox.run(&["hook", "--flag-that-does-not-exist"]);
    assert_eq!(run.stdout, "");
    run.assert_code(2);
}

#[test]
fn the_session_row_beats_the_environment() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config(
        "headers = [{ header = \"X-Project\", template = \"{project}\", env_var = \"P\" }]",
        "",
    ));
    let run = sandbox.run_with(&["hook"], &[("AOE_PROJECT_PATH", "/from/the/environment")]);
    run.assert_ok();
    assert_eq!(
        run.lines(),
        vec!["P=/var/lib/aoe/android-keyboard-worktrees/lithuanians"]
    );
}

#[test]
fn a_broken_aoe_is_a_warning_and_exit_zero() {
    let sandbox = Sandbox::new();
    let run = sandbox.run_with(&["hook"], &[("AOE_BIN", "/nonexistent/aoe")]);
    run.assert_ok();
    assert!(run.stderr.contains("falling back"), "{}", run.stderr);
    assert_eq!(run.lines(), vec![format!("AOE_LLM_TAGS={DEFAULT_VALUE}")]);
}

#[test]
fn a_failing_aoe_is_still_exit_zero() {
    let sandbox = Sandbox::new();
    let broken = sandbox.path("bin/broken-aoe");
    sandbox.write_executable(&broken, "#!/bin/sh\necho 'daemon down' >&2\nexit 1\n");
    let run = sandbox.run_with(&["hook"], &[("AOE_BIN", broken.to_str().unwrap())]);
    run.assert_ok();
    assert!(run.stderr.contains("daemon down"), "{}", run.stderr);
}

#[test]
fn malformed_settings_do_not_abort_the_session() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config("headers = [{ header = ", ""));
    let run = hook(&sandbox);
    run.assert_ok();
    assert!(run.stderr.contains("malformed TOML"), "{}", run.stderr);
    // The built-in default header still renders, so a broken file degrades instead of failing.
    assert_eq!(run.lines(), vec![format!("AOE_LLM_TAGS={DEFAULT_VALUE}")]);
}

#[test]
fn two_enabled_headers_are_two_lines() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config(common::TWO_HEADERS, ""));
    let run = hook(&sandbox);
    run.assert_ok();
    assert_eq!(
        run.lines(),
        vec![
            "AOE_LLM_TAGS=a".to_string(),
            "AOE_HDR_X_AOE_SESSION=cf11af87da7e4bf0".to_string(),
        ]
    );
}

#[test]
fn a_disabled_header_prints_nothing() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config(
        "headers = [{ header = \"X-Off\", template = \"x\", env_var = \"OFF\", enabled = false }]",
        "",
    ));
    let run = hook(&sandbox);
    run.assert_ok();
    assert_eq!(run.stdout, "");
}

#[test]
fn value_prints_the_bare_value() {
    let sandbox = Sandbox::new();
    let run = sandbox.run(&["value"]);
    run.assert_ok();
    assert_eq!(run.lines(), vec![DEFAULT_VALUE]);
}

#[test]
fn value_names_the_enabled_headers_when_asked_for_another() {
    let sandbox = Sandbox::new();
    let run = sandbox.run(&["value", "X-AoE-Session"]);
    run.assert_ok();
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains("X-AoE-Tags"), "{}", run.stderr);
}

#[test]
fn value_is_case_insensitive_about_the_name() {
    let sandbox = Sandbox::new();
    let run = sandbox.run(&["value", "x-aoe-tags"]);
    run.assert_ok();
    assert_eq!(run.lines(), vec![DEFAULT_VALUE]);
}

// --- setup ------------------------------------------------------------------

fn setup_lines(sandbox: &Sandbox) -> Vec<String> {
    let run = sandbox.run(&["setup", "--print"]);
    run.assert_ok();
    assert_eq!(run.stderr, "");
    run.lines().iter().map(|line| line.to_string()).collect()
}

#[test]
fn setup_prints_two_hook_lines_and_one_mapping_line() {
    let sandbox = Sandbox::new();
    let lines = setup_lines(&sandbox);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].starts_with("before_session = '"));
    assert!(lines[1].starts_with("before_start = '"));
    assert!(
        lines[2].contains(r#"headers["X-AoE-Tags"] = "$AOE_LLM_TAGS""#),
        "{lines:?}"
    );
}

#[test]
fn both_hook_keys_get_the_same_command() {
    let sandbox = Sandbox::new();
    let lines = setup_lines(&sandbox);
    let session = &lines[0]["before_session = ".len()..];
    let start = &lines[1]["before_start = ".len()..];
    assert_eq!(session, start);
    let aoe = sandbox.env.get("AOE_BIN").unwrap();
    assert!(session.contains(&format!("AOE_BIN=\"{aoe}\"")), "{session}");
    assert!(session.contains("/aoe-llm-tags"), "{session}");
}

#[test]
fn the_hook_line_is_valid_toml_and_its_argv_is_right() {
    let sandbox = Sandbox::new();
    let lines = setup_lines(&sandbox);
    let parsed: toml::Table = lines[0].parse().expect("valid TOML");
    assert_eq!(parsed.len(), 1);
    let command = parsed["before_session"].as_str().expect("a string");
    let argv = llm_tags::doctor::shell_words(command);
    assert!(argv[0].starts_with("AOE_BIN="), "{argv:?}");
    assert_eq!(
        argv[0].split_once('=').unwrap().1,
        sandbox.env.get("AOE_BIN").unwrap()
    );
    assert_eq!(Path::new(&argv[1]).file_name().unwrap(), "aoe-llm-tags");
    assert_eq!(argv[2], "hook");
    for (flag, variable) in [
        ("--session", "AOE_SESSION_ID"),
        ("--profile", "AOE_PROFILE"),
        ("--tool", "AOE_TOOL"),
        ("--project", "AOE_PROJECT_PATH"),
        ("--group", "AOE_GROUP_PATH"),
        ("--title", "AOE_SESSION_TITLE"),
    ] {
        assert!(argv.contains(&flag.to_string()), "{flag} missing");
        assert!(
            argv.contains(&format!("${variable}")),
            "${variable} missing"
        );
    }
}

#[test]
fn one_mapping_line_per_enabled_header() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config(common::TWO_HEADERS, ""));
    let lines = setup_lines(&sandbox);
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(lines[2].contains(r#""X-AoE-Tags""#) && lines[2].contains(r#""$AOE_LLM_TAGS""#));
    assert!(
        lines[3].contains(r#""X-AoE-Session""#) && lines[3].contains(r#""$AOE_HDR_X_AOE_SESSION""#)
    );
}

#[test]
fn a_disabled_header_is_not_mapped() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config(
        "headers = [{ header = \"X-Off\", template = \"x\", env_var = \"OFF\", enabled = false }]",
        "",
    ));
    assert_eq!(setup_lines(&sandbox).len(), 2);
}

#[test]
fn the_provider_id_comes_from_the_existing_config() {
    let sandbox = Sandbox::new();
    sandbox.pi_models(&[]);
    assert!(setup_lines(&sandbox)[2].contains("providers.litellm.headers"));
}

#[test]
fn a_placeholder_when_no_config_exists() {
    let sandbox = Sandbox::new();
    assert!(setup_lines(&sandbox)[2].contains("providers.<provider-id>.headers"));
}

#[test]
fn a_headers_only_provider_is_not_named() {
    let sandbox = Sandbox::new();
    sandbox.write_file(
        &sandbox.path("home/.pi/agent/models.json"),
        r#"{"providers": {"litellm": {"headers": {}, "models": []}}}"#,
    );
    // The launcher rebuilds a provider with no models, dropping headers with it.
    assert!(setup_lines(&sandbox)[2].contains("providers.<provider-id>.headers"));
}

#[test]
fn an_unknown_adapter_names_the_known_ones() {
    let sandbox = Sandbox::new();
    let run = sandbox.run(&["setup", "--print", "--adapter", "nope"]);
    run.assert_code(1);
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains("no adapter for tool"), "{}", run.stderr);
    assert!(run.stderr.contains("pi"), "{}", run.stderr);
}

#[test]
fn an_unresearched_adapter_is_refused() {
    let sandbox = Sandbox::new();
    let run = sandbox.run(&["setup", "--print", "--adapter", "gemini"]);
    run.assert_code(1);
    assert!(run.stderr.contains("needs_research"), "{}", run.stderr);
}

#[test]
fn a_missing_aoe_binary_fails_loudly() {
    let sandbox = Sandbox::new();
    let run = sandbox.run_with(
        &["setup", "--print"],
        &[("AOE_BIN", "/nonexistent/bin/aoe")],
    );
    run.assert_code(1);
    assert!(run.stderr.contains("AOE_BIN"), "{}", run.stderr);
}

#[test]
fn setup_without_print_is_a_usage_error() {
    let sandbox = Sandbox::new();
    let run = sandbox.run(&["setup"]);
    run.assert_code(2);
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains("--print"), "{}", run.stderr);
}

#[test]
fn setup_json_shape() {
    let sandbox = Sandbox::new();
    let run = sandbox.run(&["setup", "--print", "--json"]);
    run.assert_ok();
    let plan: serde_json::Value = serde_json::from_str(&run.stdout).expect("JSON");
    assert_eq!(plan["adapter"], "pi");
    assert_eq!(
        plan["hooks"]["before_session"],
        plan["hooks"]["before_start"]
    );
    assert_eq!(plan["mapping"][0]["header"], "X-AoE-Tags");
    assert_eq!(plan["mapping"][0]["env_var"], "AOE_LLM_TAGS");
    assert!(Path::new(plan["cli"].as_str().unwrap()).is_absolute());
    assert!(Path::new(plan["mapping"][0]["file"].as_str().unwrap()).is_absolute());
}

#[test]
fn a_different_session_tool_selects_a_different_adapter() {
    let sandbox = Sandbox::new();
    let codex = sandbox.fake_aoe(
        r#"{"id":"cf11af87da7e4bf0","title":"T","path":"/p","group":"","tool":"codex","profile":"main"}"#,
    );
    let run = sandbox.run_with(
        &["setup", "--print"],
        &[("AOE_BIN", codex.to_str().unwrap()), ("AOE_TOOL", "codex")],
    );
    run.assert_ok();
    assert!(run.stdout.contains(".codex/config.toml"), "{}", run.stdout);
    assert!(run.stdout.contains("env_http_headers"), "{}", run.stdout);
}

#[test]
fn the_plugin_id_never_appears_in_a_mapping_line() {
    // Contract: the mapping is vendor-neutral; only the header and env var are named.
    let sandbox = Sandbox::new();
    let lines = setup_lines(&sandbox);
    assert!(!lines[2].contains(PLUGIN_ID));
    assert!(!lines[2].contains("litellm.") || lines[2].contains("providers."));
}

#[test]
fn hook_cli_is_the_installed_binary() {
    // `cli_path` derives from the running executable when the tree layout is odd.
    let sandbox = Sandbox::new();
    let lines = setup_lines(&sandbox);
    let parsed: toml::Table = lines[0].parse().expect("valid TOML");
    let argv = llm_tags::doctor::shell_words(parsed["before_session"].as_str().unwrap());
    assert_eq!(Path::new(&argv[1]).file_name().unwrap(), "aoe-llm-tags");
    assert!(Path::new(&argv[1]).is_file(), "{argv:?}");
    let _ = (SESSION_JSON, CLI);
}

#[test]
fn static_tags_come_first_and_rules_add_to_them() {
    // The tag set is listed once; rules only extend it.
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config(
        "tags = [\"team:core\", \"env:prod\"]\nrules = [{ tag = \"feature:build\", match_kind = \"manual\" }]\n\n[[plugins.\"dev.oozut.llm-tags\".settings.targets]]\nname = \"X-AoE-Tags\"\ntemplate = \"{tags}\"\n",
        "",
    ));
    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    assert_eq!(
        run.lines(),
        vec!["AOE_HDR_X_AOE_TAGS=team:core,env:prod,feature:build"]
    );
}

#[test]
fn a_query_target_carries_the_tag_set_as_a_parameter() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config(
        "tags = [\"team:core\"]\n\n[[plugins.\"dev.oozut.llm-tags\".settings.targets]]\nkind = \"query\"\nname = \"tags\"\ntemplate = \"{tags}\"\nliteral = \"aoe,team:core\"\n",
        "",
    ));
    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    // A query target still mints one variable, under a query-specific prefix.
    assert_eq!(run.lines(), vec!["AOE_QRY_TAGS=team:core"]);
}

#[test]
fn a_query_target_without_a_literal_is_refused_by_apply() {
    // pi sends `baseUrl` verbatim, so an unexpanded `$VAR` there would be a lie.
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config(
        "[[plugins.\"dev.oozut.llm-tags\".settings.targets]]\nkind = \"query\"\nname = \"tags\"\ntemplate = \"{tags}\"\n",
        "",
    ));
    let run = sandbox.run(&["setup", "--adapter", "pi", "--apply", "--yes"]);
    run.assert_code(1);
    assert!(run.stderr.contains("refusing to write"), "{}", run.stderr);
    assert!(
        !sandbox.root.join("home/.pi/agent/models.json").exists(),
        "nothing was written"
    );
}

#[test]
fn the_m0_headers_key_is_still_read_as_header_targets() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config(
        "headers = [{ header = \"X-AoE-Tags\", template = \"a\", env_var = \"AOE_LLM_TAGS\" }]",
        "",
    ));
    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    assert_eq!(run.lines(), vec!["AOE_LLM_TAGS=a"]);
}

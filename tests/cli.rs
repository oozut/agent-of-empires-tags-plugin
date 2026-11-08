//! `hook`, `value` and `setup --print`, run as the user runs them: a real process,
//! a real `aoe` subprocess, stdout and stderr kept apart.

#![cfg(unix)]

mod common;

use std::path::Path;

use common::{settings_config, Run, Sandbox, CLI, PLUGIN_ID, SESSION_ID, SESSION_JSON};

fn hook(sandbox: &Sandbox) -> Run {
    sandbox.run(&["hook"])
}

#[test]
fn hook_prints_one_name_value_line() {
    let sandbox = Sandbox::new();
    let run = hook(&sandbox);
    run.assert_ok();
    // NAME is the environment variable: aoe parses hook stdout as KEY=VALUE.
    let default_value = sandbox.default_payload();
    assert_eq!(run.lines(), vec![format!("AOE_LLM_TAGS={default_value}")]);
    assert_eq!(run.stderr, "");
}

#[test]
fn a_sandboxed_launch_exports_the_variables_and_writes_no_copy() {
    // aoe runs `before_start` for a sandboxed launch, and its container cannot see
    // this plugin's app dir: pointing the harness at a copy there leaves it with no
    // config at all (`docs/harness-evidence/sandbox-pi.json`). So the sandbox
    // substrate exports the variables and writes nothing, and the harness reads
    // the mapping in its own config instead.
    let sandbox = Sandbox::new();
    sandbox.wire();

    let host = hook(&sandbox);
    host.assert_ok();
    assert!(
        host.lines()
            .iter()
            .any(|line| line.starts_with("PI_CODING_AGENT_DIR=")),
        "a host launch points the harness at its copy: {}",
        host.stdout
    );

    let run = sandbox.run(&["hook", "--substrate", "sandbox"]);
    run.assert_ok();
    assert_eq!(
        run.lines(),
        vec![format!("AOE_LLM_TAGS={}", sandbox.default_payload())],
        "{}",
        run.stdout
    );
    assert_eq!(run.stderr, "");
    let session_dir = sandbox.path(&format!(
        "home/.config/agent-of-empires/sessions/{SESSION_ID}"
    ));
    assert!(
        !session_dir.exists(),
        "a sandboxed launch wrote a copy no container can read: {}",
        session_dir.display()
    );
}

#[test]
fn a_sandboxed_launch_names_what_its_own_config_cannot_carry() {
    // vibe needs the literal in its file (`interpolates = []`), which only the
    // per-session copy can hold — so a sandboxed launch must say the tag is not
    // sent rather than closing the session silently.
    let mut sandbox = Sandbox::new();
    sandbox.session(SESSION_ID, "vibe");
    let run = sandbox.run(&["hook", "--substrate", "sandbox"]);
    run.assert_ok();
    assert!(
        run.stderr.contains("not sent for a sandboxed session"),
        "{}",
        run.stderr
    );
    assert_eq!(
        run.lines().len(),
        1,
        "the variables still travel: {}",
        run.stdout
    );
}

#[test]
fn a_sandboxed_launch_says_the_extension_does_not_reach_it() {
    // An approved extension on a session-scope row is files in this session's
    // copy, and a container reads none of them. The hook already says what the
    // row's own config cannot carry; an approval it cannot honour at all has to
    // be said too, or the user waits for a skill the sandbox never opens.
    let sandbox = Sandbox::new();
    sandbox.wire();

    let before = sandbox.run(&["hook", "--substrate", "sandbox"]);
    before.assert_ok();
    assert_eq!(
        before.stderr, "",
        "nothing was approved, so nothing is missing"
    );

    sandbox.write_config(&settings_config("harness_extension = [\"pi\"]\n", ""));
    common::approve_any(&sandbox, "pi");

    let run = sandbox.run(&["hook", "--substrate", "sandbox"]);
    run.assert_ok();
    assert!(
        run.stderr.contains("not installed for a sandboxed session"),
        "{}",
        run.stderr
    );
    assert_eq!(
        run.lines(),
        vec![format!("AOE_LLM_TAGS={}", sandbox.default_payload())],
        "the tag still travels through the launch environment: {}",
        run.stdout
    );
}

#[test]
fn a_launch_on_the_host_is_told_nothing_about_the_sandbox() {
    // The warning belongs to the substrate, not to the approval: a host launch
    // does install the copy, so saying it did not would be the lie in reverse.
    let sandbox = Sandbox::new();
    sandbox.wire();
    sandbox.write_config(&settings_config("harness_extension = [\"pi\"]\n", ""));
    common::approve_any(&sandbox, "pi");
    let run = hook(&sandbox);
    run.assert_ok();
    assert_eq!(run.stderr, "");
    assert!(
        run.lines()
            .iter()
            .any(|line| line.starts_with("AOE_LLM_TAGS_LIVE=")),
        "a host launch exports the live file: {}",
        run.stdout
    );
}

#[test]
fn a_sandboxed_launch_does_not_blame_itself_for_a_row_that_never_delivered() {
    // cline has no write path and expands nothing, so a host launch sends no tag
    // either. Warning that the sandbox "does not mount this session's copy" would
    // name a copy that never existed and hide the real reason.
    let mut sandbox = Sandbox::new();
    sandbox.session(SESSION_ID, "cline");
    let run = sandbox.run(&["hook", "--substrate", "sandbox"]);
    run.assert_ok();
    assert_eq!(run.stderr, "", "{}", run.stderr);
}

#[test]
fn an_unknown_substrate_is_a_usage_error() {
    // The value decides the delivery, so a typo must not fall back to the other one.
    let sandbox = Sandbox::new();
    let run = sandbox.run(&["hook", "--substrate", "sandbx"]);
    run.assert_code(2);
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains("host or sandbox"), "{}", run.stderr);
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
        vec![format!("P={}", sandbox.project().display())]
    );
}

#[test]
fn a_broken_aoe_is_a_warning_and_exit_zero() {
    let sandbox = Sandbox::new();
    let run = sandbox.run_with(&["hook"], &[("AOE_BIN", "/nonexistent/aoe")]);
    run.assert_ok();
    assert!(run.stderr.contains("falling back"), "{}", run.stderr);
    assert_eq!(
        run.lines(),
        vec![format!("AOE_LLM_TAGS={}", common::DEFAULT_PAYLOAD_NO_AOE)]
    );
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
    let default_value = sandbox.default_payload();
    assert_eq!(run.lines(), vec![format!("AOE_LLM_TAGS={default_value}")]);
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
    let default_value = sandbox.default_payload();
    assert_eq!(run.lines(), vec![default_value]);
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
    let default_value = sandbox.default_payload();
    assert_eq!(run.lines(), vec![default_value]);
}

// --- setup ------------------------------------------------------------------

fn setup_lines(sandbox: &Sandbox) -> Vec<String> {
    let run = sandbox.run(&["setup", "--print"]);
    run.assert_ok();
    assert_eq!(run.stderr, "");
    run.lines().iter().map(|line| line.to_string()).collect()
}

/// The value half of a printed mapping line, unescaped as the file needs it.
fn mapping_value(line: &str) -> String {
    let quoted = line.rsplit_once(" = ").expect("a value half").1;
    let escaped = quoted
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("a quoted value");
    serde_json::from_str::<String>(&format!("\"{escaped}\"")).expect("a JSON string")
}

#[test]
fn setup_prints_two_hook_lines_and_one_mapping_line() {
    let sandbox = Sandbox::new();
    let lines = setup_lines(&sandbox);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].starts_with("before_session = '"));
    assert!(lines[1].starts_with("before_start = '"));
    assert!(
        lines[2].contains(r#"headers["X-AoE-Tags"] = "!v=\"${AOE_LLM_TAGS-}\";"#),
        "{lines:?}"
    );
}

#[test]
fn each_hook_key_declares_the_substrate_it_serves() {
    // aoe runs the two keys for disjoint launches, so the line itself says where
    // the hook runs: `before_session` is a host launch (a session copy is
    // writable), `before_start` is a sandboxed one (it is not).
    let sandbox = Sandbox::new();
    let lines = setup_lines(&sandbox);
    let session = &lines[0]["before_session = ".len()..];
    let start = &lines[1]["before_start = ".len()..];
    assert!(session.contains("--substrate host"), "{session}");
    assert!(start.contains("--substrate sandbox"), "{start}");
    assert_eq!(
        session.replace("--substrate host", "--substrate sandbox"),
        start,
        "the two lines differ only in the substrate"
    );
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
    assert!(lines[2].contains(r#""X-AoE-Tags""#) && lines[2].contains(r"${AOE_LLM_TAGS-}"));
    assert!(
        lines[3].contains(r#""X-AoE-Session""#) && lines[3].contains(r"${AOE_HDR_X_AOE_SESSION-}")
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
    // A row nobody has researched is refused, not guessed at. Every shipped row
    // carries a real mechanism today, so the fixture is the table the CLI reads
    // from the plugin root.
    let sandbox = Sandbox::new();
    let dir = sandbox.path("plugin");
    sandbox.write_file(
        &dir.join("adapters.toml"),
        r#"[[adapter]]
agent = "unresearched"
tools = ["unresearched"]
label = "unresearched"
mechanism = "unknown"
config_format = "none"
status = "needs_research"
"#,
    );
    let run = sandbox.run_installed(&["setup", "--print", "--adapter", "unresearched"]);
    run.assert_code(1);
    assert!(run.stderr.contains("needs_research"), "{}", run.stderr);
    assert!(run.stderr.contains("unresearched"), "{}", run.stderr);
}

#[test]
fn a_writer_backed_row_points_at_the_session_copy_instead_of_a_line() {
    // vibe does not expand $NAME in its config, so the only place its value can
    // live is the copy the hook writes. That is not a failure to map: `setup`
    // says where the value comes from and prints nothing to paste.
    let mut sandbox = Sandbox::new();
    sandbox.session("cf11af87da7e4bf0", "vibe");
    sandbox.write_file(
        &sandbox.path("home/.vibe/config.toml"),
        "[[providers]]\nname = \"gw\"\napi_base = \"http://h/v1\"\n\n[[models]]\nname = \"m\"\nprovider = \"gw\"\n",
    );
    let run = sandbox.run(&["setup", "--print", "--adapter", "vibe"]);
    run.assert_ok();
    assert!(run.stderr.contains("per-session copy"), "{}", run.stderr);
    assert!(run.stderr.contains("overlay_dir"), "{}", run.stderr);
    assert!(!run.stdout.contains("extra_headers"), "{}", run.stdout);
}

#[test]
fn a_comma_splitting_harness_refuses_a_json_payload() {
    // gemini reads GEMINI_CLI_CUSTOM_HEADERS as a header list split on `,(name:)`,
    // so {"a":1,"b":2} is addressed as two headers and the request is dropped.
    // The plan is refused rather than written as a value gemini would mangle.
    let mut sandbox = Sandbox::new();
    sandbox.session("cf11af87da7e4bf0", "gemini");
    sandbox.write_config(&settings_config(
        "tags = [\"team:core\", \"feature:billing\"]\n[[plugins.\"dev.oozut.llm-tags\".settings.targets]]\nkind = \"header\"\nname = \"X-AoE-Tags\"\n",
        "",
    ));
    let run = sandbox.run(&["setup", "--adapter", "gemini", "--apply", "--yes"]);
    run.assert_code(1);
    assert!(run.stderr.contains("split on `,(name:)`"), "{}", run.stderr);
    assert!(run.stderr.contains("tags_b64"), "{}", run.stderr);

    // The escape hatch is accepted: the same payload, spelled without a comma.
    sandbox.write_config(&settings_config(
        "tags = [\"team:core\", \"feature:billing\"]\n[[plugins.\"dev.oozut.llm-tags\".settings.targets]]\nkind = \"header\"\nname = \"X-AoE-Tags\"\ntemplate = \"{tags_b64}\"\n",
        "",
    ));
    let run = sandbox.run(&["setup", "--adapter", "gemini", "--apply", "--yes"]);
    run.assert_ok();
    // The launch carries the same payload as one un-splittable token.
    let hook = sandbox.run(&["hook"]);
    hook.assert_ok();
    let line = hook
        .lines()
        .into_iter()
        .find(|line| line.starts_with("GEMINI_CLI_CUSTOM_HEADERS="))
        .expect("gemini's own variable is exported");
    let value = line.split_once(": ").expect("header: value").1;
    assert!(!value.contains(','), "{value}");
    assert!(!value.contains(':'), "{value}");
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
fn applying_in_a_sandbox_writes_no_config() {
    // The container cannot read the copy, so `--apply` on that substrate must say
    // so and leave the user's file alone rather than write one nothing opens.
    let sandbox = Sandbox::new();
    let path = sandbox.pi_models(&[("X-AoE-Tags", "$AOE_LLM_TAGS")]);
    let before = std::fs::read(&path).expect("models.json");
    let run = sandbox.run(&["setup", "--apply", "--yes", "--substrate", "sandbox"]);
    run.assert_ok();
    assert!(
        run.stdout.contains("writes no session config"),
        "{}",
        run.stdout
    );
    assert_eq!(std::fs::read(&path).expect("models.json"), before);
}

#[test]
fn setup_json_shape() {
    let sandbox = Sandbox::new();
    let run = sandbox.run(&["setup", "--print", "--json"]);
    run.assert_ok();
    let plan: serde_json::Value = serde_json::from_str(&run.stdout).expect("JSON");
    assert_eq!(plan["adapter"], "pi");
    // One key per substrate: the same command, declaring which launch it is for.
    let session = plan["hooks"]["before_session"].as_str().expect("a string");
    let start = plan["hooks"]["before_start"].as_str().expect("a string");
    assert!(session.contains("--substrate host"), "{session}");
    assert!(start.contains("--substrate sandbox"), "{start}");
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
fn a_stale_hook_line_is_inert_once_the_payload_is_gone() {
    // `aoe plugin uninstall` removes the plugin tree but cannot remove the
    // `[host_hooks]` lines a user pasted, and aoe aborts session creation when a
    // hook exits non-zero. The line therefore has to be a no-op — not the 127 the
    // shell returns for a missing binary — after the plugin is gone.
    let sandbox = Sandbox::new();
    let command = sandbox.hook_command();
    let cli = sandbox.path("plugin/bin/aoe-llm-tags");
    assert!(cli.is_file());
    std::fs::remove_file(&cli).expect("uninstall the CLI");

    let out = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(&command)
        .env_clear()
        .envs(&sandbox.env)
        .current_dir(&sandbox.root)
        .output()
        .expect("run the installed hook line");
    assert!(
        out.status.success(),
        "stale hook line exited {:?}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "stale hook line printed {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn the_pi_mapping_still_resolves_once_the_payload_is_gone() {
    // pi fails the whole request when a provider header names a variable it
    // cannot resolve, so a bare `$AOE_LLM_TAGS` mapping left behind by
    // `aoe plugin uninstall` breaks every launch that has no hook behind it. The
    // shipped value is a command instead: the hook's value while the plugin is
    // installed, the empty payload once it is not — so the standard uninstall is
    // enough, with no hand-edit of a file this plugin does not own.
    let sandbox = Sandbox::new();
    let lines = setup_lines(&sandbox);
    let value = mapping_value(&lines[2]);
    assert!(value.starts_with('!'), "not a command: {value}");
    let command = value.trim_start_matches('!');

    let run = |minted: Option<&str>| {
        let mut sh = std::process::Command::new("/bin/sh");
        sh.arg("-c").arg(command).env_clear().envs(&sandbox.env);
        match minted {
            Some(value) => sh.env("AOE_LLM_TAGS", value),
            None => sh.env_remove("AOE_LLM_TAGS"),
        };
        sh.output().expect("run the installed mapping value")
    };

    let gone = run(None);
    assert!(gone.status.success(), "{:?}", gone.status);
    assert_eq!(
        String::from_utf8_lossy(&gone.stdout).trim(),
        "{}",
        "a leftover mapping has to carry a value pi can resolve"
    );

    // A target whose template renders nothing exports an empty variable, which a
    // command value would otherwise read as "no output" — pi fails that too.
    let empty = run(Some(""));
    assert_eq!(String::from_utf8_lossy(&empty.stdout).trim(), "{}");

    let minted = run(Some(r#"{"session":"S-cf11","team":"core"}"#));
    assert_eq!(
        String::from_utf8_lossy(&minted.stdout).trim(),
        r#"{"session":"S-cf11","team":"core"}"#
    );
}

#[test]
fn undo_all_removes_the_overlays_and_names_what_is_left() {
    // `--all` is the first step of an uninstall: the copies this plugin built go
    // away, and the two files it never edits are named so they really get cleaned.
    let sandbox = Sandbox::new();
    sandbox.pi_models(&[("X-AoE-Tags", "$AOE_LLM_TAGS")]);
    let command = sandbox.hook_command();
    sandbox.write_config(&settings_config(
        "",
        &format!("before_session = ['{command}']\nbefore_start = ['{command}']\n"),
    ));
    let overlay = sandbox.path("config/agent-of-empires/sessions/cf11af87da7e4bf0/pi");
    std::fs::create_dir_all(&overlay).expect("overlay dir");
    std::fs::write(overlay.join("models.json"), "{}").expect("overlay file");

    let run = sandbox.run(&["setup", "--undo", "--all"]);
    run.assert_ok();
    assert!(!overlay.exists(), "the overlay survived --undo --all");
    run.assert_contains("removed 1 session config(s)");
    run.assert_contains("[host_hooks] before_session, before_start");
    run.assert_contains("$AOE_*");
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
fn a_query_target_is_refused_for_a_harness_without_one() {
    // pi appends the endpoint path to `baseUrl`, so `?tags=x` would land inside
    // the path. The plan is refused rather than written.
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config(
        "[[plugins.\"dev.oozut.llm-tags\".settings.targets]]\nkind = \"query\"\nname = \"tags\"\ntemplate = \"{tags}\"\nliteral = \"aoe\"\n",
        "",
    ));
    let run = sandbox.run(&["setup", "--adapter", "pi", "--apply", "--yes"]);
    run.assert_code(1);
    assert!(
        run.stderr.contains("cannot deliver a query value"),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains("refusing to write"), "{}", run.stderr);
    assert!(
        !sandbox.root.join("home/.pi/agent/models.json").exists(),
        "nothing was written"
    );
}

#[test]
fn a_body_target_goes_into_each_model_sampling_params() {
    // pi reads samplingParams per model; a provider-level one is dropped.
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config(
        "[[plugins.\"dev.oozut.llm-tags\".settings.targets]]\nkind = \"body\"\nname = \"metadata\"\ntemplate = \"{tags}\"\nliteral = \"aoe,team:core\"\n",
        "",
    ));
    sandbox.write_pi_models(
        r#"{"providers":{"litellm":{"baseUrl":"http://h/v1","models":[{"id":"m1"},{"id":"m2"}]}}}"#,
    );
    let run = sandbox.run(&["setup", "--adapter", "pi", "--apply", "--yes"]);
    run.assert_ok();
    // The session's own copy carries the value…
    let generated = run
        .lines()
        .first()
        .and_then(|line| line.strip_prefix("wrote "))
        .map(Path::new)
        .expect("apply names the file it wrote");
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(generated).expect("generated config"))
            .expect("valid JSON");
    for model in written["providers"]["litellm"]["models"]
        .as_array()
        .unwrap()
    {
        assert_eq!(model["samplingParams"]["metadata"], "aoe,team:core");
    }
    assert!(written["providers"]["litellm"]
        .get("samplingParams")
        .is_none());
    // …and the user's config is untouched.
    assert!(sandbox.read_pi_models()["providers"]["litellm"]
        .get("headers")
        .is_none());
    assert!(
        sandbox.read_pi_models()["providers"]["litellm"]["models"][0]
            .get("samplingParams")
            .is_none()
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

#[test]
fn a_tags_file_in_the_project_adds_every_pair() {
    // A repo declares its own tags in one JSON object; every top-level key/value
    // pair becomes a tag with its JSON type intact, and the repo's value wins over
    // a settings tag with the same key. The file stays inside the project.
    let sandbox = Sandbox::new();
    let repo = sandbox.path("repo");
    sandbox.write_file(
        &repo.join("aoe-tags.json"),
        r#"{"team":"core","cost":1.5,"ported":true,"matrix":[1,2]}"#,
    );
    sandbox.write_config(&settings_config(
        "tags = [\"team:global\", \"feature:billing\"]\ntags_file = \"aoe-tags.json\"",
        "",
    ));
    let run = sandbox.run_with(
        &["hook"],
        &[
            ("AOE_BIN", "/nonexistent/aoe"),
            ("AOE_PROJECT_PATH", repo.to_str().unwrap()),
        ],
    );
    run.assert_ok();
    assert_eq!(run.lines().len(), 1, "{}", run.stderr);
    let payload: serde_json::Value =
        serde_json::from_str(run.lines()[0].split_once('=').unwrap().1).unwrap();
    assert_eq!(payload["team"], "core");
    assert_eq!(payload["cost"], 1.5);
    assert_eq!(payload["ported"], true);
    assert_eq!(payload["matrix"], serde_json::json!([1, 2]));
    assert_eq!(payload["feature"], "billing");
}

#[test]
fn a_missing_tags_file_is_a_warning_and_the_session_still_starts() {
    let sandbox = Sandbox::new();
    let repo = sandbox.path("repo");
    sandbox.write_config(&settings_config("tags_file = \"nope.json\"", ""));
    let run = sandbox.run_with(
        &["hook"],
        &[
            ("AOE_BIN", "/nonexistent/aoe"),
            ("AOE_PROJECT_PATH", repo.to_str().unwrap()),
        ],
    );
    run.assert_ok();
    assert!(run.stderr.contains("does not exist"), "{}", run.stderr);
    assert_eq!(run.lines().len(), 1);
}

#[test]
fn a_feature_tag_is_a_key_value_entry_at_every_level() {
    // `feature_tag` is one tag, not a second taxonomy: the global config, a
    // profile override and a project form all hold the same `key:value` grammar,
    // and all of them land as an ordinary payload field.
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config("feature_tag = 'billing'", ""));
    let run = hook(&sandbox);
    run.assert_ok();
    assert!(
        run.lines()[0].contains(r#""feature":"billing""#),
        "{}",
        run.stdout
    );

    // A profile override replaces it, and may use the standard spelling.
    sandbox.write_profile_config(
        "main",
        &format!("[plugins.\"{PLUGIN_ID}\".settings]\nfeature_tag = \"feature:checkout\"\n"),
    );
    let run = hook(&sandbox);
    run.assert_ok();
    assert!(
        run.lines()[0].contains(r#""feature":"checkout""#),
        "{}",
        run.stdout
    );

    // The same pair written with its key and its value apart is the same entry.
    sandbox.write_profile_config(
        "main",
        &format!(
            "[plugins.\"{PLUGIN_ID}\".settings]\nfeature_tag = {{ key = \"feature\", value = \"reconcile\" }}\n"
        ),
    );
    let run = hook(&sandbox);
    run.assert_ok();
    assert!(
        run.lines()[0].contains(r#""feature":"reconcile""#),
        "{}",
        run.stdout
    );

    // A pair under another key is that field, not a second `feature`.
    sandbox.write_profile_config(
        "main",
        &format!("[plugins.\"{PLUGIN_ID}\".settings]\nfeature_tag = \"epic:checkout\"\n"),
    );
    let run = hook(&sandbox);
    run.assert_ok();
    let payload = &run.lines()[0];
    assert!(payload.contains(r#""epic":"checkout""#), "{payload}");
    assert!(!payload.contains(r#""feature":"#), "{payload}");
}

#[test]
fn without_a_home_a_command_refuses_instead_of_writing_beside_the_checkout() {
    let sandbox = Sandbox::new();
    // Somewhere a checked-out repository could edit, and where a relative
    // `.config/agent-of-empires` would land if the plugin guessed.
    let cwd = sandbox.path("checkout");
    std::fs::create_dir_all(&cwd).expect("cwd");

    let run = sandbox.run_within(
        &cwd,
        &["tag", "--set", "team:core"],
        &[("HOME", ""), ("XDG_CONFIG_HOME", "")],
    );
    run.assert_code(1);
    assert!(
        run.stderr.contains("XDG_CONFIG_HOME"),
        "the message says what is missing: {}",
        run.stderr
    );
    assert!(
        std::fs::read_dir(&cwd).expect("cwd").next().is_none(),
        "the command wrote into the directory it was started in"
    );
}

#[test]
fn a_launch_without_a_home_still_sends_its_header() {
    let sandbox = Sandbox::new();
    // No app dir means no session edits to read, which is a warning and not a
    // failed launch: the hook's job is to export a value either way.
    let run = sandbox.run_with(&["hook"], &[("HOME", ""), ("XDG_CONFIG_HOME", "")]);
    run.assert_ok();
    assert_eq!(run.lines().len(), 1, "{}", run.stdout);
    assert!(
        run.lines()[0].starts_with("AOE_LLM_TAGS="),
        "{}",
        run.stdout
    );
    assert!(
        run.stderr.contains("no session edits are read"),
        "the warning says the edits were skipped: {}",
        run.stderr
    );
}

#[test]
fn activity_sets_the_reserved_tag_and_the_next_hook_carries_it() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config(
        "tags = [\"team:core\"]\n\n[[plugins.\"dev.oozut.llm-tags\".settings.targets]]\nname = \"X-AoE-Tags\"\ntemplate = \"{tags}\"\n",
        "",
    ));
    let run = sandbox.run(&["activity", "test"]);
    run.assert_ok();
    assert!(
        run.stdout.contains("activity test (testing)"),
        "{}",
        run.stdout
    );
    // The value a surface renders is the label; the tag is what travels.
    let launched = hook(&sandbox);
    launched.assert_ok();
    // The template renders the tag set, so the pair appears as a token and the
    // session's other tags are still there.
    assert!(
        launched.lines()[0].contains("activity:test"),
        "{}",
        launched.stdout
    );
    assert!(
        launched.lines()[0].contains("team:core"),
        "the other tags survive: {}",
        launched.stdout
    );

    let show = sandbox.run(&["activity", "--json"]);
    show.assert_ok();
    let parsed: serde_json::Value = serde_json::from_str(&show.stdout).expect("json");
    assert_eq!(parsed["activity"], serde_json::json!("test"));
    assert_eq!(parsed["icon"], serde_json::json!("flask-conical"));
    assert_eq!(parsed["tone"], serde_json::json!("warn"));
    assert!(
        parsed["vocabulary"].as_array().unwrap().len() == 9,
        "the vocabulary is listed: {parsed}"
    );
}

/// The settings rows one family, as the settings UI writes them. The array header
/// is absolute, so it names the plugin settings table again.
const WORK_FAMILY: &str = r#"activity_family = "work"

[[plugins."dev.oozut.llm-tags".settings.activity_families]]
family = "work"
value = "write"
label = "writing"
icon = "pen-line"
tone = "info"
when = "when starting to implement the feature, move the activity to writing"

[[plugins."dev.oozut.llm-tags".settings.activity_families]]
family = "work"
value = "measure"
label = "measuring"
icon = "ruler"
"#;

#[test]
fn a_family_from_the_settings_replaces_the_builtin_vocabulary() {
    // The nine words are a default, not the vocabulary: a user defines their own
    // and names it as the family a session starts in. Nothing else changes — the
    // value is still one reserved tag.
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config(WORK_FAMILY, ""));

    let show = sandbox.run(&["activity", "--show"]);
    show.assert_ok();
    assert!(show.stdout.contains("family work"), "{}", show.stdout);
    assert!(show.stdout.contains("write"), "{}", show.stdout);
    assert!(
        !show.stdout.contains("explore"),
        "the built-in words are not this family's: {}",
        show.stdout
    );

    sandbox.run(&["activity", "writing"]).assert_ok();
    let json = sandbox.run(&["activity", "--json"]);
    json.assert_ok();
    let parsed: serde_json::Value = serde_json::from_str(&json.stdout).expect("json");
    assert_eq!(parsed["activity"], serde_json::json!("write"));
    assert_eq!(parsed["label"], serde_json::json!("writing"));
    assert_eq!(parsed["family"], serde_json::json!("work"));
    assert_eq!(parsed["source"], serde_json::json!("settings"));
    // `when` is the part an agent reads, so it travels with the value.
    assert_eq!(
        parsed["vocabulary"][0]["when"],
        serde_json::json!("when starting to implement the feature, move the activity to writing")
    );
    assert_eq!(parsed["vocabulary"].as_array().unwrap().len(), 2);

    // A word the new family does not define is refused, and the refusal names
    // that family rather than the built-in one.
    let refused = sandbox.run(&["activity", "test"]);
    refused.assert_code(2);
    assert!(refused.stderr.contains("work family"), "{}", refused.stderr);
    assert!(refused.stderr.contains("write"), "{}", refused.stderr);
    assert!(!refused.stderr.contains("explore"), "{}", refused.stderr);
}

#[test]
fn the_repository_file_wins_and_replaces_a_same_named_family_whole() {
    // The settings define `work` with two rows; the repository defines `work`
    // with one. The repository's definition replaces the family, it does not
    // extend it: `measure` is gone, so a reader never has to reason about a
    // half-and-half family.
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config(WORK_FAMILY, ""));
    sandbox.write_file(
        &sandbox.project().join(".aoe/activity.toml"),
        r#"default = "work"

[families.work.activities.code]
label = "coding"
icon = "code"
tone = "neutral"
when = "when starting to implement the feature"
"#,
    );

    let show = sandbox.run(&["activity", "--json"]);
    show.assert_ok();
    let parsed: serde_json::Value = serde_json::from_str(&show.stdout).expect("json");
    assert_eq!(parsed["family"], serde_json::json!("work"));
    assert!(
        parsed["source"].as_str().unwrap().starts_with("repo "),
        "the choice came from the repository: {parsed}"
    );
    let words: Vec<&str> = parsed["vocabulary"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["key"].as_str())
        .collect();
    assert_eq!(words, vec!["code"], "{parsed}");

    // `default` in the file outranks `activity_family`; the words it kept are
    // the repository's, and the ones it dropped are gone.
    sandbox.run(&["activity", "code"]).assert_ok();
    sandbox.run(&["activity", "write"]).assert_code(2);
}

#[test]
fn a_running_session_keeps_the_family_it_launched_with() {
    // The whole point of pinning: editing a family must never relabel a session
    // that is already running. The launch writes the family it resolved, and
    // every mid-session read uses that file.
    let sandbox = Sandbox::new();
    let file = sandbox.project().join(".aoe/activity.toml");
    sandbox.write_file(
        &file,
        r#"default = "work"

[families.work.activities.write]
label = "writing"
when = "when implementing"
"#,
    );
    let launched = hook(&sandbox);
    launched.assert_ok();
    let pin = sandbox.path(&format!(
        "config/agent-of-empires/sessions/{SESSION_ID}/activity.toml"
    ));
    assert!(pin.exists(), "the launch pins the family: {pin:?}");
    let pinned = std::fs::read_to_string(&pin).expect("pin");
    assert!(pinned.contains("writing"), "{pinned}");

    // The repository renames its activity afterwards; the running session does
    // not care.
    sandbox.write_file(
        &file,
        r#"default = "work"

[families.work.activities.code]
label = "coding"
"#,
    );
    let show = sandbox.run(&["activity", "--json"]);
    show.assert_ok();
    let parsed: serde_json::Value = serde_json::from_str(&show.stdout).expect("json");
    assert_eq!(parsed["family"], serde_json::json!("work"));
    assert_eq!(
        parsed["vocabulary"][0]["key"],
        serde_json::json!("write"),
        "the pinned family, not the edited one: {parsed}"
    );
    sandbox.run(&["activity", "write"]).assert_ok();
    sandbox.run(&["activity", "code"]).assert_code(2);
}

#[test]
fn a_project_registry_entry_chooses_the_family_a_launch_resolves() {
    // `activity_family` and `activity_families` are per_project, and a launch has
    // no host RPC: the hook reads the registry file the host writes. This is that
    // file, with the project's own settings and a different family from the
    // global one.
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config(
        "activity_family = \"global\"\n\n[[plugins.\"dev.oozut.llm-tags\".settings.activity_families]]\nfamily = \"global\"\nvalue = \"triage\"\n",
        "",
    ));
    let registry = sandbox.path("config/agent-of-empires/projects.json");
    std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
    std::fs::write(
        &registry,
        serde_json::json!([{
            "name": "project",
            "path": sandbox.project().display().to_string(),
            "overrides": {
                "plugin_settings": {
                    PLUGIN_ID: {
                        "activity_family": "project",
                        "activity_families": [{
                            "family": "project",
                            "value": "ship",
                            "label": "shipping",
                        }],
                    }
                }
            }
        }])
        .to_string(),
    )
    .unwrap();

    let show = sandbox.run(&["activity", "--json"]);
    show.assert_ok();
    let parsed: serde_json::Value = serde_json::from_str(&show.stdout).expect("json");
    assert_eq!(parsed["family"], serde_json::json!("project"), "{parsed}");
    assert_eq!(parsed["vocabulary"][0]["key"], serde_json::json!("ship"));

    // The launch resolves the same way, so the session that starts here carries
    // the project's vocabulary.
    let launched = hook(&sandbox);
    launched.assert_ok();
    let pin = sandbox.path(&format!(
        "config/agent-of-empires/sessions/{SESSION_ID}/activity.toml"
    ));
    let pinned = std::fs::read_to_string(&pin).expect("pin");
    assert!(pinned.contains("ship"), "{pinned}");
    assert!(!pinned.contains("triage"), "{pinned}");
}

#[test]
fn a_malformed_family_file_is_a_warning_and_the_session_still_starts() {
    let sandbox = Sandbox::new();
    sandbox.write_file(
        &sandbox.project().join(".aoe/activity.toml"),
        "not = = toml",
    );
    let run = hook(&sandbox);
    run.assert_ok();
    assert!(run.stderr.contains("is not valid TOML"), "{}", run.stderr);
    assert_eq!(
        run.lines().len(),
        1,
        "the tag still travels: {}",
        run.stdout
    );
    // The built-in family is what a broken file leaves behind.
    let show = sandbox.run(&["activity", "--json"]);
    show.assert_ok();
    let parsed: serde_json::Value = serde_json::from_str(&show.stdout).expect("json");
    assert_eq!(parsed["family"], serde_json::json!("default"));
}

#[test]
fn a_missing_family_file_is_silent() {
    // Unlike `tags_file`, the activity file is optional in every repository: a
    // project that declares no families is not a problem to report at every
    // launch.
    let sandbox = Sandbox::new();
    let run = hook(&sandbox);
    run.assert_ok();
    assert_eq!(run.stderr, "");
}

#[test]
fn an_unknown_activity_is_a_usage_error_that_names_the_vocabulary() {
    let sandbox = Sandbox::new();
    // A label is accepted (`testing`), so the typo has to be a real one.
    let run = sandbox.run(&["activity", "testng"]);
    run.assert_code(2);
    assert!(run.stdout.is_empty(), "{}", run.stdout);
    assert!(
        run.stderr.contains("not an activity") && !run.stderr.contains("flask-conical"),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains("explore"), "{}", run.stderr);
    // Nothing was written: a typo must not become a new activity.
    let hook = hook(&sandbox);
    hook.assert_ok();
    assert!(!hook.lines()[0].contains("activity"), "{}", hook.stdout);
}

#[test]
fn activity_is_cleared_and_a_second_verb_overwrites_rather_than_stacks() {
    let sandbox = Sandbox::new();
    sandbox.run(&["activity", "code"]).assert_ok();
    sandbox.run(&["activity", "review"]).assert_ok();
    let before = hook(&sandbox);
    before.assert_ok();
    assert!(
        before.lines()[0].contains(r#""activity":"review""#),
        "{}",
        before.stdout
    );
    assert!(
        !before.lines()[0].contains(r#""activity":"code""#),
        "one key, one value: {}",
        before.stdout
    );

    sandbox.run(&["activity", "--clear"]).assert_ok();
    let after = hook(&sandbox);
    after.assert_ok();
    assert!(!after.lines()[0].contains("activity"), "{}", after.stdout);
}

#[test]
fn activity_reaches_the_next_request_only_when_a_live_file_exists() {
    // No extension approval: the launch did not set up per-request delivery, so
    // the verb must not claim the next request carries it.
    let sandbox = Sandbox::new();
    let run = sandbox.run(&["activity", "plan"]);
    run.assert_ok();
    assert!(run.stdout.contains("next launch"), "{}", run.stdout);

    // With a live file, the same edit refreshes it instead of waiting.
    let live = sandbox.path(&format!(
        "config/agent-of-empires/sessions/{SESSION_ID}/live.json"
    ));
    std::fs::create_dir_all(live.parent().unwrap()).expect("session dir");
    std::fs::write(&live, r#"{"enabled":false,"targets":[]}"#).expect("live file");
    let again = sandbox.run(&["activity", "test"]);
    again.assert_ok();
    assert!(again.stdout.contains("next request"), "{}", again.stdout);
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&live).expect("live")).expect("json");
    assert!(
        body["targets"]
            .as_array()
            .expect("targets")
            .iter()
            .any(|target| target["value"]
                .as_str()
                .is_some_and(|value| value.contains(r#""activity":"test""#))),
        "{body}"
    );
}

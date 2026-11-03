//! Editing one session's tags — from `tag`, and from the JSON the worker pushes.
//!
//! The invariant under test is the plugin's oldest one: a per-session edit is a
//! write to that session's own file, never to the global settings, so two
//! sessions can disagree without a rule or a config edit between them.

#![cfg(unix)]

mod common;

use common::{settings_config, Sandbox};

fn overrides_path(sandbox: &Sandbox, session: &str) -> std::path::PathBuf {
    sandbox
        .path("config/agent-of-empires")
        .join("sessions")
        .join(session)
        .join("overrides.toml")
}

#[test]
fn a_tag_set_for_one_session_reaches_that_sessions_hook_and_no_other() {
    let sandbox = Sandbox::new();
    let before = std::fs::read_to_string(sandbox.config_path("")).expect("config");

    sandbox
        .run(&[
            "tag",
            "--session",
            "cf11af87da7e4bf0",
            "--set",
            "team:platform",
        ])
        .assert_ok();

    // The global settings file is byte-for-byte what it was.
    let after = std::fs::read_to_string(sandbox.config_path("")).expect("config");
    assert_eq!(before, after, "the global config must not be rewritten");

    let hook = sandbox.run(&["hook"]);
    hook.assert_ok();
    hook.assert_contains(r#""team":"platform""#);

    // A different session, same config: nothing of the edit leaks across.
    let mut other = Sandbox::new();
    other.session("0123456789abcdef", "pi-proxy");
    let hook = other.run(&["hook"]);
    hook.assert_ok();
    assert!(
        !hook.stdout.contains("platform"),
        "a second session inherited the first one's tag:\n{}",
        hook.stdout
    );
}

#[test]
fn a_set_tag_replaces_the_settings_entry_and_an_unset_one_hides_it() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config(r#"tags = ["team:core", "cost:1"]"#, ""));

    sandbox
        .run(&["tag", "--set", "team:platform", "--unset", "cost"])
        .assert_ok();
    let hook = sandbox.run(&["hook"]);
    hook.assert_ok();
    hook.assert_contains(r#""team":"platform""#);
    assert!(!hook.stdout.contains(r#""cost""#), "{}", hook.stdout);
    assert!(
        !hook.stdout.contains(r#""team":"core""#),
        "both values were sent: {}",
        hook.stdout
    );

    // The settings still carry what the session hid: dropping is per session.
    let settings = std::fs::read_to_string(sandbox.config_path("")).expect("config");
    assert!(settings.contains("team:core"), "{settings}");
    assert!(settings.contains("cost:1"), "{settings}");
}

#[test]
fn the_token_override_stands_in_for_the_payload() {
    let sandbox = Sandbox::new();
    sandbox
        .run(&[
            "tag",
            "--session",
            "cf11af87da7e4bf0",
            "--token",
            r#"{"hand":true}"#,
        ])
        .assert_ok();
    let hook = sandbox.run(&["hook"]);
    hook.assert_ok();
    hook.assert_contains(r#"AOE_LLM_TAGS={"hand":true}"#);

    sandbox
        .run(&["tag", "--session", "cf11af87da7e4bf0", "--clear-token"])
        .assert_ok();
    let hook = sandbox.run(&["hook"]);
    hook.assert_ok();
    assert!(
        hook.stdout.contains(r#""session_id""#),
        "clearing the token did not go back to the payload:\n{}",
        hook.stdout
    );
    assert!(!overrides_path(&sandbox, "cf11af87da7e4bf0").exists());
}

#[test]
fn a_target_with_a_template_keeps_it_and_ignores_the_token() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&common::settings_config(common::TWO_HEADERS, ""));
    sandbox.run(&["tag", "--token", "literal"]).assert_ok();
    let hook = sandbox.run(&["hook"]);
    hook.assert_ok();
    // Both rows name their own field through `template`, so a literal token —
    // which stands in for the *rendered payload* — has nothing to replace. A
    // target that says what it wants is not overridden by a session-wide value.
    hook.assert_contains("AOE_LLM_TAGS=a");
    hook.assert_contains("AOE_HDR_X_AOE_SESSION=cf11af87da7e4bf0");
    assert!(!hook.stdout.contains("literal"), "{}", hook.stdout);
}

#[test]
fn the_edit_reports_what_the_session_would_send_now() {
    let sandbox = Sandbox::new();
    let run = sandbox.run(&["tag", "--set", "epic:=42", "--json"]);
    run.assert_ok();
    let value: serde_json::Value = serde_json::from_str(run.stdout.trim()).expect("json");
    assert_eq!(value["session"], "cf11af87da7e4bf0");
    assert_eq!(value["tags"], serde_json::json!(["epic:=42"]));
    assert!(
        value["path"]
            .as_str()
            .expect("path")
            .ends_with("sessions/cf11af87da7e4bf0/overrides.toml"),
        "{value}"
    );
    let sent = value["targets"][0]["value"].as_str().expect("value");
    let payload: serde_json::Value = serde_json::from_str(sent).expect("payload");
    assert_eq!(payload["epic"], 42);
}

#[test]
fn an_edit_without_a_session_is_a_usage_error_rather_than_a_guess() {
    let mut sandbox = Sandbox::new();
    sandbox.env.remove("AOE_SESSION_ID");
    // The fake `aoe` still answers `session show`; a host that does not is the
    // case this guards, so point AOE_BIN at a script that fails.
    let broken = sandbox.path("bin/aoe-broken");
    sandbox.write_executable(&broken, "#!/bin/sh\nexit 1\n");
    sandbox
        .env
        .insert("AOE_BIN".to_string(), broken.display().to_string());
    let run = sandbox.run(&["tag", "--set", "team:core"]);
    run.assert_code(2);
    run.assert_contains("needs a session");
}

#[test]
fn editing_a_session_that_is_gone_is_fine_and_pruning_takes_the_edits_with_it() {
    let sandbox = Sandbox::new();
    sandbox.run(&["tag", "--set", "team:core"]).assert_ok();
    assert!(overrides_path(&sandbox, "cf11af87da7e4bf0").exists());
    // `overlay::prune` is what the worker runs on every host event; a session
    // the host no longer lists takes its overrides with it, so an edit cannot
    // outlive the session it belongs to.
    let app_dir = sandbox.path("config/agent-of-empires");
    let removed = llm_tags::overlay::prune(&app_dir, &[]);
    assert_eq!(
        removed, 0,
        "an empty live list is not evidence of staleness"
    );
    let removed = llm_tags::overlay::prune(&app_dir, &["0123456789abcdef".to_string()]);
    assert_eq!(removed, 1);
    assert!(
        !overrides_path(&sandbox, "cf11af87da7e4bf0").exists(),
        "the override outlived its session"
    );
}

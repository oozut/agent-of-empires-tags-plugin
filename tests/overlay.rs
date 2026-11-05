//! Per-session config: the property that matters is that a session's tags live
//! in a file that session owns, and that the user's own config is never touched.
//!
//! These run the real CLI as the hook does, then read the artifacts back from
//! disk — no harness installed, so the suite stays portable.

#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};

use common::{settings_config, Sandbox};

/// The `NAME=value` line a hook printed for `name`, if any.
fn emitted<'a>(lines: &'a [&'a str], name: &str) -> Option<&'a str> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
}

fn tags(headers: &str) -> String {
    settings_config(headers, "")
}

#[test]
fn the_hook_builds_a_config_this_session_owns_and_leaves_the_users_alone() {
    let sandbox = Sandbox::new();
    sandbox.write_pi_models(
        r#"{"providers":{"litellm":{"baseUrl":"http://h/v1","models":[{"id":"m1"}]}}}"#,
    );
    sandbox.write_file(&sandbox.path("home/.pi/agent/auth.json"), r#"{"k":"v"}"#);
    let before = std::fs::read_to_string(sandbox.pi_models_path()).unwrap();

    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    let dir = PathBuf::from(emitted(&run.lines(), "PI_CODING_AGENT_DIR").expect("override"));

    // The session dir holds our file plus links to everything else the harness
    // needs, so a login or a session store keeps working.
    assert!(dir.join("models.json").is_file(), "{}", dir.display());
    let auth = dir.join("auth.json");
    assert!(
        auth.symlink_metadata().unwrap().file_type().is_symlink(),
        "auth.json is linked, not copied"
    );
    assert_eq!(
        std::fs::read_to_string(&auth).unwrap(),
        r#"{"k":"v"}"#,
        "the link reads the user's file"
    );

    // The tag value is baked into the session copy…
    let generated: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("models.json")).unwrap()).unwrap();
    assert_eq!(generated["providers"]["litellm"]["models"][0]["id"], "m1");
    // …and the user's own file is byte-identical afterwards.
    assert_eq!(
        std::fs::read_to_string(sandbox.pi_models_path()).unwrap(),
        before
    );
}

#[test]
fn a_header_target_lands_in_the_session_copy_not_the_user_config() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&tags(
        "[[plugins.\"dev.oozut.llm-tags\".settings.targets]]\nname = \"X-AoE-Tags\"\ntemplate = \"aoe,{tags},session:{session}\"\nenv_var = \"AOE_LLM_TAGS\"\n",
    ));
    sandbox.write_pi_models(
        r#"{"providers":{"litellm":{"baseUrl":"http://h/v1","models":[{"id":"m1"}]}}}"#,
    );

    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    assert_eq!(
        emitted(&run.lines(), "AOE_LLM_TAGS"),
        Some("aoe,,session:S-cf11")
    );
    let dir = PathBuf::from(emitted(&run.lines(), "PI_CODING_AGENT_DIR").expect("override"));
    let generated: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("models.json")).unwrap()).unwrap();
    assert_eq!(
        generated["providers"]["litellm"]["headers"]["X-AoE-Tags"],
        "aoe,,session:S-cf11"
    );
    assert!(sandbox.read_pi_models()["providers"]["litellm"]
        .get("headers")
        .is_none());
}

#[test]
fn two_sessions_get_two_configs_with_their_own_values() {
    let mut first = Sandbox::new();
    first.write_pi_models(
        r#"{"providers":{"litellm":{"baseUrl":"http://h/v1","models":[{"id":"m1"}]}}}"#,
    );
    first.session("aaaa1111", "pi");
    let second = {
        let mut second = Sandbox::new();
        second.write_pi_models(
            r#"{"providers":{"litellm":{"baseUrl":"http://h/v1","models":[{"id":"m1"}]}}}"#,
        );
        second.session("bbbb2222", "pi");
        second
    };
    let a = first.run(&["hook"]);
    let b = second.run(&["hook"]);
    a.assert_ok();
    b.assert_ok();
    let dir_a = PathBuf::from(emitted(&a.lines(), "PI_CODING_AGENT_DIR").expect("a"));
    let dir_b = PathBuf::from(emitted(&b.lines(), "PI_CODING_AGENT_DIR").expect("b"));
    assert_ne!(dir_a, dir_b, "no two sessions share a config");
    assert!(dir_a.to_string_lossy().contains("aaaa1111"));
    assert!(dir_b.to_string_lossy().contains("bbbb2222"));
}

#[test]
fn nothing_is_redirected_when_the_harness_cannot_carry_the_target() {
    // No providers: the writer has nowhere to put the header, so the session
    // must keep reading its real config rather than an empty one.
    let sandbox = Sandbox::new();
    sandbox.write_pi_models(r#"{"providers":{}}"#);
    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    assert_eq!(emitted(&run.lines(), "PI_CODING_AGENT_DIR"), None);
    assert!(run.stderr.contains("not written"), "{}", run.stderr);
}

#[test]
fn a_launch_that_delivers_nothing_leaves_nothing_behind() {
    // Nothing to place: no session directory at all, and in particular no links
    // into the user's config dir (auth.json is the kind of thing that lives
    // there) for a harness that is never pointed at it.
    let sandbox = Sandbox::new();
    sandbox.write_pi_models(r#"{"providers":{}}"#);
    sandbox.write_file(
        &sandbox.path("home/.pi/agent/auth.json"),
        r#"{"token":"sk-live"}"#,
    );
    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    assert_eq!(emitted(&run.lines(), "PI_CODING_AGENT_DIR"), None);
    assert!(
        !sandbox.path("config/agent-of-empires/sessions").exists(),
        "a copy that carries nothing is not built either"
    );

    // And a harness that is not configured at all is the same case: no file to
    // start from, so nothing is created.
    let bare = Sandbox::new();
    let run = bare.run(&["hook"]);
    run.assert_ok();
    assert!(!bare.path("config/agent-of-empires/sessions").exists());
}

#[test]
fn a_copy_that_is_no_longer_used_does_not_stay_on_disk() {
    // A copy from an earlier launch must not outlive the launch that stops
    // delivering: the session's own overrides are the user's, and stay.
    let sandbox = Sandbox::new();
    sandbox.write_pi_models(
        r#"{"providers":{"litellm":{"baseUrl":"http://h/v1","models":[{"id":"m1"}]}}}"#,
    );
    let first = sandbox.run(&["hook"]);
    first.assert_ok();
    let dir = PathBuf::from(emitted(&first.lines(), "PI_CODING_AGENT_DIR").expect("override"));
    assert!(dir.join("models.json").is_file());

    let overrides =
        sandbox.path("config/agent-of-empires/sessions/cf11af87da7e4bf0/overrides.toml");
    sandbox.run(&["tag", "--set", "team:core"]).assert_ok();
    assert!(overrides.is_file(), "the session's own edit");

    // The live config loses its only provider, so the next launch has nowhere
    // to put the header.
    sandbox.write_pi_models(r#"{"providers":{}}"#);
    let second = sandbox.run(&["hook"]);
    second.assert_ok();
    assert_eq!(emitted(&second.lines(), "PI_CODING_AGENT_DIR"), None);
    assert!(!dir.exists(), "the unused copy is gone: {}", dir.display());
    assert!(
        overrides.is_file(),
        "the session edit is not the plugin's to delete"
    );
}

#[test]
fn the_session_copy_is_one_file_and_links_for_everything_else() {
    // "Copy what the tag needs, nothing else": the generated config is the only
    // regular file in the session dir, so no credential, session store or cache
    // is duplicated into the app dir — a link reaches the user's own copy.
    let sandbox = Sandbox::new();
    sandbox.write_pi_models(
        r#"{"providers":{"litellm":{"baseUrl":"http://h/v1","models":[{"id":"m1"}]}}}"#,
    );
    for (name, body) in [
        ("auth.json", r#"{"token":"sk-live"}"#),
        ("settings.json", r#"{"theme":"dark"}"#),
        ("models.json.bak-20260101", r#"{"providers":{}}"#),
    ] {
        sandbox.write_file(&sandbox.path(&format!("home/.pi/agent/{name}")), body);
    }
    sandbox.write_file(&sandbox.path("home/.pi/agent/sessions/keep.json"), "{}");

    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    let dir = PathBuf::from(emitted(&run.lines(), "PI_CODING_AGENT_DIR").expect("override"));

    let mut copied = Vec::new();
    let mut linked = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("session dir").flatten() {
        let path = entry.path();
        let meta = path.symlink_metadata().expect("lstat");
        if meta.file_type().is_symlink() {
            linked.push(entry.file_name().to_string_lossy().into_owned());
        } else {
            let name = entry.file_name().to_string_lossy().into_owned();
            assert_eq!(name, "models.json", "{name} was copied, not linked");
            copied.push(name);
        }
    }
    assert_eq!(copied, vec!["models.json".to_string()]);
    for name in ["auth.json", "settings.json", "models.json.bak-20260101"] {
        assert!(
            linked.contains(&name.to_string()),
            "{name} should be linked, saw {linked:?}"
        );
        let link = dir.join(name);
        assert!(
            std::fs::read_to_string(&link).is_ok(),
            "{name} links to the user's file"
        );
    }
    assert!(linked.contains(&"sessions".to_string()), "{linked:?}");

    // Private, because the one copied file carries whatever a provider needs to
    // authenticate: the directory and the copy are this user's alone, whatever
    // mode the file they were built from had.
    let mode = |path: &Path| -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&dir), 0o700, "the session dir is private");
    assert_eq!(mode(&dir.join("models.json")), 0o600, "the copy is private");
}

#[test]
fn a_source_config_that_is_world_readable_is_still_copied_privately() {
    let sandbox = Sandbox::new();
    sandbox.write_pi_models(
        r#"{"providers":{"litellm":{"baseUrl":"http://h/v1","apiKey":"sk-live","models":[{"id":"m1"}]}}}"#,
    );
    let source = sandbox.pi_models_path();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    }

    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    let dir = PathBuf::from(emitted(&run.lines(), "PI_CODING_AGENT_DIR").expect("override"));
    let copy = dir.join("models.json");
    // The key is in there because the harness has to authenticate; the mode is
    // what keeps it from being read by another account.
    assert!(std::fs::read_to_string(&copy)
        .expect("copy")
        .contains("sk-live"));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&copy)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "the copy does not inherit 0644 from the source"
    );
    assert_eq!(
        std::fs::metadata(&dir)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn an_adapter_that_rides_the_environment_writes_no_file() {
    let mut sandbox = Sandbox::new();
    sandbox.session("cf11af87da7e4bf0", "claude");
    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    let lines = run.lines();
    let raw = emitted(&lines, "AOE_LLM_TAGS").expect("the header");
    let payload: serde_json::Value = serde_json::from_str(raw).expect("JSON");
    assert_eq!(payload["session"], "S-cf11");
    assert_eq!(payload["tool"], "claude");
    assert!(
        run.lines().iter().all(|line| !line.contains("CONFIG_DIR")),
        "{:?}",
        run.lines()
    );
    assert!(!sandbox.path("config/agent-of-empires/sessions").exists());
}

#[test]
fn opencode_gets_one_generated_file_pointed_at_by_its_own_variable() {
    let mut sandbox = Sandbox::new();
    sandbox.session("cf11af87da7e4bf0", "opencode");
    sandbox.write_file(
        &sandbox.path("home/.config/opencode/opencode.json"),
        r#"{"provider":{"acme":{"options":{"baseURL":"http://h/v1"}}}}"#,
    );
    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    let file = PathBuf::from(emitted(&run.lines(), "OPENCODE_CONFIG").expect("override"));
    assert!(file.is_file(), "{}", file.display());
    let generated: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    // The value is one JSON object, so a consumer parses a field, not the string.
    let header_value: serde_json::Value = serde_json::from_str(
        generated["provider"]["acme"]["options"]["headers"]["X-AoE-Tags"]
            .as_str()
            .expect("a string header value"),
    )
    .expect("the header value is JSON");
    assert_eq!(header_value["session"], "S-cf11");
    assert_eq!(header_value["tool"], "opencode");
    // The user's file kept its single provider and gained nothing.
    let original: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(sandbox.path("home/.config/opencode/opencode.json")).unwrap(),
    )
    .unwrap();
    assert!(original["provider"]["acme"]["options"]
        .get("headers")
        .is_none());
}

#[test]
fn undo_removes_this_sessions_config_and_nothing_else() {
    let sandbox = Sandbox::new();
    sandbox.write_pi_models(
        r#"{"providers":{"litellm":{"baseUrl":"http://h/v1","models":[{"id":"m1"}]}}}"#,
    );
    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    let dir = PathBuf::from(emitted(&run.lines(), "PI_CODING_AGENT_DIR").expect("override"));
    assert!(dir.is_dir());

    let undo = sandbox.run(&["setup", "--undo"]);
    undo.assert_ok();
    assert!(!dir.exists(), "the session config is gone");
    // The user's config, which was never edited, is still there.
    assert!(sandbox.pi_models_path().is_file());
}

#[test]
fn every_overlay_row_names_a_variable_and_an_implemented_writer() {
    // A future row cannot silently claim a session config it cannot build.
    let (rows, warnings) = llm_tags::adapters::load(None);
    assert!(warnings.is_empty(), "{warnings:?}");
    for row in &rows {
        match row.write_mode.as_str() {
            "overlay_dir" | "overlay_file" => {
                assert!(!row.overlay_env.is_empty(), "{}: no overlay_env", row.agent);
                assert!(
                    matches!(
                        row.writer.as_str(),
                        "pi_models" | "opencode_json" | "codex_toml" | "vibe_toml"
                    ),
                    "{}: writer {:?} is not implemented",
                    row.agent,
                    row.writer
                );
                assert_eq!(llm_tags::overlay::verdict(row), "overlay");
            }
            "env_only" => assert_eq!(llm_tags::overlay::verdict(row), "wired"),
            _ => assert_eq!(llm_tags::overlay::verdict(row), "unsupported"),
        }
    }
}

#[test]
fn a_stale_session_dir_does_not_survive_its_session() {
    let root = std::env::temp_dir().join(format!("llm-tags-prune-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let kept = llm_tags::overlay::session_dir(&root, "live-session", "pi");
    let stale = llm_tags::overlay::session_dir(&root, "gone-session", "pi");
    for dir in [&kept, &stale] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let removed = llm_tags::overlay::prune(&root, &["live-session".to_string()]);
    assert_eq!(removed, 1);
    assert!(kept.is_dir());
    assert!(!stale.exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_session_id_cannot_escape_the_sessions_directory() {
    let dir = llm_tags::overlay::session_dir(Path::new("/base"), "../../etc", "pi");
    assert_eq!(
        dir.parent().unwrap().parent().unwrap(),
        Path::new("/base/sessions")
    );
    assert_eq!(dir.file_name().unwrap(), "pi");
    assert_eq!(
        dir.parent().unwrap().components().count(),
        Path::new("/base/sessions").components().count() + 1,
        "the id stays one segment: {}",
        dir.display()
    );
}

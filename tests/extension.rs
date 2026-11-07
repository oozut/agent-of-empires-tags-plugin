//! A harness-side extension is code that runs inside the agent, so it is only
//! installed when the user said so twice: the `harness_extension` setting (intent)
//! and a receipt naming the exact file they approved.
//!
//! These run the real CLI in a throwaway tree and read the session copy back, so
//! every state here is the state a launch would really produce — including the
//! ones that must install nothing.

#![cfg(unix)]

mod common;

use std::path::PathBuf;

use common::{settings_config, Sandbox};

/// The digest pinned for the artefact this build ships.
///
/// A change here is a decision, not a formality: users approved the previous
/// bytes, so a new digest invalidates every receipt and asks them again. Update
/// this constant together with `DISCLOSURE_VERSION` when the artefact's
/// behaviour changes, and with nothing else when only its comments do.
const ARTEFACT_SHA256: &str = "d653f348523828c1aa439f5bb3a88c4899eb47f81406f6c2fa9ce9c2c880494d";

fn emitted<'a>(lines: &'a [&'a str], name: &str) -> Option<&'a str> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
}

fn wanted() -> String {
    settings_config(
        "harness_extension = [\"pi\"]\n\
         [[plugins.\"dev.oozut.llm-tags\".settings.targets]]\n\
         name = \"X-AoE-Tags\"\ntemplate = \"aoe,{tags}\"\nenv_var = \"AOE_LLM_TAGS\"\n",
        "",
    )
}

/// The session config dir a hook printed.
fn session_dir(run: &common::Run) -> PathBuf {
    PathBuf::from(emitted(&run.lines(), "PI_CODING_AGENT_DIR").expect("the override line"))
}

/// Approve the harness the way a user does, with the digest the disclosure
/// printed. Returns the CLI's output so a test can assert on the disclosure too.
fn approve(sandbox: &Sandbox) -> common::Run {
    let printed = sandbox.run(&["extension", "enable", "pi", "--print"]);
    printed.assert_ok();
    let digest = printed
        .stdout
        .lines()
        .find_map(|line| line.split("--accept ").nth(1))
        .map(str::trim)
        .expect("the printed approve command");
    let run = sandbox.run(&["extension", "enable", "pi", "--accept", digest]);
    run.assert_ok();
    run
}

fn user_extension(sandbox: &Sandbox) -> PathBuf {
    let path = sandbox.path("home/.pi/agent/extensions/dyn.ts");
    sandbox.write_file(&path, "export default function () {}\n");
    path
}

#[test]
fn without_the_setting_a_session_copy_links_the_users_extensions_and_installs_nothing() {
    let sandbox = Sandbox::new();
    let user = user_extension(&sandbox);
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    approve(&sandbox);

    // Approved, but not requested: approval alone is not consent either.
    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    let dir = session_dir(&run);
    let extensions = dir.join("extensions");
    assert!(
        extensions
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "with no extension requested the whole directory stays a link: {}",
        extensions.display()
    );
    assert_eq!(
        std::fs::read_to_string(extensions.join("dyn.ts")).unwrap(),
        std::fs::read_to_string(&user).unwrap()
    );
    assert_eq!(emitted(&run.lines(), "AOE_LLM_TAGS_LIVE"), None);
}

#[test]
fn intent_without_approval_installs_nothing_and_says_why() {
    let sandbox = Sandbox::new();
    user_extension(&sandbox);
    sandbox.write_config(&wanted());
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);

    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    let dir = session_dir(&run);
    assert!(!dir.join("extensions/aoe-llm-tags.ts").exists());
    run.assert_contains("not approved");
    run.assert_contains("extension enable pi");
}

#[test]
fn an_approved_extension_lands_in_the_session_copy_and_reads_a_live_file() {
    let sandbox = Sandbox::new();
    let user = user_extension(&sandbox);
    sandbox.write_config(&wanted());
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);

    let approved = approve(&sandbox);
    approved.assert_contains("approved: pi");
    // The disclosure is what the user approved, so it names the file, the
    // digest and the way back out.
    approved.assert_contains("extensions/aoe-llm-tags.ts");
    approved.assert_contains("extension disable pi");
    assert!(
        approved.stdout.len() > 200,
        "the disclosure is not a one-liner"
    );

    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    let dir = session_dir(&run);
    let extensions = dir.join("extensions");
    assert!(
        !extensions
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "an extension makes the directory real: {}",
        extensions.display()
    );
    let installed = extensions.join("aoe-llm-tags.ts");
    let body = std::fs::read_to_string(&installed).expect("the extension file");
    assert!(body.contains("before_provider_headers"), "{body}");
    assert!(body.contains("AOE_LLM_TAGS_LIVE"), "{body}");

    // The user's own extension still loads: a link per entry, not a copy.
    let linked = extensions.join("dyn.ts");
    assert!(linked.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(
        std::fs::read_to_string(&linked).unwrap(),
        std::fs::read_to_string(&user).unwrap()
    );

    // And the live file carries the value this launch resolved, never a new tag.
    let live = PathBuf::from(emitted(&run.lines(), "AOE_LLM_TAGS_LIVE").expect("the live line"));
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&live).unwrap()).expect("live.json");
    assert_eq!(body["enabled"], true);
    assert_eq!(body["targets"][0]["kind"], "header");
    assert_eq!(body["targets"][0]["name"], "X-AoE-Tags");
    assert!(
        body["targets"][0]["value"]
            .as_str()
            .unwrap()
            .starts_with("aoe,"),
        "{body}"
    );
    // Nothing secret reaches a file the agent can read.
    assert!(!body.to_string().contains("token"));
}

#[test]
fn a_file_changed_since_the_approval_is_not_written() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&wanted());
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    // A receipt for bytes this build does not ship.
    let app = sandbox.path("config/agent-of-empires");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        app.join("harness-extension.toml"),
        "[extension.pi]\n\
         sha256 = \"1111111111111111111111111111111111111111111111111111111111111111\"\n\
         disclosure_version = 1\naccept_at = \"2026-11-07T00:00:00Z\"\n",
    )
    .unwrap();

    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    assert!(!session_dir(&run)
        .join("extensions/aoe-llm-tags.ts")
        .exists());
    run.assert_contains("changed since you approved it");
}

#[test]
fn an_older_disclosure_is_not_approval_for_a_newer_one() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&wanted());
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    approve(&sandbox);
    let path = sandbox.path("config/agent-of-empires/harness-extension.toml");
    let body = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        body.replace("disclosure_version = 1", "disclosure_version = 0"),
    )
    .unwrap();

    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    assert!(!session_dir(&run)
        .join("extensions/aoe-llm-tags.ts")
        .exists());
    run.assert_contains("disclosure changed");
}

#[test]
fn revoking_removes_the_copy_at_the_next_launch() {
    let sandbox = Sandbox::new();
    user_extension(&sandbox);
    sandbox.write_config(&wanted());
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    approve(&sandbox);
    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    let dir = session_dir(&run);
    assert!(dir.join("extensions/aoe-llm-tags.ts").exists());

    sandbox.run(&["extension", "disable", "pi"]).assert_ok();
    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    assert!(
        !dir.join("extensions/aoe-llm-tags.ts").exists(),
        "the revoked copy is gone at the next launch"
    );
    assert!(
        dir.join("extensions")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "the directory is the user's link again"
    );
    assert_eq!(emitted(&run.lines(), "AOE_LLM_TAGS_LIVE"), None);
}

#[test]
fn a_wrong_digest_approves_nothing() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&wanted());
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    let run = sandbox.run(&["extension", "enable", "pi", "--accept", &"0".repeat(64)]);
    run.assert_code(1);
    run.assert_contains("does not match this build's file");
    assert!(
        !sandbox
            .path("config/agent-of-empires/harness-extension.toml")
            .exists(),
        "a rejected digest writes no receipt"
    );
}

#[test]
fn doctor_says_what_each_state_means() {
    let sandbox = Sandbox::new();
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);

    // Nothing asked for: the harness keeps to launch-time tags, and doctor skips.
    let run = sandbox.doctor(&[]);
    run.assert_contains("extension pi");
    run.assert_contains("not in `harness_extension`");

    sandbox.write_config(&wanted());
    let run = sandbox.doctor(&[]);
    run.assert_contains("extension pi");
    run.assert_contains("not approved");

    approve(&sandbox);
    let run = sandbox.doctor(&[]);
    run.assert_contains("extension pi");
    run.assert_contains("approved");
    run.assert_contains("sha256 ");
}

#[test]
fn a_name_that_is_not_a_harness_is_reported_rather_than_ignored() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config("harness_extension = [\"nope\"]\n", ""));
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    let run = sandbox.doctor(&[]);
    run.assert_contains("extension nope");
    run.assert_contains("not a harness this build has a row for");
}

/// The mechanism is per-harness data, not a branch: a harness id appears only in
/// `adapters.toml` (and in the disclosure text it is substituted into), so
/// adding a second harness is a row and an approval, not an edit here.
#[test]
fn no_harness_name_is_hardcoded_in_the_mechanism() {
    for file in ["src/harness_extension.rs", "src/overlay.rs", "src/cli.rs"] {
        let body = std::fs::read_to_string(file).expect(file);
        for needle in ["\"pi\"", "pi.ts", "pi-proxy"] {
            assert!(
                !body.contains(needle),
                "{file} names {needle}: the harness belongs in adapters.toml, not in the code"
            );
        }
    }
}

#[test]
fn the_shipped_artefact_matches_the_digest_this_test_pins() {
    let source = std::fs::read("harness/pi.ts").expect("harness/pi.ts");
    let digest = llm_tags::sha256::hex(&source);
    assert_eq!(
        digest, ARTEFACT_SHA256,
        "the artefact changed: update ARTEFACT_SHA256 and DISCLOSURE_VERSION in \
         src/harness_extension.rs, or revert the change — every user-approved receipt \
         is tied to the previous bytes"
    );
}

#[test]
fn an_approved_receipt_outlives_a_plugin_release() {
    // The receipt is a file in the app dir, not plugin storage: a reinstall that
    // replaces the plugin tree leaves it working, and only the two pinned facts
    // (bytes, wording) can invalidate it.
    let sandbox = Sandbox::new();
    sandbox.write_config(&wanted());
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    approve(&sandbox);
    let path = sandbox.path("config/agent-of-empires/harness-extension.toml");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("[extension.pi]"));
    assert!(body.contains("accepted_at = "));
    // An older plugin version in the receipt changes nothing.
    std::fs::write(
        &path,
        body.replace("accepted_plugin = \"", "accepted_plugin = \"0.0.1-"),
    )
    .unwrap();
    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    assert!(session_dir(&run)
        .join("extensions/aoe-llm-tags.ts")
        .exists());
}

#[test]
fn status_lists_the_requested_harness_and_its_receipt() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&wanted());
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    approve(&sandbox);
    let run = sandbox.run(&["extension", "status", "--json"]);
    run.assert_ok();
    let rows: serde_json::Value = serde_json::from_str(&run.stdout).expect("json");
    let row = &rows.as_array().expect("an array")[0];
    assert_eq!(row["harness"], "pi");
    assert_eq!(row["requested"], true);
    assert_eq!(row["state"], "ok");
    assert_eq!(row["receipt"]["disclosure_version"], 1);
}

/// The page and the CLI must not disagree about where the file goes: the path in
/// the disclosure is the path the writer uses, from the same row.
#[test]
fn the_disclosure_names_the_path_the_writer_uses() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&wanted());
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    let printed = sandbox.run(&["extension", "enable", "pi", "--print"]);
    printed.assert_ok();
    let named = printed
        .stdout
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("<session config>/"))
        .and_then(|line| line.split(". ").next())
        .expect("the disclosure names the file");
    assert_eq!(named, "<session config>/extensions/aoe-llm-tags.ts");
    approve(&sandbox);
    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    let relative = named.replace("<session config>/", "");
    assert!(session_dir(&run).join(relative).is_file());
}

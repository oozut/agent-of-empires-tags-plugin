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
use llm_tags::harness_extension::DISCLOSURE_VERSION;

/// The digest pinned for the bundle this build ships to `pi`: the hook, the
/// skill and the always-loaded reminder, hashed in the disclosure's order.
///
/// A change here is a decision, not a formality: users approved the previous
/// bytes, so a new digest invalidates every receipt and asks them again. Update
/// this constant together with `DISCLOSURE_VERSION` when the bundle's behaviour
/// changes, and with nothing else when only its comments do.
const BUNDLE_SHA256: &str = "0228e63707ff451a4890a3a94d24f8a6e8b5b3c157524fdf903a73073dbadb75";

/// The digest pinned for the same carriers in a harness's own config: the same
/// skill and reminder, placed under `~/.claude` instead of a session copy, so a
/// different grant and a different digest.
const USER_BUNDLE_SHA256: &str = "069d8952700ca4fbd2729d319dd522a091fe3c2a875f368269e0285576e16a08";

/// The first row whose carriers go into the user's own config, with the paths a
/// user-scope approval writes. Taken from the table, not spelled here: the row
/// is what decides, and a test that hard-coded `claude` would stop covering the
/// mechanism the moment the table changed.
fn user_scope_row() -> llm_tags::adapters::Adapter {
    let (rows, warnings) = llm_tags::adapters::load(None);
    assert!(warnings.is_empty(), "{warnings:?}");
    rows.iter()
        .find(|row| {
            llm_tags::harness_extension::carrier(row)
                == Some(llm_tags::harness_extension::Carrier::User)
        })
        .cloned()
        .expect("a row whose carriers live in the user's own config")
}

/// Approve any harness by name, with the digest its disclosure printed.
fn approve_any(sandbox: &Sandbox, harness: &str) -> common::Run {
    let printed = sandbox.run(&["extension", "enable", harness, "--print"]);
    printed.assert_ok();
    let digest = printed
        .stdout
        .lines()
        .find_map(|line| line.split("--accept ").nth(1))
        .map(str::trim)
        .expect("the printed approve command");
    let run = sandbox.run(&["extension", "enable", harness, "--accept", digest]);
    run.assert_ok();
    run
}

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
    // Approval put the skill in AoE's list, but the session still carries only
    // the user's own config: an approval is not a request.
    assert!(sandbox
        .path("config/agent-of-empires/skills/aoe-activity/SKILL.md")
        .is_file());
    assert!(!dir.join("skills/aoe-activity").exists());
    assert!(!dir.join("APPEND_SYSTEM.md").exists());
}

/// The skill travels to two places: the session copy the agent loads, and AoE's
/// managed store, which is what the Skills page lists. The second one is the
/// plugin's only write outside a session folder, so it has to be exactly its own
/// entry and nothing else.
#[test]
fn approving_puts_the_skill_in_the_session_and_in_aoe_s_skill_list() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&wanted());
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);
    approve(&sandbox);
    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    let dir = session_dir(&run);

    let session_copy = dir.join("skills/aoe-activity/SKILL.md");
    let body = std::fs::read_to_string(&session_copy).expect("the skill in the session");
    assert!(body.starts_with("---\nname: aoe-activity\n"), "{body}");
    assert!(body.contains("activity <name>") || body.contains("activity testing"));
    assert!(body.contains("aoe-llm-tags: managed skill"));

    let managed = sandbox.path("config/agent-of-empires/skills/aoe-activity/SKILL.md");
    assert_eq!(
        std::fs::read_to_string(&managed).expect("the managed skill"),
        body,
        "the two copies are the same skill"
    );
    // Discovery is what makes it appear: nothing else in the managed store.
    let entries: Vec<_> = std::fs::read_dir(sandbox.path("config/agent-of-empires/skills"))
        .expect("the managed store")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, vec!["aoe-activity"]);
}

/// A skill in the managed store that a person wrote, adopted or edited carries
/// no marker, and a plugin does not overwrite it: the approval is not a licence
/// to take over somebody else's skill.
#[test]
fn a_managed_skill_somebody_else_owns_is_left_alone() {
    let sandbox = Sandbox::new();
    let mine = sandbox.path("config/agent-of-empires/skills/aoe-activity");
    std::fs::create_dir_all(&mine).unwrap();
    let theirs = "---\nname: aoe-activity\ndescription: my own take\n---\n\nmine\n";
    std::fs::write(mine.join("SKILL.md"), theirs).unwrap();
    sandbox.write_config(&wanted());
    sandbox.write_pi_models(r#"{"providers":{"litellm":{"models":[{"id":"m1"}]}}}"#);

    sandbox
        .run(&["extension", "enable", "pi", "--accept", BUNDLE_SHA256])
        .assert_ok();
    assert_eq!(
        std::fs::read_to_string(mine.join("SKILL.md")).unwrap(),
        theirs,
        "the person's skill is theirs"
    );
    // The session copy still arrives: the managed entry is discoverability, not
    // delivery.
    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    assert!(session_dir(&run)
        .join("skills/aoe-activity/SKILL.md")
        .is_file());
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
        body.replace(
            &format!("disclosure_version = {DISCLOSURE_VERSION}"),
            "disclosure_version = 0",
        ),
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
    // The rest of the bundle goes with it: the skill, its managed copy, and the
    // reminder. What is left is the user's own config, untouched.
    assert!(!dir.join("skills/aoe-activity").exists());
    assert!(!dir.join("APPEND_SYSTEM.md").exists());
    assert!(!sandbox
        .path("config/agent-of-empires/skills/aoe-activity")
        .exists());
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
fn the_shipped_bundle_matches_the_digest_this_test_pins() {
    // The bundle is what a receipt pins, so this hashes all three files in the
    // same path order the code does — a fourth file, or a reworded one, is a
    // different bundle and every receipt it invalidates is meant to be
    // invalidated.
    let (rows, warnings) = llm_tags::adapters::load(None);
    assert!(warnings.is_empty(), "{warnings:?}");
    let pi = rows
        .iter()
        .find(|row| row.agent == "pi")
        .expect("the pi row");
    assert_eq!(
        llm_tags::harness_extension::bundle_sha(pi, llm_tags::harness_extension::Carrier::Session)
            .expect("the shipped digest"),
        BUNDLE_SHA256,
        "the bundle changed: update BUNDLE_SHA256 and DISCLOSURE_VERSION, or revert the \
         change — every user-approved receipt is tied to the previous bytes"
    );
    let user = user_scope_row();
    assert_eq!(
        llm_tags::harness_extension::bundle_sha(&user, llm_tags::harness_extension::Carrier::User)
            .expect("the shipped digest"),
        USER_BUNDLE_SHA256,
        "{}'s user-scope bundle changed: update USER_BUNDLE_SHA256 and DISCLOSURE_VERSION",
        user.agent
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
    assert_eq!(row["receipt"]["disclosure_version"], DISCLOSURE_VERSION);
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
    let named: Vec<&str> = printed
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("<session config>/"))
        .filter_map(|line| line.split_once("  ").map(|(path, _)| path.trim()))
        .collect();
    // Every file the disclosure names is a file the writer writes: one hook, the
    // skill and the always-loaded reminder.
    assert_eq!(
        named,
        vec![
            "<session config>/extensions/aoe-llm-tags.ts",
            "<session config>/skills/aoe-activity/SKILL.md",
            "<session config>/APPEND_SYSTEM.md",
        ]
    );
    approve(&sandbox);
    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    for path in named {
        let relative = path.replace("<session config>/", "");
        assert!(
            session_dir(&run).join(&relative).is_file(),
            "{relative} was disclosed but not written"
        );
    }
}

/// The other half of the mechanism: a harness with no per-session config copy
/// still gets the activity carriers — in its own config, behind a marker block,
/// installed at approval time and removed at a revoke.
#[test]
fn a_harness_without_a_session_copy_gets_the_carriers_in_its_own_config() {
    let sandbox = Sandbox::new();
    let row = user_scope_row();
    let harness = row.agent.clone();
    sandbox.write_config(&settings_config(
        &format!("harness_extension = [\"{harness}\"]\n"),
        "",
    ));

    let approved = approve_any(&sandbox, &harness);
    approved.assert_contains(&format!("approved: {harness}"));
    // The disclosure has to describe the wider grant, not the session one.
    approved.assert_contains("in your own config");
    approved.assert_contains("marker");
    approved.assert_contains(&format!("extension disable {harness}"));

    let skill = sandbox.path(&format!(
        "home/{}/SKILL.md",
        row.user_skill_root.trim_start_matches("~/").to_string() + "/aoe-activity"
    ));
    assert!(skill.is_file(), "{} was not written", skill.display());
    let reminder = sandbox.path(&format!(
        "home/{}",
        row.user_instructions.trim_start_matches("~/")
    ));
    let body = std::fs::read_to_string(&reminder).expect("the reminder");
    assert!(body.contains("aoe-llm-tags:begin"), "{body}");
    assert!(body.contains("aoe-llm-tags:end"), "{body}");
    // The skill also appears in AoE's own list, from the same approval.
    assert!(sandbox
        .path("config/agent-of-empires/skills/aoe-activity/SKILL.md")
        .is_file());

    // Nothing was written into a session copy: this harness has none.
    let run = sandbox.run(&["hook"]);
    run.assert_ok();

    sandbox.run(&["extension", "disable", &harness]).assert_ok();
    assert!(!skill.exists(), "the skill stayed behind");
    assert!(
        !reminder.exists(),
        "the reminder file was ours alone, so it should be gone"
    );
}

/// A file the user already had keeps every line of their own, and gets it back
/// byte for byte when the approval is revoked.
#[test]
fn the_users_own_instruction_file_is_kept_and_restored() {
    let sandbox = Sandbox::new();
    let row = user_scope_row();
    let harness = row.agent.clone();
    sandbox.write_config(&settings_config(
        &format!("harness_extension = [\"{harness}\"]\n"),
        "",
    ));
    let reminder = sandbox.path(&format!(
        "home/{}",
        row.user_instructions.trim_start_matches("~/")
    ));
    let theirs = "# My rules\n\n- be brief\n- no filler\n";
    sandbox.write_file(&reminder, theirs);

    approve_any(&sandbox, &harness);
    let body = std::fs::read_to_string(&reminder).expect("the reminder");
    assert!(body.starts_with(theirs), "their text was reworded: {body}");
    assert!(body.contains("aoe-llm-tags:begin"), "{body}");

    // A second approval is the same file, not a second block.
    approve_any(&sandbox, &harness);
    assert_eq!(
        std::fs::read_to_string(&reminder).expect("the reminder"),
        body,
        "a second approval duplicated the block"
    );

    sandbox.run(&["extension", "disable", &harness]).assert_ok();
    assert_eq!(
        std::fs::read_to_string(&reminder).expect("their file"),
        theirs,
        "the user's own text did not come back byte for byte"
    );
}

/// A copy `aoe skill sync` deployed belongs to AoE: its propagation marker is
/// `marker_at`'s evidence that AoE, not this plugin, tracks that directory. A
/// disable removes the reminder and leaves the skill to AoE, and says so — a
/// disable that silently deleted another manager's file would be worse than one
/// that explains itself.
#[test]
fn a_skill_copy_aoe_has_taken_over_is_left_to_aoe() {
    let sandbox = Sandbox::new();
    let row = user_scope_row();
    let harness = row.agent.clone();
    sandbox.write_config(&settings_config(
        &format!("harness_extension = [\"{harness}\"]\n"),
        "",
    ));
    approve_any(&sandbox, &harness);

    let skill_dir = sandbox.path(&format!(
        "home/{}/{}",
        row.user_skill_root.trim_start_matches("~/"),
        llm_tags::harness_extension::skill_dir(&row)
    ));
    let skill = skill_dir.join("SKILL.md");
    assert!(skill.is_file(), "the skill was not installed: {skill:?}");
    // What AoE leaves behind after `aoe skill sync --replace`.
    sandbox.write_file(
        &skill_dir.join(llm_tags::harness_extension::AOE_PROPAGATION_MARKER),
        r#"{"version":1,"root":"x","directory":"aoe-activity","digest":"sha256:0"}"#,
    );

    let disabled = sandbox.run(&["extension", "disable", &harness]);
    disabled.assert_ok();
    assert!(
        skill.is_file(),
        "a disable deleted a copy AoE had taken over"
    );
    assert!(
        disabled.stdout.contains("kept:") && disabled.stdout.contains("aoe skill sync"),
        "the disable did not say the skill stayed, or why: {}",
        disabled.stdout
    );

    // The reminder is still ours, and still goes.
    let reminder = sandbox.path(&format!(
        "home/{}",
        row.user_instructions.trim_start_matches("~/")
    ));
    let body = std::fs::read_to_string(&reminder).unwrap_or_default();
    assert!(
        !body.contains("aoe-llm-tags:begin"),
        "the marker block survived a disable: {body}"
    );
}

/// A second approval of unchanged carriers writes nothing: rewriting identical
/// bytes only churns the timestamp, and on a copy AoE propagated it reads as an
/// edit AoE then has to preserve.
#[test]
fn approving_again_does_not_rewrite_files_that_are_already_right() {
    let sandbox = Sandbox::new();
    let row = user_scope_row();
    let harness = row.agent.clone();
    sandbox.write_config(&settings_config(
        &format!("harness_extension = [\"{harness}\"]\n"),
        "",
    ));
    approve_any(&sandbox, &harness);

    let skill = sandbox.path(&format!(
        "home/{}/{}/SKILL.md",
        row.user_skill_root.trim_start_matches("~/"),
        llm_tags::harness_extension::skill_dir(&row)
    ));
    let first = std::fs::metadata(&skill)
        .and_then(|meta| meta.modified())
        .expect("the skill's timestamp");
    std::thread::sleep(std::time::Duration::from_millis(20));
    approve_any(&sandbox, &harness);
    let second = std::fs::metadata(&skill)
        .and_then(|meta| meta.modified())
        .expect("the skill's timestamp");
    assert_eq!(
        first, second,
        "a second approval rewrote a file that was already right"
    );

    // And the guard is not a refusal to ever write: a copy that is no longer the
    // bytes this build ships is put right, timestamp and all.
    std::fs::write(
        &skill,
        format!("stale\n{}\n", llm_tags::harness_extension::MANAGED_MARKER),
    )
    .expect("make it stale");
    std::thread::sleep(std::time::Duration::from_millis(20));
    approve_any(&sandbox, &harness);
    let third = std::fs::metadata(&skill)
        .and_then(|meta| meta.modified())
        .expect("the skill's timestamp");
    assert_ne!(second, third, "a stale skill was left stale");
    assert!(std::fs::read_to_string(&skill)
        .expect("the skill")
        .contains(llm_tags::harness_extension::MANAGED_MARKER));
}

/// Every row that carries anything has a destination that exists, an artefact
/// this build ships and a digest — the property that makes "works for all the
/// harnesses" a check rather than a claim.
#[test]
fn every_row_with_a_destination_places_a_file_this_build_ships() {
    let (rows, warnings) = llm_tags::adapters::load(None);
    assert!(warnings.is_empty(), "{warnings:?}");
    let mut covered = Vec::new();
    for row in &rows {
        let Some(carrier) = llm_tags::harness_extension::carrier(row) else {
            continue;
        };
        let bundle = llm_tags::harness_extension::bundle(row, carrier);
        assert!(!bundle.is_empty(), "{} carries nothing", row.agent);
        for placement in bundle.parts() {
            let path = placement.artifact.to_string_lossy().into_owned();
            assert!(
                llm_tags::artifacts::bytes(&path).is_some(),
                "{} names {path}, which this build does not carry",
                row.agent
            );
            assert!(
                !placement.display.is_empty(),
                "{} has no disclosed path for {path}",
                row.agent
            );
            // A disclosed path has to say which of the two grants it is.
            match carrier {
                llm_tags::harness_extension::Carrier::Session => assert!(
                    placement.display.starts_with("<session config>/"),
                    "{} disclosed {} outside a session copy",
                    row.agent,
                    placement.display
                ),
                llm_tags::harness_extension::Carrier::User => assert!(
                    placement.display.starts_with("~/"),
                    "{} disclosed {} outside the home directory",
                    row.agent,
                    placement.display
                ),
            }
        }
        assert!(
            llm_tags::harness_extension::bundle_sha(row, carrier).is_ok(),
            "{} has no usable digest",
            row.agent
        );
        covered.push(row.agent.clone());
    }
    // The harnesses this was written for: a regression that silently drops one
    // shows up here by name.
    for harness in [
        "pi", "claude", "codex", "opencode", "gemini", "qwen", "droid",
    ] {
        assert!(
            covered.iter().any(|name| name == harness),
            "{harness} has no activity carrier any more"
        );
    }
}

/// A session-scope row that has no in-process hook: the carriers go into the
/// session copy, so the agent sees them, but nothing is re-read per request.
#[test]
fn a_session_copy_carries_the_skill_even_without_a_hook() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config("harness_extension = [\"vibe\"]\n", ""));
    let mut sandbox = sandbox;
    sandbox.session("cf11af87da7e4bf0", "vibe");
    sandbox.write_file(
        &sandbox.path("home/.vibe/config.toml"),
        "[[providers]]\nname = \"gw\"\napi_base = \"http://h/v1\"\n\n[[models]]\nname = \"m\"\nprovider = \"gw\"\n",
    );
    approve_any(&sandbox, "vibe");

    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    let dir = PathBuf::from(emitted(&run.lines(), "VIBE_HOME").expect("the override line"));
    // The skill and the reminder, in the copy the agent reads...
    assert!(
        dir.join("skills/aoe-activity/SKILL.md").is_file(),
        "the skill is missing from the session copy"
    );
    let reminder = std::fs::read_to_string(dir.join("AGENTS.md")).expect("the reminder");
    // A session copy is entirely this plugin's, so it needs no marker block: the
    // markers protect a file the user also writes in, and this one is ours.
    assert!(reminder.contains("Session activity"), "{reminder}");
    assert!(reminder.contains("activity <word>"), "{reminder}");
    // ...and no hook, so no live file: nothing here could re-read one.
    assert_eq!(emitted(&run.lines(), "AOE_LLM_TAGS_LIVE"), None);
    assert!(!dir.join("extensions").exists());

    sandbox.run(&["extension", "disable", "vibe"]).assert_ok();
    let run = sandbox.run(&["hook"]);
    run.assert_ok();
    let listing: Vec<String> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !dir.join("skills").exists(),
        "the skill stayed behind: {listing:?}"
    );
    assert!(
        !dir.join("AGENTS.md").exists(),
        "the reminder stayed behind"
    );
}

/// The disclosure is the thing the user approves, so it may not describe a
/// bundle bigger or better than the row actually ships: no per-request claim on a
/// row with no hook, no skill on a row without one, and no session copy on a row
/// that writes into the user's own config.
#[test]
fn the_disclosure_describes_exactly_what_the_row_carries() {
    let (rows, warnings) = llm_tags::adapters::load(None);
    assert!(warnings.is_empty(), "{warnings:?}");
    for row in &rows {
        let Some(carrier) = llm_tags::harness_extension::carrier(row) else {
            continue;
        };
        let sandbox = Sandbox::new();
        let printed = sandbox.run(&["extension", "enable", &row.agent, "--print"]);
        printed.assert_ok();
        let text = &printed.stdout;
        let has_hook = !row.extension_artifact.is_empty();
        let has_skill = !row.skill_artifact.is_empty();
        assert_eq!(
            text.contains("request headers from it on each model request"),
            has_hook,
            "{}: the per-request claim does not match its hook",
            row.agent
        );
        // The file list names the skill whatever the scope is; the paragraph
        // that promises to install one belongs to the user-scope wording.
        assert_eq!(
            text.contains("a skill that tells the agent"),
            has_skill,
            "{}: the skill claim does not match its artefacts",
            row.agent
        );
        match carrier {
            llm_tags::harness_extension::Carrier::Session => {
                assert!(
                    text.contains("private config copy"),
                    "{}: a session copy is not described as one",
                    row.agent
                );
                assert!(
                    !text.contains("your own config"),
                    "{}: a session copy is not the user's config",
                    row.agent
                );
            }
            llm_tags::harness_extension::Carrier::User => {
                assert!(
                    text.contains("your own"),
                    "{}: a user-scope grant is not described as one",
                    row.agent
                );
                assert!(
                    !text.contains("private config copy"),
                    "{}: claims a session copy it does not have",
                    row.agent
                );
                assert!(
                    text.contains("marker lines"),
                    "{}: a user-scope write has to name the markers",
                    row.agent
                );
                assert_eq!(
                    text.contains("put the activity skill"),
                    has_skill,
                    "{}: the skill promise does not match its artefacts",
                    row.agent
                );
            }
        }
    }
}

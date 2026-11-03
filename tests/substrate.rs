//! Per-substrate classification (`src/substrate.rs`): five verdicts, from files.
//!
//! These are the checks `plan/m2.md` M2-T02 names: `sandbox-stale` is
//! reproducible, an unknown harness never reads as `wired`, and the cache is
//! keyed by path + sha256 so an edited config is re-scanned rather than trusted.

mod common;

use common::*;
use serde_json::Value;

fn classify(sandbox: &Sandbox, extra: &[&str]) -> Value {
    let mut args = vec!["classify", "--json"];
    args.extend_from_slice(extra);
    let run = sandbox.run(&args);
    assert!(run.stderr.is_empty(), "{}", run.stderr);
    serde_json::from_str(&run.stdout).expect("classify --json")
}

fn verdict_of<'a>(report: &'a Value, scope: &str) -> &'a str {
    report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .find(|finding| finding["scope"] == scope)
        .unwrap_or_else(|| panic!("no {scope} finding in {report}"))["verdict"]
        .as_str()
        .expect("verdict")
}

#[test]
fn a_config_without_our_line_is_detected_but_not_wired() {
    let sandbox = Sandbox::new();
    sandbox.write_pi_models(
        r#"{"providers": {"gw": {"baseUrl": "http://h/v1", "models": [{"id": "m"}]}}}"#,
    );
    let report = classify(&sandbox, &["--adapter", "pi"]);
    assert_eq!(
        verdict_of(&report, "user"),
        "detected-not-wired",
        "{report}"
    );
    assert_eq!(verdict_of(&report, "harness"), "wired", "{report}");
    assert_eq!(report["worst"], "detected-not-wired", "{report}");
}

#[test]
fn a_config_carrying_the_line_is_wired() {
    let sandbox = Sandbox::new();
    sandbox.pi_models(&[("X-AoE-Tags", "$AOE_LLM_TAGS")]);
    let report = classify(&sandbox, &["--adapter", "pi"]);
    assert_eq!(verdict_of(&report, "user"), "wired", "{report}");
    assert_eq!(report["worst"], "wired", "{report}");
}

#[test]
fn a_second_scan_is_served_from_the_cache_and_an_edit_invalidates_it() {
    let sandbox = Sandbox::new();
    sandbox.write_pi_models(
        r#"{"providers": {"gw": {"baseUrl": "http://h/v1", "models": [{"id": "m"}]}}}"#,
    );
    let first = classify(&sandbox, &["--adapter", "pi"]);
    assert_eq!(first["findings"][1]["cached"], false, "{first}");

    let second = classify(&sandbox, &["--adapter", "pi"]);
    assert_eq!(second["findings"][1]["cached"], true, "{second}");
    assert_eq!(
        verdict_of(&second, "user"),
        "detected-not-wired",
        "{second}"
    );

    // A hand edit changes the digest, so the cached verdict cannot be reused —
    // this is what makes drift visible instead of silently stale.
    sandbox.pi_models(&[("X-AoE-Tags", "$AOE_LLM_TAGS")]);
    let third = classify(&sandbox, &["--adapter", "pi"]);
    assert_eq!(third["findings"][1]["cached"], false, "{third}");
    assert_eq!(verdict_of(&third, "user"), "wired", "{third}");
}

#[test]
fn a_copy_for_a_session_the_host_lost_is_sandbox_stale() {
    // Wire the host, then leave a copy behind for a session `aoe list` does not
    // know: that is the sandbox case, and it must not read as wiring.
    let mut sandbox = Sandbox::new();
    sandbox.session("cf11af87da7e4bf0", "pi");
    sandbox.pi_models(&[("X-AoE-Tags", "$AOE_LLM_TAGS")]);
    let stale = sandbox.path("config/agent-of-empires/sessions/deadbeefcafe0000/pi/models.json");
    sandbox.write_file(&stale, r#"{"providers": {}}"#);

    let report = classify(&sandbox, &[]);
    assert_eq!(verdict_of(&report, "sandbox"), "sandbox-stale", "{report}");
    assert_eq!(report["worst"], "sandbox-stale", "{report}");

    // The worker's own sweep removes it; after that the verdict goes away.
    std::fs::remove_dir_all(sandbox.path("config/agent-of-empires/sessions")).expect("rm");
    let report = classify(&sandbox, &[]);
    assert!(
        !report["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .any(|finding| finding["scope"] == "sandbox"),
        "{report}"
    );
}

#[test]
fn an_unknown_harness_is_unknown_and_never_wired() {
    let mut sandbox = Sandbox::new();
    sandbox.session("cf11af87da7e4bf0", "mystery");
    let report = classify(&sandbox, &[]);
    assert_eq!(verdict_of(&report, "harness"), "unknown", "{report}");
    assert_ne!(report["worst"], "wired", "{report}");
    let detail = report["findings"][0]["detail"].as_str().expect("detail");
    assert!(detail.contains("AOE_LLM_TAGS"), "{detail}");
}

#[test]
fn a_row_with_no_channel_is_unsupported_and_exits_nonzero() {
    let sandbox = Sandbox::new();
    let run = sandbox.run(&["classify", "--adapter", "kiro"]);
    assert_eq!(run.code, 1, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("harness unsupported"), "{}", run.stdout);
    // A reference row is not unsupported: it delivers through the user's config.
    let run = sandbox.run(&["classify", "--adapter", "codex"]);
    assert_eq!(run.code, 0, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("harness wired"), "{}", run.stdout);
}

#[test]
fn an_unreachable_host_makes_staleness_unknown_rather_than_stale() {
    // "aoe did not answer" is not evidence that a copy is stale: reporting
    // `sandbox-stale` here would be a false alarm on every daemon-less shell.
    let sandbox = Sandbox::new();
    let broken = sandbox.path("bin/aoe-broken");
    sandbox.write_executable(&broken, "#!/bin/sh\nexit 1\n");
    sandbox.write_file(
        &sandbox.path("config/agent-of-empires/sessions/deadbeefcafe0000/pi/models.json"),
        "{}",
    );
    let run = sandbox.run_with(
        &["classify", "--json"],
        &[("AOE_BIN", &broken.to_string_lossy())],
    );
    let report: Value = serde_json::from_str(&run.stdout).expect("json");
    assert_eq!(verdict_of(&report, "sandbox"), "unknown", "{report}");
}

#[test]
fn a_settings_change_is_not_answered_from_the_cache() {
    // The digest is unchanged but the question is not: one more target means one
    // more header that must be present for `wired`. The cache stores the asked
    // targets for exactly this reason.
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config(TWO_HEADERS, ""));
    sandbox.pi_models(&[
        ("X-AoE-Tags", "$AOE_LLM_TAGS"),
        ("X-AoE-Session", "$AOE_HDR_X_AOE_SESSION"),
    ]);
    let first = classify(&sandbox, &["--adapter", "pi"]);
    assert_eq!(verdict_of(&first, "user"), "wired", "{first}");
    let second = classify(&sandbox, &["--adapter", "pi"]);
    assert_eq!(second["findings"][1]["cached"], true, "{second}");

    sandbox.write_config(&settings_config(
        "headers = [\n  { header = \"X-AoE-Tags\", template = \"a\", env_var = \"AOE_LLM_TAGS\" },\n]\n",
        "",
    ));
    let third = classify(&sandbox, &["--adapter", "pi"]);
    assert_eq!(third["findings"][1]["cached"], false, "{third}");
    assert_eq!(verdict_of(&third, "user"), "wired", "{third}");
}

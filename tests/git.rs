//! Git facts in tags and templates, through the real binary: the hook, a fake
//! `git` that records every command line, and the payload it produced.
//!
//! The fake exists for two reasons. It makes the expected values fixed (a test
//! must not pass because this checkout happens to be on a branch called
//! `lithuanians`), and it records the calls, so "a launch that names no git fact
//! starts no git process" is an assertion rather than a claim.

#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};

use common::{settings_config, Run, Sandbox};

const BRANCH: &str = "release/2026-10";
const SHORT: &str = "d4f5381c2";
const FULL: &str = "d4f5381c2b1f0e9d7a6c5b4a39281706f5e4d3c2";
const SUBJECT: &str = "Repair the instrumentation test harness";

/// Answers the commands `src/gitinfo.rs` issues, appending each `-C <path> …`
/// argv to `$GIT_CALLS` before answering. `GIT_TAG` empty means "no tag names
/// this commit", the way an untagged HEAD behaves.
const GIT: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$GIT_CALLS"
shift 2
case "$*" in
  "rev-parse --is-inside-work-tree") echo true ;;
  "rev-parse --abbrev-ref HEAD") echo __BRANCH__ ;;
  "rev-parse --short HEAD") echo __SHORT__ ;;
  "rev-parse HEAD") echo __FULL__ ;;
  "describe --tags --exact-match") [ -n "$GIT_TAG" ] && echo "$GIT_TAG" || echo 'fatal: no tag exactly matches' >&2 ;;
  "describe --tags --always") echo "$GIT_TAG-3-g__SHORT__" ;;
  "log -1 --format=%s") echo "__SUBJECT__" ;;
  "log -1 --format=%an") echo gc ;;
  "status --porcelain") [ -n "$GIT_DIRTY" ] && echo " M src/resolver.rs" || true ;;
  "config --get remote.origin.url") echo "$GIT_REMOTE" ;;
  "rev-parse --show-toplevel") echo /srv/checkouts/android-keyboard ;;
  *) exit 1 ;;
esac
"#;

fn fake_git(sandbox: &Sandbox) -> PathBuf {
    let path = sandbox.path("bin/fake-git");
    let body = GIT
        .replace("__BRANCH__", BRANCH)
        .replace("__SHORT__", SHORT)
        .replace("__FULL__", FULL)
        .replace("__SUBJECT__", SUBJECT);
    sandbox.write_executable(&path, &body);
    path
}

/// The hook's one `AOE_LLM_TAGS=` line, parsed.
fn payload(run: &Run) -> serde_json::Value {
    run.assert_ok();
    let line = run
        .lines()
        .into_iter()
        .find(|line| line.starts_with("AOE_LLM_TAGS="))
        .unwrap_or_else(|| panic!("no payload line in {:?}", run.stdout));
    serde_json::from_str(line.trim_start_matches("AOE_LLM_TAGS=")).expect("payload JSON")
}

fn calls(sandbox: &Sandbox) -> Vec<String> {
    std::fs::read_to_string(sandbox.path("git-calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn run_with_git(sandbox: &Sandbox, git: &Path, extra: &[(&str, &str)]) -> Run {
    let calls_path = sandbox.path("git-calls");
    let mut env = vec![
        ("AOE_GIT_BIN", git.to_str().expect("git path")),
        ("GIT_CALLS", calls_path.to_str().expect("calls path")),
        (
            "GIT_REMOTE",
            "https://user:s3cret@github.com/oozut/android-keyboard",
        ),
        ("GIT_TAG", "v1.2.3"),
    ];
    env.extend_from_slice(extra);
    sandbox.run_with(&["hook"], &env)
}

#[test]
fn a_tag_value_can_carry_a_git_fact() {
    let sandbox = Sandbox::new();
    let git = fake_git(&sandbox);
    sandbox.write_config(&settings_config(
        r#"tags = ["vcs:git", "git.branch:{git.branch}", "git.commit:{git.commit}"]"#,
        "",
    ));
    let run = run_with_git(&sandbox, &git, &[]);
    let payload = payload(&run);
    // Ordinary key/value tags, with the git fact as the value: a consumer reads
    // tags["git.branch"] and never needs a template of its own.
    assert_eq!(payload["vcs"], "git");
    assert_eq!(payload["git.branch"], BRANCH);
    assert_eq!(payload["git.commit"], SHORT);
    assert_eq!(payload["git.sha"], serde_json::Value::Null, "{payload}");
}

#[test]
fn every_reference_reads_its_own_fact() {
    let sandbox = Sandbox::new();
    let git = fake_git(&sandbox);
    sandbox.write_config(&settings_config(
        r#"tags = [
  "branch:{git.branch}",
  "sha:{git.sha}",
  "tag:{git.tag}",
  "describe:{git.describe}",
  "subject:{git.subject}",
  "author:{git.author}",
  "dirty:{git.dirty}",
  "repo:{git.repo}",
  "root:{git.root}",
]"#,
        "",
    ));
    // Two runs, because `TAG_CAP` is five: the cap is a real limit and a test
    // that quietly exceeded it would prove five references, not nine.
    let run = run_with_git(&sandbox, &git, &[("GIT_DIRTY", "1")]);
    let first = payload(&run);
    assert_eq!(first["branch"], BRANCH);
    assert_eq!(first["sha"], FULL);
    assert_eq!(first["tag"], "v1.2.3");
    assert_eq!(first["describe"], format!("v1.2.3-3-g{SHORT}"), "{first}");
    assert_eq!(first["subject"], SUBJECT);
    assert_eq!(
        first["git.remote"],
        serde_json::Value::Null,
        "a reference inside a tag value is expanded, not kept"
    );

    sandbox.write_config(&settings_config(
        r#"tags = [
  "author:{git.author}",
  "dirty:{git.dirty}",
  "repo:{git.repo}",
  "root:{git.root}",
  "remote:{git.remote}",
]"#,
        "",
    ));
    let run = run_with_git(&sandbox, &git, &[("GIT_DIRTY", "1")]);
    let second = payload(&run);
    assert_eq!(second["author"], "gc", "{second}");
    assert_eq!(second["dirty"], "true", "{second}");
    assert_eq!(second["repo"], "android-keyboard", "{second}");
    assert_eq!(
        second["root"], "/srv/checkouts/android-keyboard",
        "{second}"
    );
    assert_eq!(
        second["remote"], "https://github.com/oozut/android-keyboard",
        "{second}"
    );
}

#[test]
fn the_remote_loses_its_credentials_before_it_reaches_a_header() {
    let sandbox = Sandbox::new();
    let git = fake_git(&sandbox);
    sandbox.write_config(&settings_config(r#"tags = ["remote:{git.remote}"]"#, ""));
    let run = run_with_git(&sandbox, &git, &[]);
    let payload = payload(&run);
    assert_eq!(
        payload["remote"], "https://github.com/oozut/android-keyboard",
        "{payload}"
    );
}

#[test]
fn one_fact_is_read_once_even_when_a_template_and_a_tag_both_name_it() {
    let sandbox = Sandbox::new();
    let git = fake_git(&sandbox);
    sandbox.write_config(&settings_config(
        r#"tags = ["git.branch:{git.branch}"]
headers = [{ header = "X-Git", template = "{git.branch}@{git.commit}", env_var = "GIT" }]"#,
        "",
    ));
    let run = run_with_git(&sandbox, &git, &[]);
    run.assert_ok();
    let lines = run.lines();
    assert!(
        lines.contains(&format!("GIT={BRANCH}@{SHORT}").as_str()),
        "{lines:?}"
    );
    let branch_calls = calls(&sandbox)
        .iter()
        .filter(|line| line.contains("rev-parse --abbrev-ref HEAD"))
        .count();
    assert_eq!(branch_calls, 1, "{:?}", calls(&sandbox));
}

#[test]
fn a_launch_that_names_no_git_fact_starts_no_git_process() {
    let sandbox = Sandbox::new();
    let git = fake_git(&sandbox);
    let run = run_with_git(&sandbox, &git, &[]);
    payload(&run);
    assert!(calls(&sandbox).is_empty(), "{:?}", calls(&sandbox));
    // And no warning about git, because git was never relevant.
    assert!(!run.stderr.contains("git"), "{}", run.stderr);
}

#[test]
fn git_is_read_from_the_sessions_own_checkout() {
    let sandbox = Sandbox::new();
    let git = fake_git(&sandbox);
    sandbox.write_config(&settings_config(
        r#"tags = ["git.branch:{git.branch}"]"#,
        "",
    ));
    run_with_git(&sandbox, &git, &[]);
    let calls = calls(&sandbox);
    assert!(!calls.is_empty());
    for line in &calls {
        assert!(
            line.starts_with("-C /var/lib/aoe/android-keyboard-worktrees/lithuanians "),
            "{line}"
        );
    }
}

#[test]
fn not_a_work_tree_is_a_warning_and_not_a_failure() {
    let sandbox = Sandbox::new();
    let git = sandbox.path("bin/fake-git-no-tree");
    sandbox.write_executable(
        &git,
        r#"#!/bin/sh
shift 2
case "$*" in
  "rev-parse --is-inside-work-tree") echo false ;;
  *) exit 1 ;;
esac
"#,
    );
    sandbox.write_config(&settings_config(
        r#"tags = ["git.branch:{git.branch}"]"#,
        "",
    ));
    let run = run_with_git(&sandbox, &git, &[]);
    let payload = payload(&run);
    assert_eq!(payload["git.branch"], "", "{payload}");
    assert!(
        run.stderr.contains("not inside a git work tree"),
        "{}",
        run.stderr
    );
}

#[test]
fn a_missing_git_binary_is_a_warning_and_the_launch_still_succeeds() {
    let sandbox = Sandbox::new();
    sandbox.write_config(&settings_config(
        r#"tags = ["git.branch:{git.branch}"]"#,
        "",
    ));
    let run = sandbox.run_with(
        &["hook"],
        &[
            ("AOE_GIT_BIN", "/nonexistent/git"),
            ("GIT_CALLS", "/nonexistent/calls"),
        ],
    );
    let payload = payload(&run);
    assert_eq!(payload["git.branch"], "", "{payload}");
    assert!(run.stderr.contains("git.branch"), "{}", run.stderr);
}

#[test]
fn an_untagged_head_leaves_the_tag_empty_rather_than_erroring() {
    let sandbox = Sandbox::new();
    let git = fake_git(&sandbox);
    sandbox.write_config(&settings_config(
        r#"tags = ["git.tag:{git.tag}", "git.describe:{git.describe}"]"#,
        "",
    ));
    let run = run_with_git(&sandbox, &git, &[("GIT_TAG", "")]);
    let payload = payload(&run);
    assert_eq!(payload["git.tag"], "", "{payload}");
    // `--always` still answers, so the describe field keeps its short form.
    assert_eq!(payload["git.describe"], format!("-3-g{SHORT}"), "{payload}");
}

#[test]
fn a_tag_value_cannot_ask_for_the_tag_set_it_is_part_of() {
    let sandbox = Sandbox::new();
    let git = fake_git(&sandbox);
    sandbox.write_config(&settings_config(
        r#"tags = ["loop:{tags_json}", "git.branch:{git.branch}"]"#,
        "",
    ));
    let run = run_with_git(&sandbox, &git, &[]);
    let payload = payload(&run);
    assert_eq!(payload["loop"], "", "{payload}");
    assert!(
        run.stderr.contains("built from the tags themselves"),
        "{}",
        run.stderr
    );
}

#[test]
fn an_unknown_reference_in_a_tag_value_renders_empty_and_says_so() {
    let sandbox = Sandbox::new();
    let git = fake_git(&sandbox);
    sandbox.write_config(&settings_config(r#"tags = ["who:{git.typo}"]"#, ""));
    let run = run_with_git(&sandbox, &git, &[]);
    let payload = payload(&run);
    assert_eq!(payload["who"], "", "{payload}");
    assert!(
        run.stderr.contains("{git.typo}") && run.stderr.contains("renders empty"),
        "{}",
        run.stderr
    );
}

#[test]
fn the_references_verb_lists_every_reference_with_its_value() {
    let sandbox = Sandbox::new();
    let git = fake_git(&sandbox);
    let run = sandbox.run_with(
        &["references", "--json"],
        &[
            ("AOE_GIT_BIN", git.to_str().expect("git path")),
            (
                "GIT_CALLS",
                sandbox.path("git-calls").to_str().expect("calls path"),
            ),
            ("GIT_REMOTE", "https://github.com/oozut/android-keyboard"),
            ("GIT_TAG", ""),
        ],
    );
    run.assert_ok();
    let value: serde_json::Value = serde_json::from_str(&run.stdout).expect("JSON");
    let rows = value["references"].as_array().expect("rows");
    let names: Vec<&str> = rows
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"git.branch"), "{names:?}");
    assert!(names.contains(&"session_id"), "{names:?}");
    let branch = rows
        .iter()
        .find(|row| row["name"] == "git.branch")
        .expect("git.branch row");
    assert_eq!(branch["reference"], "{git.branch}");
    assert_eq!(branch["group"], "Git");
    assert_eq!(branch["sample"], BRANCH);

    // The same catalogue the editor's dropdown is fed from, so the two cannot
    // drift apart: every `git.*` row here is a name `src/gitinfo.rs` knows.
    let git_names: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| name.starts_with("git."))
        .collect();
    assert_eq!(
        git_names.len(),
        llm_tags::gitinfo::REFERENCES.len(),
        "{git_names:?}"
    );
}

#[test]
fn the_git_preset_is_a_sample_a_user_can_tick() {
    let sandbox = Sandbox::new();
    let git = fake_git(&sandbox);
    sandbox.write_config(&settings_config("", ""));
    let run = sandbox.run_with(
        &["tag", "--git", "--show", "--json"],
        &[
            ("AOE_GIT_BIN", git.to_str().expect("git path")),
            (
                "GIT_CALLS",
                sandbox.path("git-calls").to_str().expect("calls path"),
            ),
            ("GIT_REMOTE", "https://github.com/oozut/android-keyboard"),
            ("GIT_TAG", "v1.2.3"),
        ],
    );
    run.assert_ok();
    let value: serde_json::Value = serde_json::from_str(&run.stdout).expect("JSON");
    assert_eq!(
        value["tags"],
        serde_json::json!(llm_tags::resolver::GIT_PRESET)
    );
    // The edit is per-session: the global settings were only read.
    let global = std::fs::read_to_string(sandbox.config_path("")).expect("config");
    assert_eq!(global, settings_config("", ""));
    let overrides =
        sandbox.path("config/agent-of-empires/sessions/cf11af87da7e4bf0/overrides.toml");
    let body = std::fs::read_to_string(&overrides).expect("overrides.toml");
    assert!(body.contains("git.branch:{git.branch}"), "{body}");
}

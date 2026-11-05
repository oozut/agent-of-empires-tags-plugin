//! Git facts against the real `git`: the shapes a scripted fake cannot prove.
//!
//! `tests/git.rs` pins the values and the exact command lines with a fake `git`,
//! which keeps the expectations stable and makes "no reference, no subprocess" an
//! assertion. This file is the other half: a linked worktree, an unborn branch, a
//! detached HEAD, a unicode subject, and a `git` that never answers. Those are the
//! cases where the fake would only ever prove what the fake was told.

#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{settings_config, Run, Sandbox};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git must be installed to run these tests");
    assert!(
        output.status.success(),
        "git {args:?} in {}: {}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn commit(repo: &Path, message: &str) {
    git(repo, &["add", "-A"]);
    git(
        repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
}

fn repository(sandbox: &Sandbox, name: &str) -> PathBuf {
    let path = sandbox.path(name);
    std::fs::create_dir_all(&path).expect("repo dir");
    git(&path, &["init", "-q", "-b", "main"]);
    std::fs::write(path.join("README.md"), "first\n").expect("file");
    commit(&path, "first commit");
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

/// Run the hook as if the session's checkout were `repo` — the `aoe` row is what
/// tells the plugin which path to read, so the fake host answers for this one.
fn hook_at(sandbox: &Sandbox, repo: &Path, extra: &[(&str, &str)]) -> Run {
    let row = format!(
        r#"{{"id":"cf11af87da7e4bf0","title":"Git","path":"{}","group":"","tool":"pi","profile":"main"}}"#,
        repo.display()
    );
    let aoe = sandbox.fake_aoe(&row);
    let mut env = vec![("AOE_BIN", aoe.to_str().expect("aoe path"))];
    env.extend_from_slice(extra);
    sandbox.run_with(&["hook"], &env)
}

#[test]
fn a_linked_worktree_reads_its_own_branch_root_and_name() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    let linked = sandbox.path("linked");
    // A linked worktree is the shape this machine's own sessions run in: the
    // checkout is not the repository, and HEAD belongs to the worktree.
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature/git-tags",
            linked.to_str().expect("path"),
        ],
    );
    sandbox.write_config(&settings_config(
        r#"tags = ["vcs:git", "branch:{git.branch}", "root:{git.root}", "repo:{git.repo}"]"#,
        "",
    ));

    let run = hook_at(&sandbox, &linked, &[]);
    let payload = payload(&run);
    assert_eq!(payload["branch"], "feature/git-tags", "{payload}");
    assert_eq!(payload["repo"], "repo", "{payload}");
    let root = payload["root"].as_str().expect("root");
    assert_eq!(
        std::fs::canonicalize(root).expect("root exists"),
        std::fs::canonicalize(&linked).expect("linked exists"),
        "the root is the worktree, not the repository"
    );
    assert!(run.stderr.is_empty(), "{}", run.stderr);
}

#[test]
fn a_directory_inside_a_checkout_is_not_a_checkout_of_its_own() {
    // The session's path is what is read, not whatever repository happens to sit
    // above it: a home directory that is itself a dotfiles clone must not turn a
    // plain project into `main` of somebody else's branch.
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    let nested = repo.join("src");
    std::fs::create_dir_all(&nested).expect("nested");
    sandbox.write_config(&settings_config(r#"tags = ["branch:{git.branch}"]"#, ""));

    let run = hook_at(&sandbox, &nested, &[]);
    let payload = payload(&run);
    assert_eq!(payload["branch"], "", "{payload}");
    assert!(
        run.stderr.contains("is not a git checkout"),
        "{}",
        run.stderr
    );
}

#[test]
fn an_unborn_branch_is_a_warning_and_never_a_failed_launch() {
    let sandbox = Sandbox::new();
    let repo = sandbox.path("empty");
    std::fs::create_dir_all(&repo).expect("dir");
    git(&repo, &["init", "-q", "-b", "main"]);
    sandbox.write_config(&settings_config(
        r#"tags = ["vcs:git", "branch:{git.branch}", "commit:{git.commit}"]"#,
        "",
    ));

    let run = hook_at(&sandbox, &repo, &[]);
    let payload = payload(&run);
    // No commit yet: the facts that need one are empty, the tag set is still sent.
    assert_eq!(payload["vcs"], "git", "{payload}");
    assert_eq!(payload["branch"], "", "{payload}");
    assert_eq!(payload["commit"], "", "{payload}");
    assert!(
        run.stderr.contains("renders empty"),
        "the user is told, once per named fact: {}",
        run.stderr
    );
}

#[test]
fn a_detached_head_renders_the_short_commit_where_the_branch_would_be() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    git(&repo, &["checkout", "-q", "--detach", "HEAD"]);
    let short = git(&repo, &["rev-parse", "--short", "HEAD"]);
    sandbox.write_config(&settings_config(r#"tags = ["at:{git.branch}"]"#, ""));

    let run = hook_at(&sandbox, &repo, &[]);
    let payload = payload(&run);
    assert_eq!(payload["at"], short.as_str(), "{payload}");
    assert!(run.stderr.is_empty(), "{}", run.stderr);
}

#[test]
fn a_unicode_subject_reaches_the_header_on_one_line() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    std::fs::write(repo.join("note.md"), "second\n").expect("file");
    commit(&repo, "fix: café ☕ — ünïcode");
    sandbox.write_config(&settings_config(r#"tags = ["subject:{git.subject}"]"#, ""));

    let run = hook_at(&sandbox, &repo, &[]);
    let payload = payload(&run);
    assert_eq!(payload["subject"], "fix: café ☕ — ünïcode", "{payload}");
    // One target, one line: a value that wrapped would break aoe's KEY=VALUE parse.
    assert_eq!(
        run.stdout.lines().count(),
        1,
        "one line per header: {:?}",
        run.stdout
    );
}

#[test]
fn a_clean_tree_is_false_and_one_untracked_file_is_true() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    sandbox.write_config(&settings_config(r#"tags = ["dirty:{git.dirty}"]"#, ""));

    let clean = payload(&hook_at(&sandbox, &repo, &[]));
    assert_eq!(clean["dirty"], "false", "{clean}");

    std::fs::write(repo.join("untracked.txt"), "hello\n").expect("file");
    let dirty = payload(&hook_at(&sandbox, &repo, &[]));
    assert_eq!(dirty["dirty"], "true", "{dirty}");
}

#[test]
fn a_git_that_never_answers_costs_one_launch_budget_and_not_one_per_fact() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    let slow = sandbox.path("bin/slow-git");
    sandbox.write_executable(&slow, "#!/bin/sh\nexec sleep 30\n");
    // The whole sample set: three facts, so a per-fact budget would be six seconds.
    sandbox.write_config(&settings_config(
        r#"tags = ["vcs:git", "git.branch:{git.branch}", "git.commit:{git.commit}"]"#,
        "",
    ));

    let started = Instant::now();
    let run = hook_at(
        &sandbox,
        &repo,
        &[("AOE_GIT_BIN", slow.to_str().expect("path"))],
    );
    let elapsed = started.elapsed();
    let payload = payload(&run);
    assert_eq!(payload["vcs"], "git", "{payload}");
    assert_eq!(payload["git.branch"], "", "{payload}");
    assert!(
        run.stderr.contains("timed out") || run.stderr.contains("launch budget"),
        "the warnings say why: {}",
        run.stderr
    );
    assert!(
        elapsed < Duration::from_millis(3_500),
        "the launch waits for one 2s budget, not one per fact: {elapsed:?}"
    );
}

#[test]
fn a_grandchild_that_outlives_git_cannot_hold_the_launch() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    // git exits at once, but the background child inherited its stdout, so the
    // pipe stays open. Waiting for the reader to finish would wait for `sleep`:
    // the drain gets a grace period and the launch keeps its budget.
    let leaky = sandbox.path("bin/leaky-git");
    sandbox.write_executable(&leaky, "#!/bin/sh\necho main\nsleep 30 &\nexit 1\n");
    sandbox.write_config(&settings_config(r#"tags = ["branch:{git.branch}"]"#, ""));

    let started = Instant::now();
    let run = hook_at(
        &sandbox,
        &repo,
        &[("AOE_GIT_BIN", leaky.to_str().expect("path"))],
    );
    let elapsed = started.elapsed();
    payload(&run);
    assert!(
        elapsed < Duration::from_millis(1_500),
        "a held pipe must not become the launch's wait: {elapsed:?}"
    );
}

#[test]
fn a_slow_aoe_and_a_slow_git_share_the_same_budget() {
    let sandbox = Sandbox::new();
    let repo = repository(&sandbox, "repo");
    // `aoe session show` takes 1.2s of the 2s budget; git may only have the rest.
    let slow_aoe = sandbox.path("bin/slow-aoe");
    sandbox.write_executable(
        &slow_aoe,
        &format!(
            "#!/bin/sh\nsleep 1.2\necho '{{\"id\":\"cf11af87da7e4bf0\",\"title\":\"Git\",\"path\":\"{}\",\"group\":\"\",\"tool\":\"pi\",\"profile\":\"main\"}}'\n",
            repo.display()
        ),
    );
    let slow_git = sandbox.path("bin/slow-git");
    sandbox.write_executable(&slow_git, "#!/bin/sh\nexec sleep 30\n");
    sandbox.write_config(&settings_config(
        r#"tags = ["branch:{git.branch}", "commit:{git.commit}"]"#,
        "",
    ));

    let started = Instant::now();
    let run = sandbox.run_with(
        &["hook"],
        &[
            ("AOE_BIN", slow_aoe.to_str().expect("path")),
            ("AOE_GIT_BIN", slow_git.to_str().expect("path")),
        ],
    );
    let elapsed = started.elapsed();
    let payload = payload(&run);
    assert_eq!(payload["branch"], "", "{payload}");
    // 2s total, not 2s for aoe and then 2s more for git.
    assert!(
        elapsed < Duration::from_millis(3_500),
        "aoe and git share one budget: {elapsed:?}"
    );
}

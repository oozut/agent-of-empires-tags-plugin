//! `{git.xxx}`: what the working tree says, read lazily and never fatal.
//!
//! A tag value or a target template may name a git fact — `git.branch:{git.branch}`
//! puts the branch in the tag set itself, `template = "{git.commit}"` sends the
//! commit on its own. Nothing is read unless a reference asks for it, for the same
//! reason `{branch}` is lazy: a launch that never mentions git must not pay for a
//! subprocess, and a repository that is not there must never cost the session its
//! tags.
//!
//! Three rules keep this honest:
//!
//! * **One runner, injectable.** Every call goes through [`Runner`], so a test
//!   answers without a repository and production shells out through
//!   [`default_runner`] (bounded by the launch budget, `AOE_GIT_BIN` overridable).
//! * **A missing fact is empty, not an error.** A repository with no tag renders
//!   `{git.tag}` as an empty string; a path that is not a working tree produces
//!   one warning and empty values, never a failed hook.
//! * **No secrets.** `{git.remote}` drops any userinfo from the URL, so a token in
//!   `https://user:token@host/…` cannot travel in a header. `{git.root}` and
//!   `{git.author}` are opt-in by reference, like `{project_path}`.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::env::Env;
use crate::resolver::{is_executable, run_text, AoeError};

/// Point this at another `git` binary (a test ships a script; a host may pin one).
pub const GIT_BIN_ENV: &str = "AOE_GIT_BIN";

/// One git fact a template or a tag value may name.
pub struct GitReference {
    /// The reference without braces: `git.branch` renders as `{git.branch}`.
    pub name: &'static str,
    /// One line for the editor's dropdown and the `references` verb.
    pub description: &'static str,
}

/// Every reference this module resolves, in the order the UI lists them.
pub const REFERENCES: [GitReference; 11] = [
    GitReference {
        name: "git.branch",
        description: "Current branch; a detached HEAD renders the short commit instead.",
    },
    GitReference {
        name: "git.commit",
        description: "Abbreviated commit SHA of HEAD.",
    },
    GitReference {
        name: "git.sha",
        description: "Full 40-character commit SHA of HEAD.",
    },
    GitReference {
        name: "git.tag",
        description: "Tag pointing exactly at HEAD; empty when no tag does.",
    },
    GitReference {
        name: "git.describe",
        description:
            "`git describe --tags --always`: the nearest tag with distance, or the short SHA.",
    },
    GitReference {
        name: "git.subject",
        description: "Subject line of the HEAD commit.",
    },
    GitReference {
        name: "git.author",
        description: "Author name of the HEAD commit.",
    },
    GitReference {
        name: "git.dirty",
        description: "`true` when the working tree has uncommitted changes, otherwise `false`.",
    },
    GitReference {
        name: "git.remote",
        description: "`origin`'s URL, with any credentials removed.",
    },
    GitReference {
        name: "git.repo",
        description: "Repository name from `origin`, or the working tree's directory name.",
    },
    GitReference {
        name: "git.root",
        description: "Absolute path of the working tree's top level.",
    },
];

/// True when `name` is a git reference (a template may hold anything).
pub fn is_reference(name: &str) -> bool {
    REFERENCES.iter().any(|item| item.name == name)
}

/// The injected `git` call: `(label, argv) → stdout`.
///
/// The label is only what a warning prints (`git rev-parse`); `argv` includes the
/// `-C <path>` prefix, so a test answers on the whole command line the way the
/// real binary would.
pub type Runner<'a> = &'a dyn Fn(&str, &[String]) -> Result<String, AoeError>;

/// `AOE_GIT_BIN` → `git` on `PATH` → `git`.
pub fn git_bin(env: &Env) -> String {
    git_available(env).unwrap_or_else(|| "git".to_string())
}

/// The binary a launch would actually run, when there is one: doctor asks this so a
/// missing `git` is a reported fault rather than an empty `{git.branch}`.
pub fn git_available(env: &Env) -> Option<String> {
    match env.non_empty(GIT_BIN_ENV) {
        Some(bin) => Some(bin.to_string()),
        None => which(env, "git"),
    }
}

fn which(env: &Env, name: &str) -> Option<String> {
    let path = match env.non_empty("PATH") {
        Some(path) => path.to_string(),
        None => std::env::var("PATH").unwrap_or_default(),
    };
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = Path::new(dir).join(name);
        if is_executable(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

pub fn default_runner<'a>(
    env: &'a Env,
    timeout: Duration,
) -> impl Fn(&str, &[String]) -> Result<String, AoeError> + 'a {
    move |verb, args| run_text(&git_bin(env), verb, args, timeout)
}

/// Resolve the requested references for the work tree at `path`.
///
/// `names` may contain anything the caller collected from templates and tag
/// values; names that are not git references are ignored (the renderer reports
/// them), so a typo never turns into a subprocess. Every requested git name gets
/// an entry in the map, empty when it could not be resolved, so a template sees a
/// known field rather than an "unknown field" warning on top of the real one.
pub fn resolve(
    path: &str,
    names: &[&str],
    runner: Option<Runner<'_>>,
    env: &Env,
    timeout: Duration,
) -> (BTreeMap<String, String>, Vec<String>) {
    let wanted: Vec<&str> = {
        let mut wanted: Vec<&str> = Vec::new();
        for name in names {
            if is_reference(name) && !wanted.contains(name) {
                wanted.push(name);
            }
        }
        wanted
    };
    let mut values: BTreeMap<String, String> = BTreeMap::new();
    if wanted.is_empty() {
        return (values, Vec::new());
    }
    for name in &wanted {
        values.insert((*name).to_string(), String::new());
    }
    let listed = wanted
        .iter()
        .map(|name| format!("{{{name}}}"))
        .collect::<Vec<_>>()
        .join(", ");

    let missing_path = path.trim().is_empty();
    if missing_path {
        return (
            values,
            vec![format!(
                "the session has no project path; {listed} render empty"
            )],
        );
    }
    if timeout.is_zero() {
        return (
            values,
            vec![format!("the launch budget is spent; {listed} render empty")],
        );
    }

    let owned;
    let runner: Runner<'_> = match runner {
        Some(runner) => runner,
        None => {
            owned = default_runner(env, timeout);
            &owned
        }
    };
    let mut git = Git {
        path,
        runner,
        deadline: Instant::now() + timeout,
        cache: BTreeMap::new(),
        warnings: Vec::new(),
    };
    if !git.inside_work_tree() {
        return (
            values,
            vec![format!(
                "{path:?} is not inside a git work tree; {listed} render empty"
            )],
        );
    }
    for name in &wanted {
        if let Some(value) = git.field(name) {
            values.insert((*name).to_string(), value);
        }
    }
    (values, git.warnings)
}

struct Git<'a> {
    path: &'a str,
    runner: Runner<'a>,
    deadline: Instant,
    /// Keyed by the command line, so two references that need the same fact
    /// (`git.commit` and a detached `git.branch`) cost one subprocess.
    cache: BTreeMap<String, Result<String, String>>,
    warnings: Vec<String>,
}

impl Git<'_> {
    fn call(&mut self, verb: &str, args: &[&str]) -> Result<String, String> {
        let key = args.join("\u{0}");
        if let Some(cached) = self.cache.get(&key) {
            return cached.clone();
        }
        let result = if Instant::now() >= self.deadline {
            Err("the git lookup used the launch budget".to_string())
        } else {
            let mut argv = vec!["-C".to_string(), self.path.to_string()];
            argv.extend(args.iter().map(|arg| arg.to_string()));
            (self.runner)(verb, &argv)
                .map(|text| text.trim().to_string())
                .map_err(|err| err.to_string())
        };
        self.cache.insert(key, result.clone());
        result
    }

    /// A command whose failure or empty answer simply means "nothing to say"
    /// (`git tag` on an untagged HEAD, an unset `origin`).
    fn optional(&mut self, verb: &str, args: &[&str]) -> Option<String> {
        match self.call(verb, args) {
            Ok(text) if text.is_empty() => None,
            Ok(text) => Some(text),
            Err(_) => None,
        }
    }

    fn inside_work_tree(&mut self) -> bool {
        matches!(
            self.call("rev-parse", &["rev-parse", "--is-inside-work-tree"]),
            Ok(text) if text == "true"
        )
    }

    fn field(&mut self, name: &str) -> Option<String> {
        match name {
            "git.branch" => match self.call("rev-parse", &["rev-parse", "--abbrev-ref", "HEAD"]) {
                Ok(branch) if branch != "HEAD" => Some(branch),
                Ok(_) => self.optional("rev-parse", &["rev-parse", "--short", "HEAD"]),
                Err(err) => {
                    self.warn("git.branch", &err);
                    None
                }
            },
            "git.commit" => {
                self.required("git.commit", "rev-parse", &["rev-parse", "--short", "HEAD"])
            }
            "git.sha" => self.required("git.sha", "rev-parse", &["rev-parse", "HEAD"]),
            "git.tag" => self.optional("describe", &["describe", "--tags", "--exact-match"]),
            "git.describe" => self.optional("describe", &["describe", "--tags", "--always"]),
            "git.subject" => self.required("git.subject", "log", &["log", "-1", "--format=%s"]),
            "git.author" => self.required("git.author", "log", &["log", "-1", "--format=%an"]),
            "git.dirty" => self
                .required("git.dirty", "status", &["status", "--porcelain"])
                .map(|text| if text.is_empty() { "false" } else { "true" }.to_string()),
            "git.remote" => self
                .optional("config", &["config", "--get", "remote.origin.url"])
                .map(|url| scrub_url(&url)),
            "git.repo" => {
                let remote = self
                    .optional("config", &["config", "--get", "remote.origin.url"])
                    .map(|url| repo_from_url(&url))
                    .filter(|name| !name.is_empty());
                remote.or_else(|| {
                    self.optional("rev-parse", &["rev-parse", "--show-toplevel"])
                        .and_then(|root| {
                            Path::new(&root)
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                        })
                })
            }
            "git.root" => self.optional("rev-parse", &["rev-parse", "--show-toplevel"]),
            _ => None,
        }
    }

    fn required(&mut self, name: &str, verb: &str, args: &[&str]) -> Option<String> {
        match self.call(verb, args) {
            Ok(text) if text.is_empty() => None,
            Ok(text) => Some(text),
            Err(err) => {
                self.warn(name, &err);
                None
            }
        }
    }

    fn warn(&mut self, name: &str, err: &str) {
        self.warnings
            .push(format!("{name}: {err}; it renders empty"));
    }
}

/// A remote URL without its credentials: the one thing in git facts that can
/// carry a secret, and a header is the last place it should end up.
pub fn scrub_url(url: &str) -> String {
    let url = url.trim();
    if let Some(sep) = url.find("://") {
        let prefix = &url[..sep + 3];
        let rest = &url[sep + 3..];
        let authority_end = rest.find('/').unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        // Keep everything after the last `@`: `user:token@host` → `host`.
        let host = authority.rsplit('@').next().unwrap_or(authority);
        return format!("{prefix}{host}{}", &rest[authority_end..]);
    }
    // The scp-like form `user@host:owner/repo.git` has no scheme to anchor on.
    match url.find('@') {
        Some(at) => url[at + 1..].to_string(),
        None => url.to_string(),
    }
}

/// The repository name a remote URL names, without `.git`.
pub fn repo_from_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let tail = url.rsplit(['/', ':']).next().unwrap_or_default();
    tail.trim_end_matches(".git").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A fake git: answers the first matching flag pattern, records the call.
    /// `answers` is ordered, so a more specific pattern (`--tags --exact-match`)
    /// must come before the general one it contains (`--tags --always`).
    fn runner(
        answers: &'static [(&'static str, &'static str)],
        seen: &'static RefCell<Vec<Vec<String>>>,
    ) -> impl Fn(&str, &[String]) -> Result<String, AoeError> {
        move |_verb, args| {
            seen.borrow_mut().push(args.to_vec());
            let command = args.join(" ");
            for (needle, answer) in answers {
                if command.contains(needle) {
                    return Ok(answer.to_string());
                }
            }
            Err(AoeError::Failed {
                bin: "git".to_string(),
                verb: "rev-parse".to_string(),
                code: 128,
                detail: ": not found".to_string(),
            })
        }
    }

    fn resolve_with(
        answers: &'static [(&'static str, &'static str)],
        names: &[&str],
    ) -> (BTreeMap<String, String>, Vec<String>, Vec<Vec<String>>) {
        let seen: &'static RefCell<Vec<Vec<String>>> =
            Box::leak(Box::new(RefCell::new(Vec::new())));
        let fake = runner(answers, seen);
        let (values, warnings) = resolve(
            "/w/repo",
            names,
            Some(&fake),
            &Env::default(),
            Duration::from_secs(1),
        );
        let seen = seen.borrow().clone();
        (values, warnings, seen)
    }

    const ANSWERS: &[(&str, &str)] = &[
        ("--tags --exact-match", "v1.4.0"),
        ("--tags --always", "v1.4.0-3-g56cf4f6"),
        ("--is-inside-work-tree", "true"),
        ("--abbrev-ref", "feature/tags"),
        ("--short", "56cf4f6"),
        ("--porcelain", " M src/resolver.rs"),
        ("--format=%s", "Add git references to the tag set"),
        ("--format=%an", "Guillaume"),
        (
            "remote.origin.url",
            "https://user:token@github.com/oozut/agent-of-empires-tags-plugin.git",
        ),
        ("--show-toplevel", "/w/repo"),
        ("HEAD", "56cf4f6d9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d"),
    ];

    #[test]
    fn every_reference_is_resolved_and_the_credential_is_scrubbed() {
        let names: Vec<&str> = REFERENCES.iter().map(|item| item.name).collect();
        let (values, warnings, _) = resolve_with(ANSWERS, &names);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(values["git.branch"], "feature/tags");
        assert_eq!(values["git.commit"], "56cf4f6");
        assert_eq!(
            values["git.sha"],
            "56cf4f6d9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d"
        );
        assert_eq!(values["git.tag"], "v1.4.0");
        assert_eq!(values["git.describe"], "v1.4.0-3-g56cf4f6");
        assert_eq!(values["git.subject"], "Add git references to the tag set");
        assert_eq!(values["git.author"], "Guillaume");
        assert_eq!(values["git.dirty"], "true");
        assert_eq!(values["git.repo"], "agent-of-empires-tags-plugin");
        assert_eq!(values["git.root"], "/w/repo");
        // A token must never ride a header.
        assert_eq!(
            values["git.remote"],
            "https://github.com/oozut/agent-of-empires-tags-plugin.git"
        );
    }

    #[test]
    fn only_the_referenced_facts_cost_a_subprocess() {
        let (values, warnings, seen) = resolve_with(ANSWERS, &["git.branch"]);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(values.len(), 1);
        assert_eq!(values["git.branch"], "feature/tags");
        // The work-tree check plus the one branch query, in that order.
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert!(seen[0].join(" ").contains("--is-inside-work-tree"));
        assert!(seen[1].join(" ").contains("--abbrev-ref"));
    }

    #[test]
    fn the_same_command_is_asked_once() {
        let (values, _, seen) = resolve_with(ANSWERS, &["git.commit", "git.sha"]);
        assert_eq!(values["git.commit"], "56cf4f6");
        assert!(values["git.sha"].starts_with("56cf4f6d"));
        // check + commit + sha: two rev-parse calls answer distinct questions.
        assert_eq!(seen.len(), 3, "{seen:?}");
    }

    #[test]
    fn a_detached_head_reads_as_the_short_commit() {
        let answers: &[(&str, &str)] = &[
            ("--is-inside-work-tree", "true"),
            ("--abbrev-ref", "HEAD"),
            ("--short", "56cf4f6"),
        ];
        let (values, warnings, _) = resolve_with(answers, &["git.branch"]);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(values["git.branch"], "56cf4f6");
    }

    #[test]
    fn a_repository_without_a_tag_renders_empty_and_does_not_warn() {
        let answers: &[(&str, &str)] = &[("--is-inside-work-tree", "true")];
        let (values, warnings, _) = resolve_with(answers, &["git.tag", "git.remote"]);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(values["git.tag"], "");
        assert_eq!(values["git.remote"], "");
    }

    #[test]
    fn a_directory_that_is_not_a_work_tree_is_one_warning_and_empty_values() {
        let answers: &[(&str, &str)] = &[];
        let (values, warnings, _) = resolve_with(answers, &["git.branch", "git.sha"]);
        assert_eq!(values["git.branch"], "");
        assert_eq!(values["git.sha"], "");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("not inside a git work tree"),
            "{warnings:?}"
        );
        assert!(
            warnings[0].contains("{git.branch}, {git.sha}"),
            "{warnings:?}"
        );
    }

    #[test]
    fn an_unknown_name_is_not_a_git_call() {
        let (values, warnings, seen) = resolve_with(ANSWERS, &["git.nope", "branch"]);
        assert!(values.is_empty(), "{values:?}");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(seen.is_empty(), "{seen:?}");
    }

    #[test]
    fn no_project_path_is_a_warning_and_no_subprocess() {
        let fake = |_: &str, _: &[String]| Ok("true".to_string());
        let (values, warnings) = resolve(
            "",
            &["git.branch"],
            Some(&fake),
            &Env::default(),
            Duration::from_secs(1),
        );
        assert_eq!(values["git.branch"], "");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("no project path"), "{warnings:?}");
    }

    #[test]
    fn urls_lose_their_credentials_and_keep_their_shape() {
        assert_eq!(
            scrub_url("https://user:secret@example.test/o/r.git"),
            "https://example.test/o/r.git"
        );
        assert_eq!(
            scrub_url("ssh://git@example.test:22/o/r.git"),
            "ssh://example.test:22/o/r.git"
        );
        assert_eq!(
            scrub_url("git@github.com:oozut/repo.git"),
            "github.com:oozut/repo.git"
        );
        assert_eq!(
            scrub_url("https://example.test/o/r.git"),
            "https://example.test/o/r.git"
        );
        assert_eq!(repo_from_url("git@github.com:oozut/repo.git"), "repo");
        assert_eq!(
            repo_from_url("https://example.test/oozut/repo.git/"),
            "repo"
        );
        assert_eq!(repo_from_url(""), "");
    }

    #[test]
    fn the_binary_can_be_pointed_at_a_test_double() {
        let env = Env::new([("AOE_GIT_BIN", "/tmp/fake-git")]);
        assert_eq!(git_bin(&env), "/tmp/fake-git");
        let env = Env::new([("PATH", "/nonexistent")]);
        assert_eq!(git_bin(&env), "git");
    }
}

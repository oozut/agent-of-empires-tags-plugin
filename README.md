# Agent of Empires LLM Tags

An Agent of Empires plugin that makes each agent session attach identifying headers — or query
parameters, or request-body fields — to every LLM request it sends, so any system on the request
path — a gateway, an egress proxy, a log pipeline, a session-control plane — can attribute,
route, or validate that request.

Status: M0 and M1 done, M2 delivering per session, with project and AoE defaults in the panel. M0's proof (two independent headers reaching a
request, including a sandboxed leg via podman) still holds; M2 changed *what* travels and *where*.

Tags are edited from the **LLM Tags panel** on the right of the dashboard, in three tabs:
**Session**, **Project** (only when the session has a project path), and **AoE**. Each row says
where its tag came from (`this session`, `project`, `AoE`, `settings`) and the two upper tabs can
add, hide, restore, copy the session's set upward, or clear that level. The session's detail page
adds a JSON viewer/editor for the same values, so a whole object can be changed in place without
writing one row at a time.

Every write stays in the plugin's own files: a session's edits in
`<app dir>/sessions/<session id>/overrides.toml`, the project and AoE defaults in
`<app dir>/tags.toml`. The global settings are read and never rewritten, so two sessions can carry
different tags at the same time with no rule and no config edit between them. The same edits from a
shell:

```sh
aoe-llm-tags tag --set team:platform        # this session only (AOE_SESSION_ID)
aoe-llm-tags tag --session <id> --unset cost --token '{"hand":true}'
aoe-llm-tags tag --reset                    # back to the settings
aoe-llm-tags tag --git                      # the standard git entries, as a sample
aoe-llm-tags tag --level project --set team:core   # this project's default (project path inferred)
aoe-llm-tags tag --level aoe --unset cost          # hide a tag for every session
aoe-llm-tags references                     # every {reference}, with this session's value
```

What travels is a **JSON object, not a string**: `tags` is a set of `key:value` (and `key:=json`)
entries, merged with the session's own fields, so a consumer reads `tags["team"]` instead of
splitting a comma list, and a new dimension never changes the shape it already parses. A value may
name a reference — `git.branch:{git.branch}` puts the branch in the tag set itself — and the git
facts (`{git.commit}`, `{git.tag}`, `{git.dirty}`, …) are read lazily from the session's checkout
only when a template or a tag names one.
`format = "csv"` and `template` remain for a consumer that wants the flat list or one field.

A repository can declare its own tags in a JSON file: with `tags_file = ".aoe-tags.json"`, every
top-level pair in `<project>/.aoe-tags.json` becomes a tag, with JSON types preserved
(`{"cost": 1.5}` → `"cost": 1.5`). The file's keys win over a same-key `tags` entry, and a
per-session edit still wins over both, and a project panel entry sits between the checkout and the
session. From most to least specific the order is **session edit → project → `tags_file` → AoE →
`tags` → rule hit**. The inherited tags spend the five-tag budget in that order,
while a session's own edits are exempt and added on top — a tag typed for one session always
travels. A rule adds a key rather than rewriting one that is already set. Unlike the other
settings this file lives with the code, so a checkout carries its own attribution; a missing or
malformed file is a `doctor` warning, not a failed launch. Since it is repository content it is
read defensively: the path may not resolve outside the project (a symlink counts), only a regular
file is read, the 64 KiB cap applies to the read itself, every string has its control characters
dropped and its whitespace trimmed, and a string value is additionally trimmed to 256 characters
while a list or an object keeps its shape.

`setup --apply` no longer edits any user config: a value rides the launch environment, or the
hook builds this session's own copy of the harness's config under the plugin's app dir and exports
the variable that points the harness at it. That copy is one file — the harness's config, because
that is where the tag lives — and every other entry of the user's config dir is linked, not
duplicated; a launch with nothing to deliver creates nothing at all. Verified end to end on this host for `pi` (header
**and** a `samplingParams` body field), `opencode` (header **and** a query parameter), `claude`
(env-only header) and `codex` (header through a reference in its own config) — evidence in
`docs/harness-evidence/`, regenerate with `tools/probe-harness.sh <agent>` and, for a real aoe
session per harness, `tools/probe-session.sh <agent>`. 310 tests green.

What each harness gets — tag, activity skill, reminder, per-request update — on a host launch and
inside a container is one table in `docs/carrier-matrix.md`, with a drift test behind it.

A **sandboxed** launch is told apart by the hook line, not by aoe: `before_start` carries
`--substrate sandbox` and exports the tag variables only, because the container cannot see the app
dir this plugin writes its per-session copy into — exporting that copy's path there left `pi` with
no models at all. Measured, not argued: `tools/probe-sandbox.sh` (evidence
`docs/harness-evidence/sandbox-pi.json`). One cell stays open — a harness that needs the literal in
its own file (`vibe`) sends no tag in a sandbox until aoe mounts that directory; the hook says so
on stderr.

The consumer read-back leg is deferred, not missing: see `plan/roadmap.md`. See `plan/README.md`.

## Install

```sh
aoe plugin install ./            # from a checkout: the host runs tools/build-worker.sh (needs cargo)
aoe plugin list                  # expect: dev.oozut.llm-tags 0.1.0 local enabled
aoe plugin info dev.oozut.llm-tags   # expect: caps: runtime.worker (granted)
```

The plugin lands in `<app_dir>/plugins/dev.oozut.llm-tags/` (on this host
`/var/lib/aoe/.config/agent-of-empires/plugins/dev.oozut.llm-tags/`), which is the path the hook
lines reference and which survives upgrades.

This plugin can also teach a harness to keep its own activity current: a **skill** that tells the
agent to run `aoe-llm-tags activity <word>` as the work moves between phases, and the same one-line
reminder in the harness's always-loaded instructions, because a skill the model never opens changes
nothing. For **pi** there is a request hook too, so an activity change reaches the next request
rather than the next launch; every other harness is launch-scope for the value itself.

Where the skill and the reminder go depends on the harness, and the disclosure says which it is:

| Scope | Harnesses | Where |
|---|---|---|
| the session's private config copy | `pi`, `vibe` | nothing of yours is touched |
| your own harness config | `claude`, `codex`, `opencode`, `hermes`, `gemini`, `droid`, `qwen`, `kiro` | a skill directory of its own, and a block between two marker lines in the file the harness reads every conversation — your own text in that file stays byte for byte |

`claude`, `codex`, `opencode`, `hermes`, `gemini`, `droid` and `qwen` have no per-session config
copy this plugin can build, so "this session's activity" can only reach them through your own
configuration; that is a different, wider grant, and the disclosure for those harnesses says so
before anything is written. `cline` and `openclaw` have no destination at all and are refused by
name: launch-time tags and nothing else.

Both are off by default. Listing a harness in `harness_extension` only asks for the disclosure;
nothing is written until you approve that exact set of files, by digest — see `docs/install.md` §6.

Setting it up on a session is three steps: `setup --print` shows the two `[host_hooks]` lines and
the mapping line per target, `doctor` says how each installed harness can be delivered
(`wired` / `overlay` / `unsupported`), and `hook` is what aoe runs per launch — it mints the
values, builds this session's config copy, and exports the variable that points the harness at
it. Nothing in `~/.pi`, `~/.codex`, `~/.claude` or `~/.config/opencode` is modified.

Build and test from a checkout (the host's own build step uses the same two commands):

```sh
cargo test --locked                  # 310 tests, no network (needs `git` for tests/git_real.rs)
cargo build --release --locked       # target/release/{aoe-llm-tags,aoe-llm-tags-worker}
```

Requirements: Rust ≥ 1.89. Local builds should set `CARGO_TARGET_DIR` outside the worktree —
`.aoe-build/` is reserved for the host's build step and its presence makes `aoe plugin install`
refuse the tree.

Two ways the binaries arrive, and the difference matters when debugging:

- **Source checkout** (this host): `[runtime] kind = "command"` + `[[runtime.build]]`, so the host
  compiles them and then launches `bin/aoe-llm-tags-worker` as the plugin worker.
- **Release tarball**: `aoe plugin install gh:<owner>/<repo>` downloads
  `aoe-llm-tags-${os}-${arch}.tar.gz` from the latest release and unpacks it — no build step. A
  *local* checkout cannot use this form (the host's fetcher only downloads from GitHub), which is
  why both are declared in `plan/parameters.md` §11 rather than picked at random.

## The problem

AoE is not in the LLM request path. The agent process talks to the provider (or to a gateway)
directly, and the plugin host has no HTTP hook, so a plugin cannot add a header to a request
itself:

- `src/plugin/host_api.rs` (`dispatch`) exposes events, session metadata, `sessions.list`,
  `config.get`, UI state and plugin storage only.
- `src/plugin/session_api.rs` builds `StructuredSessionSpec` with `extra_env: Vec::new()`,
  so a plugin cannot inject environment into a session either.
- `[host_hooks]` is global/profile config, never repo config
  (`docs/guides/configuration.md`), and the plugin has no config-write RPC.

## The decision

No tagging proxy, and no assumption about who reads the header. Tags ride on each harness's own
capability to add request headers, with the per-session value coming from the environment at
launch:

1. the harness has a one-time static mapping from a header name to an environment variable
   (or reads a header variable directly — for a harness that *fails* a request over a variable it
   cannot resolve, the printed value is a command that falls back to an empty payload, so the line
   survives the plugin being uninstalled);
2. `[host_hooks]` in **both** keys mints those variables per launch, with `AOE_SESSION_ID` in
   scope;
3. the plugin resolves which values belong to the session, so users edit templates and rules in
   the plugin's settings instead of hand-editing harness config.

The deliverable is the header set itself: which system consumes it is the operator's choice, and
the plugin must work the same whichever one it is. The plugin never sees a prompt.

A tag says what a session *is* (`feature:billing`, `team:core`). The reserved
`activity` key says what it is *doing* — `code`, `test`, `review`, whatever this session's
**family** defines. A family is a named set of activities, each with a label, a lucide icon, a
tone, and a sentence saying when to move into it ("when starting to implement the feature, move
the activity to coding"); the built-in one is the nine phases `explore`, `plan`, `code`, `test`,
`debug`, `review`, `docs`, `ops`, `idle`. A family is closed, so the badge and the panel always
show the same icon and tone, with the word ("testing") as the fallback where an icon cannot
render — but which words exist is yours to choose, per project: define a family in the plugin's
settings or in the repository's own `.aoe/activity.toml`, and pick it with the per-project
`activity_family` setting. A session pins its family at launch, so editing one applies to the next
session and never relabels a running one.

Because an activity changes while a session runs, a launch-time value is not enough: a harness
that loads an extension from its config can set the header on *each* request, which is why the
plugin ships one, per harness, behind an approval that names the exact file (`docs/install.md` §6).

Whichever side sets it, the edit is one command — `aoe-llm-tags activity test`, or `--clear`,
or `--show` to read the current one, the family it belongs to and the words that family allows —
and the panel's Session tab offers every value as a row, so the session view and the terminal UI
can change it without a page. Last writer wins: a person may override what the agent declared, and the
agent may move on when the task does.

The `when` sentences are guidance today: nothing moves the activity automatically, because a
guess about the work would be a claim sent downstream. They are stored in the shape a future
release can act on, so a machine-readable trigger can join them without changing the file.

The agent side is a shipped skill (`aoe-activity`) plus the same one-line reminder in the harness's
always-loaded instructions: a skill is advertised and loaded on demand, which is not enough on its
own. Both are part of the set an approval covers, and the skill is also added to AoE's own skill
list — visible on the Skills page, editable there, and removed with the approval.

## Docs

Design (normative, in this order):

| File | Contents |
|---|---|
| `docs/header-contract.md` | What is promised and to whom: the header set, template fields, delivery paths, consumer classes, and what the plugin deliberately leaves open |
| `docs/implementation.md` | Where each piece lives: exact paths, hook lines, flags, CLI surface, fixture, aoe anchors |
| `docs/harness-adapters.md` | Per-harness header mechanism, discovery paths, adapter data format, research status |
| `docs/reference-deployment.md` | One concrete stack (aoe → pi → proxy → gateway) as a worked example. Non-normative: no decision may depend on it |

Plan (what ships when, and with which values):

| File | Contents |
|---|---|
| `plan/README.md` | Plan index, priority legend (P0/P1/P2), P0-only subset per milestone, status |
| `plan/milestones.md` | The ladder: constraints, M0–M4, acceptance criteria, open decisions |
| `plan/m0.md` … `plan/m6.md` | One file per milestone: preconditions, priority-tagged tasks, parameters introduced, evidence |
| `plan/parameters.md` | Every parameter in one place: name, type, default, allowed values, scope, milestone, where it is read |
| `plan/roadmap.md` | Work deliberately deferred, with what has to be decided before it comes back — notably consumer read-back (`verify`, M3 usage views) |

## Package layout

| Path | Contents |
|---|---|
| `aoe-plugin.toml` | Manifest: id `dev.oozut.llm-tags`, `api_version = 18` (`aoe_version = ">=1.21.0, <2.0.0"`), `capabilities = ["runtime.worker", "session.read"]` (no `net`: the plugin hands the harness a header and never opens a connection itself), one `status` command, `[runtime]` + source-install build step |
| `Cargo.toml` | One library (`llm_tags`) and two binaries; release profile is LTO + stripped |
| `src/main.rs` | `aoe-llm-tags`: the CLI surface — `hook` / `value` / `doctor` / `setup` / `classify` / `tag` / `verify` (stub) |
| `src/worker_main.rs` | `aoe-llm-tags-worker`: the stdio JSON-RPC worker the plugin host launches (answers `status`, pushes the badge, the Session/Project/AoE tab panes and the detail page, and applies the tabs' session and level edits) |
| `src/resolver.rs` | Session facts → template fields → rendered values: rule matching, env-name derivation, the 2 s row query, the one `snapshot()` every verb shares |
| `src/session_tags.rs`, `ui/tags.html` | A session's own edits (`overrides.toml`: add, drop, a literal token) and the detail page that writes the token and edits the stored values as JSON at the chosen level |
| `src/levels.rs` | The two levels above a session, in `<app dir>/tags.toml`: a project's defaults and the AoE-wide ones, each an overlay of entries and hides; the settings are never rewritten |
| `src/activity.rs` | The activity vocabulary as data: a *family* of activities (label, lucide icon, tone, `when` sentence), the built-in `default` of nine phases, the precedence built-in → settings → repository file, and the family a session pins at launch |
| `src/registry.rs` | A launch's read of the host's `projects.json` (global, then the profile's): `activity_family` and `activity_families` are `per_project`, and a launch hook has no host RPC |
| `src/settings.rs` | Where the settings live and how a profile overrides them (arrays replace, objects recurse) |
| `src/private.rs` | What the plugin writes is the user's alone: `0700` directories and `0600` files, because a session copy and a session file can each carry a secret |
| `src/adapters.rs`, `adapters.toml` | Per-harness header mechanism, as data; the file is embedded, so an install without it still works |
| `src/setup.rs`, `src/doctor.rs`, `src/cli.rs` | The printed mapping/lines, the checks, and the clap surface with its exit codes |
| `tools/build-worker.sh` | The source-install build step (finds cargo, builds, installs both binaries into `bin/`) |
| `tools/drive-worker.py` | Plays the host side of the worker's stdio conversation, so the pane and page payloads — and a click's edit — can be checked without a browser |
| `tools/verify-install.sh` | Checks one installed copy against the host it is installed in: the binaries and artefacts, the grant, both hook lines against `setup --print`, a host launch and a sandboxed one, and `doctor`. Prints `PASS`/`FAIL` per line |
| `tools/check-page.mjs` | Runs the page's script against a stub DOM and a stub dashboard (needs `node`), which is how the bridge handshake is checked: a page can pass every static check and still throw the moment it runs |
| `tools/probe-sandbox.sh` | Measures the delivery mechanisms inside the real sandbox image, one container per mechanism plus one real `aoe add --sandbox` session, so "does the tag reach a sandboxed harness?" is answered by a request at a sink and not by reading aoe's source (evidence: `docs/harness-evidence/sandbox-pi.json`) |
| `.github/workflows/ci.yml` | fmt + clippy + tests on every push; the linux/macos × x86_64/aarch64 asset matrix on a `v*` tag |
| `tests/` | CLI, resolver and `doctor` tests behind a sandboxed app dir and a fake `aoe`; no aoe and no network needed |

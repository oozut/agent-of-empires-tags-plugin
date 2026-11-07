# Agent of Empires LLM Tags

An Agent of Empires plugin that makes each agent session attach identifying headers — or query
parameters, or request-body fields — to every LLM request it sends, so any system on the request
path — a gateway, an egress proxy, a log pipeline, a session-control plane — can attribute,
route, or validate that request.

Status: M0 and M1 done, M2 delivering per session. M0's proof (two independent headers reaching a
request, including a sandboxed leg via podman) still holds; M2 changed *what* travels and *where*.

Editing a tag is a **per-session** operation, in the UI and on the command line: the session pane
and the **LLM Tags** page list every tag with where it came from, let you add, replace or drop one,
and let you write a literal token for that session (never read back into the page: the surfaces say
*that* one is set, not what it says) — and every write lands in
`<app dir>/sessions/<session id>/overrides.toml`. The global settings are read by the plugin and
never rewritten, so two sessions can carry different tags at the same time with no rule and no
config edit between them. The same edit from a shell:

```sh
aoe-llm-tags tag --set team:platform        # this session only (AOE_SESSION_ID)
aoe-llm-tags tag --session <id> --unset cost --token '{"hand":true}'
aoe-llm-tags tag --reset                    # back to the settings
aoe-llm-tags tag --git                      # the standard git entries, as a sample
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
per-session edit still wins over both. From most to least specific the order is **session edit →
`tags_file` → `tags` → rule hit**. The inherited tags spend the five-tag budget in that order,
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
session per harness, `tools/probe-session.sh <agent>`. 244 tests green.
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

This plugin can also install an extension into your **pi** sessions, which lets a tag edited
mid-session reach the next request instead of the next launch. It does that only if you enable it
and approve the exact file, byte for byte — see `docs/install.md` §6. Nothing is written by
default, and every harness without that hook keeps to launch-time tags, which `doctor` says out
loud.

Setting it up on a session is three steps: `setup --print` shows the two `[host_hooks]` lines and
the mapping line per target, `doctor` says how each installed harness can be delivered
(`wired` / `overlay` / `unsupported`), and `hook` is what aoe runs per launch — it mints the
values, builds this session's config copy, and exports the variable that points the harness at
it. Nothing in `~/.pi`, `~/.codex`, `~/.claude` or `~/.config/opencode` is modified.

Build and test from a checkout (the host's own build step uses the same two commands):

```sh
cargo test --locked                  # 244 tests, no network (needs `git` for tests/git_real.rs)
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
| `plan/m0.md` … `plan/m4.md` | One file per milestone: preconditions, priority-tagged tasks, parameters introduced, evidence |
| `plan/parameters.md` | Every parameter in one place: name, type, default, allowed values, scope, milestone, where it is read |
| `plan/roadmap.md` | Work deliberately deferred, with what has to be decided before it comes back — notably consumer read-back (`verify`, M3 usage views) |

## Package layout

| Path | Contents |
|---|---|
| `aoe-plugin.toml` | Manifest: id `dev.oozut.llm-tags`, `api_version = 18` (`aoe_version = ">=1.21.0, <2.0.0"`), `capabilities = ["runtime.worker", "session.read"]` (no `net`: the plugin hands the harness a header and never opens a connection itself), one `status` command, `[runtime]` + source-install build step |
| `Cargo.toml` | One library (`llm_tags`) and two binaries; release profile is LTO + stripped |
| `src/main.rs` | `aoe-llm-tags`: the CLI surface — `hook` / `value` / `doctor` / `setup` / `classify` / `tag` / `verify` (stub) |
| `src/worker_main.rs` | `aoe-llm-tags-worker`: the stdio JSON-RPC worker the plugin host launches (answers `status`, pushes the badge, pane and editor page, and applies the pane's per-session edits) |
| `src/resolver.rs` | Session facts → template fields → rendered values: rule matching, env-name derivation, the 2 s row query, the one `snapshot()` every verb shares |
| `src/session_tags.rs`, `ui/tags.html` | A session's own edits (`overrides.toml`: add, drop, a literal token) and the page that writes them; the only writer, and it never touches the settings |
| `src/settings.rs` | Where the settings live and how a profile overrides them (arrays replace, objects recurse) |
| `src/private.rs` | What the plugin writes is the user's alone: `0700` directories and `0600` files, because a session copy and a session file can each carry a secret |
| `src/adapters.rs`, `adapters.toml` | Per-harness header mechanism, as data; the file is embedded, so an install without it still works |
| `src/setup.rs`, `src/doctor.rs`, `src/cli.rs` | The printed mapping/lines, the checks, and the clap surface with its exit codes |
| `tools/build-worker.sh` | The source-install build step (finds cargo, builds, installs both binaries into `bin/`) |
| `tools/drive-worker.py` | Plays the host side of the worker's stdio conversation, so the pane and page payloads — and a click's edit — can be checked without a browser |
| `tools/check-page.mjs` | Runs the page's script against a stub DOM and a stub dashboard (needs `node`), which is how the bridge handshake is checked: a page can pass every static check and still throw the moment it runs |
| `.github/workflows/ci.yml` | fmt + clippy + tests on every push; the linux/macos × x86_64/aarch64 asset matrix on a `v*` tag |
| `tests/` | CLI, resolver and `doctor` tests behind a sandboxed app dir and a fake `aoe`; no aoe and no network needed |

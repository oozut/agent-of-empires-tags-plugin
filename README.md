# Agent of Empires LLM Tags

An Agent of Empires plugin that makes each agent session attach identifying headers — or query
parameters, or request-body fields — to every LLM request it sends, so any system on the request
path — a gateway, an egress proxy, a log pipeline, a session-control plane — can attribute,
route, or validate that request.

Status: M0 done — Rust port landed (M0-T13, CI green), the package installs, `aoe plugin update`
builds and installs both binaries, `hook|value` resolves a session to one `NAME=value` per enabled
header (T02), `setup --print` emits the exact hook and mapping lines (T05), `doctor` names any
broken piece (T11), two independent headers reach a request (T10), the sink proof (incl. a
sandboxed leg via podman, 2026-10-29) and `docs/install.md` are done (T08, T12) — 76 tests green.
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

Setting it up on a session is three steps: `setup --print` shows the two `[host_hooks]` lines and
the harness mapping line, `doctor` checks what is actually wired, and `hook` is what aoe runs per
launch.

Build and test from a checkout (the host's own build step uses the same two commands):

```sh
cargo test --locked                  # 76 tests, no aoe, no network
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
   (or reads a header variable directly);
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
| `aoe-plugin.toml` | Manifest: id `dev.oozut.llm-tags`, `api_version = 18` (`aoe_version = ">=1.21.0, <2.0.0"`), `capabilities = ["runtime.worker", "session.read", "net"]`, one `status` command, `[runtime]` + source-install build step |
| `Cargo.toml` | One library (`llm_tags`) and two binaries; release profile is LTO + stripped |
| `src/main.rs` | `aoe-llm-tags`: the CLI surface — `hook` / `value` / `doctor` / `setup` / `verify` (stub) |
| `src/worker_main.rs` | `aoe-llm-tags-worker`: the stdio JSON-RPC worker the plugin host launches (answers `status`) |
| `src/resolver.rs` | Session facts → template fields → rendered values: rule matching, env-name derivation, the 2 s row query, the one `snapshot()` every verb shares |
| `src/settings.rs` | Where the settings live and how a profile overrides them (arrays replace, objects recurse) |
| `src/adapters.rs`, `adapters.toml` | Per-harness header mechanism, as data; the file is embedded, so an install without it still works |
| `src/setup.rs`, `src/doctor.rs`, `src/cli.rs` | The printed mapping/lines, the checks, and the clap surface with its exit codes |
| `tools/build-worker.sh` | The source-install build step (finds cargo, builds, installs both binaries into `bin/`) |
| `.github/workflows/ci.yml` | fmt + clippy + tests on every push; the linux/macos × x86_64/aarch64 asset matrix on a `v*` tag |
| `tests/` | CLI, resolver and `doctor` tests behind a sandboxed app dir and a fake `aoe`; no aoe and no network needed |

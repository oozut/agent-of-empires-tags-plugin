# Agent of Empires LLM Tags

An Agent of Empires plugin that makes each agent session attach identifying headers to every LLM
request it sends, so any system on the request path — a gateway, an egress proxy, a log pipeline,
a session-control plane — can attribute, route, or validate that request.

Status: M0 in progress — the plugin package installs (manifest, this README, `bin/`); the CLI
lands in M0-T02. See `plan/README.md` for status.

## Install

```sh
aoe plugin install ./          # from a checkout of this repo
aoe plugin list               # expect: dev.oozut.llm-tags
```

The plugin lands in `<app_dir>/plugins/dev.oozut.llm-tags/` (on this host
`/var/lib/aoe/.config/agent-of-empires/plugins/dev.oozut.llm-tags/`), which is the path the hook
lines reference and which survives upgrades. The M0 manifest declares no capabilities: nothing in
M0 runs inside the plugin host, the entrypoint is `bin/aoe-llm-tags` invoked by `[host_hooks]`.

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

## Package layout

| Path | Contents |
|---|---|
| `aoe-plugin.toml` | Manifest: id `dev.oozut.llm-tags`, `api_version = 17` (`aoe_version = ">=1.20.0, <2.0.0"`), capabilities added only from M1 |
| `bin/aoe-llm-tags` | The M0 CLI surface: `hook` / `value` / `doctor` / `setup` / `verify`, `#!/usr/bin/env python3`, mode 0755 |
| `adapters.toml` | Per-harness header mechanism, as data |
| `src/llm_tags/` | Resolver modules once `bin/` passes ~300 lines |
| `worker/main.py` | M1 stdio JSON-RPC worker |
| `tests/` | Resolver and `doctor` tests; no aoe and no network needed |

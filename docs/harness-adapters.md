# Harness adapters

Each harness is described by data, not code, so a user can add or fix an entry without a
plugin release. The plugin only needs to know how to turn the resolved values into what a
harness expects, and where that harness's config lives. Which system eventually reads those
values is not part of an adapter row — see `docs/header-contract.md`.

An adapter row is per **header**, not per harness: a harness that can send three headers needs
three mapping lines, all fed by the same hook. The default header is listed in each row's
`setup` column; additional enabled headers follow the same shape with their own variable.

## Adapter data format

```toml
[[adapter]]
agent = "pi"
label = "pi"
mechanism = "config_headers"          # pi also accepts command_headers; see table
headers = [                          # one entry per enabled header
  { header = "X-AoE-Tags",    env_var = "AOE_LLM_TAGS" },
  { header = "X-AoE-Session", env_var = "AOE_HDR_X_AOE_SESSION", optional = true },
]
config_env = "PI_CODING_AGENT_DIR"  # optional: directory override variable
# Where the mapping is *written* can differ from where the launched process reads it, when a
# launcher regenerates the directory. The first entry is the file a harness reads from; a
# rewritten directory is never the target.
config_paths = ["~/.pi/agent/models.json", "$PI_CODING_AGENT_DIR/models.json"]
config_format = "json"
status = "documented"               # verified | documented | needs_research | unsupported
notes = "header values resolve like apiKey: $NAME, ${NAME}, or !command"
```

| `mechanism` | Meaning | Static setup needed |
|---|---|---|
| `custom_headers_env` | The harness reads one env var holding whole header lines | no |
| `headers_from_env` | The harness config maps a header name to a variable it reads | yes, once |
| `config_headers` | The harness config holds the header value with env interpolation | yes, once |
| `command_headers` | The harness config runs a command per request to produce the value | yes, once |
| `url_param` | Only a base URL override exists; the value travels in path or query | yes, and the consumer must honor it |
| `none` | No usable mechanism found | values cannot be sent |

## Harness table

Ten targets. Only two are installed on this host (`/usr/bin/pi`, `/usr/local/bin/hermes`), so
verification order follows what can actually be tested — a deployment fact, not a ranking.

| Agent | Mechanism | Default header → var | Static setup | Discovery | Status |
|---|---|---|---|---|---|
| `pi` | `config_headers` and `command_headers` | `X-AoE-Tags` → `AOE_LLM_TAGS` | `models.json` → `providers.<id>.headers` | `~/.pi/agent/models.json`, `$PI_CODING_AGENT_DIR` | documented; verify on host (M0 gate); sandbox unverified |
| `hermes` | `config_headers` via provider plugin `extra_headers` | `X-AoE-Tags` → `AOE_LLM_TAGS` | provider plugin/config | `hermes` config dir | needs research |
| `claude` | `custom_headers_env` | whole header lines in one var (`ANTHROPIC_CUSTOM_HEADERS`) | none | `$CLAUDE_CONFIG_DIR`, `~/.claude/settings.json` | documented, verify |
| `codex` | `headers_from_env` | `X-AoE-Tags` → `AOE_LLM_TAGS` | `[model_providers.<id>.env_http_headers]` | `$CODEX_HOME`, `~/.codex/config.toml` | documented, verify |
| `opencode` | `config_headers` | `X-AoE-Tags` → `AOE_LLM_TAGS` | `provider.<id>.options.headers` with `{env:...}` | `$XDG_CONFIG_HOME/opencode/opencode.json` | documented, verify |
| `gemini` | TBD | TBD | TBD | `~/.gemini/settings.json` | needs research |
| `vibe` | TBD | TBD | TBD | `~/.config/vibe` | needs research |
| `kiro` | TBD | TBD | TBD | agent config JSON | needs research |
| `cline` | TBD | TBD | TBD | VS Code extension storage | needs research |
| `openclaw` | TBD | TBD | TBD | TBD | needs research |

Snippets for the documented rows (default header shown; a second enabled header is one more
entry in the same map, using its own `env_var`):

```jsonc
// pi, ~/.pi/agent/models.json — merge into the provider entry the harness actually uses;
// keep the existing models list (see the launcher note below)
{ "providers": { "<provider-id>": {
    "headers": { "X-AoE-Tags": "$AOE_LLM_TAGS" },
    "models": [{ "id": "<model-id>", "name": "<Model name>" }] } } }
```

```jsonc
// pi, command variant: header value comes from a command, resolved once per process
{ "providers": { "<provider-id>": {
    "headers": { "X-AoE-Tags": "!aoe-llm-tags value" },
    "models": [{ "id": "<model-id>", "name": "<Model name>" }] } } }
```

### Launchers that regenerate the harness config

A harness may be started by a wrapper, not by aoe directly. Two rules follow, and both are
adapter facts, not vendor facts:

- **Write to the wrapper's source file**, not to the directory it generates. If a launcher
  recreates `$CONFIG_DIR` on every start (this deployment's `pi-proxy` does), a mapping written
  there is deleted at the next launch, and the environment variable pointing at it is not
  evidence of anything.
- **Keep the shape the launcher expects.** A launcher may carry over only some keys, or only
  providers it considers usable; a mapping it discards is a silent failure. Record the required
  shape in the adapter row's `notes` (here: the provider entry must keep a non-empty `models`
  list, and only the provider the launcher uses is carried over).

The concrete stack, including the wrapper's exact behaviour, is in `docs/reference-deployment.md`.
The plugin must work on a host with no wrapper at all.

```toml
# codex, ~/.codex/config.toml
# Do not point aoe's hook installer at config.toml; aoe owns ~/.codex/hooks.json.
[model_providers.gw.env_http_headers]
"X-AoE-Tags" = "AOE_LLM_TAGS"
```

```jsonc
// opencode, opencode.json
{ "provider": { "gw": { "options": { "headers": { "X-AoE-Tags": "{env:AOE_LLM_TAGS}" } } } } }
```

```sh
# claude: no static config, the variable carries whole header lines
ANTHROPIC_CUSTOM_HEADERS="X-AoE-Tags: aoe,feature:review,session:S-9f3a"
```

## What the agent process actually receives

Relevant to every adapter, verified in the aoe source. The two hook keys share one
implementation: `run_before_start_hooks` and `run_before_session_hooks` both delegate to
`run_env_minting_hooks` (`src/session/config/repo_config/hooks.rs`), and both are handed the
same `lifecycle_env_vars` — `AOE_SESSION_ID`, `AOE_SESSION_TITLE`, `AOE_PROJECT_PATH`,
`AOE_PROFILE`, `AOE_TOOL`, `AOE_GROUP_PATH`, plus `AOE_SESSION_BRANCH` (worktrees) and
`AOE_REPO_SLUG` (parseable origin). So one command, installed once per key, mints the same
values in either substrate; a hook installed in one key only tags one substrate and silently
skips the other.

Where the minted value ends up:

- **Host (`before_session`)** — appended to the agent process environment on every launch
  (`host_spawn_environment` → `overlay_env`, `src/acp/supervisor/launch.rs`; terminal sessions
  use the same pairs). It re-mints per launch, including a view switch that respawns the
  agent.
- **Sandbox (`before_start`)** — becomes inherited container environment variables
  (`mint_before_start_env`, `src/session/instance/container.rs`). It runs on container create
  or restart and an already-running container reuses its stored values, so a changed value
  reaches a sandbox only on its next container start. Structured sandboxed sessions use this
  path too and get the full variable set.
- `AOE_SESSION_ID` itself is passed to hooks only, never to the agent process. Terminal-view
  agents additionally get `AOE_INSTANCE_ID` (`src/session/instance/execution.rs`), which equals
  the session id; structured agents get `AOE_AGENT_MODEL`.

**The structured-host gap.** Structured host sessions get a reduced hook environment:
`AOE_SESSION_ID`, `AOE_PROFILE`, `AOE_TOOL`, `AOE_PROJECT_PATH` only. `AOE_GROUP_PATH`,
`AOE_SESSION_TITLE`, `AOE_SESSION_BRANCH` and `AOE_REPO_SLUG` are absent there, so the hook
must treat a missing group as "no group rule matches" and never abort the launch. Sandboxed
structured sessions do receive the full set.

**Sandboxed statics.** A header variable only helps if the harness's static mapping is inside
the container too. AoE stages each sandbox from a declared projection of the agent's config
root (`docs/guides/sandbox.md`, `src/session/config/container_config.rs`): for pi that copy
list includes `agent/models.json`, and the root comes from `PI_CODING_AGENT_DIR` or
`[session.agent_config_dir]`, not a hardcoded `~/.pi`. When a launcher regenerates that
directory at start, the file to seed is the launcher's source instead (see "Launchers that
regenerate the harness config" above). Two consequences either way. First, the mapping must be
in place before the container **starts**: aoe re-seeds the store at come-up and on re-attach
(`refresh_agent_configs_for_instance` + `sync_agent_config`), but a *running* container keeps the
copy it started with, so an edit lands on the next start, never mid-flight. Second, an adapter that
cannot name the file aoe will seed is `unsupported` for sandboxes even when it is `verified` on the
host.

**Where the values come from, and when.** Two independent choices, and only the first one is
worth optimising:

- **Facts.** The session manager is authoritative: `aoe session show "$AOE_SESSION_ID"
  -p "$AOE_PROFILE" --json` returns `id`, `title`, `path`, `group`, `tool`, `profile`,
  `state`, `status`, `command` (≈130 ms on this host), and `aoe list --json` adds `worktree`
  and `workspace_repos`. That is a superset of the hook environment and the only way a
  structured host session can match on group or title. The hook environment stays as the
  fallback, with a 2 s timeout, so a stopped daemon, an unknown profile or a missing `aoe`
  binary degrades instead of blocking a launch. Rules then read row fields, so adding a
  `title_regex` or `worktree_branch` kind later costs no new environment variable and no new
  user setup.
- **Timing.** A header value is fixed for the life of the agent process either way: pi caches
  `!command` results for the process lifetime (`dist/core/resolve-config-value.js`), and a
  `$NAME` interpolation reads the environment it was launched with. So a command header buys
  no liveness over a minted variable, only a per-launch process spawn and a 10 s timeout —
  and it cannot reach the session store from inside a sandbox. Treat both as launch-time.

The portable mechanism is therefore the hook minting the variables in both keys, with the
harness config holding exactly one indirection per header: one header name to one variable.
Renaming a header or changing a value format is a plugin-side change, never a user-side config
edit.

## Auto-discovery

The requested behaviour: the user picks an agent, the plugin finds its config and offers
the exact change.

1. **Identify agents.** From `sessions.list` `tool` values, from aoe's agent registry names
   (`claude`, `codex`, `opencode`, `gemini`, `vibe`, `pi`, `omp`, `kimi`, `hermes`, `kiro`,
   plus custom), and from the user's selection in plugin settings.
2. **Resolve the config path** in this order: plugin setting override, harness environment
   variable (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `PI_CODING_AGENT_DIR`, `XDG_CONFIG_HOME`),
   aoe's `agent_config_dir` mapping, then the adapter default.
3. **Classify per substrate.** `wired` (every enabled header's mapping is present),
   `detected-not-wired` (config exists, mapping absent), `unknown` (adapter row is not
   verified), or `unsupported`. Host and sandbox are separate verdicts: a host-wired harness
   can still be unwired in a sandbox whose store was seeded before the mapping existed, and
   fixing that needs the container recreated, so report it as `sandbox-stale` rather than
   `wired`.
4. **Propose, never apply silently.** Show the patch as a diff, apply only from an explicit
   action or `aoe-llm-tags setup --yes`, write a backup, refuse symlinks and non-regular
   files, and write an identifiable marker so `undo` is exact. aoe itself refuses linked
   Codex config files, so mirror that stance.
5. **Cache** the result with the file hash and re-check on settings change, not on every
   pane poll. A config edited outside the plugin invalidates the cached state.
6. **Never write a config the user has not seen.** The worker runs unsandboxed, so nothing
   enforces this; treat it as the plugin's own contract.

Discovery is also the natural place for a capability probe: run the harness once against a
local HTTP sink with the candidate env or config and see whether the header arrives. That
turns `documented` rows into `verified` rows per user and per harness version. Keep it
opt-in and out of the default refresh path.

## Research checklist per harness

For each row still marked `needs_research`:

1. Confirm the binary name and whether aoe only supports it as a custom agent.
2. Find the effective config path, including the directory override variable.
3. Find the documented mechanism for request headers, query params, or a per-request hook.
4. Record the exact snippet and whether it merges with an existing catalog or replaces it.
5. Test that an unknown header survives every hop between the agent and the first system that
   records requests: send one request, then look for the value there. Never assume the hop that
   prices or audits is the hop that logs.
6. Record the harness and aoe versions tested in `verified_with`, or leave the row
   `needs_research`.

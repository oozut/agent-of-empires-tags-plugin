# Implementation map

What to build, in what order, in which file, and which aoe surface each piece leans on. Written
before any code exists so each step can be picked up cold, one at a time.

Read with `docs/header-contract.md` first — that file says what is promised and to whom, and it
is normative over this one. Then `plan/milestones.md` (what ships when, with the per-milestone
task files and `plan/parameters.md` for values) and
`docs/harness-adapters.md` (per-harness data). This file is the *where* and the *how*: exact
paths, exact command lines, exact flags.

Anything below that names one gateway, one proxy, or a path on one host is an example of the
reference stack, whose facts live in `docs/reference-deployment.md`. The mechanism (a header set,
minted at launch, delivered by the harness's own header capability) is deliberately independent
of it.

## Fixed decisions

| Decision | Value | Why it is fixed now |
|---|---|---|
| Plugin id | `dev.oozut.llm-tags` | Namespaces config (`[plugins."<id>".settings]`), commands (`plugin.<id>.<command>`), worker logs. Changing it later orphans user settings, so pick before the first install. |
| Install path | `<app_dir>/plugins/dev.oozut.llm-tags/` (here: `/var/lib/aoe/.config/agent-of-empires/plugins/dev.oozut.llm-tags/`) | The hook line hardcodes it. The path survives upgrades; the contents are replaced. |
| CLI entrypoint | `bin/aoe-llm-tags` (mode 0755, static Rust binary) | Must be runnable by path from a hook, with no interpreter, venv or `PATH` in the hook shell. |
| Wire unit | A **header set**: `{header, template, env_var, enabled}` list in settings. Shipped default: `X-AoE-Tags` with template `aoe,feature:{feature},session:{session}`, `env_var = "AOE_LLM_TAGS"` | One header is enough for cost attribution; discrete and signed headers exist for consumers that validate or route. The set is data, so adding one costs a settings row, not a code change. |
| Env var naming | Default `AOE_HDR_` + upper snake of the header name (`X-AoE-Session` → `AOE_HDR_X_AOE_SESSION`); the shipped default header pins `AOE_LLM_TAGS` | The hook prints one line per enabled header, so the name must be derivable by both the CLI and `setup`. Pinning keeps the common case short. |
| Tag model | Accumulate **all** matching rules, cap 5 values, grammar `[a-z0-9._:-]`, `session_tagging = short_id` → `S-<first 4 chars of the session id>`, never the title | Recorded decision. `S-cf11` is derivable from `sessions.list` by anyone auditing a request log. |
| Resolution | Session row from the session manager first, hook environment as fallback | Superset of the hook env, and the only source of `group`/`title` for structured host sessions. |
| Timing | Per launch (host) / per container start (sandbox). No live mode | Harnesses cache command-valued headers for the process lifetime, so a command buys nothing. |
| Downstream system | None assumed, none required | The header is the deliverable. A deployment picks the consumer; see `header-contract.md` for the consumer classes and `reference-deployment.md` for this host's. |

## Repo layout

```
Cargo.toml                      step 1   one library crate (llm_tags) + two bins
aoe-plugin.toml                 step 3   manifest: capabilities, [[commands]], [runtime] (+ build step)
README.md                       step 0   what it is + pointing at these docs
docs/install.md                 step 6   the one page users get: paste these lines, verify
src/lib.rs + src/main.rs        step 1   the M0 surface: hook|value|doctor|setup|verify
src/{env,settings,resolver}.rs  step 1   facts → fields → rendering, no aoe imports
src/{adapters,setup,doctor}.rs  step 4-5 adapter data, the mapping lines, the checks
src/cli.rs                      step 1   clap surface + exit codes; the hook's catch_unwind lives here
src/worker.rs + src/worker_main.rs  step 7 JSON-RPC worker (stdio, newline JSON)
adapters.toml                   step 4   adapter rows (pi first), data not code; include_str!'d
tools/build-worker.sh           step 10  source-install build: cargo → bin/ (release assets skip it)
.github/workflows/ci.yml        step 10  fmt + clippy + test, then the per-os/arch asset matrix
tools/header-sink.py            step 6   M0 proof fixture, not shipped behaviour
tests/common/mod.rs             step 1   sandbox app dir, fake aoe, explicit env
tests/{cli,doctor}.rs           step 1   the verbs and the checks, no aoe and no network
```

The official template (`cookiecutter gh:agent-of-empires/plugin-template`, Rust flavour) was read
for layout, not vendored; the crate mirrors aoe core's own idioms (clap derive, `serde` + `toml`
typed settings, `thiserror`, exit codes as `ExitCode`) instead of importing the unpublished
`aoe-plugin-api`.

## aoe surface this depends on

Verified in `/var/lib/aoe/agent-of-empires-worktrees/koreans`; treat these as the contract.

| Fact | Where | Consequence for us |
|---|---|---|
| Hooks run as `$SHELL -c <string>`, cwd = project path, stdin `/dev/null`, new session (`setsid`), with `GIT_TERMINAL_PROMPT=0`, `GIT_ASKPASS=true`, `SSH_ASKPASS=true` | `src/session/config/repo_config/hooks.rs` `build_hook_command` | One string per key. No array form, no `exec`. Quote every variable. |
| stdout is parsed as env: `KEY=VALUE` per line, first `=` splits, key trimmed, invalid env keys dropped, later duplicate wins, **value not trimmed**, lines without `=` are ignored | `parse_env_kv_lines` | Print exactly `NAME=value`, one line per enabled header; never a trailing space; diagnostics go to stderr. |
| Non-zero exit aborts the session start; stderr is included in the error | `run_env_minting_hooks` | Exit 0 always. A broken store must not be able to block a launch. |
| No default hook timeout (`current_hook_timeout()` is `None` outside recovery) | `run_hook_output` | Our own 2 s bound is the only protection. |
| Both keys get the same lifecycle env: `AOE_SESSION_ID`, `AOE_SESSION_TITLE`, `AOE_PROJECT_PATH`, `AOE_PROFILE`, `AOE_TOOL`, `AOE_GROUP_PATH`, plus `AOE_SESSION_BRANCH`/`AOE_REPO_SLUG` when applicable | `lifecycle_env_vars` | One command serves both. No substrate flag needed for the tag value. |
| Structured **host** sessions get only id/profile/tool/project | `before_session_env` in `src/acp/supervisor/launch.rs` | `handle` has no `AOE_GROUP_PATH`/title there — resolve them from the row. |
| `[host_hooks]` is global/profile only; profile arrays **replace** global arrays | `resolve_before_{start,session}_hooks`, `merge_configs` | A profile that sets its own `before_session` silently drops the global line. `doctor` must read the effective config, not just the global one. |
| Plugin settings are an opaque `toml::Table` under `[plugins."<id>".settings]`, kept even while the plugin is disabled | `src/session/config/mod.rs` (`settings: toml::Table`) | M0 can hand-write settings before any UI exists; they will not be pruned. |
| `aoe session show <id> -p <profile> --json` → `id,title,path,group,tool,command,status,state,profile` (≈130 ms). `aoe list --json` adds `worktree`, `workspace_repos` | `/usr/local/bin/aoe` | The manager call. No daemon needed; it reads the store. |
| Install extracts into `<app_dir>/plugins/<id>/` after `remove_dir_all`, and `.aoe-build/` is excluded from the tree hash | `src/plugin/install.rs`, `docs/plugin-api.md` | Hook path is stable across upgrades; a brief window has no files. Build artifacts go to `.aoe-build/` only. |
| pi resolves header values like `apiKey`: `$NAME`, `${NAME}`, `!command`; command results are cached for the process lifetime | `/usr/lib/node_modules/@earendil-works/pi-coding-agent/dist/core/resolve-config-value.js` | Map the header to `$AOE_LLM_TAGS` (or `$AOE_HDR_…`). The `!` form is a fallback, not a live mode. |
| A harness may be started by a wrapper/launcher that regenerates its config and forces an upstream; the wrapper's source file, not the env-var directory, is then the editable one | this deployment: `/usr/local/bin/pi-proxy` `gen_overlay` — details in `reference-deployment.md` | Resolve "the file the harness reads" per adapter, and treat a regenerated directory as un-writable. Never infer the source path from an env var being set. |
| Sandbox: pi's `agent/models.json` is on the copy list, seeded into `sandbox-v2/<id>` at container creation and never re-synced | `src/session/config/container_config.rs` (`tool_name: "pi"`) | The mapping must exist before the first container start. |

## Step 1 — the CLI core

One library (`llm_tags`), two thin entrypoints (`main.rs` → `cli::run`, `worker_main.rs` →
`worker::run`), one resolver (`src/settings.rs` for where the settings live, `src/resolver.rs` for
facts → fields → rendering, `src/cli.rs` for the verbs), no daemon, no aoe crate imports. The
`[[bin]]`s are the whole interface, so the installed plugin dir is self-contained and the tests
link the library directly.

| Subcommand | Reads | Writes | Used by |
|---|---|---|---|
| `hook` | settings + session row | one `env_var=value` line per enabled header | both `[host_hooks]` keys |
| `value [header]` | same | one header's value alone (default: the shipped tag header) | harnesses that take a command as the header value (`!aoe-llm-tags value X-AoE-Session`) |
| `doctor` | both hook keys, plugin path, every enabled header's mapping, `aoe` binary | human report, exit 1 on any failure | the user, after installing |
| `setup --print` | adapter row + install path + enabled headers | the exact hook lines + one mapping line per header | the user, before pasting |
| `verify` | the configured consumer's request/usage records | pass/fail | roadmap — consumer read-back is deferred (`plan/roadmap.md` §1) |

Flags for `hook`/`value` (all optional; every one is a *fallback*, the row wins):

```
--session <id>   --profile <p>   --tool <t>   --project <path>
--group <path>   --title <text>  --adapter <name>   --json
```

Resolution pipeline, in order, stopping at the first success for each field:

1. `aoe session show "$AOE_SESSION_ID" -p "$AOE_PROFILE" --json`, with `AOE_BIN` → `command -v aoe`
   → `/usr/local/bin/aoe`, hard 2 s timeout. `resolver::aoe_json` spawns the child with piped
   stdio, drains both pipes on scoped threads (a child that fills a pipe would otherwise block
   past the deadline), then polls `try_wait` against the deadline and kills + reaps on timeout.
2. The flags (which the hook filled from `AOE_*`).
3. Defaults: empty group, empty title, tool `unknown`.

Then: read settings → evaluate rules → render every enabled header's template → print one
`NAME=value` line per header. Any exception is a `stderr` line and exit 0.

Settings read: `[plugins."dev.oozut.llm-tags".settings]` from the effective config. That is the
global `<app_dir>/config.toml` with the profile's sparse override file
(`<app_dir>/profiles/<profile>/config.toml`, top level = the override tree) merged onto it the way
aoe merges: object keys recurse, **arrays replace** (`merge_configs_generic`). A missing section
means no rules, which is not an error; an explicit `headers = []` means no headers at all.
`src/settings.rs` reproduces that merge, so the hook and the settings UI agree about a
value; a file that cannot be parsed is a warning and is ignored (profile → global → defaults).

Rule evaluation (M1 grows this; M0 needs only `project` and `manual`). Rendering is per header,
so the default tag format is just the default template. The pipeline in `src/resolver.rs`, in
order: `SessionFacts` (row wins, hook env is the fallback) → `build_fields` → `matched_tags`
(every matching rule contributes, in list order, deduped, capped at 5) → `render_template` per
enabled header (`clamp_bytes` at 800) → `hook_lines` (`NAME=value`, NAME is the env var) or
`render_headers` (header → value, for `doctor` and the worker).

Default header set shipped in code, overridable by settings:

```rust
pub fn default_headers() -> Vec<Header> {
    vec![
        Header { header: "X-AoE-Tags".into(), env_var: "AOE_LLM_TAGS".into(), enabled: true,
                 template: "aoe,feature:{feature},session:{session}".into() },
        Header { header: "X-AoE-Session".into(), env_var: "AOE_HDR_X_AOE_SESSION".into(),
                 enabled: false, template: "{session_id}".into() },
    ]
}
```

One resolver, every verb: `hook`, `value`, `doctor` and the worker's `status` all call
`resolver::snapshot`, so a value cannot differ between the launch path and the UI.

Tests that need no aoe at all (`tests/cli.rs`, `tests/doctor.rs`, plus the unit tests in each
module): accumulator order/dedup/cap, grammar rejection, `short_id` stability, template field
coverage, env-name derivation, disabled headers emitting nothing, missing settings, malformed TOML
(→ exit 0, env fallback), and the hook exiting 0 on every failure path.

## Step 2 — the two hook lines

This is the whole user-facing setup. Same command, both keys, one string each (`<app_dir>` is the
aoe app directory — this host's value is in `reference-deployment.md`):

```toml
[host_hooks]
before_session = ['AOE_BIN=/usr/local/bin/aoe <app_dir>/plugins/dev.oozut.llm-tags/bin/aoe-llm-tags hook --session "$AOE_SESSION_ID" --profile "$AOE_PROFILE" --tool "$AOE_TOOL" --project "$AOE_PROJECT_PATH" --group "$AOE_GROUP_PATH" --title "$AOE_SESSION_TITLE"']
before_start   = ['AOE_BIN=/usr/local/bin/aoe <app_dir>/plugins/dev.oozut.llm-tags/bin/aoe-llm-tags hook --session "$AOE_SESSION_ID" --profile "$AOE_PROFILE" --tool "$AOE_TOOL" --project "$AOE_PROJECT_PATH" --group "$AOE_GROUP_PATH" --title "$AOE_SESSION_TITLE"']
```

The number of enabled headers does **not** change these lines: one command prints all of them.
Only the static harness mapping grows, one line per header.

Why each part:

- `AOE_BIN=/usr/local/bin/aoe` — the hook shell has neither the operator's `PATH` nor aliases.
  Pin the binary that resolves the session row (`setup` resolves it once with `command -v` and
  writes that absolute path); `doctor` prints the value it will use.
- Absolute CLI path — the plugin dir is not on `PATH`, and the path is stable across upgrades.
- `--session`/`--profile` — the row lookup, and it must be the same profile the daemon uses.
- `--group`/`--title` — fallbacks only; empty for structured host sessions, which is what the
  row is for.
- Single quotes around the whole command, double quotes around each expansion: aoe puts the
  string through `$SHELL -c` verbatim.

Placement and merge semantics:

- Global file: `<app_dir>/config.toml` (this host's `[host_hooks]` is currently empty). Applies
  to every profile that does not override the key.
- Per profile: `<app_dir>/profiles/<name>/config.toml`. Arrays **replace**, so a profile with its
  own `before_session` drops the global one for that profile. `setup --print` should emit both
  placements and say which one it recommends (global), and `doctor` must inspect the effective
  config for the profile it was asked about.
- Sandboxed sessions need nothing extra: `before_start` runs on the **host**, so the same path,
  the same `aoe` call and the same settings apply. Only the destination differs (container env
  instead of agent env).

`setup --print` output is those two lines plus one mapping line per enabled header for the
selected harness (step 4), and nothing else.

## Step 3 — manifest and settings

M0 manifest declares the worker runtime (it is how the binaries get installed) and one command;
no UI and no settings schema yet:

```toml
id = "dev.oozut.llm-tags"
name = "LLM Tags"
version = "0.1.0"
api_version = 17
aoe_version = ">=1.20.0, <2.0.0"
description = "Send identifying headers with every LLM request a session makes, so any system on the path can attribute, route or validate it."
capabilities = ["runtime.worker"]

[[commands]]
id = "status"
title = "LLM Tags: status"
description = "Report the header set this install would send, without starting a session."

[runtime]
kind = "command"                      # were: kind = "release-binary", asset = …
command = ["./bin/aoe-llm-tags-worker"]

[[runtime.build]]
command = ["./tools/build-worker.sh"]
platforms = ["linux", "macos"]
```

The released manifest differs in exactly two lines (`kind = "release-binary"`, `bin =
"aoe-llm-tags-worker"` instead of `command`), see step 10 and `parameters.md` §11.

At M1 add, in one update: `session.read`/`net` to `capabilities`, `[[ui]] row-badge`/`pane`, and `[[settings]]` for `headers`
(`object_list`, `item_id_key = "header"`), `rules`, `session_tagging`, `feature_tag`
(`per_project = true` → `api_version >= 18`, satisfied), `session_signature`.

Until M1 exists, settings are hand-written and survive as opaque TOML — the host keeps that
section even if the key is not declared yet (`settings: toml::Table` in
`src/session/config/mod.rs`):

```toml
[plugins."dev.oozut.llm-tags".settings]
session_tagging = "short_id"
session_signature = "none"          # "hmac" appends .<16 hex> to {session_id}; needs a key
signing_key = ""                    # operator-supplied; the same value at the consumer

# default row, spelled out; omitting the whole [[headers]] block uses the code default
[[plugins."dev.oozut.llm-tags".settings.headers]]
header = "X-AoE-Tags"
env_var = "AOE_LLM_TAGS"
enabled = true
template = "aoe,feature:{feature},session:{session}"

[[plugins."dev.oozut.llm-tags".settings.headers]]
header = "X-AoE-Session"   # opt-in: for consumers that validate sessions, not ones that bill
enabled = false
template = "{session_id}"

[[plugins."dev.oozut.llm-tags".settings.rules]]
tag = "review"
match_kind = "project"
match_value = "agent-of-empires-tags-plugin"
```

## Step 4 — the harness static mapping

One line per enabled header, in whatever config the harness reads. Two things decide where that
is, and neither is gateway-specific:

1. **The file the harness actually reads.** If the harness is started by a wrapper that
generates its config, the wrapper's *source* file is the editable one and the generated
directory is disposable. Never infer it from an environment variable being set — resolve it per
adapter and verify by mtime or by the wrapper's own source path.
2. **Whether the harness keeps the entry.** Some wrappers only carry over a provider they also
found usable (this deployment: an entry without a `models` list is dropped wholesale, taking
`headers` with it — `reference-deployment.md`). The adapter row records the shape the harness
needs, so `setup` writes a mapping the harness cannot silently discard.

For this deployment's pi (`providers.litellm.headers`, and the entry needs at least one model):

```jsonc
// ~/.pi/agent/models.json — merge into the existing provider entry, never replace the file
{ "providers": { "litellm": {
    "headers": { "X-AoE-Tags": "$AOE_LLM_TAGS", "X-AoE-Session": "$AOE_HDR_X_AOE_SESSION" },
    "models": [{ "id": "deepseek-v4-flash", "name": "DeepSeek V4 Flash" }] } } }
```

A disabled header's mapping line is simply absent: `setup` writes only enabled headers, which is
why `enabled = false` costs nothing anywhere else.

- `setup` must handle both "file absent" and "file present": create it (with a model, per
  point 2 above), back up when it exists, preserve every other provider, and keep the existing
  models list. Which files exist here is in `reference-deployment.md`.
- `doctor` reports the provider ids found and whether each enabled header has a mapping, plus the
  model count, because a discarded entry is the silent failure above.
- Command form (`"!aoe-llm-tags value X-AoE-Session"`) is documented for harnesses whose mapping
  cannot be edited, with the caching caveat. Not the default. `doctor` accepts either form as
  "wired", naming which one it found.
- Sandbox: the harness config is copied into the per-session private store at container creation,
  so the mapping must be right before the first start; editing it later does not reach an
  existing container. Whether the harness can run inside a sandbox at all is a deployment fact
  (a wrapper that needs a credential may not) — record it in the adapter row's sandbox verdict
  instead of assuming it.
- Whether every hop between the agent and the consumer keeps an unknown request header, and whether
  the consumer can turn one into a dimension at all: deployment knowledge
  (`reference-deployment.md`), not a plugin input. Since decision #4 was resolved as *deferred*
  (2026-10-29) nothing here blocks a release — a hop that strips the header is the hop's problem to
  fix, and the plugin's obligation ends at the sink proof (`plan/roadmap.md` §1).

## Step 5 — `doctor`

Each row is one check with its own line and exit contribution.

| Check | How | Failure message must say |
|---|---|---|
| Effective config resolved | profile config, then global | which file it read and that no hook line exists |
| `before_session` present | parse effective config | "host sessions will not send headers" |
| `before_start` present | same | "sandboxed sessions will not send headers" |
| Command path exists and is executable | `metadata.permissions().mode() & 0o111` | the path plus "plugin moved, uninstalled, or id changed" |
| `aoe` binary resolvable | `AOE_BIN`, then `PATH` lookup | the value it will use, and how to override |
| Row resolves for a given session | `aoe session show … --json` | "falling back to hook environment: group/title will be empty" |
| Each enabled header mapped | adapter row + config root | which header, which file, and whether the file was created or already had it |
| Value renders | run the same resolver locally | the template that failed and the field it needed |
| Last verification | `last_verified` in plugin settings, if one was ever recorded | `skip` when absent: consumer read-back is roadmap work (`plan/roadmap.md` §1), so an install without one is not degraded |

Exit non-zero if any line fails. Print the absolute paths it used, always — a hook debugging
session with no visible paths is the expensive failure mode.

## Step 6 — proof of arrival

Fixture (not shipped behaviour), `tools/header-sink.py`:

```python
#!/usr/bin/env python3
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

class Sink(BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length") or 0))
        with open(sys.argv[2], "a") as f:
            f.write(json.dumps({"path": self.path, "headers": dict(self.headers)}) + "\n")
        out = json.dumps({"id": "sink", "object": "chat.completion",
                          "choices": [{"index": 0, "finish_reason": "stop",
                                       "message": {"role": "assistant", "content": "ok"}}],
                          "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}})
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(out)))
        self.end_headers()
        self.wfile.write(out.encode())
    def log_message(self, *a): pass

HTTPServer(("127.0.0.1", int(sys.argv[1])), Sink).serve_forever()
```

Procedure — two separate legs, because the mechanism and the record are different claims:

1. **Mechanism leg** (an isolated harness config pointed at the sink, no proxy on that path —
   required whenever the harness launch wrapper forces an upstream):

```sh
python3 tools/header-sink.py 8787 /tmp/tag-sink.jsonl &                 # sink
mkdir -p /tmp/sink-agent && cat > /tmp/sink-agent/models.json <<'JSON'
{ "providers": { "sink": { "baseUrl": "http://127.0.0.1:8787/v1", "api": "openai-completions",
                           "apiKey": "sink", "headers": { "X-AoE-Tags": "$AOE_LLM_TAGS" },
                           "models": [{ "id": "sink", "name": "Sink" }] } } }
JSON
# inside an aoe session (terminal view), with the proxy env vars unset:
env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy \
  PI_CODING_AGENT_DIR=/tmp/sink-agent pi --print --model sink/sink "ping"
grep -o '"X-AoE-Tags": "[^"]*"' /tmp/tag-sink.jsonl                     # aoe,feature:...,session:S-xxxx
```

2. **Consumer leg** (the real path): the same harness through whatever wrapper/launch path the
   session normally uses, then the header in the first hop that records requests. Do not confuse
   the two: leg 1 proves the hook mints and the harness sends; leg 2 proves the far side keeps
   it. For this deployment the wording of both legs is in `reference-deployment.md`.

Cheapest sanity check before either: print the minted variable in a session shell
(`env | grep AOE`) — missing means the hook line is absent or failed, and nothing downstream can
work.

Repeat leg 1 for a structured-view session (same session, view switch re-mints). A sandbox leg
may be impossible on a given host for reasons unrelated to the plugin (runtime access, a harness
wrapper needing a credential); record that as a precondition failure and prove only the minting
half (`exec env` in the container showing the variables), leaving the adapter row `documented`.

## Step 7 — M1 wiring

| Piece | Host API | Notes |
|---|---|---|
| Worker | `runtime.worker`, stdio JSON-RPC | `src/worker.rs` + `src/worker_main.rs`, dispatch on the trailing segment of `plugin.dev.oozut.llm-tags.<cmd>` (the M0 build already answers `status`) |
| Settings | manifest `[[settings]]` | replaces the hand-written block from step 3, same keys (`headers` as `object_list`, `item_id_key = "header"`) |
| Row badge | `ui.state.set`, slot `row-badge`, per session | tone `warn` when unwired |
| Pane | `ui.state.set`, slot `pane` | resolved header values, adapter, wired state, copyable command |
| `status` command | `[[commands]] id = "status"` | resolved values for every session via `sessions.list` |
| Read sessions | `session.read` | also enables `session.lifecycle.changed` (`api_version >= 19`) for cache invalidation |

Reuse step 1's resolver inside the worker; the worker must reach the **same** answer as the
hook, and that is the one thing worth a shared unit test in both directions.

## Step 8 — M2 discovery

Per adapter row: resolve config path (setting → harness env var → aoe `agent_config_dir` →
default), classify per substrate (`wired`, `detected-not-wired`, `sandbox-stale`, `unknown`,
`unsupported`), cache with the file hash, show a diff, apply only on `--yes`.

Edit rules, all mandatory: backup beside the file, refuse symlinks and non-regular files, write
with the marker `"// aoe-llm-tags: managed"` so `undo` is exact, atomic replace (temp file in
the same directory + rename), re-read and re-parse after writing, and never write a config the
user has not seen in the diff.

## Step 9 — M3 page (roadmap-gated)

> Do not start this before a consumer is named again: every number on the page comes from the
> deferred read-back (`plan/roadmap.md` §1). The section describes the shape, not current work.


`[[ui]] slot = "page"` with a single self-contained HTML file (inline CSS/JS, `data:` images),
state pushed by the worker with `ui.state.set` (slot `page`, ≤64 KiB): usage per tag keyed by
whatever the configured consumer system returns, sessions it never saw, and the verify action.
A consumer that cannot answer a dimension degrades to wiring-only information with a message
naming what it cannot answer (`header-contract.md`, consumer table). Needs the same `net` grant
as M2.

## Step 10 — distribution (M0-T13)

Three ways the same two binaries can reach `<install>/bin/`; only the first is exercised by a
checkout, and that difference is the thing to keep honest.

| Form | Manifest | What happens on install/update |
|---|---|---|
| Source checkout (this host) | `[runtime] kind = "command"` + `[[runtime.build]]` | the host copies the tree, runs `tools/build-worker.sh` (cargo → `.aoe-build/`, then `install -m 755` both binaries into `bin/`), re-resolves the worker command and spawns it |
| GitHub release | `[runtime] kind = "release-binary"`, `asset = "aoe-llm-tags-${os}-${arch}.tar.gz"`, `bin = "aoe-llm-tags-worker"` | the host downloads the asset, unpacks it into the install dir, spawns `bin`; **no build step runs** |
| Anything else | — | `download_release_binary` requires `PluginSource::Github` and bails for `PluginSource::Local`, so a checkout cannot use the release form |

Consequences that shaped the code:

- **Two binaries, not a subcommand.** `release-binary` launches `bin` with no arguments, so the
  worker cannot be `aoe-llm-tags worker`.
- **The asset is the install layout** (`bin/aoe-llm-tags`, `bin/aoe-llm-tags-worker`, `README.md`,
  `LICENSE`), so unpacking *is* installing and the hook path is the same in both forms.
- **`.aoe-build/` is reserved.** The host refuses a tree that already ships it, so local builds
  use `CARGO_TARGET_DIR` outside the worktree; only the build step creates it.
- **No cargo in the daemon's `PATH`.** `tools/build-worker.sh` searches `CARGO_BIN` → `PATH` →
  `$CARGO_HOME/bin` → `/usr/local/{cargo,lib}/…` and fails with a message naming the toolchain
  requirement rather than a `cargo: not found`.
- **CI gates the port, not just the release.** `.github/workflows/ci.yml` runs
  `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo test --locked` and one
  `hook` smoke run on every push; the tag job builds `x86_64`/`aarch64` for linux (the aarch64 leg
  via `gcc-aarch64-linux-gnu`) and macos and attaches the four tarballs to the release.
  Windows is out of the matrix while the manifest has a single `bin` value (`parameters.md` §11,
  open decision #10).
- **Release-path verification is the artifact set plus one GitHub-sourced install**, never a
  local `aoe plugin update`; an asset-name mistake shows up as the host's "asset not found,
  available: …" error. Record it as such in `plan/parameters.md` §11 rather than claiming the
  release path was run here.

## Upstream aoe changes worth filing

None of these block M0; each one removes a workaround.

| Change | File | Why |
|---|---|---|
| Pass `AOE_GROUP_PATH`/`AOE_SESSION_TITLE` to structured `before_session` | `src/acp/supervisor/launch.rs` `before_session_env` | Removes the one asymmetry between views; the row lookup exists only because of it. |
| Add `sandbox`/substrate to `sessions.list` and to `aoe session show --json` | `src/plugin/session_api.rs`, `src/cli/runtime_read/render.rs` | Discovery needs per-substrate classification and cannot see it today. |
| Expose plugin settings to a read-only CLI call | `src/cli/` | Would let the hook avoid parsing `config.toml` directly. |
| A hook that can emit headers directly (or a documented per-request hook) | `src/session/config/repo_config/hooks.rs` | Would make live per-request tagging possible; today env minting is launch-scoped. |

## Still open

The general ones live in `plan/milestones.md` §Open decisions and `plan/parameters.md` §Open.
These are the two that bite during M0 and are about the build, not the design:

1. ~~Which consumer system on the reference path counts as "recorded"~~ — **resolved 2026-10-29:
   deferred.** The plugin does not read a consumer back, so no vendor's read API is a dependency;
   `verify`, `consumer.*` and M3 wait on `plan/roadmap.md` §1.
2. Whether `setup <agent>` stays print-only in M2 or applies the mapping on `--yes` with a
   backup. The edit rules above are written for the "applies" case, so deciding "print-only" is
   a subtraction, not a redesign.
3. Whether the default template should carry fewer or more fields (`tool:` today looks free, but
   every consumer that keys on the string makes a later change a breaking one). Cheap to decide
   before the first real integration, expensive after.

## Progress

Tracked per milestone, not here: `plan/m0.md` … `plan/m4.md` hold the ordered task lists and
acceptance evidence; `plan/README.md` holds the status table. This file stays a map, so the two
do not drift.

Step numbers above map to milestones like this: steps 1–6 are M0 (inside it, `plan/m0.md` tasks
1–13, step 10 is M0-T13), step 7 is M1, step 8 is M2, step 9 is M3.

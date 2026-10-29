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
| CLI entrypoint | `bin/aoe-llm-tags` (mode 0755, `#!/usr/bin/env python3`) | Must be runnable by path from a hook. `tomllib` is stdlib, so no venv for M0. |
| Wire unit | A **header set**: `{header, template, env_var, enabled}` list in settings. Shipped default: `X-AoE-Tags` with template `aoe,feature:{feature},session:{session}`, `env_var = "AOE_LLM_TAGS"` | One header is enough for cost attribution; discrete and signed headers exist for consumers that validate or route. The set is data, so adding one costs a settings row, not a code change. |
| Env var naming | Default `AOE_HDR_` + upper snake of the header name (`X-AoE-Session` → `AOE_HDR_X_AOE_SESSION`); the shipped default header pins `AOE_LLM_TAGS` | The hook prints one line per enabled header, so the name must be derivable by both the CLI and `setup`. Pinning keeps the common case short. |
| Tag model | Accumulate **all** matching rules, cap 5 values, grammar `[a-z0-9._:-]`, `session_tagging = short_id` → `S-<first 4 chars of the session id>`, never the title | Recorded decision. `S-cf11` is derivable from `sessions.list` by anyone auditing a request log. |
| Resolution | Session row from the session manager first, hook environment as fallback | Superset of the hook env, and the only source of `group`/`title` for structured host sessions. |
| Timing | Per launch (host) / per container start (sandbox). No live mode | Harnesses cache command-valued headers for the process lifetime, so a command buys nothing. |
| Downstream system | None assumed, none required | The header is the deliverable. A deployment picks the consumer; see `header-contract.md` for the consumer classes and `reference-deployment.md` for this host's. |

## Repo layout

```
aoe-plugin.toml                 step 3   manifest (api_version 19); capabilities only from M1
README.md                       step 0   what it is + pointing at these docs
docs/install.md                 step 6   the one page users get: paste these lines, verify
bin/aoe-llm-tags                step 1   the whole M0 surface: hook|value|doctor|setup|verify
adapters.toml                   step 4   adapter rows (pi first), data not code
src/llm_tags/*.py               step 1   split out once bin/ passes ~300 lines
worker/main.py                  step 7   M1 JSON-RPC worker (stdio, newline JSON)
tools/header-sink.py            step 6   M0 proof fixture, not shipped behaviour
tests/test_resolution.py        step 1   rule evaluation + rendering, no aoe needed
tests/test_doctor.py            step 5   check classification against a temp app dir
```

Generate the skeleton with the official template, then strip its example worker:

```sh
cookiecutter gh:agent-of-empires/plugin-template   # pick Python, rename to dev.oozut.llm-tags
```

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

One entrypoint (`bin/aoe-llm-tags`), one resolver (`src/llm_tags/`: `settings.py` for where the
settings live, `resolver.py` for facts → fields → rendering, `cli.py` for the verbs), no daemon, no
aoe imports. The entrypoint only puts `../src` on `sys.path` and calls `llm_tags.cli.main`, so the
installed plugin dir is self-contained and the tests can import the resolver directly.

| Subcommand | Reads | Writes | Used by |
|---|---|---|---|
| `hook` | settings + session row | one `env_var=value` line per enabled header | both `[host_hooks]` keys |
| `value [header]` | same | one header's value alone (default: the shipped tag header) | harnesses that take a command as the header value (`!aoe-llm-tags value X-AoE-Session`) |
| `doctor` | both hook keys, plugin path, every enabled header's mapping, `aoe` binary | human report, exit 1 on any failure | the user, after installing |
| `setup --print` | adapter row + install path + enabled headers | the exact hook lines + one mapping line per header | the user, before pasting |
| `verify` | the configured consumer's request/usage records (M2) | pass/fail | M2 |

Flags for `hook`/`value` (all optional; every one is a *fallback*, the row wins):

```
--session <id>   --profile <p>   --tool <t>   --project <path>
--group <path>   --title <text>  --adapter <name>   --json
```

Resolution pipeline, in order, stopping at the first success for each field:

1. `aoe session show "$AOE_SESSION_ID" -p "$AOE_PROFILE" --json`, with `AOE_BIN` → `command -v aoe`
   → `/usr/local/bin/aoe`, hard 2 s timeout, `subprocess.run(..., timeout=2)`.
2. The flags (which the hook filled from `AOE_*`).
3. Defaults: empty group, empty title, tool `unknown`.

Then: read settings → evaluate rules → render every enabled header's template → print one
`NAME=value` line per header. Any exception is a `stderr` line and exit 0.

Settings read: `[plugins."dev.oozut.llm-tags".settings]` from the effective config. That is the
global `<app_dir>/config.toml` with the profile's sparse override file
(`<app_dir>/profiles/<profile>/config.toml`, top level = the override tree) merged onto it the way
aoe merges: object keys recurse, **arrays replace** (`merge_configs_generic`). A missing section
means no rules, which is not an error; an explicit `headers = []` means no headers at all.
`src/llm_tags/settings.py` reproduces that merge, so the hook and the settings UI agree about a
value; a file that cannot be parsed is a warning and is ignored (profile → global → defaults).

Rule evaluation (M1 grows this; M0 needs only `project` and `manual`). Rendering is per header,
so the default tag format is just the default template:

```python
# 1. fields, for templates; row wins, hook env is the fallback
fields = {"session": short_id(s.id), "session_id": s.id, "feature": feature,
          "tool": s.tool, "profile": s.profile, "group_slug": slug(s.group),
          "project": s.path, "project_slug": basename(s.path)}
# 2. every matching rule contributes, in list order; dedupe; cap 5 values
features = list(dict.fromkeys(r["tag"] for r in rules if matches(r, s)))[:5]
fields["tags"] = features                              # {tags} expands to the list
# 3. render every enabled header; a template may reference {tags} or single fields
for h in [h for h in settings.get("headers", DEFAULT_HEADERS) if h.get("enabled", True)]:
    value = render(h["template"], fields)             # 800-char cap, grammar check
    print(f"{h.get('env_var') or derive_env(h['header'])}={value}")
```

Default header set shipped in code, overridable by settings:

```python
DEFAULT_HEADERS = [
  {"header": "X-AoE-Tags", "env_var": "AOE_LLM_TAGS", "enabled": True,
   "template": "aoe,feature:{feature},session:{session}"},
  {"header": "X-AoE-Session", "env_var": "AOE_HDR_X_AOE_SESSION", "enabled": False,
   "template": "{session_id}"},
]
```

Tests that need no aoe at all: accumulator order/dedup/cap, grammar rejection, `short_id`
stability, template field coverage, env-name derivation, disabled headers emitting nothing,
missing settings, malformed TOML (→ exit 0, env fallback).

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

M0 manifest is deliberately inert — no worker, no UI, no capabilities:

```toml
id = "dev.oozut.llm-tags"
name = "LLM Tags"
version = "0.1.0"
api_version = 19
description = "Send identifying headers with every LLM request a session makes, so any system on the path can attribute, route or validate it."
```

At M1 add, in one update: `capabilities = ["runtime.worker", "session.read", "net"]`,
`[[commands]] id = "status"`, `[[ui]] row-badge`/`pane`, and `[[settings]]` for `headers`
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
- Unknown, and cheap to check once: whether every hop between the agent and the consumer keeps an
  unknown request header, and whether the consumer can turn one into a dimension at all. That is
  deployment knowledge (`reference-deployment.md`), not a design input — if a hop strips it, the
  hop is the thing to change.

## Step 5 — `doctor`

Each row is one check with its own line and exit contribution.

| Check | How | Failure message must say |
|---|---|---|
| Effective config resolved | profile config, then global | which file it read and that no hook line exists |
| `before_session` present | parse effective config | "host sessions will not send headers" |
| `before_start` present | same | "sandboxed sessions will not send headers" |
| Command path exists and is executable | `os.access(X_OK)` | the path plus "plugin moved, uninstalled, or id changed" |
| `aoe` binary resolvable | `AOE_BIN`, `shutil.which` | the value it will use, and how to override |
| Row resolves for a given session | `aoe session show … --json` | "falling back to hook environment: group/title will be empty" |
| Each enabled header mapped | adapter row + config root | which header, which file, and whether the file was created or already had it |
| Value renders | run the same resolver locally | the template that failed and the field it needed |
| Last verification | stored in plugin settings | "never verified against a consumer system" |

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
| Worker | `runtime.worker`, stdio JSON-RPC | `worker/main.py`, dispatch on the trailing segment of `plugin.dev.oozut.llm-tags.<cmd>` |
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

## Step 9 — M3 page

`[[ui]] slot = "page"` with a single self-contained HTML file (inline CSS/JS, `data:` images),
state pushed by the worker with `ui.state.set` (slot `page`, ≤64 KiB): usage per tag keyed by
whatever the configured consumer system returns, sessions it never saw, and the verify action.
A consumer that cannot answer a dimension degrades to wiring-only information with a message
naming what it cannot answer (`header-contract.md`, consumer table). Needs the same `net` grant
as M2.

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

1. Which consumer system on the reference path counts as "recorded" — the first hop that keeps
   headers, or the system that can later answer a query about them. Deployment-specific, answered
   in `reference-deployment.md`; the plugin only needs to know where to look for `verify`.
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

Step numbers above map to milestones like this: steps 1–6 are M0 (inside it, `plan/m0.md` task 1–8),
step 7 is M1, step 8 is M2, step 9 is M3.

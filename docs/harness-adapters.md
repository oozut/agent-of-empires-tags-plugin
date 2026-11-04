# Harness adapters

Each harness is described by data, not code, so a user can add or fix an entry without a
plugin release. The plugin only needs to know how to turn the resolved values into what a
harness expects, and where that harness's config lives. Which system eventually reads those
values is not part of an adapter row — see `docs/header-contract.md`.

An adapter row is per **target**, not per harness: a harness that can send three headers needs
three mapping lines, all fed by the same hook. The default target is listed in each row's
`setup` column; additional enabled targets follow the same shape with their own variable.

A target is a place the tag set is delivered to: a request header (`kind = "header"`), a query
parameter on the provider URL (`kind = "query"`), or a JSON field in the request body
(`kind = "body"`). Tags are resolved once into one JSON payload — `tags` is a set of `key:value`
(and `key:=json`) entries, plus the session fields — and each target chooses where it goes and
what of it travels: `format = "json"` (default) sends the whole object, `format = "csv"` the flat
`team:core,feature:billing` list, and `template` overrides both for one target
(`{tags}`, `{tags_json}`, `{payload_json}`, `{feature}`, `{session}`…).

`interpolates` lists the delivery points where that harness expands an env reference; `supports`
lists the kinds it can carry at all. pi, claude and codex expand `$NAME` only in headers, so a
`body` target there needs `literal` — the value written as-is — and `setup --apply` refuses rather
than writing an unexpanded `$VAR`.

`supports` is absent-means-`["header"]` (the common case) but `supports = []` is taken literally
and means **no channel at all** — that is how `kiro` says so, and a loader that filled the default
in for an explicit empty list would hand it a header delivery it cannot make.

pi's `supports` is `["header", "body"]`, verified on the host (2026-11): a header arrives, a
`body` target lands in the request body through `models[].samplingParams`, and a query parameter
in `baseUrl` does **not** work — pi appends the endpoint path, so `http://h/v1?tags=x` becomes
`/v1?tags=x/chat/completions`. A `query` target for pi is refused, not written.

## Adapter data format

A row describes two things: where the harness keeps a mapping, and how this plugin gets a
*session-scoped* copy of it in front of the harness. The second half is the `write_mode` group.

```toml
[[adapter]]
agent = "pi"
label = "pi"
mechanism = "config_headers"          # pi also accepts command_headers; see table
interpolates = ["header"]           # delivery points that expand `$NAME`
guard_fallback = "{}"               # payload a leftover mapping carries; only for a harness
                                    # that fails the request on an unresolvable header
supports = ["header", "body"]       # delivery kinds it can carry; `[]` = none (not "default")
aoe_hooks = false                   # true when aoe installs its own hooks into this config
config_env = "PI_CODING_AGENT_DIR"  # directory override the harness itself honours
# Where the mapping is *written* can differ from where the launched process reads it, when a
# launcher regenerates the directory. The first entry is the file a harness reads from; a
# rewritten directory is never the target.
config_paths = ["~/.pi/agent/models.json", "$PI_CODING_AGENT_DIR/models.json"]
config_format = "json"              # json | toml
status = "verified"                 # verified | documented | needs_research | unsupported

# --- how a session gets its own config (docs/harness-adapters.md §Per-session config)
write_mode = "overlay_dir"          # env_only | overlay_file | overlay_dir | none
overlay_env = "PI_CODING_AGENT_DIR" # variable that points the harness at the session copy
overlay_file = "models.json"        # our file inside that copy
writer = "pi_models"                # src/writer.rs

# --- only for write_mode = "env_only"
native_env = "ANTHROPIC_CUSTOM_HEADERS"
native_format = "{header}: {value}"
native_sep = "\n"                     # "," for a harness whose parser only splits on commas
splits_on_commas = false              # true when the harness reads the value as `a,b,…`
```

`mapping_format` spells the reference itself when it needs a particular form. Three placeholders
matter: `{env_var}` is the bare variable *name* (codex's `env_http_headers` already means "read a
variable"), `{env_ref}` is the harness's own spelling from `env_ref`, and `{value}` is a literal when
the row does not interpolate. `env_ref` defaults to `${env_var}`, which now really renders
`${AOE_LLM_TAGS}` — the braced form hermes, qwen, droid and openclaw expand — while a row that wants
`$NAME` writes `"$"` before it and codex writes `env_ref = "{env_var}"`. A reference that silently
lost its braces (`$NAME` in a JSON value) is a mapping the harness reads as a literal string, so the
spelling is part of the row, not a rendering detail.

`guard_fallback` is the row's answer to "what does this mapping carry once the plugin is gone?".
For a harness that resolves a header strictly — pi throws `Failed to resolve … header … from
environment variable` instead of sending nothing — a bare `$VAR` mapping is a permanent fault: the
line lives in a file this plugin does not own, so it outlives an uninstall and breaks the first
launch that has no hook behind it, including a launch from a plain shell. With `guard_fallback`
set, `{value}` becomes

```
!v="${AOE_LLM_TAGS-}"; [ -n "$v" ] && printf %s "$v" || printf %s {}
```

which prints the hook's value while it exists and the fallback (`{}`, the same shape the CLI emits
when it knows nothing) afterwards. A command is the only form that survives both halves of that:
no shell-free spelling stays non-empty when the variable is missing, and `[ -n "$v" ]` also covers
set-but-empty, which a command reading nothing would turn back into a failure. The price is one
`sh` per value resolution (pi caches command results per process). Rows without `guard_fallback`
(hermes, qwen, codex, opencode, …) send the plain `env_ref`, because their harnesses treat an
unresolved reference as an empty header, not as a failed request.

For a JSON row the printed line escapes `"` inside `{value}`, so the command can be pasted into
`models.json` exactly as printed.

`splits_on_commas` is a *refusal*, not a hint: gemini's `parseCustomHeaders` splits its variable on
`,(?=\s*[^,:]+:)`, so a JSON payload `{"a":1,"b":2}` becomes two headers and the request is dropped
with `TypeError: Headers.append: invalid header name`. Such a row declares the shape, and
`setup --apply` refuses a target whose rendered value would be split (`comma_splits` checks the
value, so a comma-free `a,b` still passes). The escape hatch is `template = "{tags_b64}"` — the same
payload, base64url-encoded, so it cannot contain a splittable comma.

| `write_mode` | What a session gets | The user's config |
|---|---|---|
| `env_only` | nothing on disk: the hook exports the harness's own variable (`native_env`) carrying this session's lines | never read for writing |
| `overlay_file` | one file this plugin generates, named by `overlay_env` | copied as the starting point, then untouched |
| `overlay_dir` | a directory holding our file plus links to the user's other entries, named by `overlay_env` | linked, never edited |
| `none` | nothing: only the printed mapping is available, `doctor` says `unsupported` | untouched |

`aoe_hooks = true` marks a harness whose config file aoe itself writes hooks into (`claude`'s
`settings.json`, `codex`'s `hooks.json`, and the other agents `aoe hooks status` lists). aoe pins
that file's path for the launch and refuses a launch where `before_session` moves it:

```
Error: before_session changed the agent hook path from …/home/.claude/settings.json to
…/sessions/<id>/claude/settings.json; declare the override in the profile environment before
approving
```

So a per-session copy is not merely expensive for those agents, it is refused. A row that marks
`aoe_hooks` and still asks for `overlay_file`/`overlay_dir` is downgraded at load time to `none`
with a warning, and keeps only the `header` kind — the reference channel is what it actually has.

| `mechanism` | Meaning | Delivery |
|---|---|---|
| `custom_headers_env` | The harness reads one env var holding whole header lines | `env_only` |
| `headers_from_env` | The harness config maps a header name to a variable it reads | session copy |
| `config_headers` | The harness config holds the header value, with env interpolation | session copy |
| `command_headers` | The harness config runs a command per request to produce the value | session copy or `env_only` |
| `url_param` | Only a base URL override exists; the value travels in path or query | refused unless the harness keeps query strings intact |
| `none` | No usable mechanism found | `unsupported` |

## Per-session config

The M0 mapping wrote a header into the user's config and relied on the harness expanding
`$AOE_LLM_TAGS` at request time. That only survives while every session's value fits in one
global file — it does not for body fields, query parameters or literal-only channels, where one
session's value would freeze into everybody else's requests.

So the rule is now: **the user's config is never edited.** At launch the hook builds a copy for
that session alone, bakes this session's values into it, and exports the variable that points
the harness at the copy:

```
hook  ──►  <app dir>/sessions/<session id>/<agent>/…    (built from the user's file)
      ──►  PI_CODING_AGENT_DIR=<that dir>               (harness reads the copy)
```

Properties that follow, and that the tests pin down:

- **Two sessions never share a file.** Different session id ⇒ different directory, so
  concurrent sessions cannot overwrite each other's tags.
- **Nothing is written outside the app dir.** `setup --undo` deletes the copy; there is no
  backup to restore because the original was never touched.
- **A copy cannot outlive its session.** The worker prunes `sessions/` against the host's live
  session list, and `doctor` reports what is there.
- **A copy that carries nothing is not used.** If the writer has nowhere to put any target
  (no provider with models, no provider table), the hook exports no override, so the harness
  keeps reading its real config instead of an empty one.
- **The shape is preserved before it is modified.** The copy starts as a parse of the user's
  file; the TOML writer edits the document so the user's comments survive.

Writers (`src/writer.rs`) are per config format, not per vendor:

| `writer` | Into | Delivery |
|---|---|---|
| `pi_models` | `providers.<id>.headers`, `models[].samplingParams` | header, body (query refused) |
| `opencode_json` | `provider.<id>.options.headers`, `.queryParams` | header, query |
| `codex_toml` | `[model_providers.<id>] http_headers`, `.query_params` | header, query |
| `vibe_toml` | `[[providers]]` → `extra_headers` on the entry the `[[models]]` name | header (query and body refused) |

Both writers receive the payload, not a string: a `json` target is set as a JSON object where the
harness takes one (pi's `samplingParams`, opencode's `headers` value stays a string), and a `csv`
target as the flat list.

`codex_toml` and `opencode_json` both carry a query parameter because the harnesses do; codex's
row still declares only `header` under aoe, because `aoe_hooks` rules out the copy that would
carry it (see above). The writer stays: it is what a launcher that does not install aoe's hooks
uses, and `tools/probe-harness.sh codex` proves it.

A new harness is a row plus, at most, one small writer. `tests/overlay.rs` refuses a row that
claims `overlay_*` without an `overlay_env` and an implemented writer, so the table cannot drift
into lying about a capability.

### Verdicts: what is actually there

`doctor` used to print the *plan* — what a row would do with this machine — which reads as
`wired` on a machine where nothing is wired at all. `classify` (and one `substrate` line per
scope in `doctor`) reports what was found, under the five verdicts `plan/parameters.md` fixes:

| verdict | means | `doctor` |
|---|---|---|
| `wired` | the file exists and every enabled target is in it | ok |
| `detected-not-wired` | the harness config exists, our mapping is missing (some or all) | warn |
| `sandbox-stale` | a generated copy exists for a session `aoe list` does not have | warn |
| `unknown` | nothing to inspect: no adapter row, or no config at any known path | skip |
| `unsupported` | the row can neither write a copy nor be referenced (no channel) | fail |

Four substrates are checked per session: the **harness** (can this row deliver at all — a
`write_mode = "none"` row still passes when it interpolates, because the reference channel is a
real channel), the **user's own config** (present, and carrying our line or not), this
**session's copy** (built at launch, pointed at by the override variable), and leftovers
(`sandbox-stale`).

Two rules keep the verdicts honest:

- **Pessimistic on ignorance.** `unknown` when a check cannot be performed: an unreadable file,
  an `aoe list` that did not answer, a harness with no row. `sandbox-stale` is only claimed when
  the host proved a session gone — an empty live list means the query failed, not that every
  copy is stale.
- **Cached by path + sha256** (`<app dir>/cache/substrates.json`). The digest is the key, so a
  hand-edited config is re-scanned rather than trusted — which is also how drift shows up as
  `detected-not-wired` instead of a stale `wired`.

```
aoe-llm-tags classify                 # this session's harness, from $AOE_TOOL
aoe-llm-tags classify --adapter codex # one row, whatever the environment says
aoe-llm-tags classify --json          # findings + `worst`, the pessimistic badge verdict
```

`classify` exits 0 for a missing or unwired config (a fact to report) and 1 only for
`unsupported` (a session that cannot carry the tag at all).

### Verification, portably

```
tools/probe-harness.sh <agent>            # one harness, by absolute path
tools/probe-harness.sh --list             # agents with a recipe
```

The probe runs in a throwaway `HOME`, calls the harness binary directly (a user-prefix install
in `~/.local/bin` is fine and need not be visible to `aoe agents`), points its provider at
`tools/header-sink.py`, and records what arrived in `docs/harness-evidence/<agent>.json`. It
needs no API key and no model: the harness is expected to fail at completion, and the request it
sent is the result. `--extra-settings FILE` appends targets to the probe's own plugin config, so
body and query delivery can be checked the same way.

Verified on this host (2026-11, evidence in `docs/harness-evidence/`):

| Agent | Session scope | Delivered |
|---|---|---|
| `pi` | `PI_CODING_AGENT_DIR` → overlay dir | header, and the `samplingParams` body field |
| `claude` | `ANTHROPIC_CUSTOM_HEADERS` from the hook | header |
| `codex` | reference in the user's `config.toml` → `env_http_headers` | header (query only outside aoe) |
| `opencode` | `OPENCODE_CONFIG` → overlay file | header and query parameter |
| `vibe` | `VIBE_HOME` → overlay dir, `config.toml` | header (`extra_headers`) |
| `openclaw` | reference in the user's own JSON (`models.providers.*.headers`) | header |

Every value above is the JSON payload object, not a comma list — `docs/header-contract.md`.

The probe covers the harness in isolation. The second, stronger check is a **real aoe session per
harness**:

```
tools/probe-session.sh <agent> --cli target/debug/aoe-llm-tags --bin "$(command -v <agent>)" --sink 8950
```

It creates a throwaway `HOME`, writes the plugin settings and the hook config into a throwaway
`XDG_CONFIG_HOME`, runs `aoe hooks approve` there, adds a scratch session whose command is this
harness, starts it, waits for the sink, captures the pane, and stops it. The evidence
(`docs/harness-evidence/session-<agent>.json`) records `requests`, which channels the sink saw
(`delivered`), which variables in the launched pane carried the payload (`env_payloads`), and an
`outcome`:

| `outcome` | Meaning |
|---|---|
| `unsupported-by-aoe` | aoe has no launcher for this harness, so there is no session to probe: the harness-level evidence is the whole proof |
| `delivered` | a request from the launched harness reached the sink carrying the payload |
| `no-request` | the pane got the payload in its environment, but the harness sent nothing (no credential or endpoint it can read) |
| `refused-by-aoe` | aoe refused the launch because the hook path moved — a `write_mode` aoe cannot allow |
| `request-without-the-tag` | a request arrived without the payload: a real defect |

## Harness table

Seventeen agents exist in aoe; this table covers the twelve that have been researched, and no row
is a `needs_research` placeholder any more. Nine are `verified`: a real process sent the payload to
`tools/header-sink.py`, and a user-prefix install (`~/.local/bin`) is enough — the binaries need
not be visible to `aoe agents`. Three are `documented`, each for a stated reason: `droid` validates
a Factory account before sending (`blocked-by-auth`), `cline` exposes the channel in its SDK but
keeps its config dir behind a `--config` flag with no environment override, and `kiro` has no
channel at all (its transport is the AWS SDK, so `supports = []`). A `documented` row is not a
failure: it says exactly where the evidence stops, which is why `setup --apply` refuses a target
for `kiro` instead of writing a mapping nothing reads.

### Support matrix

What each row can actually carry, and how a session gets it. `✓` is a channel the plugin
writes today; `—` is a kind `setup --apply` refuses for that row rather than writing a value the
harness would drop.

| Agent | Header | Query | Body | Mechanism | Session scope | Evidence |
|---|---|---|---|---|---|---|
| `pi` | ✓ | — | ✓ | `config_headers`, `command_headers` | `overlay_dir` via `PI_CODING_AGENT_DIR` (`pi_models`) | verified |
| `opencode` | ✓ | ✓ | — | `config_headers` | `overlay_file` via `OPENCODE_CONFIG` (`opencode_json`) | verified |
| `claude` | ✓ | — | — | `custom_headers_env` | `env_only`: `ANTHROPIC_CUSTOM_HEADERS` (`aoe_hooks`) | verified |
| `codex` | ✓ | — | — | `headers_from_env` | reference in the user's `config.toml` → `env_http_headers` (`aoe_hooks`); query only outside aoe (`codex_toml`) | verified |
| `hermes` | ✓ | — | — | `config_headers` | reference in the user's YAML (`extra_headers`, `aoe_hooks`) | verified |
| `gemini` | ✓ | — | — | `custom_headers_env` | `env_only`: `GEMINI_CLI_CUSTOM_HEADERS` (`aoe_hooks`) | verified |
| `qwen` | ✓ | — | — | `config_headers` | reference in the user's JSON (`generationConfig.customHeaders`, `aoe_hooks`) | verified |
| `droid` | ✓ | — | — | `config_headers` | reference in the user's JSON (`customModels[].extraHeaders`, `aoe_hooks`) | documented |
| `vibe` | ✓ | — | — | `config_headers` | `overlay_dir` via `VIBE_HOME`, `config.toml` (`vibe_toml`) | verified |
| `openclaw` | ✓ | — | — | `config_headers` | reference in the user's JSON (`models.providers.*.headers`) | verified |
| `cline` | ✓ | — | — | `config_headers` | none: the SDK takes a `headers` map, the CLI's config dir is a `--config` flag | documented |
| `kiro` | — | — | — | `none` | nothing: AWS-SDK signed transport, no header setting | documented |

Where the value comes from, per row, is one of three shapes:

| Session scope | Rows | What the user has to do |
|---|---|---|
| `overlay_dir` / `overlay_file` | `pi`, `opencode`, `vibe` | nothing — the hook writes this session's copy and exports the override variable |
| `env_only` | `claude`, `gemini` | nothing — the hook exports the harness's own variable |
| reference in the user's own config | `codex`, `hermes`, `qwen`, `droid`, `openclaw` | paste the one line `setup --print` shows (it names a *variable*, never a value); aoe installs its hooks into `codex`/`hermes`/`qwen`/`droid`, so the file cannot be scoped per session |
| none | `cline`, `kiro` | nothing until the harness grows a scoping channel; `setup --apply` refuses and `classify` says `unsupported` |

Two rows carry a caveat that is a limit, not a defect:

- **gemini** splits its variable on `,(?=\s*[^,:]+:)`, so a JSON payload would become two broken
  headers and the request is dropped (`TypeError: Headers.append: invalid header name`). The row
  declares `splits_on_commas`, and the escape hatch is `template = "{tags_b64}"` — the same
  payload, base64url, so it cannot contain a splittable comma.
- **droid** can be *mapped* (its config expands `${VAR}`), but the binary refuses to send
  anything without a Factory account, so the probe records `blocked-by-auth`. The row is
  `documented`: the mapping is real, the request never was.

Discovery paths, per row, are what `doctor`/`classify` read: `~/.pi/agent/models.json` and
`$PI_CODING_AGENT_DIR`, `$XDG_CONFIG_HOME/opencode/opencode.json`, `$CLAUDE_CONFIG_DIR` and
`~/.claude/settings.json`, `$CODEX_HOME` and `~/.codex/config.toml`, `~/.hermes/config.yaml`,
`~/.gemini/settings.json`, `~/.qwen/settings.json`, `~/.factory/settings.json`,
`~/.vibe/config.toml` and `$VIBE_HOME`, `~/.openclaw/openclaw.json` and
`$OPENCLAW_CONFIG_PATH`, `~/.cline`, `~/.local/share/kiro-cli`.

Snippets for the documented rows. They describe the *shape* of the session copy; nothing here is
pasted into a user config by hand any more (default header shown):

```jsonc
// pi, ~/.pi/agent/models.json — merge into the provider entry the harness actually uses;
// keep the existing models list (see the launcher note below). This is the value
// `setup --print` gives, quotes escaped as JSON needs them.
{ "providers": { "<provider-id>": {
    "headers": { "X-AoE-Tags": "!v=\"${AOE_LLM_TAGS-}\"; [ -n \"$v\" ] && printf %s \"$v\" || printf %s {}" },
    "models": [{ "id": "<model-id>", "name": "<Model name>" }] } } }
```

```jsonc
// pi, pre-guard spelling: works only while a hook mints the variable, and pi fails
// the whole request when it does not. `doctor` warns about it for that reason.
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

- **Start from the wrapper's source file**, not from the directory it generates. If a launcher
  recreates `$CONFIG_DIR` on every start (this deployment's `pi-proxy` does), a mapping written
  there is deleted at the next launch; the per-session copy is built from the source path
  (`config_paths[0]`) instead.
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
// opencode, opencode.json — the same payload can ride a query parameter, which is the only
// channel this harness offers besides headers (options.body and options.extraBody are ignored)
{ "provider": { "gw": { "options": {
    "headers": { "X-AoE-Tags": "{env:AOE_LLM_TAGS}" },
    "queryParams": { "aoe_tags": "{env:AOE_QRY_AOE_TAGS}" } } } } }
```

```sh
# claude: no static config, the variable carries whole header lines
ANTHROPIC_CUSTOM_HEADERS="X-AoE-Tags: {\"session\":\"S-9f3a\",\"feature\":\"review\",\"team\":\"core\"}"
```

```sh
# gemini: no static config either, but its variable is a comma-separated list, so the payload
# must be spelled without a splittable comma. {tags_b64} is that spelling.
GEMINI_CLI_CUSTOM_HEADERS="X-Aoe-B64: eyJzZXNzaW9uIjoiUy05ZjNhIiwidGVhbSI6ImNvcmUifQ"
```

```jsonc
// qwen, ~/.qwen/settings.json — the entry is a model config; a provider-level customHeaders map
// is merged into it by applyProviderCustomHeaders, so set it where it reaches the wire
{ "modelProviders": { "openai": [{
    "id": "<model-id>", "name": "<Model name>", "baseUrl": "<endpoint>", "envKey": "OPENAI_API_KEY",
    "generationConfig": { "customHeaders": { "X-AoE-Tags": "${AOE_LLM_TAGS}" } } }] } }
```

```jsonc
// droid, ~/.factory/settings.json — ${VAR} is expanded for user-level sources only
{ "customModels": [{ "model": "<model-id>", "provider": "openai", "baseUrl": "<endpoint>",
    "apiKey": "<key>", "extraHeaders": { "X-AoE-Tags": "${AOE_LLM_TAGS}" } }] }
```

```toml
# vibe, ~/.vibe/config.toml — the session copy (VIBE_HOME) holds the value, because vibe does
# not expand $NAME in config strings: the literal "${AOE_LLM_TAGS}" reaches the wire
[[providers]]
name = "<provider-id>"
api_base = "<endpoint>"
api_key_env_var = "<KEY_VAR>"

[providers.extra_headers]
X-AoE-Tags = '{"session":"S-9f3a","team":"core"}'
```

```jsonc
// openclaw, ~/.openclaw/openclaw.json — a custom provider must declare models, and the
// `${VAR}` spelling *is* expanded here (an unset variable is left on the wire verbatim)
{ "models": { "providers": { "<provider-id>": {
    "baseUrl": "<endpoint>", "apiKey": "<key>", "api": "openai-completions",
    "headers": { "X-AoE-Tags": "${AOE_LLM_TAGS}" },
    "models": [{ "id": "<model-id>", "name": "<Model name>" }] } } } }
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

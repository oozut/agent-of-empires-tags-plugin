# Harness evidence

Two files per harness, from the two probes that answer two different questions.

## `<agent>.json` — does this harness carry the payload?

Written by `tools/probe-harness.sh <agent>`: what this session's tag set looked like on the way
out (`minted_values`), how the harness was pointed at its per-session config
(`session_override`), and every delivery the sink actually saw.

The probe is self-contained — throwaway `HOME`, the harness binary by absolute path, a local
sink instead of a model endpoint — so these files can be regenerated on any machine that has the
harness installed. A harness is `verified` in `adapters.toml` when its file here shows
`"delivered": true` and the delivery is the one the row claims.

## `session-<agent>.json` — does a real aoe session deliver it?

Written by `tools/probe-session.sh <agent>`: a real AoE session whose command is the harness,
created in a throwaway `HOME` and `XDG_CONFIG_HOME`, with `aoe hooks approve` run inside that
sandbox. It records which channels the sink saw (`delivered`), which variables in the launched
pane carried the payload (`env_payloads`), and an `outcome`:

| `outcome` | Meaning |
|---|---|
| `delivered` | a request from the launched harness reached the sink carrying the payload |
| `no-request` | the pane received the payload in its environment, but the harness sent nothing: its config lives in `$HOME`, which aoe overwrites, so the probe cannot point it at the sink (claude's credentials, codex's provider block) |
| `refused-by-aoe` | aoe refused the launch because the hook path moved — a `write_mode` aoe cannot allow |
| `request-without-the-tag` | a request arrived without the payload: a real defect |
| `unsupported-by-aoe` | `aoe agents` has no launcher for this harness, so there is no session to create (openclaw); the harness-level file is the whole proof |
| `blocked-by-auth` | the channel is configured, but the harness refuses to send without a logged-in account (droid) — recorded, never counted as a pass |

`no-request` is not a pass for the harness; it is the probe stating where its evidence stops.
For those agents the delivery that matters is the one in `env_payloads`: the value is minted and
present in the launched process's environment, and the reference their own config already holds
(`ANTHROPIC_CUSTOM_HEADERS` for claude, `env_http_headers` for codex) is what turns it into a
header.

## `sandbox-pi.json` — does the payload reach a *sandboxed* session?

Written by `tools/probe-sandbox.sh`: the same question one substrate further out, because a
sandboxed launch runs the hook key `before_start` and its container mounts neither the app dir nor
the host's `$HOME`. The probe starts one fresh container per candidate mechanism, each with its
own sink inside the container (a sandbox has no route to a sink on the host on every host), and
then creates one real `aoe add --sandbox` session to record what its container actually received.

| Field | What it says |
|---|---|
| `variants[]` | one mechanism per row: the overlay path aoe does not mount (`host_overlay`, no request), the guarded mapping plus the variable (`env_only`), an overlay under the project, a relative overlay dir, and a mounted overlay dir |
| `host_leg` | the same value serving the host and the container from one hook |
| `real_session.overlay_env` | the copy's path *and* whether it exists inside the container (`absent`) — the defect, measured |
| `real_session.models_without_overlay` / `with_overlay` | the harness still lists its models without the export, and lists **none** with it |
| `real_session.plugin_sandbox_hook` | the shipped `--substrate sandbox` line: the same payload, `writes_an_overlay_env_line: false`, and it needs no app dir (`HOME` unset) |

`real_session` reflects the `before_start` line installed on the host at probe time. A line from
before the substrate flag keeps the defect visible; with the shipped line the container exports
the tag and leaves the harness's own config dir alone.

## What the values are

Both files carry the JSON payload, not a comma list:

```json
{"session":"S-prob","session_id":"…","tool":"pi","profile":"main",
 "project":"project","project_path":"/tmp/aoe-probe-pi-…/project",
 "feature":"billing","team":"core","cost":1.5}
```

A `csv` target still exists (`X-AoE-Flat` in the session probe) for a consumer that wants the flat
`aoe,team:core,feature:billing,cost:1.5,session:S-…` form, and the probes check it when the harness
carries more than one header.

| Agent | Session scope | Delivered |
|---|---|---|
| `pi` | `PI_CODING_AGENT_DIR` (overlay dir) | header + `samplingParams` body field |
| `opencode` | `OPENCODE_CONFIG` (overlay file) | header + query parameter |
| `claude` | `ANTHROPIC_CUSTOM_HEADERS` (env only) | header (payload in `env_payloads`) |
| `codex` | reference in its own `config.toml` (`env_http_headers`) | header (query outside aoe only) |
| `hermes` | reference in its own `config.yaml` (`extra_headers`) | header (`${AOE_LLM_TAGS}` expanded by hermes) |
| `gemini` | `GEMINI_CLI_CUSTOM_HEADERS` (env only, comma-separated list) | header — as `{tags_b64}`, because its parser splits a JSON payload on commas |
| `qwen` | reference in its own `settings.json` (`generationConfig.customHeaders`) | header (`${AOE_LLM_TAGS}` expanded by qwen) |
| `droid` | reference in its own `settings.json` (`customModels[].extraHeaders`) | header (`${AOE_LLM_TAGS}`) — `blocked-by-auth`: no request can be observed without a Factory account |
| `vibe` | `VIBE_HOME` (overlay dir, `writer = "vibe_toml"`) | header (`extra_headers`, written per session — vibe does not expand `$NAME`) |
| `openclaw` | reference in its own `openclaw.json` (`models.providers.*.headers`) | header (`${AOE_LLM_TAGS}` expanded per process) — `unsupported-by-aoe`: aoe cannot launch it, so there is no session to probe |

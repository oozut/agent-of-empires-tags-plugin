# Parameters

Every name, value and default the plugin uses, in one place. `plan/m0.md`–`m4.md` reference these
tables; a milestone file only repeats the ones it *introduces*. When implementation needs a value
that is not here, add it here in the same change.

**Status** — `decided`: fixed by the contract or an earlier decision; `proposed`: this plan's
default, changeable until the milestone ships; `open`: named in `milestones.md` §Open decisions.

## 1. Identity and install

| Parameter | Value | Status |
|---|---|---|
| Plugin id | `dev.oozut.llm-tags` | decided |
| Manifest file | `aoe-plugin.toml` (repo root) | decided |
| `name` | `LLM Tags` | proposed |
| `version` | `0.1.0` (M0), semver after | proposed |
| `api_version` | `19` | decided |
| `capabilities` (M0) | none | decided |
| `capabilities` (M1+) | `runtime.worker`, `session.read`, `net` | proposed |
| Install dir | `<app_dir>/plugins/<id>/` (host: `/var/lib/aoe/.config/agent-of-empires/plugins/dev.oozut.llm-tags/`) | decided |
| CLI path | `<install>/bin/aoe-llm-tags`, mode 0755, `#!/usr/bin/env python3` | decided |
| Build artifacts | `<install>/.aoe-build/` only (excluded from the tree hash) | decided |
| Worker entrypoint | `worker/main.py`, stdio JSON-RPC, newline-delimited | proposed |

## 2. Hook contract (M0)

| Parameter | Value | Status |
|---|---|---|
| Keys | `[host_hooks].before_session` **and** `.before_start`, same command string | decided |
| Placement | global `<app_dir>/config.toml` (recommended); per profile `<app_dir>/profiles/<name>/config.toml`. Profile arrays replace global arrays | decided |
| `AOE_BIN` | `AOE_BIN` → `shutil.which("aoe")` → `/usr/local/bin/aoe` | proposed |
| Contract with aoe | stdout `KEY=VALUE` per line, later wins, no trailing whitespace, exit 0 always, 2 s bound on any subprocess | decided |

## 3. Settings (`[plugins."dev.oozut.llm-tags".settings]`)

| Key | Type | Default | Allowed | Milestone | Status |
|---|---|---|---|---|---|
| `headers` | `object_list`, item key `header` | 2 rows below | 1–8 rows | M0 (hand-written), M1 (manifest) | proposed |
| `headers[].header` | string | `X-AoE-Tags` | HTTP field-name grammar, ≤64 bytes | M0 | decided |
| `headers[].template` | string | `aoe,feature:{feature},session:{session}` | template fields below | M0 | decided |
| `headers[].env_var` | string | derived: `AOE_HDR_` + upper snake of header | valid env name | M0 | proposed |
| `headers[].enabled` | boolean | `true` (second row: `false`) | — | M0 | proposed |
| `rules` | `object_list`, item key `match_value` | `[]` | — | M0 | proposed |
| `rules[].tag` | string | — | tag grammar, ≤32 chars | M0 | decided |
| `rules[].match_kind` | string | `project` | `project`, `group`, `tool`, `title_regex`, `manual` | M0: `project`, `manual`; M1: all | decided |
| `rules[].match_value` | string | — | kind-dependent | M0 | decided |
| `rules[].priority` | integer | `0` | −100…100 | M1 | proposed |
| `session_tagging` | string | `short_id` | `none`, `short_id`, `full_id` | M0 | decided |
| `session_signature` | string | `none` | `none`, `hmac` | M1 | open (#8) |
| `signing_key` | string | `""` | ≥16 chars when signature is `hmac` | M1 | open (#8) |
| `feature_tag` | string | `""` | tag grammar | M1 | decided |
| `feature_tag` scope | — | `per_project = true` (requires `api_version ≥ 18`) | — | M1 | decided |

Default header rows:

```toml
# 1) combined tag header — cost attribution, log joining
header = "X-AoE-Tags"
env_var = "AOE_LLM_TAGS"
enabled = true
template = "aoe,feature:{feature},session:{session}"

# 2) discrete session header — session validation/management; off until the operator wants it
header = "X-AoE-Session"
enabled = false
template = "{session_id}"
```

## 4. Template fields

| Field | Source | Example | Note |
|---|---|---|---|
| `{session}` | short id | `S-cf11` | default; `S-` + first 4 of the session id |
| `{session_id}` | row `id` | `cf11af87da7e4bf0` | high cardinality, opt-in |
| `{feature}` | matching rules | `review` | first matching rule's tag, else `feature_tag`, else empty |
| `{tags}` | all matching rules | `review,fix` | comma-joined, capped |
| `{tool}` | row `tool` | `pi-proxy` | harness/tool id |
| `{profile}` | row `profile` | `main` | |
| `{group}` | row `group` | `Backend` | prose, opt-in |
| `{group_slug}` | slugged group | `backend` | preferred over `{group}` |
| `{project}` | row `path` | `/var/lib/aoe/…/koreans` | may contain a username; opt-in |
| `{project_slug}` | basename of path | `agent-of-empires-tags-plugin` | preferred |
| `{branch}` | `aoe list --json` `worktree.branch` | `lithuanians` | requires the list call |

## 5. Value rules

| Parameter | Value | Status |
|---|---|---|
| Tag grammar | `[a-z0-9][a-z0-9._:-]*` per token | decided |
| Tag cap per header | 5 | decided |
| Rendered value max | 800 bytes | proposed |
| Default separator | `,` between tokens; `feature:<tag>` for a rule hit | decided |
| Forbidden content | titles, raw paths, group prose, newlines, non-ASCII unless the operator writes them into a template | decided |
| Empty result | empty value ⇒ the env var is minted empty and the harness sends an empty header | open |

## 6. CLI surface

| Command / flag | Value | Milestone |
|---|---|---|
| `hook` | prints one `env_var=value` per enabled header | M0 |
| `value [header]` | prints one header's value; default `X-AoE-Tags` | M0 |
| `doctor` | report; exit 1 if any check fails | M0 |
| `setup --print` | hook lines + mapping lines, writes nothing | M0 |
| `setup <agent> --dry-run` | diff only | M2 |
| `setup <agent> --yes` | apply with backup | M2 (open #5) |
| `setup <agent> --undo` | restore the backup / remove the marker block | M2 |
| `verify [--window 24h]` | one request, then look for the header consumer-side | M2 |
| `--session` `--profile` `--tool` `--project` `--group` `--title` | fallbacks for row fields (values from `AOE_*`) | M0 |
| `--adapter <agent>` | pin the adapter row instead of detecting | M0 |
| `--json` | machine-readable output (doctor, status) | M0 |
| Exit codes | `0` ok/degraded, `1` doctor or verify failure, `2` usage error | M0 |

## 7. Runtime constants

| Constant | Value | Why |
|---|---|---|
| Session-row query timeout | 2 s | hook must never block a launch |
| Fallback on timeout/error | hook environment, exit 0 | a non-zero hook exit aborts the session |
| Doctor output | absolute paths always | hook debugging without paths is the expensive failure |

## 8. Adapter data (`adapters.toml`)

| Key | Values | Note |
|---|---|---|
| `agent` | `pi`, `claude`, `codex`, `opencode`, `gemini`, `vibe`, `kiro`, `cline`, `openclaw`, `hermes` | aoe tool id |
| `label` | display string | |
| `mechanism` | `config_headers`, `command_headers`, `custom_headers_env`, `headers_from_env`, `url_param`, `none` | |
| `headers[]` | `{ header, env_var, optional }` | one entry per enabled header |
| `config_env` | env var name or absent | directory override |
| `config_paths[]` | ordered paths, first hit wins | must not include a regenerated dir |
| `config_format` | `json`, `toml`, `yaml`, `none` | |
| `status` | `verified`, `documented`, `needs_research`, `unsupported` | per row |
| `verified_with` | `{ agent_version, aoe_version }` or absent | set only by a real run |

## 9. Discovery and config edits (M2)

| Parameter | Value | Status |
|---|---|---|
| Discovery verdicts | `wired`, `detected-not-wired`, `sandbox-stale`, `unknown`, `unsupported` | decided |
| Cache key | config path + file sha256 | proposed |
| Marker written into edited files | `aoe-llm-tags: managed` (comment syntax of the format) | proposed |
| Backup suffix | `.aoe-llm-tags.bak` (same directory, one generation) | proposed |
| Write method | temp file in the same directory + atomic rename; refuse symlinks and non-regular files | decided |

## 10. Usage views (M3)

| Parameter | Value | Status |
|---|---|---|
| Page slot / id | `page` / `llm_tags_page` | proposed |
| Page payload cap | 64 KiB (`ui.state.set`) | decided by host |
| Block types | `sparkline`, `bar`, `table` | proposed |
| Consumer adapter keys | `kind`, `base_url`, `token_env`, `window`, `capabilities[]` (`spend`, `requests`, `sessions`) | proposed |
| Default window | 24 h; presets 1 h / 24 h / 7 d / 30 d | proposed |

## Open

| # | Question | Affects |
|---|---|---|
| 1 | Accumulate every matching rule, or first match wins | `rules[]` semantics, `{tags}` |
| 2 | Value grammar per header (comma list vs JSON vs custom template) | `headers[].template` |
| 3 | `{session}` short vs full id default | `session_tagging` |
| 4 | Which consumer is the reference for `verify` here | `verify`, `consumer.*` |
| 5 | Apply config edits on `--yes`, or print only | `setup` flags, §9 |
| 6 | Neutral `X-AoE-*` names vs operator-chosen wire names | default header rows |
| 7 | How many headers ship enabled | default header rows, every adapter's static config |
| 8 | Ship HMAC session signatures in M1 or M2 | `session_signature`, `signing_key` |
| 9 | Empty render: mint an empty value, or emit no line | hook output, header presence |

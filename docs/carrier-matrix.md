# What reaches a session: every harness, both substrates

A session can get four separate things, and each is separately true or false. This page says
which, per harness, for a host launch and for a sandboxed one. It exists because the page
(`ui/tags.html`) has to say something for every harness, and "installed" is a claim: a row that
ships a skill no loader opens, or a carrier a container cannot read, must not look the same as a
row that works.

| Thing | What it is | Where it comes from |
|---|---|---|
| **tag value** | the tag string on the request (`aoe,activity:test,team:core`) | the launch environment (`AOE_LLM_TAGS`), or this session's config copy |
| **activity skill** | the `aoe-activity` skill the agent reads to know it may declare what it is doing | approval-gated: `<app dir>/skills/aoe-activity/SKILL.md`, placed in the session copy or in the harness's own config |
| **reminder** | one always-loaded paragraph saying the same, for the agent that never reads skills | `harness/activity.append.md`, placed in `APPEND_SYSTEM.md` / `AGENTS.md` / `CLAUDE.md` / … |
| **per-request update** | a tag edited mid-session reaches the *next* request without a relaunch | an in-process extension that re-reads `live.json` (only `pi` ships one) |

Two facts decide the whole table.

**1. The substrate decides whether the session copy exists.** aoe runs `before_session` for a host
launch and `before_start` for a sandboxed one, and a container cannot open anything under this
plugin's app dir. So a sandboxed launch gets the variables and nothing else — no copy, no live
file, no skill, no reminder (`docs/harness-adapters.md` §Sandboxed sessions).

**2. What a container *can* read is whatever aoe seeds into it.** A sandboxed session's harness
config is a copy made at container create from a fixed list in aoe
(`AGENT_CONFIG_MOUNTS`, `src/session/config/container_config.rs`). A `user`-scope carrier is
written into those same directories under your home, so it arrives; a `session`-scope carrier
never leaves the app dir, so it does not. That list is aoe's, not this plugin's, and it is quoted
here as it stands in 1.21:

| harness | seeded files | seeded directories |
|---|---|---|
| `claude` | `settings.json`, `CLAUDE.md`, `keybindings.json` | `plugins`, **`skills`**, `hooks`, `agents`, `commands` |
| `codex` | `config.toml`, `auth.json`, `hooks.json`, **`AGENTS.md`** | — |
| `gemini` | `settings.json`, …, **`GEMINI.md`**, `.env` | — |
| `qwen` | `settings.json`, …, **`QWEN.md`**, `AGENTS.md` | — |
| `hermes` | `config.yaml`, `.env`, `auth.json`, **`SOUL.md`** | — |
| `droid` | `settings.json`, `settings.local.json`, `config.json`, `hooks.json`, `mcp.json` | — |
| `kiro` | — | `agents`, **`steering`**, `prompts`, `settings` |
| `opencode` | `opencode.json`, `opencode.jsonc`, `config.json` (from `.config/opencode`) | — |
| `pi` | `agent/models.json`, `agent/settings.json`, `agent/APPEND_SYSTEM.md`, … | `agent/extensions`, `agent/skills`, `agent/prompts`, … |
| `vibe` | `config.toml`, `.env`, `hooks.toml`, `AGENTS.md` | — |
| `cline`, `openclaw` | — (not in the list at all) | — |

A tag still travels in a sandbox only if the harness reads a **variable at request time** in one
of those files (`interpolates` in `adapters.toml`). A row that needs the literal in its file has
nowhere to get it from inside a container.

## The matrix

`✓` delivered, `—` not delivered, `n/a` the harness has no such thing. A cell is about the
*feature*, not about the plugin: `pi` gets no skill in a sandbox because the skill is in the copy,
not because the skill is missing from the build.

| harness | tag | skill | reminder | per-request | sandbox: tag | skill | reminder | per-request |
|---|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|
| `pi` | ✓ | ✓ | ✓ | ✓ | ✓ | — | — | — |
| `vibe` | ✓ | ✓ | ✓ | — | — | — | — | — |
| `claude` | ✓ | ✓ | ✓ | — | ✓ | ✓ | ✓ | — |
| `codex` | ✓ | ✓ | ✓ | — | ✓ | — | ✓ | — |
| `opencode` | ✓ | ✓ | ✓ | — | ✓ | — | — | — |
| `hermes` | ✓ | ✓ | ✓ | — | ✓ | — | ✓ | — |
| `gemini` | ✓ | ✓ | ✓ | — | ✓ | — | ✓ | — |
| `droid` | ✓ | ✓ | ✓ | — | ✓ | — | — | — |
| `qwen` | ✓ | ✓ | ✓ | — | ✓ | — | ✓ | — |
| `kiro` | — | n/a | ✓ | — | — | n/a | ✓ | — |
| `openclaw` | ✓ | — | — | — | — | — | — | — |
| `cline` | — | — | — | — | — | — | — | — |

Read down the two `per-request` columns: **no harness gets a per-request update in a sandbox.**
`pi`'s extension is a file in the copy, so a container never loads it; `opencode`'s plugin API is
unverified and ships nothing.

## Why each cell is what it is

**`pi` — the only row that is complete on the host and thin in a sandbox.** Host: the launch
writes this session's `models.json`, `APPEND_SYSTEM.md`, the skill and the extension, and exports
`PI_CODING_AGENT_DIR` and `AOE_LLM_TAGS_LIVE`, so an edit reaches the next request. Sandbox: the
tag still travels, because aoe seeds `~/.pi/agent/models.json` into the container and that header
is a **command value** (`!v="${AOE_LLM_TAGS-}"; …`) evaluated there; the skill, the reminder and
the extension do not, and the hook says so on stderr. This is the one row measured end to end
(`docs/harness-evidence/sandbox-pi.json`).

**`vibe` — the row that loses its tag in a sandbox.** It needs the literal in `config.toml` and
expands nothing, so only a copy can carry it. The hook says the tag is not sent rather than
closing the session quietly. Its carriers are `session`-scope for the same reason (`$VIBE_HOME`
points at a whole config dir).

**`claude` — the only row whose skill survives containerisation.** `skills/` is in the seeded
directories, so `~/.claude/skills/aoe-activity` is in the container, and `CLAUDE.md` — which
holds the marker block — is seeded too. The tag is env-only (`ANTHROPIC_CUSTOM_HEADERS`), so it
needs no file at all.

**`codex`, `qwen`, `hermes`, `gemini` — reminder yes, skill no.** Their instruction file is
seeded (`AGENTS.md`, `QWEN.md`, `SOUL.md`, `GEMINI.md`) but their skills directory is not, so an
agent that reads skills opens nothing. The reminder is the whole carrier there, which is why it
is not optional.

**`kiro`** ships no skill at all — it has no skill loader — so its carrier *is* the steering
document, and `steering/` is seeded: the one thing that arrives in a container is the reminder.
It has no channel for a tag on either substrate, so its tag column is `—` twice.

**`opencode` — tag yes, carriers no.** `~/.config/opencode/opencode.json` is seeded and its
header is `{env:VAR}`, so the tag travels; neither `AGENTS.md` nor `skills/` is seeded, so
neither carrier does. Its plugin API may offer a per-request equivalent one day; unverified, so
nothing is claimed.

**`droid`** is the same shape as opencode, with one more caveat: the binary refuses to send
anything without a Factory account, so its tag column is `documented`, not measured
(`docs/harness-adapters.md` §Support matrix). `~/.factory/AGENTS.md` is not seeded, so its
reminder stays on the host.

**`openclaw` and `cline`** have no destination at all: openclaw has a channel but no row in aoe's
seeding list and no carrier scope, cline has no write path and expands nothing. Both are refused
by name (`no_destination`) rather than given an invented path, and both appear in `status`,
`doctor` and the page with that reason.

## What the page may say

The page already carries `scope` and the file list per row. Three states cover every cell, and
each has one sentence that is true of the whole row:

| state | when | label | tooltip |
|---|---|---|---|
| `installed` | `session` scope, host launch | “Installed for launches on this machine” | “In this session's private config copy: `<files>`. A sandboxed launch reads none of it — aoe does not mount this directory into the container.” |
| `installed-sandbox` | `user` scope, carriers land in a seeded directory | “Installed in your own `<harness>` config” | “`<files>`. aoe copies this into a sandboxed session's container, so the skill and the reminder arrive there too.” |
| `installed-host-only` | `user` scope, carriers land in a directory aoe does not seed (`opencode`, `droid`) | “Installed in your own `<harness>` config” | “`<files>`. A sandboxed launch does not get these: aoe seeds only `<list>` for this harness.” |

Three things the page must never do, because each is a claim the code does not back:

1. **Show a per-request badge for a sandboxed row.** No harness has one there.
2. **Say “installed” for a harness whose carriers are unreachable in the substrate the session
   is running in.** `session`-scope rows are host-only; the marker is the label, not the fine
   print.
3. **Merge the tag with the carriers.** The tag can travel where the skill cannot (`opencode`,
   `droid`, `codex`) and the skill can arrive where the tag does not (`kiro`). One badge for
   “tagged” and one for “activity skill” is what the table actually says.

## Provenance

- **Measured** — `pi` sandbox: `tools/probe-sandbox.sh` → `docs/harness-evidence/sandbox-pi.json`
  (real container, `AOE_LLM_TAGS` present, copy absent, `pi --list-models` lists its provider).
- **Measured** — which files the plugin itself writes per row, host and sandbox, from the real
  binary: a host launch writes a copy for `pi`, `opencode`, `vibe` and exports
  `PI_CODING_AGENT_DIR` / `OPENCODE_CONFIG` / `VIBE_HOME`; `claude` and `gemini` get a variable
  and no file; `codex`/`hermes`/`droid`/`qwen`/`openclaw` get neither (their own config holds
  the reference); a sandboxed launch of every row prints `AOE_LLM_TAGS` and writes nothing.
- **Derived** — every sandbox cell that depends on aoe's seeding list: it is read from aoe's
  source, not probed per harness. The columns that are the plugin's own (`pi`, `opencode`,
  `vibe` copies; `interpolates`) are covered by `tests/`; the columns that are aoe's
  (`seed_dirs`) are not, and a change there changes this table without changing this plugin.
- **Re-measure** — `tools/probe-session.sh <agent>` for a host launch per harness,
  `tools/probe-sandbox.sh` for the container side.

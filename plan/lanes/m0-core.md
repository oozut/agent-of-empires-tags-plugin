# Lane `core` — branch `m0-core`

Owns `bin/`, `worker/`, the resolver and the CLI verbs. No aoe config, no harness config, no
verification runs. Everything here is testable offline, which is the point: this lane must not
need a working aoe install to be complete.

**Read first:** `plan/m0.md` §Task table, `plan/parameters.md` §3 (settings), §5 (value rules),
§6 (CLI surface), §7 (timeouts). `docs/implementation.md` §Step 1–2 has the resolver shape.

## Tasks, in order

| ID | Do | Output that proves it |
|---|---|---|
| M0-T01 | `aoe-plugin.toml`, `README.md`, `.gitignore`, `bin/` layout, per `parameters.md` §1 | `aoe plugin list` shows `dev.oozut.llm-tags` |
| M0-T02 | `bin/aoe-llm-tags`: load settings, resolve session facts, render every enabled header, print `NAME=value` | `hook --session <id>` prints one line per enabled header |
| M0-T03 | resolver tests: accumulate/dedupe/cap, grammar, `short_id`, template fields, env-name derivation, disabled headers, empty/malformed settings | `tests/` green with no aoe and no network |
| M0-T11 | `doctor`: every check in `docs/implementation.md` §Step 5, absolute paths, exit 1 on failure | exit 0 clean; exit 1 naming each of five deliberate breaks |
| M0-T04 | classification tests for `doctor` | one green case per breakable check |

## Facts you must not re-derive

- Resolution order is **row via `aoe session show "$AOE_SESSION_ID" -p "$AOE_PROFILE" --json`**,
  then flags, then defaults. 2 s bound, exit 0 and fall back to hook env on any failure. Never let
  a slow or missing `aoe` abort a session start.
- `hook` prefers the values aoe already injected; it shells out only for fields the env lacks
  (structured host sessions have no group/title).
- All values are launch-time. Do not try to make a value live — the harness caches it.
- One resolver module. `hook`, `value`, `doctor` and later `status` all call it, or they drift.

## Handoff

- Loudly, when T05 (`setup --print`) lands: that is `m0-wire`'s starting gun.
- Quietly: nothing else here is consumable by another lane until the milestone's proof runs.

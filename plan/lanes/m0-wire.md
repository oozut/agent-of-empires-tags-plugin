# Lane `wire` — branch `m0-wire`

Owns the two places a value becomes a header: the aoe hook configuration and the harness's own
header config. No resolver code — if the printer is wrong, that is `m0-core`'s task, not a patch
here.

**Read first:** `plan/m0.md` §Task table T06/T07/T10, `plan/parameters.md` §2 (hook contract), §3
(header set), `docs/harness-adapters.md` (the pi row and the "launchers that regenerate the config"
section), `docs/implementation.md` §Step 2–3.

## Do now, before `core` merges T05

Draft the exact lines and paths, so the merge is a paste and not a research session:

- the two `[host_hooks]` lines, with the absolute `aoe` path resolved (not `aoe` unqualified);
- the pi mapping: which file, which key (`providers.<id>.headers`), and the fact that the entry
  must keep a **non-empty `models` list** or the wrapper discards it wholesale;
- the substrate note: `before_session` applies at launch, `before_start` at container start, and a
  sandbox seeded from an unwired store stays unwired (`sandbox-stale`);
- the second header's line, for T10.

Put the drafted lines in the lane's PR description, not in code.

## Tasks, once T05 is on `main`

| ID | Do | Output that proves it |
|---|---|---|
| M0-T06 | install both hook keys in the global `config.toml`, same command string | `env` inside a session shows the tag variables |
| M0-T07 | create the harness config with the header mapping (create, do not patch an absent file; back up if present) | harness config has the header, `models` list still non-empty |
| M0-T10 | enable the second header, confirm independence | two independent header lines in the sink log |

## Boundaries

- `[host_hooks]` is **global/profile config only** — never repo config. This is the user's
  one-time step; the plugin cannot install it. Write the instructions, do not invent an installer.
- Do not add a proxy, a wrapper script, or an env var the harness does not read. The header rides
  the harness's own capability or it does not ship.
- If a launcher regenerates the config file on every run (this host's pi wrapper does), the mapping
  belongs in the file the launcher reads *from*, and the fact belongs in
  `docs/harness-adapters.md` — never a special case in the resolver.

## Handoff

- To `m0-proof` when **both** T06 and T07 are merged: they are two halves of one wiring, and a
  sink run proves only the half that is actually installed.

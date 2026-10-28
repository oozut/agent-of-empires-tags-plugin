# Lane `docs` — branch `m0-docs`

Owns `docs/install.md` and this milestone's user-facing text. No `bin/`, no config files: if the
install page has to explain a workaround, the workaround is the bug and belongs to `core` or
`wire`.

**Read first:** `docs/implementation.md` §Step 2–3 (what the lines actually are), `plan/m0.md`
§Acceptance, `docs/header-contract.md` (what a reader is promised, in the order it is promised).

## Do now, before `core` merges T05

Draft the page's skeleton from `docs/implementation.md` and mark the command output as `TODO`:

1. one paragraph: what the header is and what reads it (no vendor named);
2. paste the two `[host_hooks]` lines;
3. the harness mapping for the first supported harness;
4. `aoe-llm-tags doctor` and what green looks like;
5. how to read one header off a request, for someone who does not control their consumer.

## Task, once T05 is on `main`

| ID | Do | Output that proves it |
|---|---|---|
| M0-T12 | `docs/install.md`: paste these lines, run this, expect this | a reader reproduces the setup on a machine that never had it |

## Rules

- **Three commands maximum before the first success signal.** If it takes more, the install is
  wrong, not the reader.
- **Every command is copy-pasteable and shows its expected output.** No "configure your harness"
  without saying which file and which key.
- **Name the substrate.** A sandboxed session takes the hook at container start and never re-syncs;
  a reader who wires the host and tests in a sandbox will conclude it is broken.
- **Never promise liveness.** The value is minted at launch. Say so where a user would otherwise
  expect an edit to apply immediately.

## Handoff

- When the page is written, walk it once from a clean shell and fix the page, not the memory of
  what you did. That walk is the acceptance evidence.

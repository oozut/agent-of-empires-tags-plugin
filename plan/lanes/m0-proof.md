# Lane `proof` — branch `m0-proof`

Owns the two runs that turn "it should work" into "it does". No production code: this lane may add
`tools/` fixtures and edit `docs/reference-deployment.md`, nothing else.

**Read first:** `plan/m0.md` §Acceptance, `docs/implementation.md` §Step 6 (the two-leg procedure and the fixture
it ships), `docs/reference-deployment.md` (where a local record of the hop goes).

## Do now, before `wire` lands

Build and self-test the sink fixture — it depends on nothing:

- a stdlib-only HTTP server on `127.0.0.1:8787` that appends one JSON line per request to
  `/tmp/tag-sink.jsonl`: timestamp, method, path, and **all** request headers;
- a self-test that POSTs a header and reads it back, so the fixture is trusted before it is used as
  evidence;
- a `--reset` to clear the log between runs, and no body logging at all.

## Tasks, once `wire` merges T06 + T07

| ID | Do | Output that proves it |
|---|---|---|
| M0-T08 | **mechanism leg:** one request from inside a session, isolated harness config pointed at the sink, no proxy in the path | one sink line carrying the header, `feature` and `session` tokens present |
| M0-T09 | **consumer leg — deferred** (`plan/roadmap.md` §1, decision #4): the same session through its normal launcher on its real path | the header visible at the first hop that records requests; record the hop and the exact query in `docs/reference-deployment.md`. Not needed for M0: the plugin delivers, it does not read a consumer back |

## Rules of evidence

- **Two legs, never merged.** The sink proves the plugin mints and the harness sends; the consumer
  would prove the real path delivers. Only the first is in scope (decision #4, 2026-10-29): a pass
  on one is never reported as a pass on the other, and the consumer leg is roadmap, not a gate.
- **Record the command, not the conclusion.** Every claim in the milestone's evidence table is a
  command plus its output, with the timestamp and the session id.
- **A precondition failure is a recorded result.** If the consumer is unreachable or the sandbox
  runtime is denied, say so in the evidence table with what *was* proven (e.g. the minting half),
  and leave the rest `pending`. Do not simulate a hop.
- Do not "fix" a failure here. A stripped header is a consumer-config finding for the report; a
  wrong value is `core`; a missing variable is `wire`.

## Handoff

- The evidence table in `plan/m0.md` is the milestone's exit gate — fill the rows you own, and hand
  the failures back with the lane that owns them.
- After M0 closes, the same fixture is reused by M2's `verify` capability probe; keep it runnable
  from a clean checkout.

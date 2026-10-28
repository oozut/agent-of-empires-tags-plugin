# Lane briefs

One brief per M0 lane, so a lane can be worked in its own session without reading the whole plan.
Lane = the unit of parallel work (`plan/README.md` §How tasks are split). Branch and session names
below are the ones created for this milestone.

| Lane | Branch | Tasks | Can start |
|---|---|---|---|
| `core` | `m0-core` | M0-T01, T02, T03, T11, T04 | now |
| `wire` | `m0-wire` | M0-T06, T07, T10 | `core` merges T05 |
| `proof` | `m0-proof` | M0-T08, T09 | `wire` merges T06 + T07 |
| `docs` | `m0-docs` | M0-T12 | `core` merges T05 |

Rules that hold for all four:

- **Stay inside the lane.** Being outside your brief is how two sessions start editing `bin/` at
  once. If your lane needs a change in another lane's files, write it in the brief's "handoff"
  line and stop — the dependency moves, the lane does not.
- **One task = one commit**, message names the task ID (`M0-T02: resolve and render headers`).
- **Merge to `main` when a task's output exists**, so the lane that depends on it unblocks. Do not
  batch a whole lane into one merge.
- **Report the command and its output** for the task's `Output` column. That is the only accepted
  completion signal.
- Acceptance evidence goes in the milestone file, not in the lane: `plan/m0.md` §Acceptance.

# Plan

The executable plan: one milestone per file, each with its tasks (priority-tagged, lane-tagged,
sized), its **parameters**, and its evidence. The design lives in `docs/` and is normative over
this folder — this folder says what to build first, with which values.

| File | Contents |
|---|---|
| `milestones.md` | The ladder: constraints, M0–M4, acceptance criteria, open decisions |
| `m0.md` | Prove a header on a real session — tasks, task table, parameters, evidence |
| `m1.md` | Settings and attribution UI |
| `m2.md` | Auto-discovery and one-click wiring |
| `m3.md` | Usage views |
| `m4.md` | Remaining harnesses and distribution |
| `graph.md` | Mermaid views: the ladder, one dependency graph per milestone, the M0 lane split, a relative-effort schedule |
| `lanes/` | One brief per M0 lane (`core`, `wire`, `proof`, `docs`, `release`) — scope, read-first list, tasks, handoff, so a lane can be worked in its own session |
| `parameters.md` | Every parameter in one place: name, type, default, allowed values, scope, milestone, where it is read |
| `roadmap.md` | Deferred work with the decision that has to be taken first: consumer read-back (`verify`, M3), signed session ids, Windows assets, tag-based release verification |

Design docs: `docs/header-contract.md` (what is promised) → `plan/milestones.md` (what ships when)
→ `docs/implementation.md` (where the code goes) → `docs/harness-adapters.md` (per-harness data) →
`docs/reference-deployment.md` (one stack, non-normative).

## Priorities

Milestones stay in dependency order: M0 → M1 → M2 → M3, with M4 rows available from M0 onward.
Priority orders the work **inside** a milestone; it never lets a later milestone jump the queue.

| Tag | Meaning | Test to apply | Consequence |
|---|---|---|---|
| **P0** | The milestone cannot be called done without it — an acceptance criterion depends on it. | "If I delete this, which acceptance line fails?" If none, it is not P0. | Blocks calling the milestone complete, and blocks the next milestone from starting. |
| **P1** | Needed to use the milestone for real, or by the next milestone. Not needed to prove it. | "Can the next milestone start without it?" If yes and no acceptance line names it, it is P1. | May land just after the milestone's acceptance, in the same release; tracked as open. |
| **P2** | Improvement. Nothing in the acceptance changes if it slips. | "Would I notice in a month?" If only then, P2. | May move to a later milestone without re-planning. |

Priority is not importance, it is **ordering under the gate**. A P2 can be more valuable than a P0;
it still comes after, because the P0 is what makes the value provable.

Three rules keep the tags honest:

- **A P0 task may not depend on an unfinished P1 task.** If it does, one of the two tags is wrong;
  fix the tag, not the ordering.
- **Anything not tagged is not planned.** If implementation needs work no task describes, add the
  task (with priority, lane, size) before doing it, in the same change.
- **Priority is revisable, silently.** When a P1 turns out to block an acceptance line, promote it
  to P0 in the milestone file in the same commit that discovers it.

## How tasks are split

Every task is one **verifiable output** — one command, its expected result, and one commit. That is
the sizing test, not an aesthetic one:

- If verifying a task needs two unrelated commands, it is two tasks.
- If a task has no command that distinguishes done from not-done, it is not yet a task: write the
  check first, then the task.
- Target size: **S** ≤ 2 h, **M** ≤ half a day, **L** ≈ a day. Anything L that is not one coherent
  change (the consumer adapter, the harness-verification sweep) gets split before work starts.

Each task carries four facts in its milestone file's task table, so it can be picked up cold:

| Column | Why it is there |
|---|---|
| **ID** (`M0-T07`) | Stable handle for commits, notes and cross-references. Never renumber; append instead. |
| **Priority** | P0/P1/P2 from the table above. |
| **Lane** | Which part of the tree it touches — see below. Lanes are the parallelism unit. |
| **Depends** | Task IDs that must be merged first. Empty means ready now. |
| **Size** | S/M/L, for batching the small ones. |
| **Output** | The one thing you can point at when it is done. |

Lanes exist so work can run in parallel without merge pain:

| Lane | Touches | Typical tasks |
|---|---|---|
| `core` | `src/`, `Cargo.toml`, resolver, CLI verbs, discovery logic | CLI core, `doctor`, `status`, `setup` apply, adapter interface |
| `wire` | aoe config and harness config on the host; nothing in `bin/` | hook install, harness mapping, second header |
| `ui` | `aoe-plugin.toml` settings/UI declarations, worker view code | manifest, settings UI, badge, pane, page |
| `proof` | runs and records only — no production code | sink run, consumer run, `verify` |
| `adapters` | `adapters.toml` rows and their research notes | row statuses, row verification |
| `release` | `Cargo.toml`, `[runtime]` + `capabilities`, `tools/build-worker.sh`, `.github/workflows/` | binary worker, release-binary asset, CI matrix, `aoe_version`, publish path |
| `docs` | `docs/`, `README.md` | install page, contract updates |

Rules that make the lanes pay off:

- **P0 tasks in different lanes may run at the same time.** Same-lane P0 tasks are sequential.
- **A lane that is waiting is a signal, not a reason to idle.** If `proof` is blocked on `wire`,
  pick up the next `core` P0 — the critical path is almost always one lane waiting on another.
- **A task that touches two lanes is a task that will conflict.** Split it: the `core` half and the
  `wire` half, with the wire half depending on the core half.

## Ready now

Each M0 lane is its own session and its own branch (`plan/lanes/README.md`). Within a lane, the
queue is:

| Lane | Branch | First task | Then | Gated on |
|---|---|---|---|---|
| `core` | `m0-core` | M0-T01 scaffold | T02 → T03/T11 → T04 | nothing — start now |
| `wire` | `m0-wire` | draft the exact lines (no code) | T06, T07 → T10 | `core` merges T05 |
| `proof` | `m0-proof` | build `tools/header-sink.py` + self-test | T08, T09 | `wire` merges T06 + T07 |
| `docs` | `m0-docs` | draft `docs/install.md` skeleton | T12 | `core` merges T05 |
| `release` | `m0-release` | Rust crate + `[runtime]` (`m0-release.md`) | T13 | `core` merges T05 + T11 |

M4-T01 (adapter row statuses) is also unblocked and shares no files with any of them, if a fifth
worker is wanted. Everything else in M0 waits on `M0-T02` (the resolver every other verb calls) or
on a host-side precondition (`plan/m0.md` §Preconditions).

## What ships if only P0 lands

Read this as the acceleration target: the P0 column is a releasable product at every milestone, so
a slip in P1/P2 never leaves a half-built layer that blocks the next milestone.

| Milestone | P0 tasks | The shippable core |
|---|---|---|
| M0 | 11 of 13 | Header minted at launch, delivered by both hook keys, visible at a local sink, `doctor` that names any broken piece |
| M1 | 4 of 8 | Header set and rules editable in aoe, badge showing the resolved value, worker + capabilities declared once |
| M2 | 4 of 8 | Path detection, per-substrate verdicts, diff → apply → `undo` (`verify` is roadmap-gated) |
| M3 | 3 of 6 | Consumer adapter, page board, honest degradation — **roadmap-gated**, cannot start before a consumer is named |
| M4 | 2 of 6 | Honest row statuses for every target, install from the published source |

## How to use a milestone file

1. Read the entry in `milestones.md` for *why*, then the `mN.md` file for *what exactly*.
2. Work P0 top to bottom, then P1, then P2 — but take any ready P0 from any lane rather than
   waiting. The ladder constrains milestones, not lanes.
3. Implement against the parameters table — a value not listed there is either a bug in the
   implementation or an amendment to `parameters.md` (add it in the same commit that needs it).
4. Tick acceptance only with the evidence named in "Done when". "It works on my machine" is not
   evidence; a command and its output is.
5. When a milestone reveals a parameter that should change, update `parameters.md` **and** the
   design doc it contradicts, in one change, so the two never drift.

## Status

| Milestone | State | Blocking |
|---|---|---|
| M0 | **done** — P0 complete (T01–T03, T05–T08 incl. sink + sandbox legs, T10, T11, T13); P1 T04/T12 done; T09 (consumer leg) deferred to `roadmap.md` (decision #4) | `release-binary` verifiable only from a GitHub source, not a local checkout |
| M1 | in progress — manifest (caps, ui slots, settings), resolver M1 match kinds + priority, worker protocol; outbound badge publishing + pane deferred | M0 |
| M2 | not started | M1 |
| M3 | not started — roadmap-gated (`roadmap.md` §1) | M2 + a named consumer with a read path |
| M4 | not started | M0 (P0 rows and publish path can start earlier) |

Pre-M0 setup work that is not a milestone: `plan/m0.md` §Preconditions.

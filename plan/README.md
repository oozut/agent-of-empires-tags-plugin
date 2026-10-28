# Plan

The executable plan: one milestone per file, each with its tasks (priority-tagged), its
**parameters**, and its evidence. The design itself lives in `docs/` and is normative over this
folder — this folder says what to build first, with which values.

| File | Contents |
|---|---|
| `milestones.md` | The ladder: constraints, M0–M4, acceptance criteria, open decisions |
| `m0.md` | Prove a header on a real session — tasks, parameters, evidence |
| `m1.md` | Settings and attribution UI — tasks, parameters, acceptance |
| `m2.md` | Auto-discovery and one-click wiring — tasks, parameters, acceptance |
| `m3.md` | Usage views — tasks, parameters, acceptance |
| `m4.md` | Remaining harnesses and distribution |
| `parameters.md` | Every parameter in one place: name, type, default, allowed values, scope, milestone, where it is read |

Design docs: `docs/header-contract.md` (what is promised) → `plan/milestones.md` (what ships when)
→ `docs/implementation.md` (where the code goes) → `docs/harness-adapters.md` (per-harness data) →
`docs/reference-deployment.md` (one stack, non-normative).

## Priorities

Milestones stay in dependency order: M0 → M1 → M2 → M3, with M4 rows available from M0 onward.
Priority orders the work **inside** a milestone; it never lets a later milestone jump the queue.

| Tag | Meaning | Consequence |
|---|---|---|
| **P0** | The milestone cannot be called done without it — an acceptance criterion depends on it. | Blocks calling the milestone complete, and blocks the next milestone. |
| **P1** | Needed to use the milestone for real, or by the next milestone. Not needed to prove it. | May land just after the milestone's acceptance, in the same release; tracked as open. |
| **P2** | Improvement. Nothing in the acceptance changes if it slips. | May move to a later milestone without re-planning. |

Two rules keep the tags honest:

- **A P0 task may not depend on an unfinished P1 task.** If it does, one of the two tags is wrong;
  fix the tag, not the ordering.
- **Anything not tagged is not planned.** If implementation needs work no task describes, add the
  task (with a priority) before doing it, in the same change.

## What ships if only P0 lands

| Milestone | P0 tasks | The shippable core |
|---|---|---|
| M0 | 9 of 12 | Header minted at launch, delivered by both hook keys, visible at a local sink and at the real consumer, `doctor` that names any broken piece |
| M1 | 4 of 8 | Header set and rules editable in aoe, badge showing the resolved value, worker + capabilities declared once |
| M2 | 5 of 8 | Path detection, per-substrate verdicts, diff → apply → `undo`, `verify` |
| M3 | 3 of 6 | Consumer adapter, page board, honest degradation |
| M4 | 2 of 6 | Honest row statuses for every target, install from the published source |

## How to use a milestone file

1. Read the entry in `milestones.md` for *why*, then the `mN.md` file for *what exactly*.
2. Work P0 top to bottom, then P1, then P2. A P1 that the next milestone needs becomes P0 in that
   milestone — do not carry it silently.
3. Implement against the parameters table — a value not listed there is either a bug in the
   implementation or an amendment to `parameters.md` (add it in the same commit that needs it).
4. Tick acceptance only with the evidence named in "Done when". "It works on my machine" is not
   evidence; a command and its output is.
5. When a milestone reveals a parameter that should change, update `parameters.md` **and** the
   design doc it contradicts, in one change, so the two never drift.

## Status

| Milestone | State | Blocking |
|---|---|---|
| M0 | not started | reference consumer for proof #5; sandbox runtime access (deployment) |
| M1 | not started | M0 |
| M2 | not started | M1 |
| M3 | not started | M2 |
| M4 | not started | M0 (P0 rows and publish path can start earlier) |

Pre-M0 setup work that is not a milestone: `plan/m0.md` §Preconditions.

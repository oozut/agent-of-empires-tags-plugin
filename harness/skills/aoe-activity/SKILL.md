---
name: aoe-activity
description: Keep this session's Agent of Empires activity current. Use when the work moves into a new phase — exploring the code, planning, writing code, running tests, debugging a failure, reviewing, writing docs, or ops — so the session's badge, its pane and the request headers it sends all say what it is doing right now.
---

# Keep the session's activity current

Agent of Empires shows one word for what a session is doing: **exploring, planning, coding,
testing, debugging, reviewing, writing docs, ops, idle**. It is a tag, not a log, so it is cheap
to change and it is meant to be changed — several times in a session, whenever the work moves on.

## How to set it

```sh
"${AOE_LLM_TAGS_BIN:-aoe-llm-tags}" activity testing
```

`AOE_LLM_TAGS_BIN` is exported for you when the session runs under AoE. The argument is one of
`explore plan code test debug review docs ops idle` (bare, as above) or `activity:test`.

- `activity --show` prints the current value and the vocabulary.
- `activity --clear` removes it; a session with no activity shows its ordinary tags.
- An unknown word exits non-zero and says nothing after that: do not retry with a guess.

## When to change it

Change it when the *work* changes phase, not on every tool call:

| Situation | Activity |
| --- | --- |
| reading the codebase, grepping, mapping a subsystem | `explore` |
| agreeing a design, writing a plan or task list | `plan` |
| editing or creating source files | `code` |
| writing or running unit/integration tests, or the build | `test` |
| chasing a failure to its cause | `debug` |
| reading a diff for correctness, reviewing someone's branch | `review` |
| README, docs, changelog, comments | `docs` |
| dependencies, packaging, CI, deploys, migrations | `ops` |
| waiting on the user, or nothing in flight | `idle` |

Do it **early** in a phase — before the long part, not after — because the value is what the
person watching the session sees while they wait. If a phase has no name in the list, pick the
closest one rather than inventing a value: the list is closed on purpose.

## What it is not

- Not a status report, not a summary, not free text. One word from the list.
- Not a promise: if you announced `testing` and then went back to editing, set `code` again.
- The person on the dashboard has the last word. If your activity looks wrong to them they will
  set it, and your next change simply overwrites theirs — that is expected, so read `--show`
  before writing if you have been running a while.

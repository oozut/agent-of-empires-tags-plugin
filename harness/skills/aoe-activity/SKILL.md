---
name: aoe-activity
description: Keep this session's Agent of Empires activity current. Use when the work moves into a new phase — exploring the code, planning, writing code, running tests, debugging a failure, reviewing, writing docs, or ops — so the session's badge, its pane and the request headers it sends all say what it is doing right now.
---

# Keep the session's activity current

Agent of Empires shows one word for what a session is doing. It is a tag, not a log, so it is
cheap to change and it is meant to be changed — several times in a session, whenever the work
moves on.

The words are a **family**, defined for this session (and often for the whole repository), so do
not assume a particular set:

```sh
"${AOE_LLM_TAGS_BIN:-aoe-llm-tags}" activity --show
```

That prints the family's name and every word it allows, with where the definition came from. Run
it once, early, before you announce anything — the family may be the built-in nine, or a
vocabulary this project invented, and both are equally valid.

## How to set it

```sh
"${AOE_LLM_TAGS_BIN:-aoe-llm-tags}" activity <word>
```

`AOE_LLM_TAGS_BIN` is exported for you when the session runs under AoE. `<word>` is one of the
words `--show` printed, bare (`activity test`) or prefixed (`activity:test`). The label is
accepted too if it reads better off the badge (`activity testing`), but it is stored as the
family's own word.

- `activity --show` prints the current value, the family and what it allows.
- `activity --clear` removes it; a session with no activity shows its ordinary tags.
- An unknown word exits non-zero and names the words that exist: do not retry with a guess.

## When to change it

Change it when the *work* changes phase, not on every tool call. The family says when each word
is due — `activity --show --json` carries a `vocabulary` array whose entries have a `when`
sentence, and that sentence is the rule you follow. The built-in family reads like this:

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
person watching the session sees while they wait. If the work has no word of its own, pick the
closest one rather than inventing a value: the family is closed on purpose, and a word it does
not define gets no icon and is reported as unknown.

Nothing moves the activity for you. A future release can act on these sentences automatically;
until then the change is yours to make.

## What it is not

- Not a status report, not a summary, not free text. One word from the family.
- Not a promise: if you announced `test` and then went back to editing, set `code` again.
- The person on the dashboard has the last word. If your activity looks wrong to them they will
  set it, and your next change simply overwrites theirs — that is expected, so read `--show`
  before writing if you have been running a while.

# Install

Everything on this page is copy-pasteable. Three commands get you to the first success signal
(`doctor` green); the rest is verification and the things that go wrong.

Names used below:

- `<app_dir>` — the aoe app directory (`aoe` prints it; on this host
  `/var/lib/aoe/.config/agent-of-empires`).
- `<install>` — `<app_dir>/plugins/dev.oozut.llm-tags`, where the plugin lands.
- `<cli>` — `<install>/bin/aoe-llm-tags`, the only entry point you run by hand.

## 1. Install the plugin

From a release (downloads a tarball, no toolchain needed):

```sh
aoe plugin install gh:oozut/agent-of-empires-tags-plugin
```

From a checkout (the host compiles it, so this needs Rust ≥ 1.89 somewhere `cargo` can be found):

```sh
aoe plugin install /path/to/agent-of-empires-tags-plugin
```

Expect, in both cases:

```
dev.oozut.llm-tags 0.1.0 ... enabled
```

`aoe plugin install` prompts once for `runtime.worker` — that capability is what lets the host run
the plugin's worker binary, and the answer has to be yes. If a later update changes the
capability set, re-run with `--yes` to re-approve it without a prompt.

## 2. Print the wiring lines

```sh
"<install>/bin/aoe-llm-tags" setup --print
```

It prints exactly three kinds of line and nothing else:

```
before_session = 'AOE_BIN="/usr/local/bin/aoe" "<install>/bin/aoe-llm-tags" hook --session "$AOE_SESSION_ID" …'
before_start   = 'AOE_BIN="/usr/local/bin/aoe" "<install>/bin/aoe-llm-tags" hook --session "$AOE_SESSION_ID" …'
<harness config> → providers.<id>.headers["X-AoE-Tags"] = "!v=\"${AOE_LLM_TAGS-}\"; [ -n \"$v\" ] && printf %s \"$v\" || printf %s {}"
```

The mapping value is a **command**, not `$AOE_LLM_TAGS`, for a harness that fails a request over a
header it cannot resolve (`pi`: `resolveHeadersOrThrow`). The command prints the variable while
the hook mints it and `{}` once the plugin is gone, so the line survives an uninstall instead of
breaking the first launch — including a launch the plugin never sees, from a plain shell. Rows
without such a `guard_fallback` keep the plain env reference. Paste the printed value as-is:
for a JSON config the quotes inside a command value are already escaped.

1. Put both `before_*` lines under `[host_hooks]` in `<app_dir>/config.toml`. Both keys, the same
   string: `before_session` covers host sessions, `before_start` covers sandboxed ones. If the
   profile has a `[host_hooks]` array of its own, it **replaces** the global one — put the lines in
   the profile file too, or only there.
2. Add the mapping line(s) to the harness config named, one line per enabled header, leaving the
   rest of that file alone. For `pi` that means `~/.pi/agent/models.json`,
   `providers.<id>.headers["X-AoE-Tags"] = "!v=\"${AOE_LLM_TAGS-}\"; …"` (copy the value the
   printer emitted, escapes included), and the provider entry must keep a
   non-empty `models` list or the wrapper drops the whole entry, headers included. The value is a
   JSON object (`{"session":"S-cf11","team":"core"}`), so the consumer parses it rather than
   splitting a comma list; `setup --print` also shows a `csv` target for a consumer that still
   wants `team:core,feature:billing`.

Where a harness's config cannot hold a `$VAR` (or has no config file at all), the plugin does not
ask you to edit it: for `claude` the value rides `ANTHROPIC_CUSTOM_HEADERS` in the launch
environment, and for a harness that only accepts literals the plugin generates a config inside
the session's own directory. Your own config is never rewritten — `setup --print` tells you which
of the three applies.

`setup --print` never writes anything. Editing those files is yours, deliberately.

## 3. Check it

```sh
"<install>/bin/aoe-llm-tags" doctor
```

Expect `0 failed` and one line per check, each naming the absolute path it looked at:

```
  [ok  ] before_session hook    AOE_BIN="/usr/local/bin/aoe" "<install>/bin/aoe-llm-tags" hook …
  [ok  ] mapping X-AoE-Tags     ~/.pi/agent/models.json → litellm (value: !v=\"${AOE_LLM_TAGS-}\"; …, models: 1)
  [ok  ] value rendering        X-AoE-Tags=AOE_LLM_TAGS={"session":"S-cf11","feature":"","team":"core"}
  [skip] last verification      consumer read-back is out of scope for this build; prove arrival …
```

`[skip] last verification` is expected and permanent: this build mints and delivers the header, it
does not ask a gateway or a billing system what it did with it (`plan/roadmap.md` §1). Prove
arrival with the fixture in §5. Every `warn`/`fail` names the piece to fix.

Exit codes: `0` all good (warnings allowed), `1` at least one check failed, `2` bad usage.

## 4. See a value

Inside a session (or with the variables exported by hand):

```sh
env | grep '^AOE_'
# AOE_LLM_TAGS={"session":"S-cf11","session_id":"cf11…","tool":"pi",…,"team":"core"}
```

Nothing there means the hook line is absent, in the wrong profile, or exited non-zero — `doctor`
says which. The hook never blocks a session start: on any failure it writes a line to stderr and
exits `0`.

## 5. Prove it leaves the machine (optional)

The plugin ships a fixture that records headers, so "it is sent" can be a file and a grep:

```sh
python3 tools/header-sink.py 8787 /tmp/tag-sink.jsonl --reset &
# point a harness at http://127.0.0.1:8787/v1 with headers
# {"X-AoE-Tags": "!v=\"${AOE_LLM_TAGS-}\"; [ -n \"$v\" ] && printf %s \"$v\" || printf %s {}"},
# send one request, then:
grep -o '"x-aoe-tags": "[^"]*"' /tmp/tag-sink.jsonl
```

That run is the cheapest verification: the payload leaves the harness with the value aoe's hook
minted for the session. The repeatable form is `tools/probe-harness.sh <agent>` (one harness, in a
throwaway `HOME`) and `tools/probe-session.sh <agent>` (a real aoe session per harness); both write
their evidence to `docs/harness-evidence/`. A worked example, with the exact commands and the two traps (use the
real `pi`, not a wrapper that regenerates its config; the fixture must answer streaming requests),
is in `docs/reference-deployment.md` §Proof runs.

## 6. Optional: let a running session report changes (advanced)

Everything above is launch-scoped: the values are fixed when the session starts, because a harness
reads its config once. If you edit a tag while a session runs, the next request still carries the
old one until you relaunch — and a session that moves from planning to debugging says nothing about
it.

**This plugin can install two things into a harness to fix that.** It is off by default and it
never happens unless you approve the exact set of files, by digest.

1. a **skill** that tells the agent to keep the session's activity current, and
2. the same one-line reminder in the harness's always-loaded instructions, because a skill the
   model never opens changes nothing.

Where those two go depends on the harness, and the difference is the whole consent model:

| Scope | Harnesses | Where the files land |
|---|---|---|
| session copy | `pi`, `vibe` | inside the private config copy this plugin already builds per session — nothing of yours is touched. `pi` also gets the request hook (below); `vibe` does not have one |
| your own config | `claude`, `codex`, `opencode`, `hermes`, `gemini`, `droid`, `qwen`, `kiro` | inside your own harness config under your home directory: a skill directory of its own, and a block between two marker lines in the file the harness reads into every conversation |

Those harnesses have no per-session config copy this plugin can write into, so a session's activity
can only reach them through your own configuration. That is a wider reach than a session copy, and
the disclosure for them says so in those words before anything is written.

What a `pi` session gets, in plain words:

| Where | What |
|---|---|
| `…/sessions/<id>/pi/extensions/aoe-llm-tags.ts` | the hook: pi loads it inside the agent process, with the agent's own permissions, and it sets the `X-AoE-*` headers before each model request |
| `…/sessions/<id>/pi/skills/aoe-activity/SKILL.md` | a skill that tells the agent to keep the session's activity current as the work moves between phases |
| `…/sessions/<id>/pi/APPEND_SYSTEM.md` | the same reminder in pi's always-loaded instructions |
| `<app dir>/skills/aoe-activity/` | the same skill in AoE's managed store, so it shows on the Skills page and can be read, edited or deleted there |

and what a user-scope harness gets, taking `claude` as the example:

| Where | What |
|---|---|
| `~/.claude/skills/aoe-activity/SKILL.md` | the skill, in the directory claude looks for skills |
| `~/.claude/CLAUDE.md` | the reminder, between `<!-- aoe-llm-tags:begin -->` and `<!-- aoe-llm-tags:end -->`. Your own rules in that file stay byte for byte: only the block is written, only the block is removed |
| `<app dir>/skills/aoe-activity/` | the same skill in AoE's managed store |

Both scopes also write the skill into AoE's managed store, so the Skills page lists it as
`aoe-managed`. AoE scans the harness roots it knows, so on the user-scope rows the installed copy
shows up there a second time as an *external* skill: two rows, one file the plugin owns and one the
agent reads.

### The same directories `aoe skill sync` writes

AoE's own skill system knows a skills root for six agents (`claude-user`, `agents-standard`,
`gemini-user`, `opencode-user`, `kimi-legacy`, `prime-agent-user`) and deploys managed skills into
them when you ask it to. Some of those roots are the ones this plugin uses, so the two can meet:

- `aoe skill sync` reports our copy as `conflict — a skill AoE does not manage already exists here`.
  That is AoE saying what is true: the directory is ours, and AoE will not adopt it without being
  told to. `aoe skill sync --replace aoe-activity` hands it over; from then on AoE tracks it, and
  our next approval rewrites the same bytes (AoE then reports `unchanged`).
- If a later plugin version ships different bytes, our approval writes them and AoE reports the
  copy as `drifted — the propagated copy was edited in place; preserving it`. Nothing is lost; AoE
  simply stops updating that copy. `--replace aoe-activity` gives it back to AoE.
- `extension disable` removes the reminder from the instruction file, but leaves a skill directory
  that carries AoE's own `.aoe-managed.json` marker where it is, and prints `kept: …` with the
  reason. AoE retires it at its next `aoe skill sync` (reported as `removed`), because the managed
  skill it came from is gone. A directory this plugin wrote (no AoE marker) is deleted, as before.

The reverse direction is not available to us: a plugin host cannot call AoE's `skills.*` methods (it
has no such RPC) and AoE does not deploy anything automatically, so this plugin writes its carriers
itself rather than waiting for a sync that may never run.

The hook reads one JSON file this plugin writes (`live.json`: the session's tags and its current
activity) and sets the headers from it. The skill tells the agent to run `aoe-llm-tags activity
<word>`, which writes nothing but this session's own tag. Neither reads your prompts, transcript,
files or credentials, opens a socket or writes anywhere else. Your own `~/.pi/agent` is never
written to — the session copy's directories hold links to your entries plus these files. If you
already have an `APPEND_SYSTEM.md` there it is *appended to*, not replaced; a managed skill of the
same name that carries no plugin marker is left alone.

Only `pi` re-reads the value per request, so only `pi` shows an activity change mid-session. For
every other harness the change reaches the agent at the next launch; the badge, the pane and the
page change at once either way.

The plugin's page (Dashboard → plugins → LLM Tags) shows the same disclosure for each harness, with
a checkbox and an enable button — the page sends back the digest it displayed, so a stale page
approves nothing. On the command line:

```sh
"<install>/bin/aoe-llm-tags" extension enable pi --print     # the disclosure + the sha256
"<install>/bin/aoe-llm-tags" extension enable pi --accept <sha256>
"<install>/bin/aoe-llm-tags" extension status                # what each harness is doing
```

Three things end the approval — a change to the bytes of any file in the set, a change to the
disclosure's wording, or:

```sh
"<install>/bin/aoe-llm-tags" extension disable claude
```

A plugin update, a reinstall or a harness upgrade does **not** end it: the approval records the
files and the wording, not the version. `extension disable` removes both carriers — the skill
directory and the marker block, leaving the rest of the file as it was, and deleting the file only
if the block was all it held. `doctor` prints one line per harness at all times, so "not sending"
always has a stated reason. Sandboxed sessions are not covered — aoe stages only your own
`extensions/` directory into a sandbox, so no extension of this plugin's is installed there.

## Updating, disabling, removing

```sh
aoe plugin update dev.oozut.llm-tags --yes     # rebuild (or re-download), restart the worker
aoe plugin disable dev.oozut.llm-tags          # contributions off; the pasted lines stay
```

An approved session-scope extension is part of the per-session copies, so the same step that
removes those removes it; your own `~/.pi/agent` was never touched, so nothing there needs undoing.
A user-scope one is in your own config, so remove it with `extension disable <harness>` — it
deletes the skill directory and the block between the markers, and nothing else.

The hook lines and the mapping line live in files this plugin does not own, so it never adds or
removes them. Removing it cleanly is therefore three steps, in this order:

1. Delete the per-session copies it built, and let it name what is left:

   ```sh
   "<install>/bin/aoe-llm-tags" setup --undo --all
   ```

   It prints the `[host_hooks]` entry — and the harness mapping, when that file still has one —
   with the absolute path of the file to edit.
2. Delete those by hand: the `[host_hooks]` entry (both `before_session` and `before_start`), and
   the `X-AoE-*` entry under the provider's `headers` in the harness config it names. For `pi`
   that is `~/.pi/agent/models.json` **and every config a wrapper generates from it** —
   `scripts/pi-onecli.sh` (installed as `pi-proxy`) copies the `litellm` provider, headers
   included, into `~/.config/onecli/pi_cli/pi-agent/models.json` on every run, so clean the
   source and let the wrapper regenerate.
3. `aoe plugin uninstall dev.oozut.llm-tags`.

A leftover hook line is a no-op rather than a failure: the installed command ends `|| true`, and
the hook itself exits 0 on every error. A leftover **mapping** is a no-op too, as long as it is
the printed command value (`guard_fallback`): nothing mints the variable any more, so the command
falls through to the empty payload `{}` and the harness sends a header it can parse. Both leftovers
are therefore inert on their own — the hand edits above are tidying, not a repair.

The one leftover that still breaks launches is a **bare** `$AOE_LLM_TAGS` mapping, which is the
pre-guard spelling of the line. `aoe-llm-tags doctor` names it as a warning while the plugin is
still installed (`mapping X-AoE-Tags … still maps it to a bare "$AOE_LLM_TAGS"`); replace that
value with the current `setup --print` line and the hazard is gone for good.

## When something is wrong

| Symptom | First thing to check |
|---|---|
| `aoe plugin list` has no `dev.oozut.llm-tags` | the install output; `aoe plugin info dev.oozut.llm-tags` for the recorded source |
| Install fails with "requests capabilities … but stdin is not a terminal" | re-run with `--yes` |
| Install fails with "ships the reserved build-output directory `.aoe-build`" | a local checkout has build artifacts in it; build with `CARGO_TARGET_DIR` set outside the tree |
| Install fails with "no cargo found" | source installs build from source; install Rust ≥ 1.89, or use the release form |
| `doctor`: command path moved | the plugin was uninstalled/renamed — regenerate the hook lines with `setup --print` |
| Requests fail with `Failed to resolve provider … header … from environment variable: AOE_LLM_TAGS` | the mapping is the bare `$VAR` spelling and no hook minted it (both hooks removed, or a launch outside aoe). Paste the `setup --print` value over it — the guarded command falls back to `{}` — or delete the `X-AoE-*` entries per "Updating, disabling, removing" |
| `doctor`: mapping is `warn` with "still maps it to a bare" | the mapping predates `guard_fallback`; paste the current `setup --print` value over it |
| `doctor`: no hook line | profile `[host_hooks]` replaced the global one, or the key is misspelt |
| `doctor`: mapping missing | paste the mapping line into the file it names; for `pi` keep a non-empty `models` list |
| `env` shows nothing in a session | the session started before you edited the config — restart it; values are launch-time |
| `doctor`: asset not found (release installs) | the release is missing that platform's tarball; the error lists the assets it found |
| `doctor`: `[skip] last verification` | expected — consumer read-back is roadmap work, not a broken install |

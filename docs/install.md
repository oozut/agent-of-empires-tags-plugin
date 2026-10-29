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
<harness config> → providers.<id>.headers["X-AoE-Tags"] = "$AOE_LLM_TAGS"
```

1. Put both `before_*` lines under `[host_hooks]` in `<app_dir>/config.toml`. Both keys, the same
   string: `before_session` covers host sessions, `before_start` covers sandboxed ones. If the
   profile has a `[host_hooks]` array of its own, it **replaces** the global one — put the lines in
   the profile file too, or only there.
2. Add the mapping line(s) to the harness config named, one line per enabled header, leaving the
   rest of that file alone. For `pi` that means `~/.pi/agent/models.json`,
   `providers.<id>.headers["X-AoE-Tags"] = "$AOE_LLM_TAGS"`, and the provider entry must keep a
   non-empty `models` list or the wrapper drops the whole entry, headers included.

`setup --print` never writes anything. Editing those files is yours, deliberately.

## 3. Check it

```sh
"<install>/bin/aoe-llm-tags" doctor
```

Expect `0 failed` and one line per check, each naming the absolute path it looked at:

```
  [ok  ] before_session hook    AOE_BIN="/usr/local/bin/aoe" "<install>/bin/aoe-llm-tags" hook …
  [ok  ] mapping X-AoE-Tags     ~/.pi/agent/models.json → litellm (value: $AOE_LLM_TAGS, models: 1)
  [ok  ] value rendering        X-AoE-Tags=AOE_LLM_TAGS="aoe,feature:,session:S-cf11"
  [warn] last verification      never verified against a consumer system; proving arrival is M0-T09
```

`[warn] last verification` is expected until you have seen the header arrive somewhere. It is a
warning because the plugin cannot know what consumes headers on your path; it is not a broken
install. Every other `warn`/`fail` names the piece to fix.

Exit codes: `0` all good (warnings allowed), `1` at least one check failed, `2` bad usage.

## 4. See a value

Inside a session (or with the variables exported by hand):

```sh
env | grep '^AOE_'
# AOE_LLM_TAGS=aoe,feature:,session:S-cf11
```

Nothing there means the hook line is absent, in the wrong profile, or exited non-zero — `doctor`
says which. The hook never blocks a session start: on any failure it writes a line to stderr and
exits `0`.

## 5. Prove it leaves the machine (optional)

The plugin ships a fixture that records headers, so "it is sent" can be a file and a grep:

```sh
python3 tools/header-sink.py 8787 /tmp/tag-sink.jsonl --reset &
# point a harness at http://127.0.0.1:8787/v1 with headers {"X-AoE-Tags": "$AOE_LLM_TAGS"},
# send one request, then:
grep -o '"x-aoe-tags": "[^"]*"' /tmp/tag-sink.jsonl
```

A worked run, with the exact commands and the two traps (use the real `pi`, not a wrapper that
regenerates its config; the fixture must answer streaming requests), is in
`docs/reference-deployment.md` §Proof runs.

## Updating, disabling, removing

```sh
aoe plugin update dev.oozut.llm-tags --yes     # rebuild (or re-download), restart the worker
aoe plugin disable dev.oozut.llm-tags          # contributions off; hook lines stay, so remove those
aoe plugin uninstall dev.oozut.llm-tags        # removes files and the grant; remove the hook lines
```

The hook lines are ordinary `[host_hooks]` entries in your config: the plugin cannot add or remove
them, so turning the plugin off is two edits, not one.

## When something is wrong

| Symptom | First thing to check |
|---|---|
| `aoe plugin list` has no `dev.oozut.llm-tags` | the install output; `aoe plugin info dev.oozut.llm-tags` for the recorded source |
| Install fails with "requests capabilities … but stdin is not a terminal" | re-run with `--yes` |
| Install fails with "ships the reserved build-output directory `.aoe-build`" | a local checkout has build artifacts in it; build with `CARGO_TARGET_DIR` set outside the tree |
| Install fails with "no cargo found" | source installs build from source; install Rust ≥ 1.89, or use the release form |
| `doctor`: command path moved | the plugin was uninstalled/renamed — regenerate the hook lines with `setup --print` |
| `doctor`: no hook line | profile `[host_hooks]` replaced the global one, or the key is misspelt |
| `doctor`: mapping missing | paste the mapping line into the file it names; for `pi` keep a non-empty `models` list |
| `env` shows nothing in a session | the session started before you edited the config — restart it; values are launch-time |
| `doctor`: asset not found (release installs) | the release is missing that platform's tarball; the error lists the assets it found |

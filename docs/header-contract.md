# The header contract

What this plugin actually promises, stated without reference to any gateway, proxy, or harness.
Everything downstream — `plan/milestones.md`, `docs/implementation.md`,
`docs/harness-adapters.md`, `plan/parameters.md` — has to be
consistent with this file. If a decision only makes sense for LiteLLM, OneCLI, or the harness
installed on one host, it belongs in `reference-deployment.md` as an example, not here.

## The one problem worth solving

Agent sessions spend money and act under an identity, and the party that pays for or audits that
work usually cannot tell which session, feature, project or tenant a given request belonged to.
AoE cannot fix that itself: it is not in the LLM request path, and no plugin hook sees a request
(`src/plugin/host_api.rs`). But every harness that talks to an LLM can usually attach a header.
So the durable fix is smaller and more portable than a proxy:

> Make every request that leaves a session carry headers that identify the session and what it is
> for, and let whoever cares decide what those headers mean.

Nothing about that idea is specific to cost. The same header is, in different hands, a billing
dimension, a log join key, a rate-limit bucket, a tenant check, or a validity assertion.

## Guarantees

The plugin commits to exactly three:

1. **Presence.** Every outbound LLM request from a session carries the configured payload on the
   channels that harness supports, or the plugin says plainly that it cannot, and why (`doctor`).
2. **Derivation.** Values are computed from the session's identity and the plugin's settings, at
   launch, by one implementation shared by every harness and both hook substrates.
3. **Opacity.** Nothing else about the request changes: no prompt, no response, no ordering, no
   extra network hop, and no body edit beyond the field a `body` target names. Values the plugin
   emits are values it constructed or was configured with; it never inspects traffic.

Everything else is a consumer's business: whether a header is trusted, parsed, stored, expired,
charged against, or ignored.

## What travels: one payload, three channels

A tag is a **key and a value**, not a word in a string. `tags` is a set of them:

| You write | It means |
|---|---|
| `team:core` | `"team": "core"` |
| `cost:=1.5` | `"cost": 1.5` — `:=` takes any JSON value: `true`, `2`, `["a","b"]`, `{"k":1}` |
| `platform` | `"platform": true` — a marker that carries no value |

The resolved payload is a JSON object: the session fields first, then every tag. It is what a
target carries unless the target says otherwise, so a consumer reads `tags["team"]` instead of
splitting a comma list, and adding a dimension later never changes the shape a consumer already
parses.

```json
{"session":"S-9f3a","session_id":"9f3a...","tool":"pi","profile":"main",
 "group":"Payments","project":"core","project_path":"/w/core","branch":"main",
 "feature":"billing","team":"core","cost":1.5}
```

Fields are included only when they have a value, and `feature` is written last so a `feature:`
tag overrides the derived one. A tag whose key is a session field cannot shadow it: the session
wins and the tag is dropped with a warning.

`feature_tag` — the per-project setting for the feature a session belongs to — is **one of these
entries**, not a bare word with a key implied elsewhere:

| You write | It means |
|---|---|
| `feature_tag = "feature:billing"` | `"feature": "billing"`, the standard entry |
| `feature_tag = "billing"` | the same entry; the old spelling keeps the implied key |
| `feature_tag = "epic:checkout"` | `"epic": "checkout"` — the key is the payload field, so there is no `feature` field and `{feature}` reads a `feature:` tag instead |
| `feature_tag = { key = "epic", value = "checkout" }` | the same pair, for a settings form that stores key and value apart |

Global config, a profile (`profiles/<name>/config.toml`) and a project each hold the same grammar
and are parsed by the same code, so a value means the same thing wherever it was written. Like a
tag, it cannot take a session field: `feature_tag = "session:mine"` is dropped with a warning.

### Targets: where a payload goes, and which slice

A target chooses **where** the payload travels and **what** of it travels there.

| Field | Meaning |
|---|---|
| `kind` | `header`, `query` or `body`. A harness that cannot carry a kind is refused rather than written. |
| `name` | Wire name: a header name, a query parameter, or the body field to set. |
| `format` | `json` (default) sends the whole payload object; `csv` sends the flat `team:core,feature:billing` list an older consumer expects. A field holding a comma, a quote or a line break is quoted (`"team:x,org:evil"`, internal quotes doubled), so a value cannot forge a pair the sender never wrote. |
| `template` | Overrides the format for one target and picks the slice itself: `{tags}`, `{tags_json}`, `{payload_json}`, `{tags_b64}`, or one field such as `{feature}`. |
| `literal` | Used instead of the rendered template, for a harness that does not expand `$NAME` in that position. Not a bypass: it is cleaned like every other value (control characters dropped, surrounding whitespace trimmed) and capped at 800 bytes where the settings are parsed, because a newline written as-is added a `NAME=value` line nobody set and a carriage return broke an `env_only` header list. |
| `env_var` | Variable that carries the rendered value at launch: `AOE_HDR_` / `AOE_QRY_` / `AOE_BODY_` + the name in upper snake case (`X-AoE-Tags` → `AOE_HDR_X_AOE_TAGS`); pinnable for a harness whose config already names one. A pinned name has to be one — a letter or `_`, then letters, digits or `_` — or the row is refused: the name travels into `NAME=value` lines and into the shell command an `env_only` harness runs, where a `;` is a second command. |
| `enabled` | Lets a set ship dormant (`X-AoE-Group` off by default, since group names are user prose and may be sensitive). |

Template fields, resolved from the session manager first and the hook environment second:

`{session}` `{session_id}` `{feature}` `{tool}` `{profile}` `{group}` `{group_slug}` `{project}`
`{project_slug}` `{branch}` `{tags}` `{tags_csv}` `{tags_json}` `{payload_json}` `{tags_b64}`

and the git facts, read from the session's own checkout:

`{git.branch}` `{git.commit}` `{git.sha}` `{git.tag}` `{git.describe}` `{git.subject}`
`{git.author}` `{git.dirty}` `{git.remote}` `{git.repo}` `{git.root}`
`{git.branch}` comes from git itself (`rev-parse --abbrev-ref HEAD`, or the short SHA on a
detached HEAD), `{git.commit}` is the abbreviated SHA and `{git.sha}` the full one; `{branch}` is
untouched — it stays whatever the session manager reports, so an existing template keeps its
meaning. `{git.tag}` and `{git.describe}` are empty when no tag names the commit, and
`{git.remote}` drops any credentials in the URL. `{git.repo}` goes through the same scrub before
it takes the last path segment, so a remote with no path at all (`https://token@host`) names its
host instead of its token. `{git.dirty}` always answers — `false` for a
clean tree, not an empty string — because "clean" is a fact and an empty value would read as
"unknown". `{git.repo}` names the repository the checkout belongs to, so a linked worktree
reports `android-keyboard` rather than the worktree's own directory name.

**Only the session's own checkout.** `<project>/.git` has to be there before anything runs — a
directory in a clone, a file in a linked worktree or a submodule. What is inside it never matters:
an empty `.git` and a repository with no commits are checkouts too, and their facts are simply
empty. A directory that is merely *near* a repository gets no git facts at all: the resolver reads
the path the session names, not whatever repository sits above it, so a home directory that is
itself a dotfiles clone cannot make a plain project report `main`. A subdirectory of a checkout is
refused for the same reason — the session's path is the checkout.

**Nothing runs unless a reference is named**: a launch whose templates and tags mention no git fact
starts no `git` process at all, and a launch that names several pays one timeout budget for all of
them, shared with the session-manager lookups — a `git` that hangs costs one wait, not one per
fact. The budget is a real one: the pipes are read on detached threads, so a grandchild that
inherits stdout cannot hold the launch past it, and each lookup's output is capped at 4 MiB.
`AOE_GIT_BIN` overrides the binary for a wrapper or a test double. A git fact that cannot be read
is empty and a warning, never a failed launch.

**A checkout is data, not code.** Every call carries `--no-pager` and pins the configuration keys
that can start a program — `core.fsmonitor` (which `git status` runs, and `{git.dirty}` asks for
exactly that) and `core.pager` — on the command line, where the repository cannot reach them. The
launch environment is scrubbed the other way round: `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`,
`GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`, `GIT_COMMON_DIR`, `GIT_NAMESPACE` and
the `GIT_CONFIG_*` family are removed from the child, because they would point `git -C <session>`
at another tree or feed it config this plugin never chose.

### References as tag values

A tag value is a template too, expanded once at launch:

```toml
tags = ["vcs:git", "git.branch:{git.branch}", "git.commit:{git.commit}"]
```

That is how a consumer reads the revision from the payload itself — `tags["git.branch"]` — instead
of the operator writing a target template for one consumer. The session fields are available too,
but a value cannot reference a field that is built *from* the tags (`{tags}`, `{tags_csv}`,
`{tags_json}`, `{payload_json}`, `{tags_b64}`, `{feature}`): that is reported and renders empty
rather than producing a tag that looks like an answer.

Two ways to get the standard set without writing it by hand, both writing the same four entries:

```sh
aoe-llm-tags tag --git                      # vcs:git, git.branch, git.commit, git.tag
aoe-llm-tags references                     # every reference, with this session's value
```

The session pane offers the same set as one "Standard git information" row, and the page's old
"Insert a reference" dropdown was removed with the tag editor: references still come from the same
catalogue the `references` verb prints, so the CLI can never offer one the renderer does not know.

Rules that keep the contract honest:

- **Low cardinality by default.** `{session}` is the short id (`S-9f3a`); `{session_id}` is the
  full id and is opt-in, because a billing dimension with one bucket per session is unusable.
- **Never user prose.** Titles, group names and paths are opt-in, as slugged or hashed forms.
  A path can contain a username; a title can contain anything at all.
- **Fixed per launch.** The value cannot change while the process lives — that is a property of
  the delivery mechanism, not a limitation to be worked around.
- **Bounded.** Keys start with a letter or digit and stop at 96 characters; values are printable
  JSON. A key or value outside the grammar is dropped with a warning rather than written.
- **Values are typed.** `:=` is the only way to get a number, boolean or nested value into a
  payload, and it stays typed all the way to the consumer; a `csv` target flattens it back to
  text, because a comma list has no types, and a field with a comma, a quote or a line break in
  it is quoted so it cannot pass for two fields.

### A repository's own tags

`tags_file` names a JSON object that lives inside the session's project: the path is relative to
the project root and cannot leave it, even through a symlink — the resolved path is what is
checked, because `..` is not the only way out of a tree. Every top-level pair is one tag with its
JSON type intact (`{"cost": 1.5}` is the number `1.5`, not the string `"1.5"`). Keys follow the
same grammar as a settings tag; a key that does not is skipped with a warning rather than
rewritten.

The file is repository content, so it is read as untrusted input. It has to exist before anything
is opened; only a regular file is read, so a FIFO or a device cannot stall the launch; the 64 KiB
cap is applied to the read itself rather than to the length `stat` reports, since that length is a
claim rather than a bound. Values are sanitised before they can reach a header: every string has
its control characters dropped and its surrounding whitespace trimmed, and a **string** value is
additionally trimmed to 256 characters. A list or an object is left alone — it is a structure
rather than text, and the file cap already bounds it.

The repo file is one rung below a session's own edit and one above a project's panel entry: its
entries replace a `tags` entry with the same key, while a per-session edit still wins over both.
From most to least specific, the order is **session edit → project → `tags_file` → AoE → `tags` →
rule hit**. The project and AoE rungs live in `<app dir>/tags.toml`, written by the pane's sections
and by `tag --level`, never in the settings' file. The inherited
tags spend the five-tag budget in that order; a session's own edits are exempt and added on top,
so a tag typed for one session always travels, even when the inherited set already fills the
header. A rule adds a key the set does not already have; it never rewrites one a session, file
or setting already set. The file is read at launch and is configuration rather than a fourth
delivery channel — a missing or malformed file is a warning `doctor` prints, never a failed
launch.

## Who reads it

The same header serves four families of consumer, and the design has to be usable by all of them
rather than tuned to one:

| Consumer | Needs | Served by |
|---|---|---|
| Cost / chargeback (gateway, finops) | A stable, low-cardinality dimension attached before the request is priced; must survive a proxy hop. | Default combined tag header; `{feature}` + `{session}`; values set at launch. |
| Observability / metadata pipeline | Join keys that match the orchestration system's own records; high cardinality is fine here. | `{session_id}`, `{project_slug}`, `{tool}`; optional discrete headers. |
| Session validation / management | Assert the request belongs to a live session, and let a control plane act on it (quota, pause, revoke). | A discrete `X-AoE-Session` header carrying the full id, plus an optional signed form: `<id>.<hmac>`, HMAC-SHA256 with a key the consumer is given, truncated to 16 hex. |
| Policy / routing | Route or restrict by tenant, team or environment. | `{group_slug}`, `{profile}`, or any template the operator writes. |

Consequences for the plugin: it must support **several headers, discrete values, and an optional
signature**, and it must not assume the consumer parses a comma list. That is why the header set
is a list of templates rather than one fixed format.

## Delivery, and why it is launch-scoped

Three paths, all fed by the same resolved payload:

1. **Environment.** The hook prints `AOE_HDR_…=<value>` lines and the harness's own config maps
   its header to that variable once, statically. This is the portable path — no proxy, no daemon,
   no network call from inside a sandbox — and it is the only one that works for a harness whose
   config aoe itself writes hooks into, because aoe refuses a launch that moves that file.
2. **A per-session file** (`overlay_file` / `overlay_dir`). For a harness whose config can only
   hold a literal, the plugin builds a generated config inside the session's own directory and
   points the harness at it with an environment variable. The user's config is never touched, and
   the copy is deleted when the session is gone. The copy is the harness config, which is what
   carries the tag, and the rest of the user's config dir is linked, never duplicated. A copy
   that would carry nothing is not built at all, so no session directory is left behind for a
   harness that was never pointed at it.

   This copy can also hold **code**: a harness that loads extensions from its config directory
   runs them in-process, so a file there can re-read the plugin's live file before each request
   (`docs/harness-adapters.md` §Per-request values). One directory — the extension subdirectory —
   becomes a real directory of links to the user's own entries plus the plugin's file, because a
   link would be a write through into the user's config. That file exists only while the user has
   approved that exact file for that harness, and the approval is what names it, so the copy is
   still described by one receipt and one line in `doctor`.
3. **Command** (fallback). Harnesses that accept a shell command as a header value call the CLI
   directly (`!aoe-llm-tags value X-AoE-Session`). Same values, one process later, and subject to
   the harness's own caching.

None of these three paths updates mid-session: they all resolve once, at launch. The exception is
the extension in 2 — when a harness loads code from its config and the user approved it, the
extension re-reads `sessions/<id>/live.json` before each request and re-renders the same targets.
The tags, the template and the values are still the plugin's; the only thing that changes is that
they can change *between* requests instead of only between launches. A consumer that needs
liveness for a harness with no such hook still has to query the control plane; the header
identifies, it does not assert current state.

## Changing a tag for one session

A tag belongs to a session, so changing one is a per-session act. The plugin has no path that
rewrites the settings: `setup --apply` writes a backup and a session copy, the worker has no
config-write RPC, and the editor below writes only inside the session's own directory. Two
sessions can therefore carry different tags at the same time without a rule, a profile or a
config edit between them.

Three surfaces, one store — `<app dir>/sessions/<session id>/overrides.toml`, where `<app dir>` is
`$XDG_CONFIG_HOME/agent-of-empires` when that names an absolute path and `<HOME>/.config/
agent-of-empires` otherwise. When neither does, there is no app dir and no session store: the
commands that write one say so and exit 1, and the hook skips the session's edits with a warning
and sends the header anyway. It never falls back to a path relative to the launch directory, which
would let a checked-out repository ship the file that configures it.

| Surface | What it does |
|---|---|
| Session pane (`llm_tags_pane`) | Three sections — this session, this project, AoE — each tag with its origin and the values this session would send. A session row hides a tag for this session; a level row hides/restores it at that level, copies the session's set upward, or clears the level. The session section also holds the git preset and reset. An **Activity** section under it
offers one row per word in the family this session is pinned to, and names that family, its
source (`built-in`, `settings`, `repo <file>`) and the other families defined here — so
editing a family changes the next launch, never a running session. |
| Session details page (`ui/tags.html`) | The literal token for the selected session and the harness extension approvals. It carries no tag editor; tags live in the pane. |
| `aoe-llm-tags tag` | The same edits from a shell, for a script or a session with no dashboard; `--level project\|aoe` edits the upper rungs. |

The token is written but never read back into the UI. A surface is told that one is set, and the
target row, the pane and the session badge say *that* it stands in for the value rather than what
it says: `ui.state.set` payloads are kept by the host and rendered to anyone who can see the
session, so a secret there would be a secret in a dashboard. `aoe-llm-tags tag --show` prints it in
the user's own terminal, and the session file that holds it is `0600` in a `0700` directory.

The file holds the same tag grammar the settings use, in three keys:

```toml
tags  = ["team:platform", "epic:=42"]  # add, or replace the same key
unset = ["cost"]                       # drop a tag the settings carry
token = '{"hand":true}'                # a literal standing in for the rendered payload
```

Resolution order, in one place (`session_tags::apply`, inside `snapshot`), so the hook, `status`,
`doctor`, `classify` and the UI cannot disagree: the settings are read first, the session's `unset`
keys are dropped, its own entries replace a same-key entry (then append), and finally a `token`
replaces what a target with **no** `template` would have rendered. A target that names its own
field (`{session_id}`) keeps it: a session-wide literal does not override a target that says what
it wants.

Two honest limits:

- An edit applies to the **next** launch. The value a running process sends came from the
  environment it was started with, and the plugin does not reach into a live process; the pane
  shows what the next launch mints, not what the current one holds.
- Dropping a tag the settings carry is still per-session: it records the key in `unset` rather than
  editing the settings, so the same tag stays on every other session.

Both clickable surfaces reach the worker the same way: the host forwards a block's `method`
**verbatim**, so `tag.set`, `tag.remove`, `token.set`, `token.clear` and `tags.reset` are whole
names, not `plugin.<id>.<command>` — which is how a host *request* is addressed. `worker::EDITS`
lists them, `tests/docs.rs` holds the page and the worker to the same list, and the host adds the
session id it trusts (a `session_id` in the page's params is overwritten, never believed).

Checked against the running daemon, not only in tests: `POST /api/plugins/dev.oozut.llm-tags/action`
with `{"method":"tag.set","params":{"key":"team","value":"platform"}}` answered `202`, wrote
`sessions/cf11af87da7e4bf0/overrides.toml`, moved that session's UI revision (so the clicked
control's spinner clears), and the next `/api/plugins/ui-state` read carried the tag with
`origin: "session"` in both the pane row and the page payload; `tags.reset` removed the file
again. `tools/drive-worker.py` reproduces the same conversation without a browser, which is what
makes the payload shape checkable on a machine with no dashboard open.

`overlay::prune` runs on every host event and removes the generated harness copies of a session
directory the host no longer lists, and it treats an empty live list as a failed query rather than
as evidence that every session is gone. It leaves `overrides.toml` alone: that file is not a copy
of anything, it is the only record of what someone typed, so a host list that has merely not caught
up (a failed read, a restart mid-sweep) cannot delete a tag. `setup --undo` is how a session's
edits are removed, and it says what it removed.

## What the plugin does not decide

Deliberately open, because they are consumer-side and differ per deployment:

- which system is authoritative (any HTTP-visible hop: gateway, egress proxy, log pipeline, WAF);
- the header names, beyond shipped defaults;
- whether values are signed, and by which key;
- retention, aggregation and access control on the consumer side;
- the canonical tag vocabulary — `feature:` and `session:` are defaults, not a schema.

A design decision that only works for one of these systems is a deployment choice. Keep it in
`reference-deployment.md`.

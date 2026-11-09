# Milestones

Order matters: the header has to arrive on a real request before any UI is worth building.
M0 is the gate for everything else.

Each milestone below is summarised here and specified in its own file, where the **parameters**
it it introduces are tabulated: `m0.md`, `m1.md`, `m2.md`, `m3.md`, `m4.md`. Every parameter also
appears in `parameters.md`, which is the single source of truth for names, defaults and allowed
values.

The ladder is fixed: M0 → M1 → M2 → M3, with M4 rows available from M0 onward. Which task inside
a milestone comes first is decided by priority (P0/P1/P2, legend and P0-only subset in
`plan/README.md`), never by moving a milestone.

Every task has a stable ID (`M0-T07`) and a lane, so work can be taken in parallel without two
people touching the same files; the task table at the top of each `mN.md` carries the ID, priority,
lane, dependencies, size and the one output that proves it done. Milestone-level acceptance below
is what P0 exists to satisfy — it is not a per-task checklist.

**Normative order of authority:** `docs/header-contract.md` (what is promised, to whom) →
this ladder → `docs/implementation.md` (where the code goes) →
`docs/harness-adapters.md` (per-harness data) → `docs/reference-deployment.md` (one stack, an
example, never a requirement). If a milestone here only makes sense for one gateway, one proxy
or the harness on one host, it is wrong and belongs in the reference deployment.

## Constraints

- **No system is assumed on the other side.** The header is the product; whether it is read by a
  gateway, an egress proxy, a log pipeline, a WAF or a session-control plane is the operator's
  choice. Nothing may depend on a specific vendor's tag syntax, and the plugin must not need to
  know which consumer is in front.
- **Headers, plural.** The unit is a header set (`header` + `template` + `env_var` + `enabled`),
  not one fixed tag string. Cost attribution, log joining, tenant policy and session validation
  are different consumers with different cardinality needs — see the consumer table in the
  contract. The shipped default is a single combined tag header; discrete headers and an
  optional signed session id exist because other consumers need them.
- No tagging proxy. Headers ride on each harness's own header capability.
- The plugin never reads prompts or completions, and never logs request bodies.
- One hook implementation, installed in **both** hook keys. aoe gives `before_session` (host
  sessions) and `before_start` (sandboxed sessions) the same contract — the same lifecycle
  variables and the same stdout `KEY=VALUE` minting — so the values have to come out of one CLI
  invocation and one set of settings. Installing one key and not the other silently un-tags
  half the sessions, so `setup` prints both and `doctor` treats each key as its own finding.
- Effect timing differs by substrate, and the UI must name the one that applies instead of
  saying "next launch". A host session re-mints before every agent launch, including a view
  switch. A sandboxed session mints once when its container is created or restarted and reuses
  those values while that container lives, so a changed value reaches it on the next container
  start, not the next agent launch.
- Resolution has one source of truth: the session manager. The hook reads the session row with
  `aoe session show "$AOE_SESSION_ID" -p "$AOE_PROFILE" --json` — `id`, `title`, `path`,
  `group`, `tool`, `profile`, `state`, `status`, `command` — and falls back to the hook
  environment only when that call fails. (`aoe list --json` is the superset, adding `worktree`
  and `workspace_repos`, for a template field that needs them.) This is what makes the model
  cheap to grow: a new template field reads another field from the row, with no new environment
  variable and no new user setup. It also supplies the fields the hook environment lacks —
  structured host sessions get no `AOE_GROUP_PATH` or title.
- That call is a subprocess from inside aoe's own hook, so it is bounded: 2 s timeout, exit 0
  with the hook-environment fallback on timeout or error, and the hook environment used
  directly when it already carries the field a template needs. A slow or blocked store must
  never turn into a failed launch (a non-zero hook exit aborts the session start).
- One indirection in the harness config, not two. The static mapping is always "one header
  name → one variable" (or "→ one command"); the value is built by the plugin CLI. Renaming
  the header, changing the value format or adding values is therefore a plugin settings or
  plugin release change, never a re-edit of `models.json` or the hook lines.
- Values are minted per launch, not per request. A harness that accepts a command as a header
  value caches the result for the lifetime of the process, so a command is equivalent to a
  launch-time variable — there is no mid-session live update to design for. Rule or template
  edits apply on the next launch of a host session and on the next container start of a
  sandboxed one.
- The `[host_hooks]` entries are a one-time user step, per profile (hooks are global/profile
  only, never repo config). The plugin generates, validates and reports them, and cannot
  install them.
- `aoe serve` must be running for the worker, panes, badges and pages. The hook and CLI work
  without it, in both substrates.

## M0: prove a header on a real session → `m0.md`

Goal: a session launched by aoe sends the configured headers, and a system outside aoe records
them. The proof is a local header sink, so it needs no named consumer: the plugin mints and delivers
a header, and reading a consumer back is roadmap work (`plan/roadmap.md`, decision #4).

Acceptance, in a real aoe session (host now, sandbox once the runtime is reachable):

1. Terminal view and structured view sessions of the same tool both deliver the header (host
   launches, so `before_session`).
2. The same session launched **sandboxed** delivers it too, through `before_start`: the value
   reaches the container environment and the harness inside sends it. If the host cannot reach
   a container runtime, record that as a precondition failure, not as a pass — the sandbox half
   may be proven as (a) `hook` printing the same value under `before_start`'s environment and
   (b) the container environment carrying the variable, with the adapter row left `documented`.
3. Stopping and restarting a sandboxed session keeps the same session value, picks up an edit
   made before the restart, and does not pick up one made after it while the container lives.
4. The default header value contains at least a feature token and a stable, distinct session
   token; a second enabled header (e.g. a discrete session header) is delivered independently.
5. Proof of arrival: a temporary local HTTP sink logging request headers, fed by the harness with
   the value aoe's hook minted for a real session. The second half (the same header seen in a
   named consumer's records) is deferred with decision #4 — proving it needs a consumer, a read
   credential and a header→dimension mapping, none of which the plugin should depend on.
6. `doctor` reports wired with both keys present, says whether it resolved the session row or
   fell back to the hook environment, and names exactly which of `before_session` /
   `before_start` / static mapping / command path is missing when one is removed.
7. A session started without the hook sends no header and the plugin says so; nothing breaks.
   Same for a sandboxed session whose only hook is in `before_session`.

Verification technique: point the harness at a local header-logging sink for one run (test
fixture only, not a production proxy), with the value read from the launched agent's environment
so aoe's hook is what is being proven, not just the harness. A harness may launch through a
wrapper that regenerates its config or forces an upstream, in which case the sink run needs an
isolated config and no proxy on that path; that is a per-harness note, not a change to the
acceptance. For the sandbox case
the static mapping must exist **before** the container is created: aoe copies the harness's
config into the private per-session store at creation and never re-syncs it afterwards. Record
harness and aoe versions in the adapter row.

Out of scope: any UI, any other harness, any cost display.

## M1: settings and attribution UI → `m1.md`

Goal: users define the header set and the feature rules in aoe's own settings surfaces and see
them attributed to sessions.

Acceptance:

1. Editing a rule changes the badge on the next launch, with no config file edits.
2. A project-level `feature_tag` overrides the global one and is editable from the web
   project form.
3. An unwired harness shows a visibly different badge, not a silent empty one.
4. Settings validation rejects header names outside the HTTP field-name grammar, values outside
   the documented character set, and over-long values.
5. Enabling a second header and re-running `setup --print` adds exactly one env var and one
   mapping line, with no change to the first.

## M2: auto-discovery and one-click wiring → `m2.md`

Goal: pick an agent, get the right change proposed.

Acceptance:

1. Each supported harness reaches `wired` from a clean config and the header is seen on the
   real path, at the consumer the operator configured.
2. Applying twice is idempotent and `undo` restores the byte-identical original file.
3. A config written by hand after the plugin touched it is detected, not overwritten.
4. Unknown harness reports `unknown` with the generic env fallback, never a false `wired`.

## M3: usage views → `m3.md`

**Roadmap-gated.** Every acceptance line below needs a consumer the plugin can query, which decision
#4 deferred (`plan/roadmap.md` §1). M3 stays in the ladder because it is the honest shape of the
feature, but no M3 task may start before that decision is taken again.

Goal: answer "what did this feature and this session cost", where the operator's consumer system
can answer it.

Acceptance:

1. Tag spend in the page matches the consumer's own view for the same window, within its own
   aggregation granularity.
2. A session with no recorded spend renders an empty state, not an error.
3. A consumer without a spend dimension degrades to wiring-only information, with a message
   saying what it cannot answer — not a wrong number.

## M4: remaining harnesses and distribution → `m4.md`

- Remaining adapter rows filled from research, each marked with tested versions or left
  `unsupported`.
- Release-binary worker, tagged release, publish path for `aoe plugin install`.
- Featured-index PR with the source tree hash if it should claim a reserved namespace.

## M5: per-request values and activity → `m5.md`

A launch hook is once per process, so a tag edited mid-session and anything describing what a
session is *doing* cannot ride it. A harness that loads code from its config directory can run an
extension this plugin ships and the user approves, which re-reads a live file before each request.

Acceptance:

1. Nothing is installed without the user's approval, and every surface that mentions the mechanism
   says it is code running inside the agent.
2. The approval is durable across plugin and harness upgrades and specific to one exact file;
   changing those bytes or the disclosure, or revoking, installs nothing.
3. The user's own harness config is untouched — the session copy gets a directory of links plus
   this plugin's file, never a write through to `~/.pi/agent`.
4. Two requests in one agent process carry two different values, without a restart.
5. Harnesses without an in-process hook stay launch-scope in `doctor` and in the support matrix.
   No proxy: `net` stays undeclared.

## M6: activity families → `m6.md`

M5 made the activity real but kept the vocabulary compiled in. M6 makes the words data: a *family*
of activities — label, icon, tone and a `when` sentence — defined in the plugin settings or in the
repository's own `.aoe/activity.toml`, chosen per project, and pinned per session at launch so an
edit applies to the next session and never relabels a running one.

Acceptance:

1. A user replaces the vocabulary from the settings UI or a repository file, with no patch, and a
   word outside the family is refused by name rather than guessed.
2. Precedence is one rule: built-in → settings → repository, a same-named family replaced whole,
   the repository winning a conflict.
3. `when` is stored, rendered and quoted to the agent, and nothing infers a transition from it yet.
4. A running session keeps the family it launched with; only the current activity changes
   mid-session.
5. A missing repository file is silent, and nothing a repository declares can fail a launch.

## Upstream

Recorded for a future host release, not work this plugin can do:

- A typed per-request hook in aoe itself, attached to a session the way `[host_hooks]` is today.
  With it the extension becomes unnecessary for every harness, and the receipt can be replaced by
  the host's own approval (then delete `src/harness_extension.rs` and say so in the changelog).
- A `harness.hook` capability, if the host wants to keep the decision at install time. Until then
  this plugin's own two-part consent has to be at least as strict as the host's, because no
  capability name covers "install executable code into a harness's config".
- Per-platform `bin` in `ReleaseBinary` (Windows), and a `PluginSource::Local` release path
  (`plan/roadmap.md` §3, §4).

## Open decisions

Full parameter-level consequences: `parameters.md` §Open.

1. Rule semantics: accumulate every matching rule (feature + env + owner) or first match
   wins. Accumulate reads better for header values, with a cap of about five.
2. Value grammar: propose `[a-z0-9][a-z0-9._:-]*`, comma-separated by default, with a per-header
   template so a consumer that needs a different shape can be served without code. JSON is one
   such template, not a plugin mode.
3. Session token form: `S-9f3a` short id by default, full id opt-in, never the title.
4. ~~Which consumer system is the reference for verification in this deployment.~~
   **Resolved 2026-10-29: deferred — the plugin does not read a consumer back.** Naming a consumer
   would make one vendor's read API a hard dependency for `verify`, the M3 usage page and one
   `doctor` line, and it changes nothing about whether the header is minted or delivered. The M0
   proof is the local sink; `doctor`'s `last verification` line is `skip` when nothing is recorded.
   Coming back: `plan/roadmap.md` §1, which also states what to decide first (consumer, read path,
   header→dimension mapping).
5. Config writes: keep them opt-in per harness, or print-only with the user pasting. Print-only
   is safer for M2, one-click is the better long-term UX.
6. Header naming: ship neutral `X-AoE-*` names and let a template/consumer-facing alias fix the
   case where a system wants its own name, or make the wire name fully operator-chosen. Not
   blocked on any vendor; a deployment that needs `x-litellm-tags` configures it.
7. How many headers ship enabled. Proposal: one combined tag header on, one discrete session
   header off, everything else operator-added — with the cost of each extra header being one
   more line in every harness's static config.
8. Signed session ids: ship `hmac` in M1 or defer to M2. A consumer that only attributes does
   not need it; one that validates does.

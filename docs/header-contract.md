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

### Targets: where a payload goes, and which slice

A target chooses **where** the payload travels and **what** of it travels there.

| Field | Meaning |
|---|---|
| `kind` | `header`, `query` or `body`. A harness that cannot carry a kind is refused rather than written. |
| `name` | Wire name: a header name, a query parameter, or the body field to set. |
| `format` | `json` (default) sends the whole payload object; `csv` sends the flat `team:core,feature:billing` list an older consumer expects. |
| `template` | Overrides the format for one target and picks the slice itself: `{tags}`, `{tags_json}`, `{payload_json}`, `{tags_b64}`, or one field such as `{feature}`. |
| `literal` | Written as-is instead of the rendered template. |
| `env_var` | Variable that carries the rendered value at launch: `AOE_HDR_` / `AOE_QRY_` / `AOE_BODY_` + the name in upper snake case (`X-AoE-Tags` → `AOE_HDR_X_AOE_TAGS`); pinnable for a harness whose config already names one. |
| `enabled` | Lets a set ship dormant (`X-AoE-Group` off by default, since group names are user prose and may be sensitive). |

Template fields, resolved from the session manager first and the hook environment second:

`{session}` `{session_id}` `{feature}` `{tool}` `{profile}` `{group}` `{group_slug}` `{project}`
`{project_slug}` `{branch}` `{tags}` `{tags_csv}` `{tags_json}` `{payload_json}` `{tags_b64}`

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
  text, because a comma list has no types.

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
   the copy is deleted when the session is gone. A copy that would carry nothing is not used.
3. **Command** (fallback). Harnesses that accept a shell command as a header value call the CLI
   directly (`!aoe-llm-tags value X-AoE-Session`). Same values, one process later, and subject to
   the harness's own caching.

Neither path can update mid-session. A consumer that needs liveness has to query the control
plane instead of trusting the header; the header identifies, it does not assert current state.

## What the plugin does not decide

Deliberately open, because they are consumer-side and differ per deployment:

- which system is authoritative (any HTTP-visible hop: gateway, egress proxy, log pipeline, WAF);
- the header names, beyond shipped defaults;
- whether values are signed, and by which key;
- retention, aggregation and access control on the consumer side;
- the canonical tag vocabulary — `feature:` and `session:` are defaults, not a schema.

A design decision that only works for one of these systems is a deployment choice. Keep it in
`reference-deployment.md`.

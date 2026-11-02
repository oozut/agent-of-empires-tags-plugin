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

1. **Presence.** Every outbound LLM request from a session carries the configured headers, or the
   plugin says plainly that it cannot, and why (`doctor`).
2. **Derivation.** Header values are computed from the session's identity and the plugin's
   settings, at launch, by one implementation shared by every harness and both hook substrates.
3. **Opacity.** Nothing else about the request changes: no body, no prompt, no response, no
   ordering, no extra network hop. Values the plugin emits are strings it constructed; it never
   inspects traffic.

Everything else is a consumer's business: whether a header is trusted, parsed, stored, expired,
charged against, or ignored.

## What is in the headers

A set of targets, not a single tag. The tag set is resolved once (`tags` plus the rule matches);
each target then chooses **where** it travels — a request header, a query parameter, or a
request-body field — and **which slice** of the set it carries:

| Field | Meaning |
|---|---|
| `kind` | `header` (default), `query` or `body`. A harness that cannot carry a kind is refused rather than written (pi appends the endpoint path to `baseUrl`, so it has no query channel). |
| `name` | Wire name, e.g. `X-AoE-Tags`, `X-AoE-Session`, `x-tenant`, or a parameter name. A header name must be a valid HTTP field name; no plugin-owned prefix is required. |
| `template` | Value template with fields from the session, e.g. `aoe,feature:{feature},session:{session}`, or `{tags}` for the whole set. |
| `literal` | Optional override: written as-is instead of the rendered template. A literal that is JSON object/array becomes a JSON value in the session config. |
| `env_var` | Variable that carries the rendered value at launch. Default: `AOE_HDR_` / `AOE_QRY_` / `AOE_BODY_` + the name in upper snake case (`X-AoE-Tags` → `AOE_HDR_X_AOE_TAGS`); pinnable for compatibility. |
| `enabled` | Lets a set ship dormant (`X-AoE-Group` off by default, since group names are user prose and may be sensitive). |

Available template fields, all resolved from the session manager first and the hook environment
second:

`{session}` `{session_id}` `{feature}` `{tool}` `{profile}` `{group}` `{group_slug}` `{project}`
`{project_slug}` `{branch}` `{tags}`

Rules that keep the contract honest:

- **Low cardinality by default.** `{session}` is the short id (`S-9f3a`); `{session_id}` is the
  full id and is opt-in, because a billing dimension with one bucket per session is unusable.
- **Never user prose.** Titles, group names and paths are opt-in, as slugged or hashed forms.
  A path can contain a username; a title can contain anything at all.
- **Fixed per launch.** The value cannot change while the process lives — that is a property of
  the delivery mechanism, not a limitation to be worked around.
- **Bounded.** At most 5 tag values per target, and the grammar is the consumer's; the plugin
  emits lowercase `[a-z0-9._:-]` tokens, one list per target, comma-separated by default.

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

Two paths, both fed by the same resolved values:

1. **Environment** (default). The hook prints `AOE_HDR_…=<value>` lines; a harness maps its header
   to that variable once, statically. This is the portable path — it needs no proxy, no daemon,
   and no network call from inside a sandbox.
2. **Command** (fallback). Harnesses that accept a shell command as a header value call the CLI
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

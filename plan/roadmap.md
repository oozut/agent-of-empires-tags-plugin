# Roadmap — deferred work

Things the plan deliberately does **not** build now. Each entry says what it is, why it was
deferred, and what has to be decided before it comes back, so a later session can restart it
without re-deriving the reasoning.

The rule that put most of this here: **the plugin mints and delivers the header; it does not ask a
consumer what happened to it.** Reading a consumer back needs a named consumer, a read credential
and a mapping from header to dimension — a deployment decision, not a plugin feature. Deferring it
costs nothing in M0–M2 and removes a hard dependency on one vendor's API. Decided 2026-10-29
(open decision #4, resolved as *deferred*).

## 1. Consumer read-back (`verify`, usage views)

**What was planned:** `verify [--window]` sends one request and finds the header in the consumer's
records, writing a "last verification" into plugin settings; the M3 page shows spend or request
count per tag, with a consumer adapter interface (`kind`, `base_url`, `token_env`, `window`,
`capabilities[]`) as the only vendor code.

**Why deferred:** it needs all three of (a) a consumer named as authoritative, (b) a read path the
plugin may use, (c) whether headers become a dimension there at all. On any deployment where one
of them is missing, the feature degrades to "cannot answer", which is a lot of machinery for a
message. The M0 proof already shows the header leaving the harness (`tools/header-sink.py`), which
is the part the plugin controls.

**Where it lives in the plan, marked as gated:** `plan/m2.md` task 5 (M2-T05), `plan/m3.md` in
full, `plan/parameters.md` §10, `docs/implementation.md` steps 5/8/9.

**To un-defer:** answer open decision #4 — name the system, its read path, and the dimension it can
return — then implement `verify` against it and let `doctor`'s `last verification` line switch from
`skip` to a recorded value. Nothing already built needs to change: the resolver, hook, `setup` and
`doctor` are unaffected, and `last_verified` is still read by `doctor` when present.

**Cheap probe when a consumer is named:** send one request through the real path with a distinctive
tag (`aoe,feature:sink-probe,session:S-0000`), then look for that string in the consumer's records.
If the string is absent, the hop that dropped it is the thing to change — that is deployment
configuration, not plugin code.

## 2. Signed session ids (`session_signature = "hmac"`)

Deferred with open decision #8. Only a consumer that *validates* sessions needs it (a consumer that
attributes does not), so it lands with the read-back work, not before. Shipped constant stays
`none`; `signing_key` stays empty and unused.

## 3. Windows in the release matrix

Blocked by the host, not by us: `ReleaseBinary` has one `bin` value and Windows needs the `.exe`
suffix, so a Windows asset cannot be named by the same template (`plan/parameters.md` §11, open
decision #10). Revisit when the host accepts a per-platform `bin`, or when a Windows user shows up
with a build-from-source story that works.

## 4. Release-path verification from a tag

`PluginSource::Local` + `kind = "release-binary"` is rejected by the host's fetcher, so the release
form can only be exercised by a GitHub-sourced install. Until a `v*` tag is pushed and installed
that way, the release half of `plan/parameters.md` §11 is build-verified (CI artifacts), not
install-verified. Pushing the first tag is the whole task.

## 5. Live per-request tagging

A launch-time hook fills a header once per process: a harness reads its config once and caches
what it finds there, so a value that changes *within* a session — and anything that says what the
session is **doing** rather than what it **is** — is not reachable from the hook
(`docs/harness-adapters.md` §Per-request values).

For a harness that loads code from its config directory this needs no proxy and no new capability:
the plugin ships a small extension, the harness runs it in-process, and pi re-emits
`before_provider_headers` before each model request, so a tag edited mid-session reaches the next
request. It is code running inside the agent, so it is opt-in twice — intent in the
`harness_extension` setting, approval of one exact bundle recorded with its sha256 — and revocable.

The activity itself does not depend on that hook. A **skill** plus one always-loaded reminder is
what makes the agent keep the session's activity current, and every harness this plugin has a row
for can take those two: into the session's copy where the harness reads one (`pi`, `vibe`), or into
the harness's own config behind a marker block where it does not (`claude`, `codex`, `opencode`,
`hermes`, `gemini`, `droid`, `qwen`, `kiro`) — a wider grant, disclosed as such. What the hook adds
is only timeliness: `pi` re-reads the value per request, so an activity change reaches the next
request; everywhere else it reaches the next launch. See `plan/m5.md`.

**Still rejected: a tagging proxy.** A listener in the request path would buy the same thing for
every harness at the price of a new capability (`net`), a second process that can fail requests,
and a secret to hold. Two independent harnesses with an in-process hook are worth more than one
proxy, and a harness with no hook stays launch-scope — stated in `doctor`, in the support matrix
and in the disclosure.

The remaining upstream option is unchanged: aoe itself could expose a per-request hook
(`plan/milestones.md` §Upstream). If it does, this extension is the migration path, not a dead end.

## 6. A tag board — in a pane, not a page

**What was planned:** an M3 "page board" (`slot = "page"`, `llm_tags_page`) drawing spend or
request count per tag, unseen headers and sessions with no spend (`plan/m3.md`).

**Decided against, 2026-11-15:** the plugin now declares **one pane and no page**. The host draws a
dashboard-header button for every declared `page` — with the generic puzzle piece when the page
names no icon — so a page is a second LLM Tags entry in the header, and the surface it offered (a
JSON view of the stored values, the literal token, the extension approvals) is either in the pane
already or on the CLI (`aoe-llm-tags tag`, `extension enable|disable`). `tests/docs.rs`
(`the_plugin_declares_a_pane_and_no_page`) fails if a manifest grows a `page` back.

**If a board comes back:** it is a fourth tab in the existing pane, or a second pane — never a
page. The pane vocabulary has no table kind, so it would be `bar`/`sparkline` rows plus a `note`.

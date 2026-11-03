# Parameters

Every name, value and default the plugin uses, in one place. `plan/m0.md`–`m4.md` reference these
tables; a milestone file only repeats the ones it *introduces*. When implementation needs a value
that is not here, add it here in the same change.

**Status** — `decided`: fixed by the contract or an earlier decision; `proposed`: this plan's
default, changeable until the milestone ships; `open`: named in `milestones.md` §Open decisions.

## 1. Identity and install

| Parameter | Value | Status |
|---|---|---|
| Plugin id | `dev.oozut.llm-tags` | decided |
| Manifest file | `aoe-plugin.toml` (repo root) | decided |
| `name` | `LLM Tags` | proposed |
| `version` | `0.1.0` (M0), semver after | proposed |
| `api_version` | `17` is the floor this plugin ships at (aoe ≥1.20.0 supports 1..=17); `18` — required by `per_project` — is available on the ≥1.21.0 host; `19` only for `session.lifecycle.changed` (M1-T08) | decided: floor 17, target 18 |
| Host version checked | aoe **1.21.0** on this host (2026-10-29), so 18 is available; the manifest stays at 17 through M0 because nothing in M0 uses a schema-18 feature. The bump lands in the M1-T03 commit together with `aoe_version` | decided |
| `aoe_version` | `">=1.20.0, <2.0.0"` at 17; raise to `">=1.21.0, <2.0.0"` when the manifest moves to 18 | proposed |
| `capabilities` (M0) | `runtime.worker` — declaring any runtime requires it, and the grant is prompted on install/update | decided |
| `capabilities` (M1+) | `runtime.worker`, `session.read`, `net` | proposed |
| Install dir | `<app_dir>/plugins/<id>/` (host: `/var/lib/aoe/.config/agent-of-empires/plugins/dev.oozut.llm-tags/`) | decided |
| CLI path | `<install>/bin/aoe-llm-tags`, mode 0755, a Rust binary (no interpreter) | decided |
| Worker entrypoint | `<install>/bin/aoe-llm-tags-worker`, stdio JSON-RPC, newline-delimited | decided |
| Source layout | one library crate `llm_tags` + two bins (`src/main.rs`, `src/worker_main.rs`); integration tests in `tests/` | decided |
| Toolchain | Rust ≥1.89, edition 2021, `--locked` (committed `Cargo.lock`), `clippy -D warnings` | decided |
| Build artifacts | `<install>/.aoe-build/` only (excluded from the tree hash, and refused if present in an install tree) | decided |

## 2. Hook contract (M0)

| Parameter | Value | Status |
|---|---|---|
| Keys | `[host_hooks].before_session` **and** `.before_start`, same command string | decided |
| Placement | global `<app_dir>/config.toml` (recommended); per profile `<app_dir>/profiles/<name>/config.toml`. Profile arrays replace global arrays | decided |
| `AOE_BIN` | `AOE_BIN` → `PATH` lookup → `/usr/local/bin/aoe` | proposed |
| Contract with aoe | stdout `KEY=VALUE` per line, later wins, no trailing whitespace, exit 0 always, 2 s bound on any subprocess | decided |

## 3. Settings (`[plugins."dev.oozut.llm-tags".settings]`)

| Key | Type | Default | Allowed | Milestone | Status |
|---|---|---|---|---|---|
| `tags` | `string_list` | `[]` | lowercase tags, ≤48 chars, deduped, cap 8 | M2 | proposed |
| `targets` | `object_list`, item key `name` | 2 rows below | 0–8 rows | M2 (manifest); `headers` still read | proposed |
| `targets[].kind` | `select` | `header` | `header`, `query`, `body` | M2 | proposed |
| `targets[].name` | string | `X-AoE-Tags` | HTTP field-name grammar, or a param name | M0 as `header` | decided |
| `targets[].template` | string | `aoe,feature:{feature},session:{session}` | template fields below, `{tags}` for the whole set | M0 | decided |
| `targets[].env_var` | string | derived: `AOE_HDR_` / `AOE_QRY_` / `AOE_BODY_` + upper snake of name | valid env name | M0 | proposed |
| `targets[].param` | string | `""` → use `name` | `[A-Za-z0-9_.\-\[\]]`, ≤64 | M2 | proposed |
| `targets[].literal` | string | `""` | any string | M2 | proposed |
| `targets[].enabled` | boolean | `true` (second row: `false`) | — | M0 | proposed |
| `rules` | `object_list`, item key `match_value` | `[]` | — | M0 | proposed |
| `rules[].tag` | string | — | tag grammar, ≤32 chars | M0 | decided |
| `rules[].match_kind` | string | `project` | `project`, `group`, `tool`, `title_regex`, `manual` | M0: `project`, `manual`; M1: all | decided |
| `rules[].match_value` | string | — | kind-dependent | M0 | decided |
| `rules[].priority` | integer | `0` | −100…100 | M1 | proposed |
| `session_tagging` | string | `short_id` | `none`, `short_id`, `full_id` | M0 | decided |
| `session_signature` | string | `none` | `none`, `hmac` | M1 | open (#8) |
| `signing_key` | string | `""` | ≥16 chars when signature is `hmac` | M1 | open (#8) |
| `feature_tag` | string | `""` | one tag entry: `feature:billing`, a bare `billing` (= `feature:billing`), or a stored pair `{ key = "feature", value = "billing" }` | M1 | decided |
| `feature_tag` scope | — | `per_project = true` (requires `api_version ≥ 18`) | global, profile and project hold the same entry grammar | M1 | decided |

M0 rule semantics (implemented in `src/resolver.rs`, M0-T02):

- `project` matches `match_value` against the project slug (basename, lowercased, non-alphanumerics
  → `-`) or the full project path, so both `agent-of-empires-tags-plugin` and the absolute path work.
- `manual` is an **unconditional** operator-written tag in M0: it matches every session.
  `match_value` is only the `object_list` item key. Assignable per-session manual tags arrive in
  M3-T06; until then an unconditional rule is what a hand-written settings block can honour.
- M1 kinds (`group`, `tool`, `title_regex`) are parsed and kept but never match in M0, each with one
  stderr warning per launch. Unknown kinds likewise never match.
- Settings resolution: global `config.toml`, then the profile's sparse override file merged onto it
  the way aoe merges (object keys recurse, **arrays replace**). A missing section means the defaults
  below; hand-written settings survive the plugin being disabled.

Default header rows:

```toml
# 1) combined tag header — cost attribution, log joining
header = "X-AoE-Tags"
env_var = "AOE_LLM_TAGS"
enabled = true
template = "aoe,feature:{feature},session:{session}"

# 2) discrete session header — session validation/management; off until the operator wants it
header = "X-AoE-Session"
enabled = false
template = "{session_id}"
```

## 4. Template fields

| Field | Source | Example | Note |
|---|---|---|---|
| `{session}` | short id | `S-cf11` | default; `S-` + first 4 of the session id |
| `{session_id}` | row `id` | `cf11af87da7e4bf0` | high cardinality, opt-in |
| `{feature}` | matching rules | `review` | first matching rule's tag, else `feature_tag` when its key is `feature`, else empty |
| `{tags}` | all matching rules | `review,fix` | comma-joined, capped |
| `{tool}` | row `tool` | `pi-proxy` | harness/tool id |
| `{profile}` | row `profile` | `main` | |
| `{group}` | row `group` | `Backend` | prose, opt-in |
| `{group_slug}` | slugged group | `backend` | preferred over `{group}` |
| `{project}` | row `path` | `/var/lib/aoe/…/koreans` | may contain a username; opt-in |
| `{project_slug}` | basename of path | `agent-of-empires-tags-plugin` | preferred |
| `{branch}` | `aoe list --json` `worktree.branch` | `lithuanians` | resolved only when a template references it |

Template-resolution notes (implemented in `src/resolver.rs`, M0-T02):

- `{branch}` costs a **second** `aoe` call, so it is resolved only when an enabled template
  actually references it. The row query and the list query share one 2 s launch budget; when the
  budget is gone the field renders empty with a warning instead of delaying the session.
- `{group_slug}` slugs the whole group string (`Backend/web` → `backend-web`); `{project_slug}`
  slugs the basename of `{project}`.
- A field the session genuinely lacks renders empty (never an error). A field that does not exist
  at all (a template typo) also renders empty, with one stderr warning naming it.

## 5. Value rules

| Parameter | Value | Status |
|---|---|---|
| Tag grammar | `[a-z0-9][a-z0-9._:-]*` per token | decided |
| Tag cap per header | 5 | decided |
| Rendered value max | 800 bytes | proposed |
| Default separator | `,` between tokens; `feature:<tag>` for a rule hit | decided |
| Forbidden content | titles, raw paths, group prose, newlines, non-ASCII unless the operator writes them into a template | decided |
| Empty result | empty value ⇒ the env var is minted empty and the harness sends an empty header | open |
| Over-long render | truncate to 800 bytes on a UTF-8 boundary, warn on stderr, still exit 0 | proposed |
| Control characters | stripped from the rendered value (a newline would corrupt the `KEY=VALUE` stdout contract); outer whitespace trimmed | decided |
| Invalid header / env name | that header is skipped with a warning; `doctor` reports it | decided |
| Invalid / over-cap rule tags | invalid tags dropped with a warning; valid tags deduped, then capped at 5 | decided |
| Absent `targets`/`headers` key | built-in target set (default rows above); an explicit `targets = []` means the tags are resolved but delivered nowhere | decided |
| A target whose `kind` the harness cannot interpolate | `literal` is written verbatim; without one, `setup --apply` refuses the whole plan | decided |
| A target whose `kind` the harness does not support | `setup --apply` refuses the whole plan; `doctor` fails that target | decided |
| Malformed config TOML | warn on stderr, ignore that file (profile falls back to global, global to defaults), exit 0 | decided |

## 6. CLI surface

| Command / flag | Value | Milestone |
|---|---|---|
| `hook` | prints one `env_var=value` per enabled header (implemented, M0-T02) | M0 |
| `value [header]` | prints one header's value; default `X-AoE-Tags` (implemented, M0-T02) | M0 |
| `doctor` | report; exit 1 if any check fails | M0 |
| `setup --print` | hook lines + mapping lines, writes nothing | M0 |
| `setup <agent> --dry-run` | diff only | M2 |
| `setup <agent> --yes` | apply with backup | M2 (open #5) |
| `setup <agent> --undo` | restore the backup / remove the marker block | M2 |
| `verify [--window 24h]` | one request, then look for the header consumer-side — **roadmap-gated** (`plan/roadmap.md` §1): not in M0–M2 | roadmap |
| `--session` `--profile` `--tool` `--project` `--group` `--title` | fallbacks for row fields (values from `AOE_*`) | M0 |
| `--adapter <agent>` | pin the adapter row instead of detecting | M0 |
| `--json` | machine-readable output (doctor, status) | M0 |
| Exit codes | `0` ok/degraded, `1` a failing `doctor` check, `2` usage error | M0 |

## 7. Runtime constants

| Constant | Value | Why |
|---|---|---|
| Session-row query timeout | 2 s | hook must never block a launch |
| Launch budget | 2 s total across every subprocess (row query + optional `aoe list` for `{branch}`) | one slow `aoe` delays a launch once, never twice |
| Fallback on timeout/error | hook environment, exit 0 | a non-zero hook exit aborts the session |
| Doctor output | absolute paths always | hook debugging without paths is the expensive failure |

## 8. Adapter data (`adapters.toml`)

| Key | Values | Note |
|---|---|---|
| `agent` | `pi`, `claude`, `codex`, `opencode`, `gemini`, `vibe`, `kiro`, `cline`, `openclaw`, `hermes` | aoe tool id |
| `label` | display string | |
| `mechanism` | `config_headers`, `command_headers`, `custom_headers_env`, `headers_from_env`, `url_param`, `none` | |
| `interpolates` | `header`, `query`, `body` — where this harness expands `$NAME` | a `body` target elsewhere needs `literal` |
| `supports` | kinds the harness can carry at all; default `["header"]` | pi: `["header","body"]` (verified); no query channel |
| `env_ref` | template for an env reference, default `${env_var}` | opencode uses `{{env:{env_var}}}` |
| `config_env` | env var name or absent | directory override |
| `config_paths[]` | ordered paths, first hit wins | must not include a regenerated dir |
| `config_format` | `json`, `toml`, `yaml`, `none` | |
| `status` | `verified`, `documented`, `needs_research`, `unsupported` | per row |
| `verified_with` | `{ agent_version, aoe_version }` or absent | set only by a real run |

## 9. Discovery and config edits (M2)

| Parameter | Value | Status |
|---|---|---|
| Discovery verdicts | `wired`, `detected-not-wired`, `sandbox-stale`, `unknown`, `unsupported` | decided |
| Cache key | config path + file sha256 | proposed |
| Marker written into edited files | `aoe-llm-tags: managed` (comment syntax of the format) | proposed |
| Backup suffix | `.aoe-llm-tags.bak` (same directory, one generation) | proposed |
| Write method | temp file in the same directory + atomic rename; refuse symlinks and non-regular files | decided |

## 10. Usage views (M3) — roadmap-gated

Every parameter in this section belongs to the deferred consumer read-back (`plan/roadmap.md` §1),
not to a milestone that can start today.

| Parameter | Value | Status |
|---|---|---|
| Page slot / id | `page` / `llm_tags_page` | proposed |
| Page payload cap | 64 KiB (`ui.state.set`) | decided by host |
| Block types | `sparkline`, `bar`, `table` | proposed |
| Consumer adapter keys | `kind`, `base_url`, `token_env`, `window`, `capabilities[]` (`spend`, `requests`, `sessions`) | proposed |
| Default window | 24 h; presets 1 h / 24 h / 7 d / 30 d | proposed |

## 11. Distribution and release

Cross-cutting, paid for in M0-T13; not ordered with the milestone sections above.

| Parameter | Value | Status |
|---|---|---|
| Source install runtime | `[runtime] kind = "command"`, `command = ["./bin/aoe-llm-tags-worker"]` | decided |
| Source install build | `[[runtime.build]] command = ["./tools/build-worker.sh"]`, `platforms = ["linux", "macos"]` | decided |
| Build script contract | search `CARGO_BIN` → `PATH` → `${CARGO_HOME:-$HOME/.cargo}/bin` → `/usr/local/{cargo,lib}/…`; `cargo build --release --locked --target-dir .aoe-build`; `install -m 755` both binaries into `bin/` | decided |
| Release runtime | `[runtime] kind = "release-binary"`, `bin = "aoe-llm-tags-worker"` | decided |
| Asset template | `aoe-llm-tags-${os}-${arch}.tar.gz` (`${os}` = `linux`/`macos`, `${arch}` = `x86_64`/`aarch64`); no `${version}` — one asset set per release tag | proposed |
| Asset contents | `bin/aoe-llm-tags`, `bin/aoe-llm-tags-worker`, `README.md`, `LICENSE` — the install layout, so unpacking is the install | decided |
| CI matrix | `ubuntu-latest`→`x86_64-unknown-linux-gnu`, `ubuntu-latest`→`aarch64-unknown-linux-gnu` (gcc cross linker), `macos-13`→`x86_64-apple-darwin`, `macos-14`→`aarch64-apple-darwin` | proposed |
| CI gates | `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo test --locked`, one `hook` smoke run | decided |
| Release trigger | tag `v*` → build matrix → attach the four tarballs to the GitHub release | proposed |
| Windows | not in the matrix: the manifest has one `bin` value and Windows needs the `.exe` suffix (a host limitation, not a decision) | open (#10) |
| Why the two forms | `PluginSource::Local` + `release-binary` errors in `src/plugin/fetch.rs`, so a checkout can only build; a GitHub source downloads | decided |

Verification split, and it must stay named:

- **Source install** — verified on this host (2026-10-29): `aoe plugin update` compiled both
  binaries in 26 s, `aoe plugin info` reports `caps: runtime.worker (granted)`, the reloaded
  daemon spawned `<install>/bin/aoe-llm-tags-worker` (its log exists under
  `<app_dir>/plugin-workers/`), and `doctor` is green from the installed binary.
- **Release install** — verified only by the CI artifacts plus a GitHub-sourced `aoe plugin
  install gh:oozut/android-keyboard-llm-tags`; it cannot be exercised from a local checkout. An
  asset-name mistake surfaces as the host's "asset not found, available: …" error.

## Open

| # | Question | Affects |
|---|---|---|
| 1 | Accumulate every matching rule, or first match wins | `rules[]` semantics, `{tags}` |
| 2 | Value grammar per header (comma list vs JSON vs custom template) | `headers[].template` |
| 3 | `{session}` short vs full id default | `session_tagging` |
| 4 | ~~Which consumer is the reference for `verify` here~~ — **resolved 2026-10-29: deferred.** The plugin mints and delivers; it does not read a consumer back. `verify` and every `consumer.*` parameter are roadmap (`plan/roadmap.md` §1), and `doctor`'s `last verification` line is `skip` until one is recorded | `verify`, `consumer.*`, M3 |
| 5 | Apply config edits on `--yes`, or print only | `setup` flags, §9 |
| 6 | Neutral `X-AoE-*` names vs operator-chosen wire names | default header rows |
| 7 | How many headers ship enabled | default header rows, every adapter's static config |
| 8 | Ship HMAC session signatures in M1 or M2 | `session_signature`, `signing_key` |
| 9 | Empty render: mint an empty value, or emit no line | hook output, header presence |
| 10 | Windows: wait for a host that accepts a per-platform `bin`, or ship a `aoe-llm-tags-worker.exe` alias | §11 asset/`bin` naming |

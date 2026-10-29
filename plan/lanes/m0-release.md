# Lane `release` — branch `m0-release`

Owns the manifest's `runtime` half, the build script, the crate's toolchain pin and the CI
workflow. Nothing here changes what a header *is*; everything here decides how the code gets onto
a host and how the host starts the worker.

**Read first:** `plan/m0.md` §M0-T13, `plan/parameters.md` §1 and §11, `docs/implementation.md`
§Step 10.

## Tasks, in order

| ID | Do | Output that proves it |
|---|---|---|
| M0-T13 | Rust crate (`Cargo.toml`, `src/`, `tests/`), `tools/build-worker.sh`, `[runtime]` + `capabilities` in `aoe-plugin.toml`, `.github/workflows/ci.yml` | `aoe plugin update` builds both binaries; `aoe plugin info` shows `runtime.worker (granted)`; `cargo test` green |

## Facts you must not re-derive

- `release-binary` launches `bin` **with no arguments**, so the worker is its own binary. A
  subcommand of the CLI cannot be the runtime entry point.
- `src/plugin/fetch.rs::download_release_binary` requires `PluginSource::Github` and **bails for
  `PluginSource::Local`**. A checkout-based install can only be `kind = "command"`; the release
  path is verified by CI artifacts and by a GitHub-sourced install.
- The asset name is rendered from a template with `${os}`, `${arch}`, `${version}`; a missing
  asset errors with the list of available names, so over-naming is a loud failure, not a silent
  one.
- Declaring any runtime needs `capabilities = ["runtime.worker"]` and the grant it implies — an
  update that changes the capability set prompts for re-approval (`--yes` in scripts).
- `.aoe-build/` is reserved: the host refuses to install a tree that already contains it. Local
  builds therefore use `CARGO_TARGET_DIR` outside the worktree (or `target/`), and only the build
  step may create `.aoe-build/`.
- The daemon's `PATH` has no `~/.cargo/bin`, which is why `tools/build-worker.sh` searches for
  cargo instead of assuming it is on `PATH`.

## Handoff

- Loudly, when the installed binary passes `doctor`: `wire` regenerates its two hook lines from
  `setup --print` (the path does not change, the file it points at does).
- Quietly: a release-tag install is the only consumer of the `release-binary` half until a tag is
  pushed, so keep that difference named in `parameters.md` §11 rather than pretending it is
  tested.

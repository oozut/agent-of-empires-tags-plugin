#!/bin/sh
# Install-time build for a source install: produce the two binaries in bin/.
#
# A release install never runs this — the same two files arrive in the tarball —
# so the only requirement here is a Rust toolchain somewhere standard. The host
# daemon's PATH does not include ~/.cargo/bin, hence the search.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

cargo_bin=${CARGO_BIN:-}
if [ -z "$cargo_bin" ]; then
  if command -v cargo >/dev/null 2>&1; then
    cargo_bin=$(command -v cargo)
  else
    for candidate in "${CARGO_HOME:-$HOME/.cargo}/bin/cargo" \
                     /usr/local/cargo/bin/cargo \
                     /usr/local/bin/cargo \
                     /usr/bin/cargo; do
      if [ -x "$candidate" ]; then
        cargo_bin=$candidate
        break
      fi
    done
  fi
fi
if [ -z "$cargo_bin" ]; then
  echo "aoe-llm-tags: no cargo found; install a Rust toolchain (>= 1.89)" \
       "or install the plugin from a release" >&2
  exit 1
fi

# .aoe-build/ is the host-managed build directory, but a checkout that already contains one is
# refused by `aoe plugin install/update` ("ships the reserved build-output directory"). Honour
# CARGO_TARGET_DIR so a developer can build outside the tree and still install from it.
target_dir=${CARGO_TARGET_DIR:-.aoe-build}
"$cargo_bin" build --release --locked --target-dir "$target_dir"
mkdir -p bin
install -m 755 "$target_dir/release/aoe-llm-tags" bin/aoe-llm-tags
install -m 755 "$target_dir/release/aoe-llm-tags-worker" bin/aoe-llm-tags-worker

#!/bin/sh
# Builds the static remote helper (harness-remote-agent) for the given
# musl targets and lays the files out as harness_remote::install expects:
#   <out>/harness-<version>-<arch>-linux-musl  plus a .sha256 sidecar.
#
# usage: scripts/build-remote-helpers.sh OUT_DIR [ARCH...]   (default: x86_64 aarch64)
#
# Needs the rustup targets (<arch>-unknown-linux-musl). Cross builds use
# cargo-zigbuild when it is installed, else plain cargo (native target only).
set -eu
out=${1:?usage: $0 OUT_DIR [ARCH...]}
shift
[ $# -gt 0 ] || set -- x86_64 aarch64
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
mkdir -p "$out"
for arch in "$@"; do
  target="$arch-unknown-linux-musl"
  if command -v cargo-zigbuild >/dev/null 2>&1; then
    cargo zigbuild --release -p harness-remote --bin harness-remote-agent --target "$target"
  else
    cargo build --release -p harness-remote --bin harness-remote-agent --target "$target"
  fi
  f="$out/harness-$version-$arch-linux-musl"
  cp "target/$target/release/harness-remote-agent" "$f"
  ( cd "$out" && sha256sum "$(basename "$f")" > "$(basename "$f").sha256" )
  echo "built $f"
done

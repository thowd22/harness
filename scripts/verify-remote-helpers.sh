#!/bin/sh
# Verifies the static remote helpers written by build-remote-helpers.sh:
#   * every harness-<version>-<arch>-linux-musl has a matching .sha256 sidecar,
#   * the ELF machine matches <arch>,
#   * the binary is static (no PT_INTERP program header, no NEEDED entries),
#   * it answers the `hello` handshake with the expected protocol version,
#     arch and helper version. Foreign arches run under qemu-<arch> when it is
#     on PATH (or QEMU_<ARCH> names it); otherwise the run is skipped, unless
#     REQUIRE_RUN=1, in which case a skipped run is a failure.
#
# usage: scripts/verify-remote-helpers.sh DIR [ARCH...]   (default: x86_64 aarch64)
set -eu
dir=${1:?usage: $0 DIR [ARCH...]}
shift
[ $# -gt 0 ] || set -- x86_64 aarch64
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
protocol=$(sed -n 's/^pub const PROTOCOL_VERSION: u32 = \([0-9]*\);/\1/p' crates/harness-remote/src/protocol.rs)
host=$(uname -m)
fail=0
err() { echo "FAIL: $*" >&2; fail=1; bad=1; }

for arch in "$@"; do
  bad=0
  name="harness-$version-$arch-linux-musl"
  f="$dir/$name"
  [ -f "$f" ] || { err "$f missing"; continue; }
  [ -f "$f.sha256" ] || { err "$f.sha256 missing"; continue; }
  ( cd "$dir" && sha256sum -c "$name.sha256" >/dev/null ) || err "$name: sha256 sidecar does not match"

  case $arch in
    x86_64) machine='Advanced Micro Devices X86-64' ;;
    aarch64) machine='AArch64' ;;
    *) err "$arch: unsupported arch"; continue ;;
  esac
  readelf -h "$f" | grep -q "Machine:.*$machine" || err "$name: ELF machine is not $arch"
  if readelf -lW "$f" | grep -q 'INTERP'; then err "$name: has an ELF interpreter (not static)"; fi
  if readelf -dW "$f" 2>/dev/null | grep -q 'NEEDED'; then err "$name: links shared libraries"; fi

  runner=""
  if [ "$arch" != "$host" ]; then
    var="QEMU_$(echo "$arch" | tr '[:lower:]' '[:upper:]')"
    runner=$(eval "echo \${$var:-}")
    [ -n "$runner" ] || runner=$(command -v "qemu-$arch" || command -v "qemu-$arch-static" || true)
    if [ -z "$runner" ]; then
      if [ "${REQUIRE_RUN:-0}" = 1 ]; then err "$name: no qemu-$arch to run it"; else echo "skip run: $name (no qemu-$arch)"; fi
      [ "$bad" = 1 ] || echo "checked $name (static, sha256 OK)"
      continue
    fi
  fi
  reply=$( (printf '{"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocol":%s,"version":"%s"}}\n' "$protocol" "$version"; sleep 2) \
    | timeout 30 $runner "$f" --stdio 2>&1 | head -1 ) || true
  for want in "\"arch\":\"$arch\"" "\"protocol\":$protocol" "\"version\":\"$version\""; do
    case $reply in *"$want"*) ;; *) err "$name: hello reply lacks $want: $reply" ;; esac
  done
  [ "$bad" = 1 ] || echo "checked $name (static, sha256 OK, hello OK${runner:+ via $(basename "$runner")})"
done
exit $fail

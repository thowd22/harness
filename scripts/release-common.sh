# shellcheck shell=sh
# Shared helpers for the scripts/release-*.sh scripts. Sourced, not run.
# POSIX sh only: the release scripts run on Linux and macOS runners.

die() {
  echo "error: $*" >&2
  exit 1
}

# valid_version VERSION: succeeds for X.Y.Z or X.Y.Z-pre (no leading v).
valid_version() {
  printf '%s\n' "$1" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$'
}

# strip_v VERSION_OR_TAG: v1.2.3 -> 1.2.3
strip_v() {
  printf '%s\n' "${1#v}"
}

# workspace_version [CARGO_TOML]: the version in [workspace.package].
workspace_version() {
  awk '
    /^\[/ { in_pkg = ($0 == "[workspace.package]") ; next }
    in_pkg && /^version[ \t]*=/ {
      v = $0; sub(/^[^"]*"/, "", v); sub(/".*$/, "", v); print v; exit
    }
  ' "${1:-Cargo.toml}"
}

# sha256_of FILE: prints the hex digest (sha256sum on Linux, shasum on macOS).
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

# write_sha256 FILE: writes FILE.sha256 in `sha256sum -c` format, next to FILE.
write_sha256() {
  ( cd "$(dirname "$1")" && n=$(basename "$1") && printf '%s  %s\n' "$(sha256_of "$n")" "$n" > "$n.sha256" )
}

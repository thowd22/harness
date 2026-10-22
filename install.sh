#!/bin/sh
# Installs the harness binary from a GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/thowd22/harness/main/install.sh | bash
#
# Environment:
#   HARNESS_VERSION      version to install, e.g. 0.2.0 (default: latest release)
#   HARNESS_INSTALL_DIR  where to put the binary (default: ~/.local/bin)
#
# It never uses sudo. It downloads harness-VERSION-TARGET.tar.gz and its
# .sha256 file, checks the digest, then moves the binary into place in one
# rename, so re-running it upgrades safely. Everything is inside main(), which
# is only called on the last line: a truncated download runs nothing.
#
# Every download is HTTPS-only, redirects included. HARNESS_RELEASES_URL and
# HARNESS_API_URL exist only so tests can point the script at a local fake
# release server: they are refused unless HARNESS_INSTALL_TEST=1 is also set,
# and when honoured they print a warning naming the URL and allow a plain-http
# first request (redirects must still be HTTPS).

set -eu
# Fail a pipeline when any part fails, where the shell supports it (bash does).
# shellcheck disable=SC3040 # guarded: only set where the shell supports it
(set -o pipefail) 2>/dev/null && set -o pipefail

repo=thowd22/harness

say() { printf 'harness-install: %s\n' "$*"; }
die() {
  printf 'harness-install: error: %s\n' "$*" >&2
  exit 1
}

# test_mode: true when the test-only URL overrides are in effect.
test_mode() { [ "${HARNESS_INSTALL_TEST:-}" = 1 ]; }

# fetch URL FILE: download URL to FILE with curl or wget, HTTPS only. Only in
# test mode may URL itself be plain http; a redirect must always be HTTPS.
fetch() {
  case $1 in
    https://*) ;;
    *) test_mode || die "refusing non-HTTPS URL: $1" ;;
  esac
  if command -v curl >/dev/null 2>&1; then
    if test_mode; then
      curl -fsSL --proto '=https,http' --proto-redir '=https' -o "$2" "$1"
    else
      curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"
    fi
  elif command -v wget >/dev/null 2>&1; then
    wget_https "$1" "$2"
  else
    die "need curl or wget"
  fi
}

# wget_https URL FILE: wget cannot restrict the protocol of a redirect, so
# follow redirects by hand (at most 5) and refuse any that is not HTTPS.
wget_https() {
  u=$1 hops=0
  while ! wget -q -S --max-redirect=0 -O "$2" "$u" 2>"$2.hdr"; do
    loc=$(sed -n 's/^ *[Ll]ocation: *\([^[:cntrl:]]*\).*/\1/p' "$2.hdr" | head -n 1)
    [ -n "$loc" ] || return 1
    case $loc in
      /*) loc=$(printf '%s\n' "$u" | sed 's|^\([a-z]*://[^/]*\).*|\1|')$loc ;;
    esac
    case $loc in
      https://*) ;;
      *) die "refusing non-HTTPS redirect: $u -> $loc" ;;
    esac
    hops=$((hops + 1))
    [ "$hops" -le 5 ] || die "too many redirects: $1"
    u=$loc
  done
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    die "need sha256sum or shasum to verify the download"
  fi
}

# target: the release target triple for this machine.
target() {
  os=$(uname -s) arch=$(uname -m)
  case $arch in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
  esac
  # An x86_64 shell under Rosetta on Apple silicon: use the native build.
  if [ "$os-$arch" = Darwin-x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null)" = 1 ]; then
    arch=aarch64
  fi
  case $os-$arch in
    Linux-x86_64 | Linux-aarch64) echo "$arch-unknown-linux-musl" ;;
    Darwin-x86_64 | Darwin-aarch64) echo "$arch-apple-darwin" ;;
    *) die "no prebuilt binary for $(uname -s) $(uname -m). Build from source instead:
  cargo install --git https://github.com/$repo --locked harness" ;;
  esac
}

# override VAR DEFAULT: the URL to use for VAR; a test-only override is honoured
# only with HARNESS_INSTALL_TEST=1, and always with a warning naming it.
override() {
  eval "v=\${$1:-}"
  if [ -z "$v" ]; then
    echo "$2"
  elif test_mode; then
    printf 'harness-install: WARNING: %s is set; downloading from %s (test override, not GitHub)\n' "$1" "$v" >&2
    echo "$v"
  else
    die "$1 is a test-only override and is set to $v; unset it to install from GitHub"
  fi
}

latest_version() {
  fetch "$api/releases/latest" "$1/latest.json" ||
    die "could not look up the latest release (set HARNESS_VERSION to pick one)"
  sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"v\{0,1\}\([^"]*\)".*/\1/p' "$1/latest.json" | head -n 1
}

main() {
  dir=${HARNESS_INSTALL_DIR:-$HOME/.local/bin}
  triple=$(target)
  api=$(override HARNESS_API_URL "https://api.github.com/repos/$repo")
  releases=$(override HARNESS_RELEASES_URL "https://github.com/$repo/releases")
  tmp=$(mktemp -d)
  stage=
  trap 'rm -rf "$tmp"; [ -z "$stage" ] || rm -f "$stage"' EXIT
  trap 'exit 130' INT TERM

  version=${HARNESS_VERSION:-$(latest_version "$tmp")}
  version=${version#v}
  [ -n "$version" ] || die "could not find the latest release version"
  # The version becomes part of a URL and of paths: allow only X.Y.Z[-pre].
  case $version in
    *[!0-9A-Za-z.-]* | .* | *..*) die "invalid version: $version" ;;
  esac

  name="harness-$version-$triple"
  url="$releases/download/v$version/$name.tar.gz"
  say "downloading $url"
  fetch "$url" "$tmp/$name.tar.gz" || die "download failed: $url"
  fetch "$url.sha256" "$tmp/$name.sha256" || die "download failed: $url.sha256"

  want=$(awk '{print $1; exit}' "$tmp/$name.sha256")
  got=$(sha256 "$tmp/$name.tar.gz")
  if [ -z "$want" ] || [ "$want" != "$got" ]; then
    die "sha256 mismatch for $name.tar.gz (expected $want, got $got); nothing was installed"
  fi

  tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
  [ -f "$tmp/$name/harness" ] || die "archive has no $name/harness"

  # Copy next to the target, then rename: the swap is atomic, and a running
  # harness keeps its old inode.
  mkdir -p "$dir"
  stage="$dir/.harness.new.$$"
  cp "$tmp/$name/harness" "$stage"
  chmod 755 "$stage"
  mv -f "$stage" "$dir/harness"
  stage=
  say "installed harness $version to $dir/harness"

  case ":$PATH:" in
    *":$dir:"*) ;;
    *) say "$dir is not on your PATH. Add it, e.g.:
  echo 'export PATH=\"$dir:\$PATH\"' >> ~/.profile" ;;
  esac
}

main "$@"

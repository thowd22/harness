#!/bin/sh
# Tests install.sh against a local fake release server (TASK-139).
#
# Builds fake release assets with scripts/release-package.sh for every target
# in .github/workflows/release-harness.yml (so the names cannot drift), serves
# them with `python3 -m http.server`, and runs `curl ... | bash` with a fake
# uname for each supported platform. Needs python3, curl, wget, tar, shasum.
#
# usage: tests/install-test.sh
#
# `check && pass || fail` is used on purpose: pass only echoes.
# shellcheck disable=SC2015
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
fixtures=$root/tests/fixtures/install
work=$(mktemp -d)
www=$work/www
server_pid=
cleanup() {
  [ -z "$server_pid" ] || kill "$server_pid" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT

fails=0
pass() { echo "ok   - $1"; }
fail() {
  echo "FAIL - $1" >&2
  [ ! -f "$work/out" ] || sed 's/^/       | /' "$work/out" >&2
  fails=$((fails + 1))
}

# --- fake releases -----------------------------------------------------------

targets=$(sed -n 's/^ *- target: *//p' "$root/.github/workflows/release-harness.yml")
[ "$(printf '%s\n' "$targets" | wc -l | tr -d ' ')" -eq 4 ] ||
  { echo "expected 4 release targets in release-harness.yml, got: $targets" >&2; exit 1; }

# release VERSION: one archive + .sha256 per target, named as the release does.
release() {
  for t in $targets; do
    mkdir -p "$work/src/$1-$t"
    printf '#!/bin/sh\necho "harness %s %s"\n' "$1" "$t" > "$work/src/$1-$t/harness"
    chmod 755 "$work/src/$1-$t/harness"
    "$root/scripts/release-package.sh" "$www/download/v$1" "harness-$1-$t" \
      "$work/src/$1-$t/harness" > /dev/null
  done
}
release 0.1.0
release 0.2.0
release 0.0.9
for f in "$www"/download/v0.0.9/*.sha256; do # tampered: digest no longer matches
  printf '%064d  %s\n' 0 "$(basename "${f%.sha256}")" > "$f"
done
mkdir -p "$www/api/releases"
printf '{\n  "url": "x",\n  "tag_name": "v0.2.0",\n  "name": "harness 0.2.0"\n}\n' \
  > "$www/api/releases/latest"
cp "$root/install.sh" "$www/install.sh"

python3 -u -m http.server --bind 127.0.0.1 --directory "$www" 0 > "$work/server.log" 2>&1 &
server_pid=$!
port=
for _ in $(seq 50); do
  port=$(sed -n 's/.* port \([0-9][0-9]*\).*/\1/p' "$work/server.log" | head -n 1)
  [ -z "$port" ] || break
  sleep 0.1
done
[ -n "$port" ] || { cat "$work/server.log" >&2; echo "fake release server did not start" >&2; exit 1; }
base=http://127.0.0.1:$port

# --- helpers -----------------------------------------------------------------

# tools DIR TOOL...: a PATH directory holding only the named tools.
tools() {
  mkdir -p "$1"
  d=$1
  shift
  for t in "$@"; do
    p=$(command -v "$t") || { echo "missing tool for the tests: $t" >&2; exit 1; }
    ln -sf "$p" "$d/$t"
  done
}
basic="sh bash tar gzip awk sed head mktemp rm mkdir cp chmod mv cat dirname"
# shellcheck disable=SC2086 # $basic is a word list
{
  tools "$work/t-wget" $basic wget sha256sum
  tools "$work/t-shasum" $basic curl shasum perl
  tools "$work/t-none" $basic sha256sum
}

n=0
# install [VAR=VALUE...]: `curl BASE/install.sh | bash` with a fake uname
# (FAKE_UNAME_S/M, default Linux x86_64). Output goes to $work/out.
install() {
  # shellcheck disable=SC2016 # $1 expands in the inner sh
  env HOME="$work/home" HARNESS_INSTALL_TEST=1 HARNESS_RELEASES_URL="$base" HARNESS_API_URL="$base/api" \
    FAKE_UNAME_S=Linux FAKE_UNAME_M=x86_64 PATH="$fixtures/bin:$PATH" "$@" \
    sh -c 'curl -fsSL "$1/install.sh" | bash' sh "$base" > "$work/out" 2>&1
}
fresh() {
  n=$((n + 1))
  dir=$work/dir$n
}
has() { grep -qF -- "$1" "$work/out"; }
runs() { [ -x "$dir/harness" ] && [ "$("$dir/harness")" = "$1" ]; }

# --- AC1/AC2: latest release on every supported platform name ----------------

covered=
for p in Linux:x86_64:x86_64-unknown-linux-musl Linux:amd64:x86_64-unknown-linux-musl \
  Linux:aarch64:aarch64-unknown-linux-musl Linux:arm64:aarch64-unknown-linux-musl \
  Darwin:arm64:aarch64-apple-darwin Darwin:x86_64:x86_64-apple-darwin; do
  s=${p%%:*} rest=${p#*:}
  m=${rest%%:*} t=${rest#*:}
  fresh
  if install FAKE_UNAME_S="$s" FAKE_UNAME_M="$m" HARNESS_INSTALL_DIR="$dir" &&
    runs "harness 0.2.0 $t"; then
    pass "$s $m installs latest ($t)"
    covered="$covered $t"
  else
    fail "$s $m installs latest ($t)"
  fi
done
for t in $targets; do
  case " $covered " in
    *" $t "*) pass "release target $t is installed by the script" ;;
    *) fail "release target $t is installed by the script" ;;
  esac
done

fresh
install FAKE_UNAME_S=Darwin FAKE_UNAME_M=x86_64 FAKE_ROSETTA=1 HARNESS_INSTALL_DIR="$dir" &&
  runs "harness 0.2.0 aarch64-apple-darwin" &&
  pass "x86_64 shell under Rosetta gets the arm64 build" ||
  fail "x86_64 shell under Rosetta gets the arm64 build"

# --- AC1: HARNESS_VERSION, HARNESS_INSTALL_DIR and the default dir ------------

for v in 0.1.0 v0.1.0; do
  fresh
  install HARNESS_VERSION=$v HARNESS_INSTALL_DIR="$dir" && runs "harness 0.1.0 x86_64-unknown-linux-musl" &&
    pass "HARNESS_VERSION=$v pins the version" || fail "HARNESS_VERSION=$v pins the version"
done

dir=$work/home/.local/bin
install && runs "harness 0.2.0 x86_64-unknown-linux-musl" &&
  pass "default install dir is ~/.local/bin" || fail "default install dir is ~/.local/bin"

# --- AC2: unsupported platforms ----------------------------------------------

for p in FreeBSD:amd64 Linux:i686 Linux:armv7l Darwin:ppc; do
  fresh
  if ! install FAKE_UNAME_S="${p%%:*}" FAKE_UNAME_M="${p#*:}" HARNESS_INSTALL_DIR="$dir" &&
    has "no prebuilt binary for ${p%%:*} ${p#*:}" &&
    has "cargo install --git https://github.com/thowd22/harness --locked harness" &&
    [ ! -e "$dir" ]; then
    pass "unsupported $p fails with the cargo install fallback"
  else
    fail "unsupported $p fails with the cargo install fallback"
  fi
done

# --- AC3: sha256 verification, curl/wget and sha256sum/shasum -----------------

# shellcheck disable=SC2016 # the literal $x is the point of the test
for v in "../0.1.0" "0.1.0/../x" "0.1 0" '0.1.0$x'; do
  fresh
  if ! install HARNESS_VERSION="$v" HARNESS_INSTALL_DIR="$dir" && has "invalid version" && [ ! -e "$dir" ]; then
    pass "HARNESS_VERSION=$v is rejected before downloading"
  else
    fail "HARNESS_VERSION=$v is rejected before downloading"
  fi
done

fresh
if ! install HARNESS_VERSION=0.0.9 HARNESS_INSTALL_DIR="$dir" && has "sha256 mismatch" && [ ! -e "$dir" ]; then
  pass "sha256 mismatch aborts before installing"
else
  fail "sha256 mismatch aborts before installing"
fi
fresh
install HARNESS_VERSION=0.1.0 HARNESS_INSTALL_DIR="$dir" || fail "setup: install 0.1.0"
if ! install HARNESS_VERSION=0.0.9 HARNESS_INSTALL_DIR="$dir" && has "sha256 mismatch" &&
  runs "harness 0.1.0 x86_64-unknown-linux-musl"; then
  pass "sha256 mismatch leaves the installed binary alone"
else
  fail "sha256 mismatch leaves the installed binary alone"
fi

fresh
if ! install HARNESS_VERSION=9.9.9 HARNESS_INSTALL_DIR="$dir" && has "download failed" && [ ! -e "$dir" ]; then
  pass "a missing release fails cleanly"
else
  fail "a missing release fails cleanly"
fi

# run_with TOOLDIR SCRIPT [VAR=VALUE...]: `bash < SCRIPT` (read and run as it
# streams in, like a pipe) with PATH limited to TOOLDIR.
run_with() {
  tdir=$1 script=$2
  shift 2
  env HOME="$work/home" HARNESS_INSTALL_TEST=1 HARNESS_RELEASES_URL="$base" HARNESS_API_URL="$base/api" \
    FAKE_UNAME_S=Linux FAKE_UNAME_M=aarch64 PATH="$fixtures/bin:$tdir" "$@" \
    bash < "$script" > "$work/out" 2>&1
}
fresh
run_with "$work/t-wget" "$root/install.sh" HARNESS_INSTALL_DIR="$dir" && runs "harness 0.2.0 aarch64-unknown-linux-musl" &&
  pass "works with wget and no curl" || fail "works with wget and no curl"
fresh
run_with "$work/t-shasum" "$root/install.sh" HARNESS_INSTALL_DIR="$dir" && runs "harness 0.2.0 aarch64-unknown-linux-musl" &&
  pass "works with shasum and no sha256sum" || fail "works with shasum and no sha256sum"
fresh
if ! run_with "$work/t-none" "$root/install.sh" HARNESS_INSTALL_DIR="$dir" && has "need curl or wget"; then
  pass "no curl or wget is a clear error"
else
  fail "no curl or wget is a clear error"
fi

# --- HTTPS only; test-only URL overrides are opt-in and loud -----------------

grep -qF "curl -fsSL --proto '=https' --tlsv1.2 " "$root/install.sh" &&
  pass "normal curl downloads are HTTPS-only (TLS 1.2+)" ||
  fail "normal curl downloads are HTTPS-only (TLS 1.2+)"

for v in HARNESS_RELEASES_URL HARNESS_API_URL; do
  fresh
  if ! install HARNESS_INSTALL_TEST= HARNESS_RELEASES_URL= HARNESS_API_URL= "$v=$base" \
    HARNESS_VERSION=0.2.0 HARNESS_INSTALL_DIR="$dir" &&
    has "$v is a test-only override and is set to" && ! has downloading && [ ! -e "$dir" ]; then
    pass "$v without HARNESS_INSTALL_TEST=1 is refused"
  else
    fail "$v without HARNESS_INSTALL_TEST=1 is refused"
  fi
done

fresh
# shellcheck disable=SC2016 # $1 expands in the inner sh
env HOME="$work/home" HARNESS_INSTALL_TEST=1 HARNESS_RELEASES_URL="$base" HARNESS_API_URL="$base/api" \
  FAKE_UNAME_S=Linux FAKE_UNAME_M=x86_64 PATH="$fixtures/bin:$PATH" HARNESS_INSTALL_DIR="$dir" \
  sh -c 'curl -fsSL "$1/install.sh" | bash' sh "$base" > "$work/stdout" 2> "$work/out" || true
if has "WARNING: HARNESS_RELEASES_URL is set; downloading from $base (test override, not GitHub)" &&
  has "WARNING: HARNESS_API_URL is set; downloading from $base/api (test override, not GitHub)" &&
  grep -qF "downloading $base/download/v0.2.0/harness-0.2.0-x86_64-unknown-linux-musl.tar.gz" "$work/stdout" &&
  runs "harness 0.2.0 x86_64-unknown-linux-musl"; then
  pass "honoured overrides warn on stderr naming the URL; the full download URL is printed"
else
  cat "$work/stdout" >> "$work/out"
  fail "honoured overrides warn on stderr naming the URL; the full download URL is printed"
fi

# python's http.server answers a directory path without a trailing slash
# with a 301 to plain http, so $base/download? makes every download redirect
# to http. Even in test mode a redirect must be HTTPS.
fresh
if ! install HARNESS_RELEASES_URL="$base/download?" HARNESS_VERSION=0.2.0 HARNESS_INSTALL_DIR="$dir" &&
  has "download failed" && [ ! -e "$dir" ]; then
  pass "curl refuses an https->http style redirect to plain http"
else
  fail "curl refuses an https->http style redirect to plain http"
fi
fresh
if ! run_with "$work/t-wget" "$root/install.sh" HARNESS_RELEASES_URL="$base/download?" HARNESS_VERSION=0.2.0 \
  HARNESS_INSTALL_DIR="$dir" && has "refusing non-HTTPS redirect: $base/download?" && [ ! -e "$dir" ]; then
  pass "wget refuses a redirect to plain http"
else
  fail "wget refuses a redirect to plain http"
fi

# --- AC4: no sudo, atomic re-runs, PATH hint ---------------------------------

if grep -v '^[[:space:]]*#' "$root/install.sh" | grep -qw sudo; then
  fail "install.sh never runs sudo"
else
  pass "install.sh never runs sudo"
fi

fresh
install HARNESS_VERSION=0.1.0 HARNESS_INSTALL_DIR="$dir" &&
  install HARNESS_INSTALL_DIR="$dir" && runs "harness 0.2.0 x86_64-unknown-linux-musl" &&
  install HARNESS_INSTALL_DIR="$dir" && [ "$(ls -A "$dir")" = harness ] &&
  pass "re-running upgrades in place and leaves no temp files" ||
  fail "re-running upgrades in place and leaves no temp files"

fresh
install HARNESS_INSTALL_DIR="$dir" && has "$dir is not on your PATH" &&
  pass "PATH hint when the dir is not on PATH" || fail "PATH hint when the dir is not on PATH"
fresh
install HARNESS_INSTALL_DIR="$dir" PATH="$fixtures/bin:$dir:$PATH" && ! has "not on your PATH" &&
  pass "no PATH hint when the dir is on PATH" || fail "no PATH hint when the dir is on PATH"

# --- AC5: a truncated script runs nothing ------------------------------------

[ "$(tail -n 1 "$root/install.sh")" = 'main "$@"' ] &&
  pass "the last line calls main" || fail "the last line calls main"
lines=$(wc -l < "$root/install.sh" | tr -d ' ')
bytes=$(wc -c < "$root/install.sh" | tr -d ' ')
bad=
fresh
i=1
while [ "$i" -lt "$lines" ]; do
  head -n "$i" "$root/install.sh" > "$work/cut.sh"
  run_with "$work/t-wget" "$work/cut.sh" HARNESS_INSTALL_DIR="$dir" || true
  { [ -e "$dir" ] || has downloading; } && bad="$bad line:$i"
  i=$((i + 1))
done
# Byte cuts around the final `main "$@"` line. Dropping 1, 5 or 6 bytes leaves
# `main "$@"` without its newline, `main ` or `main`: the whole function has
# arrived, so running it is fine. Every other cut must run nothing.
for i in 2 3 4 7 8 9 10 11 12; do
  head -c "$((bytes - i))" "$root/install.sh" > "$work/cut.sh"
  run_with "$work/t-wget" "$work/cut.sh" HARNESS_INSTALL_DIR="$dir" || true
  { [ -e "$dir" ] || has downloading; } && bad="$bad byte:-$i"
done
rm -f "$work/out"
[ -z "$bad" ] && pass "every truncated prefix of install.sh does nothing" ||
  fail "truncated prefixes ran:$bad"

echo
if [ "$fails" -eq 0 ]; then
  echo "install.sh: all tests passed"
else
  echo "install.sh: $fails test(s) failed" >&2
  exit 1
fi

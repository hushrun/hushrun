#!/bin/sh
# install-hushrun.sh: installs a hushrun release on a Linux or macOS CI runner, the newest by
# default, after checking the archive's SHA-256.
#
# Vendor it into your repository as ci/install-hushrun.sh: take the copy attached to a release,
# and read it before you commit it. It checks a release it lists below against the checksum it
# pins, and any other, such as a newer one, against the checksum published with the release.
#
#   HUSHRUN_VERSION       the release to install, such as 0.9.0, or latest (default: latest, the
#                         newest release)
#   HUSHRUN_SHA256        the archive's SHA-256, with HUSHRUN_VERSION, instead of the checksum
#                         this copy pins or the release publishes
#   HUSHRUN_DOWNLOAD_URL  where HUSHRUN_VERSION's archives and checksums are, such as a mirror
#                         your runners can reach (default: the GitHub release)
#   HUSHRUN_INSTALLER_GITHUB_BASE_URL
#                         GitHub's address, as hushrun's installers take it (default:
#                         https://github.com)
#   HUSHRUN_INSTALL_DIR   where hushrun goes (default: $HOME/.local/share/hushrun/<version>)
#
# It prints the binary's path on stdout, for `export PATH="$(dirname "$(sh ci/install-hushrun.sh)"):$PATH"`,
# and everything else on stderr. The verified archive stays in HUSHRUN_INSTALL_DIR: a second
# run checks it again and reinstalls from it, without downloading it.
set -eu

fail() {
  printf 'install-hushrun.sh: %s\n' "$*" >&2
  exit 1
}

# The SHA-256 of a release's archive, by "<version>/<target>".
pinned() {
  case "$1" in
    # hushrun-pins: begin (cargo xtask installer-pins writes these lines)
    # hushrun-pins: end
    *) return 1 ;;
  esac
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | sed 's/^.*= *//'
  else
    fail "no sha256sum, shasum or openssl to check the archive with"
  fi
}

# Only an explicitly http:// address is fetched without TLS: the checksum, not the transport,
# makes an archive trustworthy.
proto() {
  case "$1" in
    https://*) echo '=https' ;;
    *) echo '=http,https' ;;
  esac
}

# Whether what $1 answers can be trusted without a checksum: it comes over TLS, or from this
# machine.
trusted() {
  case "$1" in
    https://* | http://127.0.0.1[:/]* | http://localhost[:/]* | "http://[::1]"[:/]*) return 0 ;;
    *) return 1 ;;
  esac
}

download() {
  if command -v curl >/dev/null 2>&1; then
    curl --proto "$(proto "$1")" --tlsv1.2 -fsSL --retry 3 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1"
  else
    fail "no curl or wget to download $1 with"
  fi
}

# The newest release's version. GitHub redirects releases/latest to the newest release's tag,
# which is read rather than followed.
latest() {
  url=$github/hushrun/hushrun/releases/latest
  if command -v curl >/dev/null 2>&1; then
    location=$(curl --proto "$(proto "$url")" --tlsv1.2 -fsS --retry 3 -o /dev/null -w '%{redirect_url}' "$url") || return 1
  elif command -v wget >/dev/null 2>&1; then
    # GNU and BusyBox wget print each answer's headers with -S.
    location=$(wget -S --spider "$url" 2>&1 | sed -n 's/^ *[Ll]ocation: *\([^ ]*\).*/\1/p' | tr -d '\r' | head -n 1)
  else
    fail "no curl or wget to find the newest release with"
  fi
  case "$location" in
    */releases/tag/v*) printf '%s\n' "${location##*/releases/tag/v}" ;;
    *) return 1 ;;
  esac
}

github=${HUSHRUN_INSTALLER_GITHUB_BASE_URL:-https://github.com}
github=${github%/}
trusted "$github/" ||
  fail "HUSHRUN_INSTALLER_GITHUB_BASE_URL must be an https:// URL, not '$github'"

version=${HUSHRUN_VERSION:-latest}
if [ "$version" = latest ]; then
  [ -z "${HUSHRUN_SHA256:-}" ] ||
    fail "HUSHRUN_SHA256 needs HUSHRUN_VERSION: the newest release's archive changes with every release"
  [ -z "${HUSHRUN_DOWNLOAD_URL:-}" ] ||
    fail "HUSHRUN_DOWNLOAD_URL needs HUSHRUN_VERSION: a mirror holds the releases it copied"
  version=$(latest) ||
    fail "can't tell the newest hushrun release from $github: set HUSHRUN_VERSION to the release to install"
fi
printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$' ||
  fail "HUSHRUN_VERSION must be a release such as 0.9.0, or latest, not '$version'"

case "$(uname -s)" in
  Linux) os=unknown-linux-musl ;;
  Darwin) os=apple-darwin ;;
  *) fail "no hushrun build for $(uname -s): use the release's PowerShell installer on Windows" ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) fail "no hushrun build for $(uname -m)" ;;
esac
target=$arch-$os
archive=hushrun-$target.tar.xz

base=${HUSHRUN_DOWNLOAD_URL:-$github/hushrun/hushrun/releases/download/v$version}
base=${base%/}
case "$base" in
  https://* | http://*) ;;
  *) fail "HUSHRUN_DOWNLOAD_URL must be an https:// URL, not '$base'" ;;
esac

if [ -n "${HUSHRUN_INSTALL_DIR:-}" ]; then
  dir=$HUSHRUN_INSTALL_DIR
else
  [ -n "${HOME:-}" ] || fail "set HUSHRUN_INSTALL_DIR: HOME is not set"
  dir=$HOME/.local/share/hushrun/$version
fi
mkdir -p "$dir"

work=$dir/extract.$$
trap 'rm -rf "$work"' EXIT
rm -rf "$work"
mkdir -p "$work"

if [ -n "${HUSHRUN_SHA256:-}" ]; then
  expected=$(printf '%s' "$HUSHRUN_SHA256" | tr 'A-F' 'a-f')
  printf '%s\n' "$expected" | grep -Eq '^[0-9a-f]{64}$' ||
    fail "HUSHRUN_SHA256 must be 64 hex digits"
elif ! expected=$(pinned "$version/$target"); then
  # A release this copy doesn't list: the checksum published with it, over TLS, fetched again
  # each time, so that nothing kept from an earlier run decides what is trusted.
  trusted "$base/" ||
    fail "this copy pins no checksum for hushrun $version on $target, and one read from $base over plain HTTP proves nothing: set HUSHRUN_SHA256, or vendor the copy attached to that release"
  download "$base/$archive.sha256" "$work/$archive.sha256" ||
    fail "downloading $base/$archive.sha256 failed"
  read -r expected name <"$work/$archive.sha256" || true
  expected=$(printf '%s' "$expected" | tr 'A-F' 'a-f')
  printf '%s\n' "$expected" | grep -Eq '^[0-9a-f]{64}$' ||
    fail "$archive.sha256 doesn't start with a SHA-256"
  [ -z "${name:-}" ] || [ "${name#\*}" = "$archive" ] ||
    fail "$archive.sha256 is the checksum of ${name#\*}, not of $archive"
fi

cached=$dir/$archive
if [ ! -f "$cached" ] || [ "$(sha256 "$cached")" != "$expected" ]; then
  partial=$cached.partial
  rm -f "$partial"
  download "$base/$archive" "$partial" || fail "downloading $base/$archive failed"
  actual=$(sha256 "$partial")
  if [ "$actual" != "$expected" ]; then
    rm -f "$partial"
    fail "$archive has SHA-256 $actual, expected $expected"
  fi
  mv -f "$partial" "$cached"
fi

tar -xJf "$cached" -C "$work" 2>/dev/null ||
  fail "can't extract $archive: this tar needs xz (apk add xz, or apt-get install xz-utils)"
binary=$(find "$work" -type f -name hushrun | head -n 1)
[ -n "$binary" ] || fail "no hushrun binary in $archive"
chmod 0755 "$binary"
mv -f "$binary" "$dir/hushrun"
printf 'hushrun %s for %s in %s (sha256 %s)\n' "$version" "$target" "$dir" "$expected" >&2
printf '%s\n' "$dir/hushrun"

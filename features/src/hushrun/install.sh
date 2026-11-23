#!/bin/sh
# The hushrun Dev Container Feature: installs hushrun into /usr/local/hushrun/bin with the release's
# own installer (which checks what it downloads), and writes the script `onCreateCommand` runs.
# It holds no secret: a credential, such as HUSHRUN_AGENT or a bearer token for HUSHRUN_TOKEN, is the
# container's own, from its secrets, never this feature's.
set -eu

VERSION="${VERSION:-latest}"
FETCH="${FETCH:-true}"
PULL="${PULL:-false}"
DIR=/usr/local/hushrun

need() {
  command -v "$1" >/dev/null 2>&1 && return 0
  if command -v apt-get >/dev/null 2>&1; then
    apt-get update -y >/dev/null && apt-get install -y --no-install-recommends "$2" >/dev/null
  elif command -v apk >/dev/null 2>&1; then
    apk add --no-cache "$2" >/dev/null
  else
    echo "hushrun feature: $1 is needed; install it first" >&2
    exit 1
  fi
}
need curl curl
need git git
command -v update-ca-certificates >/dev/null 2>&1 || need update-ca-certificates ca-certificates

case "$VERSION" in
  latest) URL="https://github.com/hushrun/hushrun/releases/latest/download/hushrun-installer.sh" ;;
  *) URL="https://github.com/hushrun/hushrun/releases/download/v${VERSION#v}/hushrun-installer.sh" ;;
esac

mkdir -p "$DIR/bin"
# The installer's flat layout: the binary straight into the folder, no receipt, no PATH edits.
curl --proto '=https' --tlsv1.2 -LsSf "$URL" \
  | env HUSHRUN_UNMANAGED_INSTALL="$DIR/bin" INSTALLER_NO_MODIFY_PATH=1 sh
test -x "$DIR/bin/hushrun" || { echo "hushrun feature: the installer didn't install hushrun" >&2; exit 1; }

cat > "$DIR/on-create.sh" <<SCRIPT
#!/bin/sh
# Written by the hushrun Dev Container Feature. What fails here never stops the container.
cd "\${containerWorkspaceFolder:-\$PWD}" 2>/dev/null || true
if [ "$FETCH" = true ]; then
  $DIR/bin/hushrun fetch || true
fi
if [ "$PULL" = true ]; then
  $DIR/bin/hushrun build pull || true
fi
exit 0
SCRIPT
chmod 0755 "$DIR/on-create.sh"
echo "hushrun feature: installed $("$DIR/bin/hushrun" --version 2>/dev/null || echo hushrun)"

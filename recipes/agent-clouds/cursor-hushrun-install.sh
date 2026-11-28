#!/bin/sh
# hushrun init writes this for Cursor's cloud agents. Cursor runs it while it builds the
# environment's snapshot: it installs hushrun, pulls the repository's newest baked store into
# /hushrun, and fetches the newest results. It always exits 0.
set +e
# Root, directly or through sudo when sudo needs no password; else this user, and a step that
# needs root fails without stopping the rest.
root=
if [ "$(id -u)" != 0 ] && command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
  root='sudo -n'
fi
if ! command -v hushrun >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -LsSf \
    https://github.com/hushrun/hushrun/releases/latest/download/hushrun-installer.sh \
    | $root env HUSHRUN_UNMANAGED_INSTALL=/usr/local/bin INSTALLER_NO_MODIFY_PATH=1 sh
fi
if command -v hushrun >/dev/null 2>&1; then
  if [ -n "$HUSHRUN_BUILD_LOWER" ]; then
    hushrun build pull
  else
    $root ${root:+--preserve-env=HUSHRUN_REGISTRY_TOKEN,HUSHRUN_BACKEND,HUSHRUN_HOST} \
      "$(command -v hushrun)" build pull
  fi
  hushrun fetch
fi
exit 0

#!/bin/sh
# hushrun for Google Antigravity Remote Agents. Paste this whole as the remote project's
# Environment setup script (its saved setup routine): Antigravity runs it after it clones the
# repository. It installs hushrun if it is missing, pulls the repository's baked store (compiled
# code) and package store, installs the dependencies through the package store, and fetches the
# newest results, so the agent skips what this exact code already passed. It always exits 0.
#
# Spots marked [probe] are what docs/validation/antigravity-remote.md hasn't answered yet: whether
# setup runs once (and is snapshotted) or once per conversation, whether it runs as root or with
# sudo, whether what it exports reaches the agent's shell, and where the workspace is. The script
# works whichever way each turns out; once the probe has run, those spots can be simplified.
set +e

# [probe] The workspace: Antigravity names no variable for it that we know of. Setup is expected
# to start in the clone; else the first of these folders that is a git checkout.
if ! git rev-parse --show-toplevel >/dev/null 2>&1; then
  for dir in "${ANTIGRAVITY_WORKSPACE:-}" /workspace "$HOME/workspace" /home/antigravity "$HOME"/*/; do
    if [ -n "$dir" ] && [ -d "$dir" ] && git -C "$dir" rev-parse --show-toplevel >/dev/null 2>&1; then
      cd "$dir" || continue
      break
    fi
  done
fi
cd "$(git rev-parse --show-toplevel 2>/dev/null || pwd)" || true

# hushrun's platform id for this sandbox (Antigravity sets no marker hushrun knows yet).
export HUSHRUN_SANDBOX=antigravity-remote
# The package store's image, while hushrun's own registry doesn't take package stores: the
# repository your CI's `hushrun packages bake --oci` publishes to (recipes/github/packages-bake.yml).
# Edit it, or set it in the project's settings. Once `--oci hushrun` works, delete this line: the
# pull then finds the store on hushrun's registry by itself.
export HUSHRUN_PACKAGES_IMAGE="${HUSHRUN_PACKAGES_IMAGE:-ghcr.io/acme/shop}"
# [probe] The user who runs the agent's commands, when setup runs as root and the agent doesn't:
# the stores are given to them. Empty: setup's own user.
AGENT_USER="${AGENT_USER:-}"
agent_home=$HOME
if [ -n "$AGENT_USER" ]; then
  agent_home=$(getent passwd "$AGENT_USER" | cut -d: -f6)
  agent_home=${agent_home:-$HOME}
fi

# What the agent's shells should see: written to ~/.profile (login shells) and ~/.bashrc
# (interactive ones) in one block this script replaces on every run. [probe] Which of the two, if
# either, the agent's commands read.
profile_lines="export HUSHRUN_SANDBOX=antigravity-remote
export HUSHRUN_PACKAGES_IMAGE='$HUSHRUN_PACKAGES_IMAGE'"

# hushrun, if it is missing: the release installer, into ~/.local/bin, which needs no root; as
# root, into /usr/local/bin, where every user's PATH finds it.
bin="$HOME/.local/bin"
if [ "$(id -u)" = 0 ]; then
  bin=/usr/local/bin
fi
PATH="$bin:$PATH"
export PATH
if ! command -v hushrun >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -LsSf \
    https://github.com/hushrun/hushrun/releases/latest/download/hushrun-installer.sh \
    | env HUSHRUN_UNMANAGED_INSTALL="$bin" INSTALLER_NO_MODIFY_PATH=1 sh
fi
hushrun=$(command -v hushrun)
if [ "$hushrun" = "$HOME/.local/bin/hushrun" ]; then
  profile_lines="$profile_lines
export PATH=\"$HOME/.local/bin:\$PATH\""
fi

# A pull that exits 12 found nothing published yet: not a failure.
said() {
  case $1 in
    0) ;;
    12) echo "hushrun: $2: none published yet" ;;
    *) echo "hushrun: $2 failed (exit $1); the agent's checks run as they would without it" ;;
  esac
}

if [ -n "$hushrun" ]; then
  # Both pulls swap a folder into place by renames in /, which takes root: directly, through
  # sudo when it needs no password, or else into folders under $HOME that the profile names.
  if [ -w / ]; then
    "$hushrun" build pull; said $? "build pull"
    "$hushrun" packages pull; said $? "packages pull"
    if [ -n "$AGENT_USER" ]; then
      chown -R "$AGENT_USER" /hushrun /hushrun-packages 2>/dev/null
      chmod 755 /hushrun /hushrun-packages 2>/dev/null
    fi
  elif command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
    keep=HUSHRUN_REGISTRY_TOKEN,HUSHRUN_PACKAGES_REGISTRY_TOKEN,HUSHRUN_PACKAGES_IMAGE,HUSHRUN_SANDBOX,HUSHRUN_BACKEND,HUSHRUN_HOST
    sudo -n --preserve-env="$keep" "$hushrun" build pull; said $? "build pull"
    sudo -n --preserve-env="$keep" "$hushrun" packages pull; said $? "packages pull"
    # B-65: the agent owns the stores, mode 755, so its runs read them and nothing needs sudo.
    for store in /hushrun /hushrun-packages; do
      if [ -d "$store" ]; then
        sudo -n chown -R "${AGENT_USER:-$(id -u):$(id -g)}" "$store"
        sudo -n chmod 755 "$store"
      fi
    done
  else
    HUSHRUN_BUILD_LOWER="$HOME/.hushrun-store"
    HUSHRUN_PACKAGES_LOWER="$HOME/.hushrun-packages"
    export HUSHRUN_BUILD_LOWER HUSHRUN_PACKAGES_LOWER
    "$hushrun" build pull; said $? "build pull"
    "$hushrun" packages pull; said $? "packages pull"
    profile_lines="$profile_lines
export HUSHRUN_BUILD_LOWER=\"\$HOME/.hushrun-store\"
export HUSHRUN_PACKAGES_LOWER=\"\$HOME/.hushrun-packages\""
  fi

  # Your repository's install command: edit it. Through `hushrun packages exec` it reads the
  # package store (K-10); a plain `npm ci` downloads everything again.
  if [ -f package-lock.json ]; then
    "$hushrun" packages exec -- npm ci
  fi

  # Tools that read their part of the store in place (Gradle, Maven, NuGet) see it in the agent's
  # plain commands too, through the variables `hushrun packages env` prints. Set
  # HUSHRUN_PACKAGES_ENV_IN_PROFILE=0 to leave them out.
  if [ "${HUSHRUN_PACKAGES_ENV_IN_PROFILE:-1}" = 1 ]; then
    env_lines=$("$hushrun" packages env 2>/dev/null)
    if [ -n "$env_lines" ]; then
      profile_lines="$profile_lines
$env_lines"
    fi
  fi

  # The newest results for this commit. It needs a credential (a hushrun token in the project's
  # settings); without one it says so and changes nothing.
  "$hushrun" fetch
fi

# [probe] Setup may run once and be snapshotted, or once per conversation: the block is replaced,
# never appended twice.
for rc in "$agent_home/.profile" "$agent_home/.bashrc"; do
  if [ -f "$rc" ]; then
    sed '/^# >>> hushrun >>>$/,/^# <<< hushrun <<<$/d' "$rc" >"$rc.hushrun" && cat "$rc.hushrun" >"$rc"
    rm -f "$rc.hushrun"
  fi
  printf '# >>> hushrun >>>\n%s\n# <<< hushrun <<<\n' "$profile_lines" >>"$rc"
  if [ -n "$AGENT_USER" ]; then
    chown "$AGENT_USER" "$rc" 2>/dev/null
  fi
done
exit 0

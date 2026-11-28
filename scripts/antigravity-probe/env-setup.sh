#!/bin/sh
# env-setup.sh: the environment setup script of hushrun's Antigravity probe
# (docs/validation/antigravity-remote.md). Paste it whole into the remote project's settings, as
# its environment setup script. It lives there, not in the repository, so it finds the repository's
# copy of agent-cloud-probe.sh itself.
#
# It records when and as whom it ran (~/.probe-setup and /tmp/probe-setup.log), writes
# PROBE_FROM_SETUP=1 where a login or interactive shell would read it, and runs the probe, whose
# report goes to /tmp/probe-setup.txt. It prints variable names only, never a value, and always
# exits 0, so the probe can't stop a conversation from starting.
set -u

# Seconds to keep running after the probe, in steps of 30, each written down: the last step that
# lands is how long the platform lets a setup script run. 0 for the first runs; 900 for the
# time-limit run.
PROBE_SETUP_SLEEP=0

now() { date -u +%Y-%m-%dT%H:%M:%SZ; }
boot=$(cut -c1-8 /proc/sys/kernel/random/boot_id 2>/dev/null || echo unknown)
if [ "$(id -u)" = 0 ]; then
  root=yes
elif command -v sudo >/dev/null 2>&1 && sudo -n true >/dev/null 2>&1; then
  root=passwordless-sudo
else
  root=no
fi
note() { # what: to both logs, since the agent's $HOME may not be this script's
  echo "$(now) $1" >> "${HOME:-/tmp}/.probe-setup" 2>/dev/null
  echo "$(now) $1" >> /tmp/probe-setup.log 2>/dev/null
}
note "setup-start pid=$$ ppid=$PPID uid=$(id -u) user=$(id -un 2>/dev/null) root=$root boot=$boot cwd=$(pwd) home=${HOME:-unset}"

# Q5: does what a setup script writes reach the agent's shell?
export PROBE_FROM_SETUP_ENV=1
for home in "${HOME:-}" /home/antigravity; do
  [ -n "$home" ] && [ -d "$home" ] || continue
  for rc in "$home/.profile" "$home/.bashrc"; do
    if ! grep -q 'PROBE_FROM_SETUP=' "$rc" 2>/dev/null; then
      echo 'export PROBE_FROM_SETUP=1  # hushrun Antigravity probe; delete this line after' >> "$rc" 2>/dev/null \
        && note "wrote PROBE_FROM_SETUP to $rc"
    fi
  done
done
if [ -w /etc/environment ] && ! grep -q 'PROBE_FROM_SETUP_ETC=' /etc/environment; then
  echo 'PROBE_FROM_SETUP_ETC=1' >> /etc/environment && note "wrote PROBE_FROM_SETUP_ETC to /etc/environment"
fi

# The repository's probe, wherever the platform cloned it.
probe=""
for dir in "$(pwd)" "$(pwd)"/* "${HOME:-/nonexistent}"/* /home/antigravity/* /workspace /workspace/*; do
  if [ -f "$dir/scripts/agent-cloud-probe.sh" ]; then probe="$dir/scripts/agent-cloud-probe.sh"; break; fi
done
if [ -n "$probe" ]; then
  note "probe at $probe"
  sh "$probe" --mode setup >/dev/null 2>&1
  note "probe done: /tmp/probe-setup.txt"
else
  note "probe not found: no scripts/agent-cloud-probe.sh under the cwd, \$HOME or /home/antigravity"
fi

slept=0
while [ "$slept" -lt "$PROBE_SETUP_SLEEP" ]; do
  sleep 30
  slept=$((slept + 30))
  note "setup-still-running ${slept}s"
done
note "setup-end"
exit 0

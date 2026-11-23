#!/bin/sh
# agent-cloud-probe.sh: what a hosted coding agent's sandbox looks like to hushrun, in one report
# to paste into an issue or a platform's page. Run it inside the sandbox, from anywhere:
#
#   sh scripts/agent-cloud-probe.sh            the report, on stdout
#   sh scripts/agent-cloud-probe.sh --no-net   the same, without the three reachability checks
#
# It prints the NAMES of the environment's variables, never a value: a sandbox's variables hold
# tokens. The only values it shows are whether a few CI and platform markers read as true, and the
# versions of the tools it finds. It writes only under a temporary directory it removes, and sends
# nothing but three unauthenticated HTTPS requests that carry no credential: the server's health
# check, the registry's API root and the shared store's host.
set -u

net=1
case "${1:-}" in
  --no-net) net=0 ;;
  "") ;;
  *) echo "usage: sh scripts/agent-cloud-probe.sh [--no-net]" >&2; exit 2 ;;
esac

section() { printf '\n## %s\n' "$1"; }
# Whether a variable is set, and whether it reads as true, without its value.
marker() {
  name=$1
  if env | grep -q "^$name="; then
    value=$(printenv "$name")
    case $(printf '%s' "$value" | tr '[:upper:]' '[:lower:]') in
      1|true|yes) state="set, reads true" ;;
      "") state="set, empty" ;;
      0|false|no) state="set, reads false" ;;
      *) state="set" ;;
    esac
  else
    state="unset"
  fi
  printf '%-28s %s\n' "$name" "$state"
}
version() { # name command...
  name=$1; shift
  if command -v "$1" >/dev/null 2>&1; then
    printf '%-10s %s\n' "$name" "$("$@" 2>&1 | head -n 1)"
  else
    printf '%-10s %s\n' "$name" "not installed"
  fi
}

echo "# hushrun agent cloud probe"
echo "date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"

section "Platform markers"
for name in CI GITHUB_ACTIONS GITLAB_CI BUILDKITE CIRCLECI CODESPACES REMOTE_CONTAINERS \
  DEVCONTAINER IS_SANDBOX AI_AGENT CLAUDECODE CLAUDE_CODE_REMOTE CLAUDE_CODE_REMOTE_ENVIRONMENT_TYPE \
  CODEX_SANDBOX CODEX_CI CODEX_THREAD_ID CODEX_PROXY_CERT CURSOR_AGENT CURSOR_AGENT_SOCKET \
  COPILOT_AGENT_SESSION_ID COPILOT_AGENT_ACTION GITHUB_COPILOT_API_TOKEN JULES_SESSION_ID \
  DEVIN_ID HUSHRUN_AGENT HUSHRUN_TOKEN HUSHRUN_AUTH HUSHRUN_BACKEND HUSHRUN_PROVIDER; do
  marker "$name"
done

section "Variable names (no values)"
env | sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p' | sort -u | tr '\n' ' '
echo

section "System"
printf '%-10s %s\n' os "$(uname -s)"
printf '%-10s %s\n' arch "$(uname -m)"
if command -v ldd >/dev/null 2>&1; then
  printf '%-10s %s\n' libc "$(ldd --version 2>&1 | head -n 1)"
else
  printf '%-10s %s\n' libc "unknown (no ldd)"
fi
printf '%-10s %s\n' user "$(id -un 2>/dev/null || echo unknown) (uid $(id -u))"
printf '%-10s %s\n' cpus "$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo unknown)"
printf '%-10s %s\n' disk "$(df -h . 2>/dev/null | awk 'NR==2 {print $4 " free of " $2}')"
for dir in /hushrun "${HUSHRUN_BUILD_LOWER:-}"; do
  [ -n "$dir" ] && [ -d "$dir" ] && printf '%-10s %s\n' store "$dir exists"
done

section "Toolchains"
version rustc rustc --version
version cargo cargo --version
version go go version
version node node --version
version npm npm --version
version python3 python3 --version
version docker docker --version
version git git --version
version hushrun hushrun --version

section "Git"
git_conf() {
  if git config --global --get "$1" >/dev/null 2>&1; then
    printf '%-24s %s\n' "$1" "set globally"
  else
    printf '%-24s %s\n' "$1" "unset"
  fi
}
git_conf core.hooksPath
git_conf http.proxy
git_conf url.insteadOf
if git config --global --get-regexp '^url\..*\.insteadof$' >/dev/null 2>&1; then
  echo "url rewrites:            $(git config --global --get-regexp '^url\..*\.insteadof$' | wc -l | tr -d ' ') (a git proxy or credential helper rewrites remotes)"
fi
if git config --global --get credential.helper >/dev/null 2>&1; then
  echo "credential.helper:       set globally"
fi
[ -n "${GIT_CONFIG_COUNT:-}" ] && echo "GIT_CONFIG_COUNT:        $GIT_CONFIG_COUNT entries from the environment"

# Does a pre-push hook fire on push? To a bare repository on this disk, so nothing leaves.
tmp=$(mktemp -d 2>/dev/null || mktemp -d -t probe)
trap 'rm -rf "$tmp"' EXIT INT TERM
(
  cd "$tmp" || exit 1
  git init -q --bare remote.git
  git init -q work
  cd work || exit 1
  git config user.name probe
  git config user.email probe@example.invalid
  git config commit.gpgsign false
  git commit -q --allow-empty -m probe
  git remote add origin ../remote.git
  mkdir -p .git/hooks
  printf '#!/bin/sh\ntouch "%s/fired"\n' "$tmp" > .git/hooks/pre-push
  chmod +x .git/hooks/pre-push
  git push -q origin HEAD:refs/heads/probe >/dev/null 2>&1
)
if [ -f "$tmp/fired" ]; then
  echo "pre-push hook:           fires on git push (local bare remote)"
else
  echo "pre-push hook:           did NOT fire on git push (hooks off, or core.hooksPath elsewhere)"
fi
echo "note: whether the platform's own push (a pull request it opens for the agent) runs hooks"
echo "      can't be seen from here: push from the agent and look for hushrun's hook output."

section "Reachability (HTTPS, no credentials)"
if [ "$net" = 0 ]; then
  echo "skipped (--no-net)"
elif ! command -v curl >/dev/null 2>&1; then
  echo "skipped: no curl"
else
  if [ -n "${HTTPS_PROXY:-${https_proxy:-}}" ]; then
    echo "proxy: HTTPS_PROXY is set"
  else
    echo "proxy: none set"
  fi
  # The system trust store, as a statically linked hushrun reads it, then curl's default. A
  # proxy that re-terminates TLS answers the second but not the first unless its CA is in the
  # system store.
  system_ca=""
  for file in /etc/ssl/certs/ca-certificates.crt /etc/pki/tls/certs/ca-bundle.crt /etc/ssl/cert.pem; do
    [ -f "$file" ] && { system_ca=$file; break; }
  done
  probe() { # label url
    if [ -n "$system_ca" ]; then
      code=$(curl -sS -o /dev/null -m 15 --cacert "$system_ca" -w '%{http_code}' "$2" 2>"$tmp/err")
      status=$?
      [ "$status" = 0 ] && sys="HTTP $code" || sys="failed: $(head -c 120 "$tmp/err" | tr '\n' ' ')"
    else
      sys="no system CA file found"
    fi
    code=$(curl -sS -o /dev/null -m 15 -w '%{http_code}' "$2" 2>"$tmp/err")
    status=$?
    [ "$status" = 0 ] && dflt="HTTP $code" || dflt="failed: $(head -c 120 "$tmp/err" | tr '\n' ' ')"
    printf '%-22s system store: %s | curl default: %s\n' "$1" "$sys" "$dflt"
  }
  echo "system CA file: ${system_ca:-none}"
  probe api.hushrun.com https://api.hushrun.com/healthz
  probe registry.hushrun.com https://registry.hushrun.com/v2/
  probe r2.cloudflarestorage.com https://r2.cloudflarestorage.com/
  echo "any HTTP status means the host answered; a refusal from the proxy shows as 403 or a failure."
fi

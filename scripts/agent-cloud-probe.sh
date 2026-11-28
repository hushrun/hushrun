#!/bin/sh
# agent-cloud-probe.sh: what a hosted coding agent's sandbox looks like to hushrun, in one report
# to paste into an issue or a platform's page. Run it inside the sandbox, from anywhere:
#
#   sh scripts/agent-cloud-probe.sh                 the report, on stdout
#   sh scripts/agent-cloud-probe.sh --no-net        the same, without the reachability checks
#   sh scripts/agent-cloud-probe.sh --mode MODE     the same, also written to /tmp/probe-MODE.txt
#
# MODE says where it runs: `agent` (the agent's own shell), `setup` (a platform's setup script) or
# `hook` (a session hook, whose stdout belongs to the platform: the report goes to the file only).
# scripts/antigravity-probe/ calls it in all three places.
#
# It prints the NAMES of the environment's variables, never a value: a sandbox's variables hold
# tokens. The only values it shows are whether a few CI and platform markers read as true, the
# versions of the tools it finds, paths, and the host (never the user or path) of the repository's
# remote. It writes only under a temporary directory it removes, and /tmp/probe-MODE.txt with
# --mode. It sends nothing but unauthenticated HTTPS requests that carry no credential, to the
# hosts in its reachability section.
set -u

net=1
mode=""
usage() { echo "usage: sh scripts/agent-cloud-probe.sh [--no-net] [--mode agent|setup|hook]" >&2; exit 2; }
while [ $# -gt 0 ]; do
  case $1 in
    --no-net) net=0 ;;
    --mode)
      [ $# -ge 2 ] || usage
      case $2 in agent|setup|hook) mode=$2 ;; *) usage ;; esac
      shift ;;
    *) usage ;;
  esac
  shift
done

section() { printf '\n## %s\n' "$1"; }
# Whether a variable is set, and whether it reads as true, without its value.
marker() {
  name=$1
  if value=$(printenv "$name"); then
    case $(printf '%s' "$value" | tr '[:upper:]' '[:lower:]') in
      1|true|yes) state="set, reads true" ;;
      "") state="set, empty" ;;
      0|false|no) state="set, reads false" ;;
      *) state="set" ;;
    esac
  else
    state="unset"
  fi
  printf '%-34s %s\n' "$name" "$state"
}
version() { # name command...
  name=$1; shift
  if command -v "$1" >/dev/null 2>&1; then
    printf '%-10s %s\n' "$name" "$("$@" 2>&1 | head -n 1)"
  else
    printf '%-10s %s\n' "$name" "not installed"
  fi
}
# The names of the variables that start with one of the prefixes given, from awk's ENVIRON.
names_with_prefix() {
  awk -v prefixes="$*" 'BEGIN {
    n = split(prefixes, p, " ")
    for (name in ENVIRON) for (i = 1; i <= n; i++) if (index(name, p[i]) == 1) { print name; break }
  }' | sort -u
}
exists() { # label path
  if [ -e "$2" ]; then printf '%-28s %s\n' "$1" "exists ($2)"; else printf '%-28s %s\n' "$1" "absent ($2)"; fi
}

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t probe)
trap 'rm -rf "$tmp"' EXIT INT TERM

report() {
echo "# hushrun agent cloud probe"
echo "date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "mode: ${mode:-none}"
echo "cwd:  $(pwd)"
echo "home: ${HOME:-unset}"
if command -v ps >/dev/null 2>&1; then
  # The process names only: a command line can hold a token.
  echo "parent process: $(ps -o comm= -p "$PPID" 2>/dev/null || echo unknown)"
fi
if [ -r /proc/sys/kernel/random/boot_id ]; then
  # Two reports with the same boot id ran on the same machine without a restart.
  echo "boot id: $(cut -c1-8 /proc/sys/kernel/random/boot_id)"
fi

section "Platform markers"
for name in CI GITHUB_ACTIONS GITLAB_CI BUILDKITE CIRCLECI CODESPACES REMOTE_CONTAINERS \
  DEVCONTAINER IS_SANDBOX AI_AGENT CLAUDECODE CLAUDE_CODE_REMOTE CLAUDE_CODE_REMOTE_ENVIRONMENT_TYPE \
  CODEX_SANDBOX CODEX_CI CODEX_THREAD_ID CODEX_PROXY_CERT CURSOR_AGENT CURSOR_AGENT_SOCKET \
  COPILOT_AGENT_SESSION_ID COPILOT_AGENT_ACTION GITHUB_COPILOT_API_TOKEN JULES_SESSION_ID \
  DEVIN_ID ANTIGRAVITY_AGENT ANTIGRAVITY_CONVERSATION_ID ANTIGRAVITY_PROJECT_ID \
  ANTIGRAVITY_LS_ADDRESS CLOUD_SHELL K_SERVICE KUBERNETES_SERVICE_HOST \
  HUSHRUN_AGENT HUSHRUN_TOKEN HUSHRUN_AUTH HUSHRUN_BACKEND HUSHRUN_PROVIDER HUSHRUN_SANDBOX \
  PROBE_FROM_SETUP PROBE_FROM_SETUP_ENV PROBE_FROM_SETUP_ETC; do
  marker "$name"
done
# Every variable of Google's and Antigravity's prefixes, whatever its name, since which one marks a
# remote environment is what the probe asks.
listed=" ANTIGRAVITY_AGENT ANTIGRAVITY_CONVERSATION_ID ANTIGRAVITY_PROJECT_ID ANTIGRAVITY_LS_ADDRESS "
for name in $(names_with_prefix ANTIGRAVITY_ AGY_ GOOGLE_ GEMINI_ GCE_ CLOUDSDK_); do
  case $listed in
    *" $name "*) ;;
    *) marker "$name" ;;
  esac
done

section "Variable names (no values)"
# From awk's ENVIRON, never from `env`, whose lines of a value that spans several could pass for
# names.
awk 'BEGIN { for (name in ENVIRON) print name }' | sort -u | tr '\n' ' '
echo

section "System"
printf '%-10s %s\n' os "$(uname -s)"
printf '%-10s %s\n' kernel "$(uname -r)"
printf '%-10s %s\n' arch "$(uname -m)"
if [ -r /etc/os-release ]; then
  printf '%-10s %s\n' distro "$(sed -n 's/^PRETTY_NAME=//p' /etc/os-release | tr -d '"')"
fi
if command -v ldd >/dev/null 2>&1; then
  printf '%-10s %s\n' libc "$(ldd --version 2>&1 | head -n 1)"
else
  printf '%-10s %s\n' libc "unknown (no ldd)"
fi
printf '%-10s %s\n' user "$(id -un 2>/dev/null || echo unknown) (uid $(id -u))"
if [ "$(id -u)" = 0 ]; then
  printf '%-10s %s\n' root "yes"
elif command -v sudo >/dev/null 2>&1 && sudo -n true >/dev/null 2>&1; then
  printf '%-10s %s\n' root "no, but sudo works without a password"
else
  printf '%-10s %s\n' root "no, and no passwordless sudo"
fi
printf '%-10s %s\n' cpus "$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo unknown)"
if [ -r /proc/meminfo ]; then
  printf '%-10s %s\n' memory "$(awk '/^MemTotal:/ {printf "%.1f GiB", $2 / 1048576}' /proc/meminfo)"
fi
printf '%-10s %s\n' disk "$(df -h . 2>/dev/null | awk 'NR==2 {print $4 " free of " $2 " (here)"}')"
for dir in / "${HOME:-/}" /tmp; do
  printf '%-10s %s\n' df "$dir: $(df -h "$dir" 2>/dev/null | awk 'NR==2 {print $4 " free of " $2 ", " $1}')"
done
# Whether / and $HOME are one file system: a store under $HOME can be hard-linked or renamed from
# a checkout on / only if they are.
if dev_root=$(stat -c %d / 2>/dev/null) && dev_home=$(stat -c %d "${HOME:-/}" 2>/dev/null); then
  fs_root=$(stat -f -c %T / 2>/dev/null || echo unknown)
  fs_home=$(stat -f -c %T "${HOME:-/}" 2>/dev/null || echo unknown)
  if [ "$dev_root" = "$dev_home" ]; then
    printf '%-10s %s\n' same-fs "/ and \$HOME are one file system ($fs_root)"
  else
    printf '%-10s %s\n' same-fs "/ ($fs_root, device $dev_root) and \$HOME ($fs_home, device $dev_home) differ"
  fi
fi
for file in /.dockerenv /run/.containerenv; do
  [ -e "$file" ] && printf '%-10s %s\n' container "$file exists"
done
for dir in /hushrun "${HUSHRUN_BUILD_LOWER:-}"; do
  [ -n "$dir" ] && [ -d "$dir" ] && printf '%-10s %s\n' store "$dir exists"
done

section "Toolchains"
version rustc rustc --version
version cargo cargo --version
version go go version
version node node --version
version npm npm --version
version npx npx --version
version java java -version
version python3 python3 --version
version docker docker --version
if command -v docker >/dev/null 2>&1; then
  if docker info >/dev/null 2>&1; then echo "docker     daemon answers"; else echo "docker     daemon does not answer"; fi
fi
version git git --version
version gh gh --version
version gcloud gcloud --version
version hushrun hushrun --version

section "Antigravity"
exists "upload folder" /home/antigravity/upload
exists "/home/antigravity" /home/antigravity
exists "home's .gemini" "${HOME:-/nonexistent}/.gemini"
exists "home's .antigravity" "${HOME:-/nonexistent}/.antigravity"
exists "home's .agents" "${HOME:-/nonexistent}/.agents"
top=$(git rev-parse --show-toplevel 2>/dev/null || true)
if [ -n "$top" ]; then
  echo "repository root:             $top"
  exists "the repository's hooks.json" "$top/.agents/hooks.json"
else
  echo "repository root:             none (cwd is not in a git work tree)"
fi
if command -v ps >/dev/null 2>&1; then
  # Names only, as above.
  echo "processes: $(ps -eo comm= 2>/dev/null | sort -u | tr '\n' ' ')"
fi
# The files scripts/antigravity-probe/ writes: dates, process IDs, paths and hashes, nothing else.
for file in "${HOME:-/nonexistent}/.probe-setup" /tmp/probe-setup.log \
  "${HOME:-/nonexistent}/.probe-hooks.log" "${HOME:-/nonexistent}/.probe-bg"; do
  if [ -f "$file" ]; then
    echo "--- $file ($(wc -l < "$file" | tr -d ' ') lines; the last 40)"
    tail -n 40 "$file"
  else
    echo "--- $file: absent"
  fi
done
for file in /tmp/probe-setup.txt /tmp/probe-hook.txt /tmp/probe-agent.txt; do
  if [ -f "$file" ]; then
    echo "$file: written $(date -u -r "$file" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo "at an unknown time")"
  else
    echo "$file: absent"
  fi
done
for file in "${HOME:-/nonexistent}/.profile""${HOME:-/nonexistent}/.bashrc" /etc/environment; do
  if [ -f "$file" ] && grep -q 'PROBE_FROM_SETUP' "$file" 2>/dev/null; then
    echo "$file holds the setup script's PROBE_FROM_SETUP line"
  fi
done

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
if git config --system --get credential.helper >/dev/null 2>&1; then
  echo "credential.helper:       set for the system"
fi
[ -n "${GIT_CONFIG_COUNT:-}" ] && echo "GIT_CONFIG_COUNT:        $GIT_CONFIG_COUNT entries from the environment"
[ -f "${HOME:-/nonexistent}/.git-credentials" ] && echo ".git-credentials:        exists in \$HOME"
[ -f "${HOME:-/nonexistent}/.config/gh/hosts.yml" ] && echo "gh hosts.yml:            exists (gh is logged in)"
if [ -n "$top" ]; then
  # The remote's scheme and host only: its URL can carry a user and a token.
  remote=$(git -C "$top" remote get-url origin 2>/dev/null || true)
  if [ -n "$remote" ]; then
    host=$(printf '%s' "$remote" | sed -e 's#^[a-z+]*://##' -e 's#^[^@/]*@##' -e 's#[:/].*##')
    scheme=$(printf '%s' "$remote" | sed -n 's#^\([a-z+]*\)://.*#\1#p')
    echo "origin:                  ${scheme:-ssh} to $host"
  fi
  if git -C "$top" config --local --get core.hooksPath >/dev/null 2>&1; then
    echo "core.hooksPath:          set in the repository"
  fi
  hooks=$(find "$top/.git/hooks" -maxdepth 1 -type f ! -name '*.sample' 2>/dev/null | sed 's#.*/##' | sort | tr '\n' ' ')
  echo "the repository's hooks:  ${hooks:-none}"
fi

# Does a pre-push hook fire on push? To a bare repository on this disk, so nothing leaves.
(
  cd "$tmp" || exit 1
  git init -q --bare remote.git
  git init -q work
  cd work || exit 1
  git config user.name probe
  git config user.email probe@example.invalid
  git config commit.gpgsign false
  git -c core.hooksPath=/dev/null commit -q --allow-empty -m probe
  git remote add origin ../remote.git
  mkdir -p .git/hooks
  printf '#!/bin/sh\ntouch "%s/fired"\n' "$tmp" > .git/hooks/pre-push
  chmod +x .git/hooks/pre-push
  # This clone's own hooks, whatever a global core.hooksPath says.
  git -c core.hooksPath=.git/hooks push -q origin HEAD:refs/heads/probe >/dev/null 2>&1
)
if [ -f "$tmp/fired" ]; then
  echo "pre-push hook:           fires on git push (local bare remote)"
else
  echo "pre-push hook:           did NOT fire on git push (hooks off, or core.hooksPath elsewhere)"
fi
echo "note: whether the platform's own push (a pull request it opens for the agent) runs hooks"
echo "      can't be seen from here: push from the agent and look for hushrun's hook output."

section "Proxy and trust"
for name in HTTPS_PROXY https_proxy HTTP_PROXY http_proxy ALL_PROXY NO_PROXY no_proxy \
  SSL_CERT_FILE SSL_CERT_DIR NODE_EXTRA_CA_CERTS REQUESTS_CA_BUNDLE CURL_CA_BUNDLE; do
  marker "$name"
done

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
      code=$(curl -sS -o /dev/null -m 10 --cacert "$system_ca" -w '%{http_code}' "$2" 2>"$tmp/err")
      status=$?
      [ "$status" = 0 ] && sys="HTTP $code" || sys="failed: $(head -c 120 "$tmp/err" | tr '\n' ' ')"
    else
      sys="no system CA file found"
    fi
    code=$(curl -sS -o /dev/null -m 10 -w '%{http_code}' "$2" 2>"$tmp/err")
    status=$?
    [ "$status" = 0 ] && dflt="HTTP $code" || dflt="failed: $(head -c 120 "$tmp/err" | tr '\n' ' ')"
    printf '%-38s system store: %s | curl default: %s\n' "$1" "$sys" "$dflt"
  }
  echo "system CA file: ${system_ca:-none}"
  probe api.hushrun.com https://api.hushrun.com/healthz
  probe registry.hushrun.com https://registry.hushrun.com/v2/
  # Your organization's store is a subdomain of it, which only a run that uses the store learns.
  probe r2.cloudflarestorage.com https://r2.cloudflarestorage.com/
  probe ghcr.io https://ghcr.io/v2/
  probe pkg-containers.githubusercontent.com https://pkg-containers.githubusercontent.com/
  probe registry-1.docker.io https://registry-1.docker.io/v2/
  probe us-docker.pkg.dev https://us-docker.pkg.dev/v2/
  probe registry.npmjs.org https://registry.npmjs.org/hushrun
  probe github.com https://github.com/
  # Who signed the certificate this machine sees: the site's own CA, or a proxy that re-terminates
  # TLS. An issuer names a CA, never a credential.
  issuer=$(curl -sS -v -o /dev/null -m 10 https://api.hushrun.com/healthz 2>&1 | sed -n 's/^\* *issuer: *//p' | head -n 1)
  echo "TLS issuer seen for api.hushrun.com: ${issuer:-unknown}"
  echo "an HTTP status means the host answered, but 403 or 407 may be a proxy refusing it."
fi
}

if [ -z "$mode" ]; then
  report
elif [ "$mode" = hook ]; then
  report > "/tmp/probe-$mode.txt" 2>&1
else
  report 2>&1 | tee "/tmp/probe-$mode.txt"
fi

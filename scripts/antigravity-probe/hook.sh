#!/bin/sh
# hook.sh: the hooks of hushrun's Antigravity probe (docs/validation/antigravity-remote.md).
# hooks.json runs it as a SessionStart or Stop hook:
#
#   sh hook.sh SessionStart          log, detach two background jobs, sleep 50 s, then PROBE-OK
#   sh hook.sh SessionStart slow     the same, but sleep 100 s (a timeout above 90 s honored?)
#   sh hook.sh SessionStart empty    log, print nothing
#   sh hook.sh SessionStart text     log, print plain text (PROBE-TEXT)
#   sh hook.sh SessionStart fail     log, print PROBE-FAIL as JSON, exit 1
#   sh hook.sh Stop                  log, let the agent stop
#
# Each run appends lines to ~/.probe-hooks.log: the time, the event, the process, the hook's cwd,
# the names of stdin's fields, its workspacePaths, fullyIdle, and a hash of conversationId that
# tells conversations apart. Never another value of stdin: lastUserInput is the user's prompt, and
# no line holds it or any part of it. Nothing leaves the machine but agent-cloud-probe.sh's
# unauthenticated reachability checks.
set -u

event=${1:-SessionStart}
variant=${2:-main}
# The decision that lets the agent stop. Unconfirmed: Antigravity's hooks.md lists the values of
# Stop's required `decision`; change this one if it names another.
stop_decision=allow

here=$(cd "$(dirname "$0")" && pwd)
log=${HOME:-/tmp}/.probe-hooks.log
now() { date -u +%Y-%m-%dT%H:%M:%SZ; }
boot=$(cut -c1-8 /proc/sys/kernel/random/boot_id 2>/dev/null || echo unknown)

# stdin, kept only in a file this run removes. A terminal means no stdin: don't wait on it.
input=$(mktemp 2>/dev/null || echo "/tmp/probe-stdin.$$")
trap 'rm -f "$input"' EXIT INT TERM
if [ -t 0 ]; then : > "$input"; else cat > "$input"; fi

# The fields the log may hold, from a JSON parser, never from text matching, which a prompt that
# quotes a field's name could fool. Without python3 or node, the log says so.
parse_py='
import hashlib, json, sys
try:
    data = json.load(open(sys.argv[1]))
except Exception as error:
    print("stdin=unparsed(%s)" % type(error).__name__); sys.exit()
if not isinstance(data, dict):
    print("stdin=not-an-object"); sys.exit()
conv = data.get("conversationId")
paths = data.get("workspacePaths")
out = ["keys=" + ",".join(sorted(data))]
out.append("conv=" + (hashlib.sha256(str(conv).encode()).hexdigest()[:10] if conv is not None else "none"))
if isinstance(paths, list):
    out.append("workspacePaths=" + ",".join(p for p in paths if isinstance(p, str)))
else:
    out.append("workspacePaths=" + ("absent" if paths is None else "not-a-list"))
if "fullyIdle" in data:
    out.append("fullyIdle=" + json.dumps(data["fullyIdle"]))
print(" ".join(out))
'
parse_js='
const fs = require("fs"), crypto = require("crypto");
let data;
try { data = JSON.parse(fs.readFileSync(process.argv[1], "utf8")); }
catch (e) { console.log("stdin=unparsed(" + e.name + ")"); process.exit(0); }
if (!data || typeof data !== "object" || Array.isArray(data)) { console.log("stdin=not-an-object"); process.exit(0); }
const out = ["keys=" + Object.keys(data).sort().join(",")];
const conv = data.conversationId;
out.push("conv=" + (conv === undefined || conv === null ? "none" : crypto.createHash("sha256").update(String(conv)).digest("hex").slice(0, 10)));
const paths = data.workspacePaths;
out.push("workspacePaths=" + (Array.isArray(paths) ? paths.filter(p => typeof p === "string").join(",") : (paths === undefined ? "absent" : "not-a-list")));
if ("fullyIdle" in data) out.push("fullyIdle=" + JSON.stringify(data.fullyIdle));
console.log(out.join(" "));
'
if [ ! -s "$input" ]; then
  fields="stdin=empty"
elif command -v python3 >/dev/null 2>&1; then
  fields=$(python3 -I -c "$parse_py" "$input" 2>/dev/null || echo "stdin=parser-failed")
elif command -v node >/dev/null 2>&1; then
  fields=$(node -e "$parse_js" "$input" 2>/dev/null || echo "stdin=parser-failed")
else
  fields="stdin=$(wc -c < "$input" | tr -d ' ')-bytes-unparsed(no-python3-or-node)"
fi
rm -f "$input"

line() { # what
  echo "$(now) $event $variant $1 pid=$$ ppid=$PPID boot=$boot cwd=$(pwd) $fields" >> "$log"
}
# A JSON string's body: paths and times only ever reach it.
json_escape() { printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'; }
inject() { # message
  printf '{"injectSteps":[{"systemMessage":{"systemMessage":"%s"}}]}\n' "$(json_escape "$1")"
}
detach() { # command: in its own session where setsid exists, so the hook's end doesn't stop it
  if command -v setsid >/dev/null 2>&1; then
    setsid sh -c "$1" </dev/null >/dev/null 2>&1 &
  else
    nohup sh -c "$1" </dev/null >/dev/null 2>&1 &
  fi
}

case "$event:$variant" in
  SessionStart:main|SessionStart:slow)
    pause=50
    [ "$variant" = slow ] && pause=100
    started=$(now)
    line start
    # Q4: does a process the hook detaches outlive the hook, and an idle and resume?
    bg=${HOME:-/tmp}/.probe-bg
    echo "$started bg-start hook-pid=$$ boot=$boot" >> "$bg"
    detach "sleep 120; echo \"\$(date -u +%Y-%m-%dT%H:%M:%SZ) bg-done started=$started boot=\$(cut -c1-8 /proc/sys/kernel/random/boot_id 2>/dev/null)\" >> '$bg'"
    # The whole report, in the background, to /tmp/probe-hook.txt.
    if [ -f "$here/../agent-cloud-probe.sh" ]; then
      detach "sh '$here/../agent-cloud-probe.sh' --mode hook"
    fi
    # Q2: is the hook blocking, and does it get the timeout hooks.json gives it?
    sleep "$pause"
    ended=$(now)
    line end
    inject "PROBE-OK: hushrun's probe SessionStart hook ran from $(pwd), started $started, ended $ended after a $pause s sleep. If the user asks whether you saw PROBE-OK, say yes and quote this message whole."
    ;;
  SessionStart:empty)
    line ran
    ;;
  SessionStart:text)
    line ran
    echo "PROBE-TEXT: hushrun's probe SessionStart hook printed this as plain text at $(now). If the user asks whether you saw PROBE-TEXT, say yes and quote this line."
    ;;
  SessionStart:fail)
    line ran
    inject "PROBE-FAIL: hushrun's probe SessionStart hook printed this JSON and then exited 1, at $(now). If the user asks whether you saw PROBE-FAIL, say yes and quote this message."
    exit 1
    ;;
  Stop:*)
    line stop
    printf '{"decision":"%s"}\n' "$stop_decision"
    ;;
  *)
    line unknown
    ;;
esac
exit 0

//! \[M8\] A fake Docker-compatible CLI for isolated runs (E-12): it records every call, and runs
//! `run`'s command on the host, in the mounted export, with a clean environment that holds only
//! the variables passed with `--env`. POSIX hosts only: it is a shell script.
//!
//! What it can't fake, the tests check through the recorded calls: `--network none`, the
//! platform, the image. `container inspect` says every container started, with the exit code
//! its check returned. `rm` ends the check and every process it started, as removing a container
//! does. `HUSHRUN_CONTAINER_RUNTIME` points hushrun at it.

use std::path::{Path, PathBuf};

/// The script. Every call's arguments go to `<state>/calls/<n>`, NUL-separated.
const SCRIPT: &str = r#"#!/bin/sh
state="$(dirname "$0")/fake-runtime"
mkdir -p "$state/calls"
n=$(ls "$state/calls" | wc -l | tr -d ' ')
printf '%s\0' "$@" > "$state/calls/$(printf '%05d' "$n")"
# Ends process $1 and every process under it, children first, as removing a container ends
# everything in it. Killing the check's shell alone isn't enough: dash runs the last command of
# `sh -c` as a child, which would keep running and keep hushrun's stderr open (bash, macOS's sh,
# replaces itself with that command).
end_tree() {
  for child in $(pgrep -P "$1"); do end_tree "$child"; done
  kill "$1" 2>/dev/null
}
case "$1" in
  --version)
    echo "fake version 99.1.0, build test"
    exit 0 ;;
  image)
    [ -f "$state/absent" ] && exit 1
    cat "$state/platform" 2>/dev/null || echo linux/amd64
    exit 0 ;;
  pull)
    [ -f "$state/pull-fails" ] && { echo "pull refused" >&2; exit 1; }
    rm -f "$state/absent"
    exit 0 ;;
  rm)
    pid=$(cat "$state/$3.pid" 2>/dev/null) && end_tree "$pid"
    rm -f "$state/$3.pid" "$state/$3.exit"
    exit 0 ;;
  container)
    case "$2" in
      prune) exit 0 ;;
      inspect)
        name=$5
        [ -f "$state/$name.pid" ] || { echo "no such container: $name" >&2; exit 1; }
        printf '2026-10-16T12:00:00.000000000Z|%s|\n' "$(cat "$state/$name.exit" 2>/dev/null || echo 0)"
        exit 0 ;;
    esac
    echo "fake runtime: unexpected container $2" >&2; exit 125 ;;
  run) shift ;;
  *) echo "fake runtime: unexpected $1" >&2; exit 125 ;;
esac
[ -f "$state/run-fails" ] && { echo "the fake daemon is down" >&2; exit 125; }
name=""; volume=""; entry=""; envs=""
while [ $# -gt 0 ]; do
  case "$1" in
    --rm|--init) shift ;;
    --name) name=$2; shift 2 ;;
    --volume) volume=$2; shift 2 ;;
    --env) envs="$envs $2"; shift 2 ;;
    --entrypoint) entry=$2; shift 2 ;;
    --pull|--network|--platform|--security-opt|--workdir|--label) shift 2 ;;
    -*) echo "fake runtime: unexpected option $1" >&2; exit 125 ;;
    *) shift; break ;;
  esac
done
dir=${volume%%:/src*}
cd "$dir" || exit 125
set -- "$entry" "$@"
for var in $envs; do
  set -- "$var=$(printenv "$var")" "$@"
done
# The check runs as a child, so that its exit code is the container's, for `container inspect`.
env -i PATH=/usr/bin:/bin "$@" &
echo $! > "$state/$name.pid"
wait $!
code=$?
echo $code > "$state/$name.exit"
exit $code
"#;

/// The fake runtime, installed in a directory.
pub struct FakeRuntime {
    pub path: PathBuf,
    state: PathBuf,
}

impl FakeRuntime {
    /// Writes the script to `<dir>/fake-docker`, executable.
    pub fn install(dir: &Path) -> FakeRuntime {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("fake-docker");
        std::fs::write(&path, SCRIPT).expect("write the fake runtime");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make it executable");
        let state = dir.join("fake-runtime");
        std::fs::create_dir_all(state.join("calls")).expect("state");
        FakeRuntime { path, state }
    }

    /// Every call so far, oldest first, each as its arguments.
    pub fn calls(&self) -> Vec<Vec<String>> {
        let mut names: Vec<PathBuf> = std::fs::read_dir(self.state.join("calls"))
            .expect("calls")
            .map(|e| e.expect("entry").path())
            .collect();
        names.sort();
        names
            .iter()
            .map(|p| {
                let bytes = std::fs::read(p).expect("call");
                bytes
                    .split(|b| *b == 0)
                    .filter(|a| !a.is_empty())
                    .map(|a| String::from_utf8_lossy(a).into_owned())
                    .collect()
            })
            .collect()
    }

    /// The containers that are still there: started and not removed.
    pub fn containers(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.state)
            .expect("state")
            .filter_map(|e| {
                let name = e.expect("entry").file_name().into_string().ok()?;
                name.strip_suffix(".pid").map(str::to_string)
            })
            .collect();
        names.sort();
        names
    }

    /// The `run` calls so far.
    pub fn runs(&self) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|c| c.first().map(String::as_str) == Some("run"))
            .collect()
    }

    /// Forgets the calls so far.
    pub fn clear(&self) {
        std::fs::remove_dir_all(self.state.join("calls")).expect("clear");
        std::fs::create_dir_all(self.state.join("calls")).expect("calls");
    }

    /// Makes the image absent until a pull, or the pull fail, or the runtime fail on `run`.
    pub fn set(&self, flag: &str, on: bool) {
        let path = self.state.join(flag);
        if on {
            std::fs::write(path, "").expect("flag");
        } else {
            let _ = std::fs::remove_file(path);
        }
    }

    /// The platform `image inspect` reports.
    pub fn set_platform(&self, platform: &str) {
        std::fs::write(self.state.join("platform"), format!("{platform}\n")).expect("platform");
    }
}

/// The value after `option` in a call's arguments.
pub fn option<'a>(call: &'a [String], option: &str) -> Option<&'a str> {
    call.iter()
        .position(|a| a == option)
        .and_then(|i| call.get(i + 1))
        .map(String::as_str)
}

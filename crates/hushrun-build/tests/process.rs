//! The wrapper as a process (`examples/hushrun-rustc.rs`) in front of a fake `rustc` script:
//! its name, exit codes and the messages it passes on.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use hushrun_build::model::{Mode, NotStored, Outcome};
use hushrun_build::session::{self, Session, Start};
use hushrun_build::settings;

/// A rustc that answers `-vV`, writes its outputs and a dep-info, prints the notices on stderr
/// and exits with `FAKE_EXIT`.
const FAKE_RUSTC: &str = r#"#!/bin/sh
if [ "$1" = "-vV" ]; then
  printf 'rustc 1.94.0 (fake)\nhost: x86_64-unknown-linux-gnu\nrelease: 1.94.0\n'
  exit 0
fi
name=""; out=""; extra=""; src=""; prev=""
for a in "$@"; do
  case "$prev" in
    --crate-name) name="$a" ;;
    --out-dir) out="$a" ;;
    -C) case "$a" in extra-filename=*) extra="${a#extra-filename=}" ;; esac ;;
  esac
  case "$a" in *.rs) src="$a" ;; esac
  prev="$a"
done
if [ -n "$FAKE_EXIT" ] && [ "$FAKE_EXIT" != 0 ]; then
  echo '{"$message_type":"diagnostic","message":"boom","code":null,"level":"error","spans":[],"children":[],"rendered":"error: boom\n"}' >&2
  exit "$FAKE_EXIT"
fi
stem="$name$extra"
cat "$src" > "$out/lib$stem.rmeta"
cat "$src" > "$out/lib$stem.rlib"
printf '%s: %s\n\n%s:\n' "$out/$stem.d" "$src" "$src" > "$out/$stem.d"
echo "{\"\$message_type\":\"artifact\",\"artifact\":\"$out/$stem.d\",\"emit\":\"dep-info\"}" >&2
echo "{\"\$message_type\":\"artifact\",\"artifact\":\"$out/lib$stem.rmeta\",\"emit\":\"metadata\"}" >&2
echo "{\"\$message_type\":\"artifact\",\"artifact\":\"$out/lib$stem.rlib\",\"emit\":\"link\"}" >&2
exit 0
"#;

fn wrapper_exe() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let debug = exe.parent().unwrap().parent().unwrap();
    debug.join("examples").join("hushrun-rustc")
}

struct World {
    dir: tempfile::TempDir,
    rustc: PathBuf,
    session: Session,
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let rustc = dir.path().join("toolchain/bin/rustc");
    fs::create_dir_all(rustc.parent().unwrap()).unwrap();
    fs::write(&rustc, FAKE_RUSTC).unwrap();
    fs::set_permissions(&rustc, fs::Permissions::from_mode(0o755)).unwrap();
    let settings = dir.path().join("build.json");
    settings::update(&settings, |s| {
        s.mode = Mode::On;
        s.min_free_gb = 0;
    })
    .unwrap();
    let session = session::begin(&Start {
        settings: &settings,
        cache_dir: Some(&dir.path().join("cache")),
        exe: &wrapper_exe(),
        in_ci: false,
        repository: None,
        env: &|_| None,
        now: SystemTime::now(),
    })
    .unwrap();
    World {
        dir,
        rustc,
        session,
    }
}

impl World {
    fn checkout(&self, name: &str) -> PathBuf {
        let ws = self.dir.path().join(name);
        fs::create_dir_all(ws.join("a/src")).unwrap();
        fs::create_dir_all(ws.join("target/deps")).unwrap();
        fs::write(ws.join("a/src/lib.rs"), "pub fn f() {}\n").unwrap();
        PathBuf::from(hushrun_build::roots::resolve_on_disk(ws.to_str().unwrap()).unwrap())
    }

    /// Runs the wrapper as cargo would, through the link named `hushrun-rustc`.
    fn run(&self, ws: &Path, session: bool, exit: &str) -> std::process::Output {
        let env = self.session.env();
        let link = &env[0].1;
        let deps = ws.join("target/deps");
        let mut command = Command::new(link);
        command
            .arg(&self.rustc)
            .args([
                "--crate-name",
                "a",
                "--edition=2021",
                "a/src/lib.rs",
                "--error-format=json",
                "--json=artifacts",
                "--crate-type",
                "lib",
                "--emit=dep-info,metadata,link",
                "-C",
                "extra-filename=-1",
                "--out-dir",
            ])
            .arg(&deps)
            .current_dir(ws)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("CARGO_PKG_NAME", "a")
            .env("CARGO_MANIFEST_DIR", ws.join("a"))
            .env("FAKE_EXIT", exit);
        if session {
            command.env(&env[1].0, &env[1].1);
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
        command.output().unwrap()
    }
}

// covers: B-2, B-3, B-12, B-19
#[test]
fn the_wrapper_process_keeps_rustcs_exit_code_and_passes_its_messages_on() {
    let w = world();
    let one = w.checkout("one");
    let two = w.checkout("two-longer");

    // No session: rustc as asked, its code kept, nothing logged.
    let out = w.run(&one, false, "5");
    assert_eq!(out.status.code(), Some(5));
    assert!(w.session.current().entries().is_empty());

    // A failed compile: rustc's code and its error, nothing stored.
    let out = w.run(&one, true, "3");
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("boom"));
    let entries = w.session.current().entries();
    assert_eq!(
        entries.last().unwrap().outcome,
        Outcome::BuiltNotStored(NotStored::RustcFailed)
    );

    // Built and stored, then reused in another checkout with its notices, the rmeta's last of
    // the two outputs cargo waits on.
    let out = w.run(&one, true, "0");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        w.session.current().entries().last().unwrap().outcome,
        Outcome::BuiltAndStored
    );
    let out = w.run(&two, true, "7");
    assert_eq!(
        out.status.code(),
        Some(0),
        "rustc never ran: its code is not used"
    );
    assert_eq!(
        w.session.current().entries().last().unwrap().outcome,
        Outcome::Reused
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 3, "{stderr}");
    let deps = two.join("target/deps");
    assert!(lines[1].contains(&format!("{}/liba-1.rmeta", deps.display())));
    assert_eq!(
        fs::read(deps.join("liba-1.rlib")).unwrap(),
        fs::read(one.join("target/deps/liba-1.rlib")).unwrap()
    );
}

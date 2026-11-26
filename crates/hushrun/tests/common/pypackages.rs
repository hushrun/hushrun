//! What the baked Python packages tests share (`packages_pip.rs`, `packages_poetry.rs`): pip
//! (`HUSHRUN_TEST_PIP`) and Poetry (`HUSHRUN_TEST_POETRY`), each found or the test says so, and a
//! wheel built by hand, so that no test needs the network.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use crate::python::tools;

/// pip, as `HUSHRUN_TEST_PIP` names it (`pip3` by default), when it runs: its path and the folder
/// that holds it, whose `python3` runs it.
pub fn pip() -> Option<(String, PathBuf)> {
    let pip = std::env::var("HUSHRUN_TEST_PIP").unwrap_or_else(|_| "pip3".into());
    let here = runs(&pip);
    if !tools("pip (HUSHRUN_TEST_PIP)", here) {
        return None;
    }
    let dir = which(&pip)?.parent()?.to_path_buf();
    Some((pip, dir))
}

/// Poetry, as `HUSHRUN_TEST_POETRY` names it (`poetry` by default), when it runs.
pub fn poetry() -> Option<String> {
    let poetry = std::env::var("HUSHRUN_TEST_POETRY").unwrap_or_else(|_| "poetry".into());
    tools("Poetry (HUSHRUN_TEST_POETRY)", runs(&poetry)).then_some(poetry)
}

fn runs(program: &str) -> bool {
    hushrun_testkit::tool::command(program)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The path of `program`, as `PATH` finds it, or itself when it names a path.
fn which(program: &str) -> Option<PathBuf> {
    let given = Path::new(program);
    if given.components().count() > 1 {
        return Some(given.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(program))
        .find(|p| p.is_file())
}

/// `PATH` with `dir` first.
pub fn path_with(dir: &Path) -> std::ffi::OsString {
    let mut dirs = vec![dir.to_path_buf()];
    dirs.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(dirs).unwrap()
}

/// The wheel the tests install: `hushdemo` 1.0, whose module holds `VALUE = 7`.
pub const WHEEL: &str = "hushdemo-1.0-py3-none-any.whl";

/// Writes [`WHEEL`] into `dir` with the `python3` beside pip, and returns its SHA-256.
pub fn write_wheel(python_dir: &Path, dir: &Path) -> String {
    std::fs::create_dir_all(dir).unwrap();
    let script = r#"
import base64, hashlib, sys, zipfile
out = sys.argv[1]
files = {
    "hushdemo/__init__.py": "VALUE = 7\n",
    "hushdemo-1.0.dist-info/METADATA": "Metadata-Version: 2.1\nName: hushdemo\nVersion: 1.0\n",
    "hushdemo-1.0.dist-info/WHEEL": "Wheel-Version: 1.0\nGenerator: hand\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
}
record = []
with zipfile.ZipFile(out, "w") as z:
    for name, text in files.items():
        data = text.encode()
        z.writestr(zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0)), data)
        digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
        record.append(f"{name},sha256={digest},{len(data)}")
    record.append("hushdemo-1.0.dist-info/RECORD,,")
    z.writestr(zipfile.ZipInfo("hushdemo-1.0.dist-info/RECORD", (1980, 1, 1, 0, 0, 0)), "\n".join(record) + "\n")
print(hashlib.sha256(open(out, "rb").read()).hexdigest())
"#;
    let python = python_dir.join(if cfg!(windows) {
        "python.exe"
    } else {
        "python3"
    });
    let out = hushrun_testkit::tool::command(python.to_str().unwrap())
        .args(["-I", "-c", script])
        .arg(dir.join(WHEEL))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Changes one byte of `file` in place, its size the same.
pub fn tamper(file: &Path) {
    let mut bytes = std::fs::read(file).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    std::fs::write(file, bytes).unwrap();
}

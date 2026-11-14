//! Real programs made by the system's own C compiler, and files crafted to look like one: what
//! the check before a downloaded program is put in place (B-38) says of each, with nothing run.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use hushrun_build::loader::{Here, can_start};

fn asked(program: &Path) -> Result<(), String> {
    let folder: PathBuf = program.parent().unwrap().to_path_buf();
    can_start(
        program,
        &Here {
            folder: &folder,
            sdk: None,
        },
    )
}

/// A program the system's C compiler links from `int main(void){return 0;}`, with `more`.
fn compiled(dir: &Path, name: &str, more: &[&str]) -> PathBuf {
    let source = dir.join("p.c");
    std::fs::write(&source, "int main(void){return 0;}\n").unwrap();
    let program = dir.join(name);
    let out = Command::new("cc")
        .arg(&source)
        .arg("-o")
        .arg(&program)
        .args(more)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    program
}

// covers: B-38
#[test]
fn a_file_whose_loader_is_a_script_is_a_miss_and_the_script_never_runs() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    let payload = dir.path().join("payload");
    std::fs::write(
        &payload,
        format!("#!/bin/sh\necho \"$@\" > '{}'\nexit 0\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o755)).unwrap();
    // A 64-bit little-endian ELF file with one program header: `PT_INTERP`, naming the script.
    let mut interp = payload.to_str().unwrap().as_bytes().to_vec();
    interp.push(0);
    let mut elf = vec![0u8; 64 + 56];
    elf[..6].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1]);
    let machine: u16 = if cfg!(target_arch = "aarch64") {
        183
    } else {
        62
    };
    elf[18..20].copy_from_slice(&machine.to_le_bytes());
    elf[0x20..0x28].copy_from_slice(&64u64.to_le_bytes());
    elf[0x36..0x38].copy_from_slice(&56u16.to_le_bytes());
    elf[0x38..0x3a].copy_from_slice(&1u16.to_le_bytes());
    elf[64..68].copy_from_slice(&3u32.to_le_bytes());
    elf[64 + 8..64 + 16].copy_from_slice(&120u64.to_le_bytes());
    elf[64 + 0x20..64 + 0x28].copy_from_slice(&(interp.len() as u64).to_le_bytes());
    elf.extend(interp);
    let program = dir.path().join("build-script-build");
    std::fs::write(&program, elf).unwrap();
    let said = asked(&program);
    assert!(said.is_err(), "{said:?}");
    assert!(!marker.exists(), "the file it names ran");
}

// covers: B-38
#[test]
fn a_real_program_starts_and_one_cut_in_half_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let good = compiled(dir.path(), "good", &[]);
    assert_eq!(asked(&good), Ok(()));
    let bytes = std::fs::read(&good).unwrap();
    let cut = dir.path().join("cut");
    std::fs::write(&cut, &bytes[..bytes.len() / 2]).unwrap();
    let why = asked(&cut).unwrap_err();
    assert!(why.contains("the file ends before"), "{why}");
}

// covers: B-38
#[cfg(target_os = "macos")]
#[test]
fn a_real_mach_o_program_that_cant_start_is_a_miss() {
    let dir = tempfile::tempdir().unwrap();
    let good = compiled(dir.path(), "good", &[]);
    // A system library that is not there: its path under `/usr/lib/` is no proof.
    let nolib = dir.path().join("nolib");
    std::fs::copy(&good, &nolib).unwrap();
    let renamed = Command::new("install_name_tool")
        .args([
            "-change",
            "/usr/lib/libSystem.B.dylib",
            "/usr/lib/libNOPE.B.dylib",
        ])
        .arg(&nolib)
        .output()
        .unwrap();
    assert!(renamed.status.success());
    let why = asked(&nolib).unwrap_err();
    assert!(why.contains("libNOPE.B.dylib"), "{why}");
    // Built for the other CPU only.
    let other = if cfg!(target_arch = "aarch64") {
        "x86_64"
    } else {
        "arm64"
    };
    let foreign = compiled(dir.path(), "foreign", &["-arch", other]);
    let why = asked(&foreign).unwrap_err();
    assert!(why.contains("another CPU"), "{why}");
    // Built for both: this machine's part counts.
    let both = compiled(dir.path(), "both", &["-arch", "x86_64", "-arch", "arm64"]);
    assert_eq!(asked(&both), Ok(()));
}

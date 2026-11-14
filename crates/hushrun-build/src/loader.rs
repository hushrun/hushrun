//! Whether a program that came from another machine can start on this one (B-38).
//!
//! A downloaded build-script binary is run by cargo, not by rustc: one that can't start fails
//! the build with "failed to run custom build command", no rule that watches rustc's errors
//! sees it (B-15), and cargo takes the binary for up to date, so every later build fails the
//! same way until the target folder is cleaned. A program that can't start is a test that
//! fails for no reason in the code. With a link environment that holds everything (B-37) this
//! can't happen; the check is for what it missed. It runs none of the program's code.
//!
//! [`can_start`] decides by the file's own format:
//!
//! - **ELF** with a loader (`PT_INTERP`): the loader must be a file here, and is asked to list
//!   what the program needs (`<loader> --list <program>`), as `ldd` does. It finds every
//!   library and checks the symbol versions the program asks of each; a library found nowhere
//!   or a version one doesn't have makes it exit with an error. Without a loader (a static
//!   program) there is nothing to find.
//! - **Mach-O**: each library a load command names must be a file where dyld would look
//!   (`@rpath`, `@loader_path`, `@executable_path` followed), or one of the system's own, which
//!   live only in the shared cache (`/usr/lib/…`, `/System/…`). Nothing is asked of dyld: macOS
//!   has no loader that lists without running.
//! - anything else is not a program hushrun can read: nothing to check.

use std::ffi::OsStr;
use std::path::Path;

use crate::link::Ask;
use crate::linkenv::{MachOPlace, binary, macho_place};

/// `Ok` when `program` can start here as far as its loader can tell without running it; else
/// why not, in words for the run's log.
pub fn can_start(program: &Path, ask: &Ask<'_>) -> Result<(), String> {
    match binary::read(program) {
        Some(binary::Binary::Elf(elf)) => {
            let Some(loader) = elf.interpreter else {
                return Ok(());
            };
            let loader = Path::new(&loader);
            if !std::fs::metadata(loader).is_ok_and(|meta| meta.is_file()) {
                return Err(format!(
                    "its loader, {}, is not on this machine",
                    loader.display()
                ));
            }
            let args = ["--list".to_owned(), program.to_string_lossy().into_owned()];
            let Some(answer) = ask(loader, &args, OsStr::new(""), &[]) else {
                return Err(format!("its loader, {}, can't be run", loader.display()));
            };
            let said = format!("{}\n{}", answer.stdout, answer.stderr);
            let missing = said
                .lines()
                .map(str::trim)
                .find(|line| line.contains("not found") || line.contains("cannot open"));
            match (answer.success, missing) {
                (true, None) => Ok(()),
                (_, Some(line)) => Err(format!("its loader says: {line}")),
                (false, None) => Err(format!(
                    "its loader refuses it: {}",
                    said.lines()
                        .map(str::trim)
                        .find(|line| !line.is_empty())
                        .unwrap_or("no reason given")
                )),
            }
        }
        Some(binary::Binary::MachO(macho)) => {
            for dylib in &macho.dylibs {
                if let MachOPlace::Nowhere = macho_place(program, program, &macho, dylib) {
                    return Err(format!("{dylib}, which it loads, is not on this machine"));
                }
            }
            Ok(())
        }
        None => Ok(()),
    }
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::*;
    use crate::link::Answer;

    /// A 64-bit Mach-O program that loads `dylibs` and nothing more.
    pub(crate) fn macho(dylibs: &[&str]) -> Vec<u8> {
        let mut commands = Vec::new();
        for name in dylibs {
            // LC_LOAD_DYLIB: cmd, cmdsize, the name's offset, a date, two versions, the name.
            let mut text = name.as_bytes().to_vec();
            text.push(0);
            while (24 + text.len()) % 8 != 0 {
                text.push(0);
            }
            let size = 24 + text.len() as u32;
            for word in [0xc_u32, size, 24, 0, 0, 0] {
                commands.extend(word.to_le_bytes());
            }
            commands.extend(text);
        }
        let mut out = Vec::new();
        // magic, cputype (arm64), cpusubtype, filetype (MH_EXECUTE), ncmds, sizeofcmds, flags,
        // reserved.
        for word in [
            0xfeed_facf_u32,
            0x0100_000c,
            0,
            2,
            dylibs.len() as u32,
            commands.len() as u32,
            0,
            0,
        ] {
            out.extend(word.to_le_bytes());
        }
        out.extend(commands);
        out
    }

    /// A 64-bit little-endian ELF program whose loader is `loader`.
    pub(crate) fn elf(loader: &str) -> Vec<u8> {
        let mut interp = loader.as_bytes().to_vec();
        interp.push(0);
        let mut out = vec![0u8; 64 + 56];
        out[..6].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1]);
        out[0x20..0x28].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
        out[0x36..0x38].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        out[0x38..0x3a].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
        // One program header: PT_INTERP, at the end of the file.
        out[64..68].copy_from_slice(&3u32.to_le_bytes());
        out[64 + 8..64 + 16].copy_from_slice(&120u64.to_le_bytes());
        out[64 + 0x20..64 + 0x28].copy_from_slice(&(interp.len() as u64).to_le_bytes());
        out.extend(interp);
        out
    }

    fn never(_: &Path, _: &[String], _: &OsStr, _: &[(String, Option<String>)]) -> Option<Answer> {
        panic!("nothing is asked");
    }

    // covers: B-38
    #[test]
    fn a_mach_o_program_starts_when_each_library_it_loads_is_here() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("p");
        std::fs::write(&program, macho(&["/usr/lib/libSystem.B.dylib"])).unwrap();
        assert_eq!(can_start(&program, &never), Ok(()));
        // One beside the program, by `@loader_path`: there, then gone.
        std::fs::write(
            &program,
            macho(&["/usr/lib/libSystem.B.dylib", "@loader_path/libq.dylib"]),
        )
        .unwrap();
        let why = can_start(&program, &never).unwrap_err();
        assert!(why.contains("libq.dylib"), "{why}");
        std::fs::write(dir.path().join("libq.dylib"), b"q").unwrap();
        assert_eq!(can_start(&program, &never), Ok(()));
        // An absolute path outside the system's own.
        std::fs::write(&program, macho(&["/opt/gone/lib/libz.9.dylib"])).unwrap();
        assert!(can_start(&program, &never).is_err());
    }

    // covers: B-38
    #[test]
    fn an_elf_program_starts_when_its_loader_is_here_and_lists_it_without_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("p");
        // No loader of that name on this machine.
        std::fs::write(&program, elf("/nonexistent/ld-linux.so.9")).unwrap();
        let why = can_start(&program, &never).unwrap_err();
        assert!(why.contains("/nonexistent/ld-linux.so.9"), "{why}");
        // A loader that is there is asked to list the program, and its answer decides.
        let loader = dir.path().join("ld.so");
        std::fs::write(&loader, b"loader").unwrap();
        std::fs::write(&program, elf(loader.to_str().unwrap())).unwrap();
        let answering = |success: bool, stdout: &'static str, stderr: &'static str| {
            let (loader, program) = (loader.clone(), program.clone());
            move |asked: &Path, args: &[String], _: &OsStr, _: &[(String, Option<String>)]| {
                assert_eq!(asked, loader);
                assert_eq!(args, ["--list".to_owned(), program.display().to_string()]);
                Some(Answer {
                    success,
                    stdout: stdout.into(),
                    stderr: stderr.into(),
                })
            }
        };
        let fine = answering(true, "\tlibc.so.6 => /lib/libc.so.6 (0x0000)\n", "");
        assert_eq!(can_start(&program, &fine), Ok(()));
        let no_library = answering(true, "\tlibq.so.1 => not found\n", "");
        let why = can_start(&program, &no_library).unwrap_err();
        assert!(why.contains("libq.so.1 => not found"), "{why}");
        let no_version = answering(
            false,
            "",
            "p: /lib/libc.so.6: version `GLIBC_2.38' not found (required by p)\n",
        );
        let why = can_start(&program, &no_version).unwrap_err();
        assert!(why.contains("GLIBC_2.38"), "{why}");
        let refused = answering(false, "", "p: error while loading\n");
        assert!(can_start(&program, &refused).is_err());
        let gone = |_: &Path, _: &[String], _: &OsStr, _: &[(String, Option<String>)]| None;
        assert!(can_start(&program, &gone).is_err());
        // A static program has no loader, and a file that is no program nothing to check.
        let mut none = elf("x");
        none[64..68].copy_from_slice(&1u32.to_le_bytes());
        std::fs::write(&program, none).unwrap();
        assert_eq!(can_start(&program, &never), Ok(()));
        std::fs::write(&program, b"#!/bin/sh\n").unwrap();
        assert_eq!(can_start(&program, &never), Ok(()));
    }

    // covers: B-38
    #[test]
    fn this_test_program_can_start_by_its_real_loader() {
        // The real thing on whatever this runs on: the test's own program, which did start.
        let ask = |program: &Path, args: &[String], _: &OsStr, _: &[(String, Option<String>)]| {
            let out = std::process::Command::new(program)
                .args(args)
                .output()
                .ok()?;
            Some(Answer {
                success: out.status.success(),
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            })
        };
        let me = std::env::current_exe().unwrap();
        assert_eq!(can_start(&me, &ask), Ok(()));
    }
}

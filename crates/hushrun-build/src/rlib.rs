//! Whether a library's `.rlib` holds anything but what rustc compiled from the crate itself
//! (B-6 `native folders`).
//!
//! An `.rlib` is an `ar` archive. rustc writes into it the crate's metadata (`lib.rmeta`) and its
//! own object files, each named `<crate name><extra filename>.<codegen unit>.rcgu.o`, and, for
//! each static library it bundles (`-l static=` or a `#[link(kind = "static")]` in the source),
//! that library's members under their own names. A library whose archive holds nothing else
//! bundled nothing, so the static libraries in the native folders it was handed are not in its
//! bytes. A member's suffix proves nothing: a build script can name its object `inner.rcgu.o`,
//! and a bundled Rust static library's objects all end in `.rcgu.o`. Only the crate's own prefix,
//! from its `--crate-name` and `-C extra-filename`, marks rustc's objects. Both `ar` formats
//! rustc writes are read: GNU's (Linux, with long names in the `//` member) and BSD's (Apple,
//! with `#1/<length>` names). Only the member headers are read.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const MAGIC: &[u8; 8] = b"!<arch>\n";
const HEADER: usize = 60;

/// The prefix of the objects rustc writes for a crate into its `.rlib`: its crate name, its
/// `-C extra-filename` and a dot, then the codegen unit's name
/// (`memchr-caca4131.memchr.7959cbc2-cgu.0.rcgu.o`, or a hashed name when incremental).
pub fn own_prefix(crate_name: &str, extra_filename: &str) -> String {
    format!("{crate_name}{extra_filename}.")
}

/// Whether the archive at `path` holds only rustc's own members for the crate whose objects
/// start with `prefix` ([`own_prefix`]): `Some(true)` when it bundled nothing, `Some(false)`
/// when it holds another member, `None` when it can't be read as an `ar` archive (the caller
/// then assumes it bundled something).
pub fn bundles_nothing(path: &Path, prefix: &str) -> Option<bool> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic).ok()?;
    if &magic != MAGIC {
        return None;
    }
    let mut names: Vec<u8> = Vec::new();
    let mut at = 8u64;
    while at < len {
        if at % 2 == 1 {
            at += 1;
            if at >= len {
                break;
            }
        }
        file.seek(SeekFrom::Start(at)).ok()?;
        let mut header = [0u8; HEADER];
        file.read_exact(&mut header).ok()?;
        if &header[58..60] != b"`\n" {
            return None;
        }
        let size: u64 = std::str::from_utf8(&header[48..58])
            .ok()?
            .trim()
            .parse()
            .ok()?;
        let raw = std::str::from_utf8(&header[..16]).ok()?.trim_end();
        let data = at + HEADER as u64;
        let name: String = if let Some(n) = raw.strip_prefix("#1/") {
            // BSD: the name is the first `n` bytes of the data.
            let n: usize = n.parse().ok()?;
            let mut buf = vec![0u8; n];
            file.read_exact(&mut buf).ok()?;
            String::from_utf8_lossy(&buf)
                .trim_end_matches('\0')
                .to_string()
        } else if raw == "//" {
            // GNU: the table of long names.
            let mut buf = vec![0u8; usize::try_from(size).ok()?];
            file.read_exact(&mut buf).ok()?;
            names = buf;
            at = data + size;
            continue;
        } else if let Some(offset) = raw.strip_prefix('/').filter(|o| !o.is_empty()) {
            match offset.parse::<usize>() {
                Ok(offset) => {
                    let rest = names.get(offset..)?;
                    let end = rest.iter().position(|b| *b == b'\n').unwrap_or(rest.len());
                    String::from_utf8_lossy(&rest[..end])
                        .trim_end_matches('/')
                        .to_string()
                }
                // `/SYM64/` and the like: an index, not a member.
                Err(_) => raw.to_string(),
            }
        } else {
            raw.trim_end_matches('/').to_string()
        };
        at = data + size;
        if index(&name) {
            continue;
        }
        let own = name == "lib.rmeta"
            || name
                .strip_prefix(prefix)
                .is_some_and(|unit| unit.ends_with(".rcgu.o"));
        if !own {
            return Some(false);
        }
    }
    Some(true)
}

/// Whether a member is the archive's symbol index rather than a member of its own.
fn index(name: &str) -> bool {
    matches!(
        name,
        "" | "/" | "/SYM64/" | "/SYM64" | "__.SYMDEF" | "__.SYMDEF SORTED" | "__.SYMDEF_64"
    )
}

/// Writes an `ar` archive in GNU's form with these members, for tests.
#[cfg(test)]
pub(crate) fn write_gnu(path: &Path, members: &[(&str, &[u8])]) -> std::io::Result<()> {
    let mut out = MAGIC.to_vec();
    let mut table = Vec::new();
    let mut offsets = Vec::new();
    for (name, _) in members {
        if name.len() > 15 {
            offsets.push(Some(table.len()));
            table.extend_from_slice(format!("{name}/\n").as_bytes());
        } else {
            offsets.push(None);
        }
    }
    let header = |name: &str, size: usize| {
        format!("{name:<16}{:<12}{:<6}{:<6}{:<8}{size:<10}`\n", 0, 0, 0, 644).into_bytes()
    };
    let pad = |out: &mut Vec<u8>| {
        if out.len() % 2 == 1 {
            out.push(b'\n');
        }
    };
    if !table.is_empty() {
        out.extend(header("//", table.len()));
        out.extend_from_slice(&table);
        pad(&mut out);
    }
    for ((name, bytes), offset) in members.iter().zip(offsets) {
        let label = match offset {
            Some(offset) => format!("/{offset}"),
            None => format!("{name}/"),
        };
        out.extend(header(&label, bytes.len()));
        out.extend_from_slice(bytes);
        pad(&mut out);
    }
    std::fs::write(path, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-6
    #[test]
    fn an_rlib_with_only_rustcs_own_members_bundled_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let rlib = dir.path().join("libx.rlib");
        let prefix = own_prefix("x", "-1a2b");
        let own: &[(&str, &[u8])] = &[
            ("lib.rmeta", b"meta"),
            ("x-1a2b.x.3c4d-cgu.0.rcgu.o", b"obj"),
            ("x-1a2b.x.3c4d-cgu.1.rcgu.o", b"obj1"),
        ];
        write_gnu(&rlib, own).unwrap();
        assert_eq!(bundles_nothing(&rlib, &prefix), Some(true));
        let bundled = |member: &str| {
            let mut members = own.to_vec();
            members.push((member, b"bundled"));
            write_gnu(&rlib, &members).unwrap();
            bundles_nothing(&rlib, &prefix)
        };
        // A C object, a build script's object named as rustc names its own, another crate's
        // objects (a bundled Rust static library), and the bare prefix.
        for member in [
            "25ac62e5b3c53843-curve25519.o",
            "inner.rcgu.o",
            "std-5f49f7b3.std.2c1a-cgu.0.rcgu.o",
            "x.x.3c4d-cgu.0.rcgu.o",
            "x-1a2b.rcgu.o",
        ] {
            assert_eq!(bundled(member), Some(false), "{member}");
        }
        write_gnu(&rlib, &[("lib.rmeta", b"m"), ("sqlite3.o", b"c")]).unwrap();
        assert_eq!(bundles_nothing(&rlib, &prefix), Some(false));
        std::fs::write(&rlib, b"not an archive").unwrap();
        assert_eq!(bundles_nothing(&rlib, &prefix), None);
    }

    // covers: B-6
    #[test]
    fn real_rlibs_from_rustc_say_whether_they_bundled_a_static_library() {
        let dir = tempfile::tempdir().unwrap();
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let run = |args: &[&str]| {
            let out = std::process::Command::new(&rustc)
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        std::fs::write(
            dir.path().join("x.rs"),
            "pub fn f() -> u32 { 1 }\npub fn g() -> u32 { 2 }\n",
        )
        .unwrap();
        // A static library whose one object is named as rustc names its own.
        std::fs::write(
            dir.path().join("inner.rs"),
            "#[unsafe(no_mangle)] pub extern \"C\" fn inner_value() -> u32 { 1 }\n",
        )
        .unwrap();
        run(&[
            "--crate-type=lib",
            "--emit=obj",
            "-o",
            "inner.rcgu.o",
            "inner.rs",
        ]);
        let object = std::fs::read(dir.path().join("inner.rcgu.o")).unwrap();
        std::fs::create_dir_all(dir.path().join("native")).unwrap();
        write_gnu(
            &dir.path().join("native/libinner.a"),
            &[("inner.rcgu.o", &object)],
        )
        .unwrap();
        // A Rust static library: every member is an `.rcgu.o` of std, core or the crate, and on
        // a target whose `compiler_builtins` holds no C object, nothing else.
        run(&[
            "--crate-type=staticlib",
            "--crate-name=inner2",
            "-o",
            "native/libinner2.a",
            "inner.rs",
        ]);
        let prefix = own_prefix("x", "-1a2b");
        let build = |extra: &[&str]| {
            let mut args = vec![
                "--crate-type=rlib",
                "--crate-name=x",
                "-Cextra-filename=-1a2b",
                "-Ccodegen-units=2",
                "-Lnative=native",
            ];
            args.extend_from_slice(extra);
            args.push("x.rs");
            run(&args);
            bundles_nothing(&dir.path().join("libx-1a2b.rlib"), &prefix)
        };
        assert_eq!(build(&[]), Some(true));
        assert_eq!(build(&["-Cembed-bitcode=yes"]), Some(true));
        assert_eq!(build(&["-Clinker-plugin-lto"]), Some(true));
        assert_eq!(build(&["-Cembed-bitcode=no"]), Some(true));
        assert_eq!(build(&["-Cincremental=incr"]), Some(true));
        assert_eq!(build(&["-Copt-level=3", "-Ccodegen-units=16"]), Some(true));
        assert_eq!(build(&["-lstatic=inner"]), Some(false));
        assert_eq!(build(&["-lstatic:+whole-archive=inner"]), Some(false));
        assert_eq!(build(&["-lstatic=inner2"]), Some(false));
        // Not bundled: the archive is linked later, by whoever links the program.
        assert_eq!(build(&["-lstatic:-bundle=inner"]), Some(true));
    }
}

//! Whether a library's `.rlib` holds anything but what rustc compiled from the crate itself
//! (B-6 `native folders`).
//!
//! An `.rlib` is an `ar` archive. rustc writes into it the crate's metadata (`lib.rmeta`, which
//! holds rustc's metadata header, as is or in an object file's section) and its own object files, each named
//! `<crate name><extra filename>.<codegen unit>.rcgu.o`, and, for each static library it
//! bundles (`-l static=` or a `#[link(kind = "static")]` in the source), that library's members
//! as they are. A library bundled nothing only when both hold: every member is the one
//! `lib.rmeta` with rustc's metadata in it or an object with the crate's own prefix; and no
//! member is, byte for byte, a member of a static library in the folders rustc searched for
//! native libraries. Names alone prove nothing: a build script can name its object
//! `lib.rmeta`, or read the dependent's extra filename from `target/<profile>/.fingerprint/`
//! before it runs; but rustc copies a bundled member's bytes unchanged. Both `ar` formats rustc
//! writes are read: GNU's (Linux, with long names in the `//` member) and BSD's (Apple, with
//! `#1/<length>` names, the name being the start of the member's data).

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use hushrun_core::Digest;

const MAGIC: &[u8; 8] = b"!<arch>\n";

/// What rustc's crate metadata starts with, before its format version: in `lib.rmeta` as is, or
/// in a section of the object file rustc wraps it in.
const METADATA_HEADER: &[u8] = b"rust\0\0\0";
const HEADER: usize = 60;

/// The prefix of the objects rustc writes for a crate into its `.rlib`: its crate name, its
/// `-C extra-filename` and a dot, then the codegen unit's name
/// (`memchr-caca4131.memchr.7959cbc2-cgu.0.rcgu.o`, or a hashed name when incremental).
pub fn own_prefix(crate_name: &str, extra_filename: &str) -> String {
    format!("{crate_name}{extra_filename}.")
}

/// Calls `each` with every member of the `ar` archive at `path` but its symbol index, by name
/// and bytes, in order. `None` when it isn't an `ar` archive rustc writes or `each` says stop.
fn members(path: &Path, mut each: impl FnMut(&str, &[u8]) -> bool) -> Option<()> {
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
        at = data + size;
        let mut bytes = vec![0u8; usize::try_from(size).ok()?];
        file.read_exact(&mut bytes).ok()?;
        let (name, body): (String, &[u8]) = if let Some(n) = raw.strip_prefix("#1/") {
            // BSD: the name is the first `n` bytes of the data.
            let n: usize = n.parse().ok()?;
            let name = bytes.get(..n)?;
            (
                String::from_utf8_lossy(name)
                    .trim_end_matches('\0')
                    .to_string(),
                &bytes[n..],
            )
        } else if raw == "//" {
            // GNU: the table of long names.
            names = bytes;
            continue;
        } else if let Some(offset) = raw.strip_prefix('/').filter(|o| !o.is_empty()) {
            match offset.parse::<usize>() {
                Ok(offset) => {
                    let rest = names.get(offset..)?;
                    let end = rest.iter().position(|b| *b == b'\n').unwrap_or(rest.len());
                    (
                        String::from_utf8_lossy(&rest[..end])
                            .trim_end_matches('/')
                            .to_string(),
                        &bytes[..],
                    )
                }
                // `/SYM64/` and the like: an index, not a member.
                Err(_) => (raw.to_string(), &bytes[..]),
            }
        } else {
            (raw.trim_end_matches('/').to_string(), &bytes[..])
        };
        if index(&name) {
            continue;
        }
        if !each(&name, body) {
            return None;
        }
    }
    Some(())
}

/// Whether the `.rlib` at `path` holds only rustc's own members for the crate whose objects
/// start with `prefix` ([`own_prefix`]), none of them a member of one of `archives` (the static
/// libraries in the folders rustc searched for native libraries): `Some(true)` when it bundled
/// nothing, `Some(false)` when it holds another member, `None` when it, or one of the archives,
/// can't be read as an `ar` archive (the caller then assumes it bundled something).
pub fn bundles_nothing(path: &Path, prefix: &str, archives: &[PathBuf]) -> Option<bool> {
    let mut theirs: BTreeSet<Digest> = BTreeSet::new();
    for archive in archives {
        members(archive, |_, bytes| {
            if !bytes.is_empty() {
                theirs.insert(Digest::of(bytes));
            }
            true
        })?;
    }
    let mut metadata = 0usize;
    let mut own = true;
    let read = members(path, |name, bytes| {
        let named = if name == "lib.rmeta" {
            metadata += 1;
            metadata == 1 && memchr::memmem::find(bytes, METADATA_HEADER).is_some()
        } else {
            name.strip_prefix(prefix)
                .is_some_and(|unit| unit.ends_with(".rcgu.o"))
        };
        own = named && !theirs.contains(&Digest::of(bytes));
        own
    });
    match (own, read) {
        (false, _) => Some(false),
        (true, Some(())) => Some(true),
        (true, None) => None,
    }
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

    /// A `lib.rmeta` member: rustc's metadata header, its length and the metadata.
    fn rmeta(text: &[u8]) -> Vec<u8> {
        let mut inner = b"rust\0\0\0\x0a".to_vec();
        inner.extend_from_slice(text);
        let mut out = b"rust\0\0\0\x0a".to_vec();
        out.extend_from_slice(&(inner.len() as u64).to_le_bytes());
        out.extend_from_slice(&inner);
        out
    }

    // covers: B-6
    #[test]
    fn an_rlib_with_only_rustcs_own_members_bundled_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let rlib = dir.path().join("libx.rlib");
        let archive = dir.path().join("libinner.a");
        write_gnu(&archive, &[("inner.o", b"C object")]).unwrap();
        let archives = [archive.clone()];
        let prefix = own_prefix("x", "-1a2b");
        let meta = rmeta(b"crate x");
        let own: Vec<(&str, &[u8])> = vec![
            ("lib.rmeta", &meta),
            ("x-1a2b.x.3c4d-cgu.0.rcgu.o", b"obj"),
            ("x-1a2b.x.3c4d-cgu.1.rcgu.o", b"obj1"),
        ];
        write_gnu(&rlib, &own).unwrap();
        assert_eq!(bundles_nothing(&rlib, &prefix, &archives), Some(true));
        let with = |member: &str, bytes: &[u8]| {
            let mut members = own.clone();
            members.push((member, bytes));
            write_gnu(&rlib, &members).unwrap();
            bundles_nothing(&rlib, &prefix, &archives)
        };
        // Bundled by name: a C object, a build script's object named as rustc names its own,
        // another crate's objects (a bundled Rust static library), the bare prefix.
        for member in [
            "25ac62e5b3c53843-curve25519.o",
            "inner.rcgu.o",
            "std-5f49f7b3.std.2c1a-cgu.0.rcgu.o",
            "x-1a2b.rcgu.o",
        ] {
            assert_eq!(with(member, b"bundled"), Some(false), "{member}");
        }
        // Bundled under rustc's own names: a second `lib.rmeta`, and the crate's own prefix
        // (a build script can read the extra filename): found by its bytes.
        assert_eq!(with("lib.rmeta", b"C object"), Some(false));
        assert_eq!(with("lib.rmeta", &rmeta(b"another")), Some(false));
        assert_eq!(with("x-1a2b.inner.rcgu.o", b"C object"), Some(false));
        // An object of its own that no archive holds is rustc's.
        assert_eq!(with("x-1a2b.x.3c4d-cgu.2.rcgu.o", b"obj2"), Some(true));
        // A `lib.rmeta` without rustc's metadata in it is no metadata.
        write_gnu(&rlib, &[("lib.rmeta", b"C object, not metadata")]).unwrap();
        assert_eq!(bundles_nothing(&rlib, &prefix, &archives), Some(false));
        write_gnu(&rlib, &own).unwrap();
        // An archive that can't be read, or an rlib: can't tell.
        let odd = dir.path().join("libodd.a");
        std::fs::write(&odd, b"!<thin>\n").unwrap();
        assert_eq!(bundles_nothing(&rlib, &prefix, &[odd]), None);
        std::fs::write(&rlib, b"not an archive").unwrap();
        assert_eq!(bundles_nothing(&rlib, &prefix, &archives), None);
    }

    // covers: B-6
    #[test]
    fn real_rlibs_from_rustc_say_whether_they_bundled_a_static_library() {
        // What the rule rests on: rustc copies a bundled static library's members as they are.
        // A rustc that rewrote them would fail here rather than reuse a stale library.
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
        std::fs::write(
            dir.path().join("inner.rs"),
            "#[unsafe(no_mangle)] pub extern \"C\" fn inner_value() -> u32 { 1 }\n",
        )
        .unwrap();
        run(&[
            "--crate-type=lib",
            "--emit=obj",
            "-o",
            "inner.o",
            "inner.rs",
        ]);
        let object = std::fs::read(dir.path().join("inner.o")).unwrap();
        let native = dir.path().join("native");
        std::fs::create_dir_all(&native).unwrap();
        // One object under a plain name, one named as the crate's own, one as `lib.rmeta`.
        for (lib, member) in [
            ("inner", "inner.o"),
            ("forged", "x-1a2b.inner.rcgu.o"),
            ("meta", "lib.rmeta"),
        ] {
            write_gnu(&native.join(format!("lib{lib}.a")), &[(member, &object)]).unwrap();
        }
        // A Rust static library: every member an `.rcgu.o` of std, core or the crate.
        run(&[
            "--crate-type=staticlib",
            "--crate-name=inner2",
            "-o",
            "native/libinner2.a",
            "inner.rs",
        ]);
        let archives: Vec<PathBuf> = ["inner", "forged", "meta", "inner2"]
            .iter()
            .map(|lib| native.join(format!("lib{lib}.a")))
            .collect();
        let prefix = own_prefix("x", "-1a2b");
        let build = |search: &str, extra: &[&str]| {
            let mut args = vec![
                "--crate-type=rlib",
                "--crate-name=x",
                "-Cextra-filename=-1a2b",
                "-Ccodegen-units=2",
                search,
            ];
            args.extend_from_slice(extra);
            args.push("x.rs");
            run(&args);
            bundles_nothing(&dir.path().join("libx-1a2b.rlib"), &prefix, &archives)
        };
        // Each kind of folder rustc searches for a static library.
        for search in ["-Lnative=native", "-Lall=native", "-Lnative"] {
            assert_eq!(build(search, &[]), Some(true), "{search}");
            for lib in ["inner", "forged", "meta", "inner2"] {
                let bundle = format!("-lstatic={lib}");
                assert_eq!(build(search, &[&bundle]), Some(false), "{search} {lib}");
            }
            assert_eq!(
                build(search, &["-lstatic:+whole-archive=forged"]),
                Some(false),
                "{search}"
            );
            // Not bundled: the archive is linked later, by whoever links the program.
            assert_eq!(
                build(search, &["-lstatic:-bundle=inner"]),
                Some(true),
                "{search}"
            );
        }
        for flags in [
            &["-Cembed-bitcode=yes"][..],
            &["-Cembed-bitcode=no"],
            &["-Clinker-plugin-lto"],
            &["-Cincremental=incr"],
            &["-Copt-level=3", "-Ccodegen-units=16"],
        ] {
            assert_eq!(build("-Lnative=native", flags), Some(true), "{flags:?}");
        }
    }
}

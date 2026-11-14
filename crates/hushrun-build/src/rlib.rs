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

/// Calls `each` with every member of the `ar` archive at `path` but its symbol index: its
/// name, its size and a way to read its bytes, which only a caller that needs them pays for.
/// `None` when it isn't an `ar` archive rustc writes, a read fails, or `each` says stop.
fn members(
    path: &Path,
    mut each: impl FnMut(&str, u64, &mut dyn FnMut() -> Option<Vec<u8>>) -> bool,
) -> Option<()> {
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
        at = data.checked_add(size)?;
        if at > len {
            return None;
        }
        let read_at = |file: &mut File, from: u64, size: u64| -> Option<Vec<u8>> {
            file.seek(SeekFrom::Start(from)).ok()?;
            let mut bytes = vec![0u8; usize::try_from(size).ok()?];
            file.read_exact(&mut bytes).ok()?;
            Some(bytes)
        };
        let (name, body, body_size) = if let Some(n) = raw.strip_prefix("#1/") {
            // BSD: the name is the first `n` bytes of the data.
            let n: u64 = n.parse().ok()?;
            let name = read_at(&mut file, data, n.min(size))?;
            (
                String::from_utf8_lossy(&name)
                    .trim_end_matches('\0')
                    .to_string(),
                data + n,
                size.checked_sub(n)?,
            )
        } else if raw == "//" {
            // GNU: the table of long names.
            names = read_at(&mut file, data, size)?;
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
                        data,
                        size,
                    )
                }
                // `/SYM64/` and the like: an index, not a member.
                Err(_) => (raw.to_string(), data, size),
            }
        } else {
            (raw.trim_end_matches('/').to_string(), data, size)
        };
        if index(&name) {
            continue;
        }
        let mut bytes = || read_at(&mut file, body, body_size);
        if !each(&name, body_size, &mut bytes) {
            return None;
        }
    }
    Some(())
}

/// Whether the file at `path` is an `ar` archive (`!<arch>`) or a thin one (`!<thin>`), whatever
/// its name: rustc bundles one under any name through `+verbatim`.
pub fn is_archive(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    let mut magic = [0u8; 8];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .is_ok()
        && (&magic == MAGIC || &magic == b"!<thin>\n")
}

/// Every archive ([`is_archive`]) directly in these folders, sorted: what rustc could bundle
/// from the folders it searched for native libraries. A folder that can't be listed adds
/// nothing.
pub fn archives_in(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = dirs
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flat_map(|entries| entries.flatten().map(|entry| entry.path()))
        .filter(|path| is_archive(path))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Whether the `.rlib` at `path` holds only rustc's own members for the crate whose objects
/// start with `prefix` ([`own_prefix`]), none of them a member of one of `archives` (the
/// archives in the folders rustc searched for native libraries): `Some(true)` when it bundled
/// nothing, `Some(false)` when it holds another member, `None` when it, or one of the archives,
/// can't be read as an `ar` archive (the caller then assumes it bundled something).
///
/// Cost: the `.rlib`'s and each archive's member headers, `lib.rmeta`'s bytes, and the bytes of
/// only those members whose size is the size of a member on the other side, which a library
/// that bundled nothing seldom has. A large archive beside a library costs its headers.
pub fn bundles_nothing(path: &Path, prefix: &str, archives: &[PathBuf]) -> Option<bool> {
    // The `.rlib`'s names, and its members' sizes.
    let mut metadata = 0usize;
    let mut named = true;
    let mut sizes: BTreeSet<u64> = BTreeSet::new();
    let read = members(path, |name, size, bytes| {
        named = if name == "lib.rmeta" {
            metadata += 1;
            metadata == 1
                && bytes().is_some_and(|b| memchr::memmem::find(&b, METADATA_HEADER).is_some())
        } else {
            name.strip_prefix(prefix)
                .is_some_and(|unit| unit.ends_with(".rcgu.o"))
        };
        if size > 0 {
            sizes.insert(size);
        }
        named
    });
    match (named, read) {
        (false, _) => return Some(false),
        (true, None) => return None,
        (true, Some(())) => {}
    }
    // The archives' members of a size the `.rlib` has, by their bytes.
    let mut theirs: BTreeSet<(u64, Digest)> = BTreeSet::new();
    for archive in archives {
        members(archive, |_, size, bytes| {
            if !sizes.contains(&size) {
                return true;
            }
            match bytes() {
                Some(b) => {
                    theirs.insert((size, Digest::of(&b)));
                    true
                }
                None => false,
            }
        })?;
    }
    if theirs.is_empty() {
        return Some(true);
    }
    // The `.rlib`'s members of those sizes: one of them among the archives' means it bundled.
    let wanted: BTreeSet<u64> = theirs.iter().map(|(size, _)| *size).collect();
    let mut copied = false;
    let read = members(path, |_, size, bytes| {
        if !wanted.contains(&size) {
            return true;
        }
        match bytes() {
            Some(b) => {
                copied = theirs.contains(&(size, Digest::of(&b)));
                !copied
            }
            None => false,
        }
    });
    match (copied, read) {
        (true, _) => Some(false),
        (false, Some(())) => Some(true),
        (false, None) => None,
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
        // An archive under any name is one; a thin archive can't be read: can't tell.
        let renamed = dir.path().join("inner.archive");
        std::fs::copy(&archive, &renamed).unwrap();
        assert!(is_archive(&renamed) && !is_archive(dir.path()));
        assert_eq!(with("x-1a2b.inner.rcgu.o", b"C object"), Some(false));
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
        // One object under a plain name, one named as the crate's own, one as `lib.rmeta`; and
        // one named as the crate's own in an archive named as no static library is, which
        // `+verbatim` bundles.
        for (file, member) in [
            ("libinner.a", "inner.o"),
            ("libforged.a", "x-1a2b.inner.rcgu.o"),
            ("libmeta.a", "lib.rmeta"),
            ("inner.archive", "x-1a2b.inner.rcgu.o"),
        ] {
            write_gnu(&native.join(file), &[(member, &object)]).unwrap();
        }
        // A Rust static library: every member an `.rcgu.o` of std, core or the crate.
        run(&[
            "--crate-type=staticlib",
            "--crate-name=inner2",
            "-o",
            "native/libinner2.a",
            "inner.rs",
        ]);
        std::fs::write(native.join("notes.txt"), b"not an archive").unwrap();
        let archives = archives_in(std::slice::from_ref(&native));
        assert_eq!(archives.len(), 5, "{archives:?}");
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
            assert_eq!(
                build(search, &["-lstatic:+verbatim=inner.archive"]),
                Some(false),
                "{search} +verbatim"
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

//! B-65, B-66: a baked store as an OCI image, in the forms another program must match byte for
//! byte: the layers' tar bytes, and the tags that say which image is whose.
//!
//! **Layer bytes** (B-65) are a pure function of an ordered list of files, each a path and its
//! bytes: [`chunks`] says, for that list, every byte of the layer as header blocks, the files'
//! own bytes in turn and zero padding, so that a backend can stream the same layer from its
//! object store, file by file, and get the same digest the CLI pushed. Nothing else goes in: no
//! timestamps, owners or extended headers that depend on the machine, and no compression (the
//! packs are compressed and sealed already, and a compressor's version would change the digest).
//!
//! The layout, which [`header`] writes, is POSIX ustar (`ustar\0`, version `00`), one 512-byte
//! header per entry, sorted by the entry's path as bytes:
//!
//! | Field | Value |
//! |---|---|
//! | entries | every file given, and before them every folder above one (`hushrun/`, `hushrun/packs/` …), each once; sorted by path as bytes, a folder's path ending in `/` |
//! | `name`, `prefix` | the path; when longer than 100 bytes, split at the first `/` (from the left) that leaves `name` at most 100 bytes and `prefix` at most 155: the shortest `prefix` that fits, as GNU tar splits |
//! | `mode` | `0000644` for a file, `0000755` for a folder, then NUL |
//! | `uid`, `gid` | `0000000`, then NUL |
//! | `size` | the file's size in 11 octal digits, then NUL; `00000000000` for a folder |
//! | `mtime` | `00000000000`, then NUL |
//! | `chksum` | the unsigned sum of the header's bytes with this field as 8 spaces, in 6 octal digits, then NUL and a space |
//! | `typeflag` | `0` for a file, `5` for a folder |
//! | `linkname`, `uname`, `gname`, `devmajor`, `devminor` | all NUL |
//! | `magic`, `version` | `ustar\0`, `00` |
//!
//! Each file's bytes follow its header, padded with NULs to a multiple of 512; the layer ends with
//! two 512-byte blocks of NULs. A file of 8 GiB or more, a path that isn't printable ASCII, is
//! absolute, holds `.` or `..` as a part, or can't be split as above is refused.
//!
//! **Tags** (B-66) are the change signal: [`fixed_tag`] names one store, [`latest_tag`] moves to
//! the newest, and [`branch_tag`] moves to a branch's newest.
//!
//! Like the rest of this crate it does no IO.

use sha2_hw::{Digest as _, Sha256};

/// The OCI media type of a layer: an uncompressed tar.
pub const LAYER_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar";

/// The folder at the image's root that holds the baked store: `/hushrun`.
pub const STORE_ROOT: &str = "hushrun";

/// The prefix of every tag hushrun gives an image.
pub const TAG_PREFIX: &str = "hushrun-";

/// How many hex characters of a store's manifest a fixed tag holds.
pub const SHORT_MANIFEST: usize = 16;

/// The most bytes one file of a layer may have: ustar's 11 octal digits.
pub const MOST_FILE: u64 = (1 << 33) - 1;

/// One block of a tar.
pub const BLOCK: u64 = 512;

/// A file of a layer: its path from the image's root (`hushrun/packs/<address>.pack`) and size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// The path, `/`-separated, relative.
    pub path: String,
    /// Its size in bytes.
    pub size: u64,
}

/// A piece of a layer's bytes, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chunk {
    /// These bytes: a header.
    Header(Box<[u8; 512]>),
    /// The bytes of the file at this index of the list given, all of them.
    File(usize),
    /// This many NULs: a file's padding, or the end.
    Zeros(u64),
}

/// Why a list of files can't be a layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// Whether `path` may be an entry's path: relative, printable ASCII, no empty, `.` or `..` part.
pub(crate) fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path.bytes().all(|b| (0x21..=0x7e).contains(&b))
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// The header of one entry: a file of `size` bytes at `path`, or a folder (`size` none) at
/// `path`, written without its trailing `/`.
pub fn header(path: &str, size: Option<u64>) -> Result<[u8; 512], Refused> {
    if !valid_path(path) {
        return Err(Refused(format!("{path:?} can't be a layer's path")));
    }
    let name = match size {
        Some(_) => path.to_string(),
        None => format!("{path}/"),
    };
    let (prefix, base) =
        split(&name).ok_or_else(|| Refused(format!("{path:?} is too long for a ustar header")))?;
    let mut h = [0u8; 512];
    h[..base.len()].copy_from_slice(base.as_bytes());
    let octal = |h: &mut [u8; 512], at: usize, width: usize, value: u64| {
        let text = format!("{value:0w$o}", w = width - 1);
        h[at..at + width - 1].copy_from_slice(text.as_bytes());
        h[at + width - 1] = 0;
    };
    let size_value = match size {
        Some(size) if size > MOST_FILE => {
            return Err(Refused(format!(
                "{path} has {size} bytes, more than a ustar header can say"
            )));
        }
        Some(size) => size,
        None => 0,
    };
    octal(&mut h, 100, 8, if size.is_some() { 0o644 } else { 0o755 });
    octal(&mut h, 108, 8, 0);
    octal(&mut h, 116, 8, 0);
    octal(&mut h, 124, 12, size_value);
    octal(&mut h, 136, 12, 0);
    h[148..156].copy_from_slice(b"        ");
    h[156] = if size.is_some() { b'0' } else { b'5' };
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    let text = format!("{sum:06o}");
    h[148..154].copy_from_slice(text.as_bytes());
    h[154] = 0;
    h[155] = b' ';
    Ok(h)
}

/// A name's ustar `prefix` and `name`: whole when it fits in 100 bytes, else split at the first
/// `/` from the left that leaves at most 100 bytes after it and at most 155 before it, as GNU
/// tar does (a folder's trailing `/` stays in `name`).
fn split(name: &str) -> Option<(&str, &str)> {
    if name.len() <= 100 {
        return Some(("", name));
    }
    let body = name.strip_suffix('/').unwrap_or(name);
    let mut best = None;
    for (at, byte) in body.bytes().enumerate() {
        if byte == b'/' && name.len() - at - 1 <= 100 && at <= 155 {
            best = Some(at);
            break;
        }
    }
    let at = best?;
    Some((&name[..at], &name[at + 1..]))
}

/// The entries of a layer of `files`, in its order: each folder above a file once, and each file,
/// by path as bytes (a folder's with its trailing `/`). `Err` on a path given twice, or a path
/// that is also a folder above another.
fn entries(files: &[File]) -> Result<Vec<(String, Option<usize>)>, Refused> {
    let mut all: Vec<(String, Option<usize>)> = Vec::new();
    let mut folders = std::collections::BTreeSet::new();
    for (i, file) in files.iter().enumerate() {
        if !valid_path(&file.path) {
            return Err(Refused(format!("{:?} can't be a layer's path", file.path)));
        }
        let mut at = 0;
        while let Some(slash) = file.path[at..].find('/') {
            folders.insert(file.path[..at + slash + 1].to_string());
            at += slash + 1;
        }
        all.push((file.path.clone(), Some(i)));
    }
    for folder in folders {
        all.push((folder, None));
    }
    // A path given twice, or a file's path that is also a folder above another file, whatever
    // sorts between them (`a`, `a-x`, `a/b`).
    let mut names = std::collections::HashSet::new();
    for (path, _) in &all {
        if !names.insert(path.trim_end_matches('/').to_string()) {
            return Err(Refused(format!(
                "{} is in the layer twice, or both a file and a folder",
                path.trim_end_matches('/')
            )));
        }
    }
    all.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    Ok(all)
}

/// B-65: every byte of the layer of `files`, in order. The same list always gives the same bytes.
pub fn chunks(files: &[File]) -> Result<Vec<Chunk>, Refused> {
    let mut out = Vec::new();
    for (path, file) in entries(files)? {
        match file {
            Some(i) => {
                let size = files[i].size;
                out.push(Chunk::Header(Box::new(header(&path, Some(size))?)));
                if size > 0 {
                    out.push(Chunk::File(i));
                }
                let pad = (BLOCK - size % BLOCK) % BLOCK;
                if pad > 0 {
                    out.push(Chunk::Zeros(pad));
                }
            }
            None => {
                let name = path.trim_end_matches('/');
                out.push(Chunk::Header(Box::new(header(name, None)?)));
            }
        }
    }
    out.push(Chunk::Zeros(2 * BLOCK));
    Ok(out)
}

/// The size of the layer of `files`.
pub fn size(files: &[File]) -> Result<u64, Refused> {
    Ok(chunks(files)?
        .iter()
        .map(|chunk| match chunk {
            Chunk::Header(_) => BLOCK,
            Chunk::File(i) => files[*i].size,
            Chunk::Zeros(n) => *n,
        })
        .sum())
}

/// The layer of `files` written whole into memory, with `bytes(i)` the bytes of file `i`, and its
/// SHA-256. `Err` when a file's bytes aren't its size.
pub fn layer(
    files: &[File],
    bytes: &dyn Fn(usize) -> Vec<u8>,
) -> Result<(Vec<u8>, [u8; 32]), Refused> {
    let mut out = Vec::new();
    for chunk in chunks(files)? {
        match chunk {
            Chunk::Header(h) => out.extend_from_slice(&h[..]),
            Chunk::File(i) => {
                let data = bytes(i);
                if data.len() as u64 != files[i].size {
                    return Err(Refused(format!(
                        "{} has {} bytes, not the {} its entry says",
                        files[i].path,
                        data.len(),
                        files[i].size
                    )));
                }
                out.extend_from_slice(&data);
            }
            Chunk::Zeros(n) => out.resize(out.len() + n as usize, 0),
        }
    }
    let digest: [u8; 32] = Sha256::digest(&out).into();
    Ok((out, digest))
}

/// The short form of a store's manifest identity a fixed tag holds: its first
/// [`SHORT_MANIFEST`] hex characters.
pub fn short_manifest(manifest_hex: &str) -> &str {
    &manifest_hex[..manifest_hex.len().min(SHORT_MANIFEST)]
}

/// Whether `platform` may stand in a tag: a platform's words (`linux-x64`), or a target triple,
/// as `baked::platform_of` gives it, in lowercase letters, digits, `_` and `-`.
pub fn valid_platform(platform: &str) -> bool {
    !platform.is_empty()
        && platform.len() <= 64
        && platform
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        && !platform.starts_with('-')
}

/// B-66: the fixed tag of the store with manifest `manifest_hex` for `platform`:
/// `hushrun-<platform>-<first 16 hex of the manifest>`.
pub fn fixed_tag(platform: &str, manifest_hex: &str) -> String {
    format!("{TAG_PREFIX}{platform}-{}", short_manifest(manifest_hex))
}

/// B-66: the moving tag of the newest store of the default branch for `platform`:
/// `hushrun-<platform>-latest`.
pub fn latest_tag(platform: &str) -> String {
    format!("{TAG_PREFIX}{platform}-latest")
}

/// B-66: the moving tag of the newest store of `branch` for `platform`:
/// `hushrun-<platform>-branch-<name>-latest`, `<name>` the branch as [`branch_name`] gives it.
pub fn branch_tag(platform: &str, branch: &str) -> String {
    let room = 128 - TAG_PREFIX.len() - platform.len() - "-branch-".len() - "-latest".len();
    format!(
        "{TAG_PREFIX}{platform}-branch-{}-latest",
        branch_name(branch, room)
    )
}

/// B-66: a branch's name as a tag holds it, in at most `room` characters: `refs/heads/` dropped;
/// every run of characters other than `a-z`, `0-9`, `.` and `_` (an uppercase letter among them)
/// one `-`, lowercased; `-` and `.` trimmed from both ends. A name that comes out as it was given
/// and fits is itself. Any other is cut to leave room and ends with `--` and the first 16 hex
/// characters of the SHA-256 of the branch as given (after `refs/heads/`). A name kept as it was
/// never holds `--` (a run of characters becomes one `-`), so it is never another's cut form; two
/// cut forms are one only where 64 bits of SHA-256 are.
pub fn branch_name(branch: &str, room: usize) -> String {
    let given = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    let mut out = String::new();
    let mut dash = false;
    for c in given.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' {
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    let trimmed = out.trim_matches(|c| c == '-' || c == '.').to_string();
    if trimmed == given && !trimmed.is_empty() && trimmed.len() <= room {
        return trimmed;
    }
    let digest = Sha256::digest(given.as_bytes());
    let suffix: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    let keep = room.saturating_sub(suffix.len() + 2);
    let cut: String = trimmed.chars().take(keep).collect();
    let cut = cut.trim_end_matches(['-', '.']);
    format!("{cut}--{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64) -> File {
        File {
            path: path.into(),
            size,
        }
    }

    // covers: B-65
    #[test]
    fn a_layer_is_sorted_folders_first_and_ends_with_two_zero_blocks() {
        let a = "a".repeat(64);
        let files = vec![
            file("hushrun/baked.json", 3),
            file(&format!("hushrun/packs/{a}.pack"), 600),
        ];
        let chunks = chunks(&files).unwrap();
        let names: Vec<String> = chunks
            .iter()
            .filter_map(|c| match c {
                Chunk::Header(h) => {
                    let end = h.iter().position(|&b| b == 0).unwrap_or(100);
                    Some(String::from_utf8_lossy(&h[..end]).into_owned())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            [
                "hushrun/".to_string(),
                "hushrun/baked.json".into(),
                "hushrun/packs/".into(),
                format!("hushrun/packs/{a}.pack"),
            ]
        );
        assert_eq!(chunks.last(), Some(&Chunk::Zeros(1024)));
        // 4 headers, 3 bytes padded to 512, 600 padded to 1024, the end.
        assert_eq!(size(&files).unwrap(), 4 * 512 + 512 + 1024 + 1024);
    }

    // covers: B-65
    #[test]
    fn the_same_files_give_the_same_bytes_and_any_change_another_digest() {
        let files = vec![file("hushrun/x", 2), file("hushrun/y", 1)];
        let one = layer(&files, &|i| vec![i as u8 + 1; files[i].size as usize]).unwrap();
        let again = layer(&files, &|i| vec![i as u8 + 1; files[i].size as usize]).unwrap();
        assert_eq!(one, again);
        let other = layer(&files, &|i| vec![i as u8 + 7; files[i].size as usize]).unwrap();
        assert_ne!(one.1, other.1);
        // Given in another order, the same layer.
        let reversed = vec![files[1].clone(), files[0].clone()];
        let back = layer(&reversed, &|i| vec![2 - i as u8; reversed[i].size as usize]).unwrap();
        assert_eq!(one, back);
        // A file whose bytes aren't its size is refused.
        assert!(layer(&files, &|_| vec![0]).is_err());
    }

    // covers: B-65
    #[test]
    fn the_layout_is_the_one_documented() {
        // A known header, byte for byte: what another implementation must match.
        let h = header("hushrun/baked.json", Some(1234)).unwrap();
        assert_eq!(&h[..18], b"hushrun/baked.json");
        assert_eq!(&h[100..108], b"0000644\0");
        assert_eq!(&h[108..116], b"0000000\0");
        assert_eq!(&h[116..124], b"0000000\0");
        assert_eq!(&h[124..136], b"00000002322\0");
        assert_eq!(&h[136..148], b"00000000000\0");
        assert_eq!(h[156], b'0');
        assert_eq!(&h[257..265], b"ustar\x0000");
        let mut copy = h;
        copy[148..156].copy_from_slice(b"        ");
        let sum: u32 = copy.iter().map(|&b| u32::from(b)).sum();
        assert_eq!(&h[148..156], format!("{sum:06o}\0 ").as_bytes());
        let d = header("hushrun/packs", None).unwrap();
        assert_eq!(&d[..14], b"hushrun/packs/");
        assert_eq!(&d[100..108], b"0000755\0");
        assert_eq!(d[156], b'5');
        // A digest pinned, so that a change to the layout is a change to this test.
        let files = vec![file("hushrun/baked.json", 2)];
        let (_, digest) = layer(&files, &|_| b"{}".to_vec()).unwrap();
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "3e9199c0dac5ddbc268e2400c7aecd50da7d664003bce758b751774e887a6da2"
        );
        // And one with a hint's long path, cross-checked with an implementation of the table
        // above written apart from this one, and read back by tar.
        let (a, b) = ("a".repeat(64), "b".repeat(64));
        let files = vec![
            file("hushrun/baked.json", 2),
            file(&format!("hushrun/hints/{a}/{b}"), 10),
            file(&format!("hushrun/packs/{a}.pack"), 600),
        ];
        let data = [b"{}".to_vec(), vec![b'x'; 10], vec![b'y'; 600]];
        let (_, digest) = layer(&files, &|i| data[i].clone()).unwrap();
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "4670ffa9fb012ee9bf46866c94c0b207ce9ac431643138230e669c884f8c8125"
        );
    }

    // covers: B-65
    #[test]
    fn a_hint_s_long_path_is_split_and_bad_paths_are_refused() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let path = format!("hushrun/hints/{a}/{b}");
        assert!(path.len() > 100);
        let h = header(&path, Some(10)).unwrap();
        assert_eq!(&h[..64], b.as_bytes());
        assert_eq!(h[64], 0);
        assert_eq!(&h[345..345 + 78], format!("hushrun/hints/{a}").as_bytes());
        for bad in ["/abs", "a/../b", "a//b", "a/./b", "", "a b", "é"] {
            assert!(header(bad, Some(1)).is_err(), "{bad:?}");
        }
        assert!(header(&"x".repeat(101), Some(1)).is_err());
        assert!(chunks(&[file("a/b", 1), file("a/b", 1)]).is_err());
        assert!(chunks(&[file("a", 1), file("a/b", 1)]).is_err());
        assert!(header("a", Some(MOST_FILE + 1)).is_err());
    }

    // covers: B-66
    #[test]
    fn tags_name_the_platform_and_the_store() {
        let m = "0123456789abcdef".repeat(4);
        assert_eq!(
            fixed_tag("linux-x64", &m),
            "hushrun-linux-x64-0123456789abcdef"
        );
        assert_eq!(latest_tag("linux-arm64"), "hushrun-linux-arm64-latest");
        assert_eq!(
            branch_tag("linux-x64", "refactor"),
            "hushrun-linux-x64-branch-refactor-latest"
        );
        assert_eq!(
            branch_tag("linux-x64", "refs/heads/release-1.2"),
            "hushrun-linux-x64-branch-release-1.2-latest"
        );
        assert!(valid_platform("linux-x64"));
        assert!(valid_platform("riscv64gc-unknown-linux-gnu"));
        assert!(!valid_platform("Linux"));
    }

    // covers: B-66
    #[test]
    fn branch_names_are_sanitised_and_never_collide() {
        let a = branch_name("feature/x", 100);
        let b = branch_name("feature-x", 100);
        assert_eq!(b, "feature-x");
        assert_ne!(a, b);
        assert!(a.starts_with("feature-x--"), "{a}");
        assert_eq!(a.len(), "feature-x--".len() + 16);
        // Case is a change: the digest tells `Main` from `main`.
        assert!(branch_name("Main", 100).starts_with("main--"));
        // Too long: cut, with the digest.
        let long = "a".repeat(200);
        let cut = branch_name(&long, 50);
        assert_eq!(cut.len(), 50);
        // Nothing left: the digest alone.
        assert_eq!(branch_name("///", 50).len(), 18);
        // Every tag fits OCI's rule.
        let tag = branch_tag("riscv64gc-unknown-linux-gnu", &"ü/".repeat(100));
        assert!(tag.len() <= 128, "{tag}");
        assert!(
            tag.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        );
    }

    // covers: B-66
    #[test]
    fn no_two_branches_share_a_tag_whatever_their_names() {
        // Case is a change: `Main` and `main` are two branches.
        assert_ne!(
            branch_tag("linux-x64", "Main"),
            branch_tag("linux-x64", "main")
        );
        // A branch named as another's tag word, digest and all, is another branch.
        let room = 128 - TAG_PREFIX.len() - "linux-x64".len() - "-branch-".len() - "-latest".len();
        let sanitised = branch_name("feature/x", room);
        assert_ne!(
            branch_tag("linux-x64", "feature/x"),
            branch_tag("linux-x64", &sanitised)
        );
        // A name of 16 hex characters is not the digest of an empty one.
        let empty = branch_name("///", 50);
        assert_ne!(branch_name(&empty, 50), empty);
    }

    // covers: B-65
    #[test]
    fn a_file_and_a_folder_of_one_name_are_refused_wherever_they_sort() {
        assert!(chunks(&[file("a", 1), file("a-x", 1), file("a/b", 1)]).is_err());
        assert!(chunks(&[file("a/b", 1), file("a-x", 1), file("a/b", 1)]).is_err());
    }

    // covers: B-65
    #[test]
    fn a_long_path_is_split_at_the_first_slash_that_fits_as_gnu_tar_does() {
        let path = format!("{}/{}/{}", "a".repeat(50), "b".repeat(50), "c".repeat(40));
        let h = header(&path, Some(1)).unwrap();
        let name_end = h[..100].iter().position(|&b| b == 0).unwrap_or(100);
        let name = std::str::from_utf8(&h[..name_end]).unwrap();
        assert_eq!(name, format!("{}/{}", "b".repeat(50), "c".repeat(40)));
        let prefix_end = h[345..500].iter().position(|&b| b == 0).unwrap_or(155);
        assert_eq!(&h[345..345 + prefix_end], "a".repeat(50).as_bytes());
    }
}

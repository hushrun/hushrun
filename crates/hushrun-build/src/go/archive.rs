//! What a body Go puts is, judged from its bytes alone (B-42): a package archive whose code is
//! all Go's own, made for the action id it is put under, or something that stays on the machine.
//!
//! Go checks only the size of what a cache program returns, and keys every dependent by the
//! content id an import's build ID *claims* (`buildActionID` hashes `contentID(a1.buildID)`,
//! never the bytes). So everything about an archive's integrity is decided here, from the bytes,
//! before it is stored and again before it is served:
//!
//! - it is an `ar` archive whose first member is `__.PKGDEF`, and every other member is a Go
//!   object: `_go_.o`, or an object the assembler made for a package of the standard library
//!   (its header's flags `ObjFlagFromAssembly` and `ObjFlagStd`). Any other member (a cgo
//!   object, a `.syso`, assembly of another package, a text member) keeps the archive here;
//! - its build ID, read exactly where Go's `buildid.ReadFile` reads it, is `<action>/<content>`
//!   with `<action>` Go's encoding of the action id asked;
//! - `<content>` is what Go's `updateBuildID` would compute from these bytes: the SHA-256 of the
//!   archive with every occurrence of the build ID zeroed, as `HashToString` writes it.

use serde::{Deserialize, Serialize};

/// What an `ar` archive starts with.
pub const AR_MAGIC: &[u8] = b"!<arch>\n";
/// The magic of a Go object file's binary header (`cmd/internal/goobj`, `Magic`).
const GOOBJ_MAGIC: &[u8] = b"\x00go120ld";
/// What every Go object member's text header starts with.
const GO_OBJECT: &[u8] = b"go object ";
/// `ObjFlagFromAssembly`: the object is from assembly source, not Go.
const FROM_ASSEMBLY: u32 = 1 << 2;
/// `ObjFlagStd`: the object is of a standard library package.
const STD: u32 = 1 << 4;
/// The length of each half of a package archive's build ID (`HashToString`'s 20 characters).
const ID_PART: usize = 20;

/// Why a body put by Go stays on this machine (B-42, B-41). Stable words: the run's log counts
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Local {
    /// Not a package archive: the package index, a test result, compiler output, cgo's files,
    /// vet's facts. Go's own machine-local shortcuts, keyed by paths, sizes and dates.
    NotArchive,
    /// An archive that can't be read as `ar` with `__.PKGDEF` first.
    Malformed,
    /// A member that is no Go object: cgo's objects, a `.syso`, a text member.
    Native,
    /// A member the assembler made for a package outside the standard library: it may include
    /// a header from outside its package, which Go's id doesn't hold (F1).
    Assembly,
    /// The build ID can't be read where Go reads it, or doesn't name the action id asked.
    BuildId,
    /// The content id in the build ID is not that of the bytes (F2): never stored or served.
    ContentId,
    /// The compile printed something, which Go would not print again from another machine's
    /// archive (F4).
    Printed,
    /// The `go` command was altered, or its arguments couldn't be read (B-42):
    /// [`crate::go::command::Altered`] says how.
    Altered,
    /// hushrun's identity of the toolchain couldn't be computed (B-43).
    Toolchain,
    /// The run doesn't share: nothing is kept beside Go's own cache.
    NotSharing,
}

impl Local {
    /// The reason, as a line ends with it.
    pub fn words(self) -> &'static str {
        match self {
            Local::NotArchive => "not a package archive",
            Local::Malformed => "an archive that can't be read",
            Local::Native => "native code (cgo or .syso)",
            Local::Assembly => "assembly outside the standard library",
            Local::BuildId => "a build ID that doesn't name its action",
            Local::ContentId => "a content id that isn't its bytes'",
            Local::Printed => "the compile printed something",
            Local::Altered => {
                "the go command was altered (-toolexec, -overlay, flags naming files)"
            }
            Local::Toolchain => "the toolchain couldn't be identified",
            Local::NotSharing => "the run doesn't share",
        }
    }
}

/// Go's `HashToString`: the first 15 bytes of a hash in Go's base64 alphabet, 20 characters.
pub fn hash_to_string(hash: &[u8; 32]) -> String {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(ID_PART);
    for chunk in hash[..15].chunks(3) {
        let v = u32::from(chunk[0]) << 16 | u32::from(chunk[1]) << 8 | u32::from(chunk[2]);
        for shift in [18, 12, 6, 0] {
            out.push(B64[((v >> shift) & 0x3f) as usize] as char);
        }
    }
    out
}

/// The build ID of an archive, read as Go's `buildid.ReadFile` reads it: in the first 1024
/// bytes, line 0 is `!<arch>`, line 1 starts with `__.PKGDEF`, line 2 with `go object `, and
/// line 3 is `build id ` and a quoted string. `None` where Go would read none or fall back to
/// gccgo's reader, which a package archive of the gc toolchain never needs.
pub fn build_id(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(1024)];
    let mut lines = head.split(|b| *b == b'\n');
    if lines.next()? != b"!<arch>" {
        return None;
    }
    if !lines.next()?.starts_with(b"__.PKGDEF") {
        return None;
    }
    if !lines.next()?.starts_with(GO_OBJECT) {
        return None;
    }
    // Line 3 must be followed by a newline inside the first 1024 bytes, as Go's loop needs.
    let line = lines.next()?;
    lines.next()?;
    let quoted = line.strip_prefix(b"build id ")?;
    unquote(quoted)
}

/// A Go string literal with no escapes, which is how a build ID is written (Go's
/// `strconv.Quote` of base64 and `/`): anything else is not read.
fn unquote(quoted: &[u8]) -> Option<String> {
    let inner = quoted.strip_prefix(b"\"")?.strip_suffix(b"\"")?;
    if inner
        .iter()
        .any(|b| matches!(b, b'"' | b'\\') || !b.is_ascii_graphic())
    {
        return None;
    }
    String::from_utf8(inner.to_vec()).ok()
}

/// The content id Go computes for `bytes` whose build ID is `id` (`buildid.FindAndHash`, then
/// `HashToString`): the SHA-256 of the bytes with every occurrence of `id` replaced by zeros.
/// An archive has no Mach-O signature and no ELF note for Go to leave out.
pub fn content_id(bytes: &[u8], id: &str) -> String {
    let mut hasher = hushrun_digest::hash::Hasher::new();
    let needle = id.as_bytes();
    let zeros = vec![0u8; needle.len()];
    let mut start = 0;
    if !needle.is_empty() {
        for at in memchr::memmem::find_iter(bytes, needle) {
            if at < start {
                continue;
            }
            hasher.update(&bytes[start..at]);
            hasher.update(&zeros);
            start = at + needle.len();
        }
    }
    hasher.update(&bytes[start..]);
    hash_to_string(&hasher.finish())
}

/// One member of an `ar` archive.
struct Member<'a> {
    name: &'a [u8],
    data: &'a [u8],
}

/// The members of an `ar` archive, or `None` when it can't be read whole: every header is 60
/// bytes ending with `` `\n ``, every size a decimal number, every member inside the file.
fn members(bytes: &[u8]) -> Option<Vec<Member<'_>>> {
    let mut at = AR_MAGIC.len();
    if !bytes.starts_with(AR_MAGIC) {
        return None;
    }
    let mut out = Vec::new();
    while at < bytes.len() {
        let header = bytes.get(at..at + 60)?;
        if &header[58..60] != b"`\n" {
            return None;
        }
        let size: usize = std::str::from_utf8(&header[48..58])
            .ok()?
            .trim()
            .parse()
            .ok()?;
        let start = at + 60;
        let data = bytes.get(start..start.checked_add(size)?)?;
        let name = trim_name(&header[..16]);
        out.push(Member { name, data });
        at = start + size + (size & 1);
    }
    Some(out)
}

/// A member's name without the spaces `ar` pads it with, nor a trailing `/`.
fn trim_name(raw: &[u8]) -> &[u8] {
    let mut end = raw.len();
    while end > 0 && raw[end - 1] == b' ' {
        end -= 1;
    }
    let name = &raw[..end];
    name.strip_suffix(b"/").unwrap_or(name)
}

/// The flags of a Go object member's binary header, or `None` when it is no Go object: its
/// text header starts with `go object `, ends with the first line that is `!`, as Go's reader
/// reads header lines until it, however long the header is (on macOS `runtime`'s carries its
/// `cgo_import_dynamic` lines, past 4 KiB), and the binary header that follows starts with Go's
/// magic, then an 8-byte fingerprint and the flags, little-endian.
fn object_flags(data: &[u8]) -> Option<u32> {
    if !data.starts_with(GO_OBJECT) {
        return None;
    }
    let end = memchr::memmem::find(data, b"\n!\n")? + 3;
    let header = data.get(end..end + GOOBJ_MAGIC.len() + 8 + 4)?;
    if !header.starts_with(GOOBJ_MAGIC) {
        return None;
    }
    let flags = &header[GOOBJ_MAGIC.len() + 8..];
    Some(u32::from_le_bytes([flags[0], flags[1], flags[2], flags[3]]))
}

/// Whether `bytes` start as an archive does: Go's other entries never do.
pub fn is_archive(bytes: &[u8]) -> bool {
    bytes.starts_with(AR_MAGIC)
}

/// Whether `bytes`, put or served under `action`, may be a block that leaves the machine
/// (B-42): a package archive of Go's own code, made for that action, whose content id is its
/// bytes'. The first reason it may not otherwise.
pub fn portable(bytes: &[u8], action: &[u8; 32]) -> Result<(), Local> {
    if !is_archive(bytes) {
        return Err(Local::NotArchive);
    }
    let members = members(bytes).ok_or(Local::Malformed)?;
    let Some((first, rest)) = members.split_first() else {
        return Err(Local::Malformed);
    };
    if first.name != b"__.PKGDEF" {
        return Err(Local::Malformed);
    }
    for member in rest {
        let flags = object_flags(member.data).ok_or(Local::Native)?;
        let assembly = flags & FROM_ASSEMBLY != 0;
        if member.name == b"_go_.o" && !assembly {
            continue;
        }
        if !(assembly && flags & STD != 0) {
            return Err(if assembly {
                Local::Assembly
            } else {
                Local::Native
            });
        }
    }
    let id = build_id(bytes).ok_or(Local::BuildId)?;
    let (action_part, content_part) = id.split_once('/').ok_or(Local::BuildId)?;
    if content_part.contains('/')
        || action_part.len() != ID_PART
        || content_part.len() != ID_PART
        || action_part != hash_to_string(action)
    {
        return Err(Local::BuildId);
    }
    if content_id(bytes, &id) != content_part {
        return Err(Local::ContentId);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An `ar` member: its header and its bytes, padded to an even length.
    fn member(name: &str, data: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            0,
            0,
            0,
            644,
            data.len()
        )
        .into_bytes();
        assert_eq!(out.len(), 60);
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(b'\n');
        }
        out
    }

    /// A Go object member's bytes: its text header, `!`, and a binary header with `flags`.
    fn object(build_id: Option<&str>, flags: u32, body: &[u8]) -> Vec<u8> {
        let mut out = b"go object darwin arm64 go1.27.4 X:none\n".to_vec();
        if let Some(id) = build_id {
            out.extend_from_slice(format!("build id \"{id}\"\n").as_bytes());
        }
        out.extend_from_slice(b"\n!\n");
        out.extend_from_slice(GOOBJ_MAGIC);
        out.extend_from_slice(&[7u8; 8]);
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    /// A package archive for `action`, as the compiler and `go` write one: its build ID's
    /// content half computed from its bytes. `extra` are more members, by name and bytes.
    pub(crate) fn archive(action: &[u8; 32], body: &[u8], extra: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let placeholder = format!("{}/{}", hash_to_string(action), "A".repeat(ID_PART));
        let pkgdef = {
            let mut text = format!(
                "go object darwin arm64 go1.27.4 X:none\nbuild id \"{placeholder}\"\n\n$$B\n"
            )
            .into_bytes();
            text.extend_from_slice(body);
            text
        };
        let mut out = AR_MAGIC.to_vec();
        out.extend(member("__.PKGDEF", &pkgdef));
        out.extend(member("_go_.o", &object(Some(&placeholder), STD, body)));
        for (name, data) in extra {
            out.extend(member(name, data));
        }
        let content = content_id(&out, &placeholder);
        let id = format!("{}/{content}", hash_to_string(action));
        replace_all(&mut out, placeholder.as_bytes(), id.as_bytes());
        out
    }

    fn replace_all(bytes: &mut [u8], from: &[u8], to: &[u8]) {
        let at: Vec<usize> = memchr::memmem::find_iter(bytes, from).collect();
        for i in at {
            bytes[i..i + to.len()].copy_from_slice(to);
        }
    }

    // covers: B-42
    #[test]
    fn go_encodes_hashes_as_its_own_hash_to_string_does() {
        // The 20 characters `go tool buildid` printed for action id 7bd1b65d…, the design's
        // proof (p7-design §1): the first 15 bytes in Go's alphabet.
        let mut action = [0u8; 32];
        action[..4].copy_from_slice(&[0x7b, 0xd1, 0xb6, 0x5d]);
        assert!(hash_to_string(&action).starts_with("e9G2X"));
        assert_eq!(hash_to_string(&[0xff; 32]), "____________________");
        assert_eq!(hash_to_string(&[0; 32]), "AAAAAAAAAAAAAAAAAAAA");
    }

    // covers: B-42
    #[test]
    fn a_pure_go_archive_made_for_its_action_may_leave() {
        let action = [3u8; 32];
        let bytes = archive(&action, b"export data and code", &[]);
        assert_eq!(portable(&bytes, &action), Ok(()));
        // The standard library's assembly travels with it.
        let std_asm = archive(
            &action,
            b"bytealg",
            &[("index_amd64.o", object(None, FROM_ASSEMBLY | STD, b"asm"))],
        );
        assert_eq!(portable(&std_asm, &action), Ok(()));
    }

    // covers: B-42
    #[test]
    fn a_go_object_with_a_long_text_header_is_still_go_s() {
        // macOS `runtime`'s `_go_.o`: its `cgo_import_dynamic` lines put `!` past 4 KiB.
        let action = [14u8; 32];
        let mut long = b"go object darwin arm64 go1.27.4 X:none\n".to_vec();
        for i in 0..200 {
            long.extend_from_slice(
                format!("cgo_import_dynamic libc_f{i} f{i} \"/usr/lib/libSystem.B.dylib\"\n")
                    .as_bytes(),
            );
        }
        long.extend_from_slice(b"\n!\n");
        long.extend_from_slice(GOOBJ_MAGIC);
        long.extend_from_slice(&[7u8; 8]);
        long.extend_from_slice(&STD.to_le_bytes());
        assert!(long.len() > 8192);
        assert_eq!(object_flags(&long), Some(STD));
        let bytes = archive(&action, b"runtime", &[]);
        let mut with_long = AR_MAGIC.to_vec();
        let members = members(&bytes).unwrap();
        with_long.extend(member("__.PKGDEF", members[0].data));
        with_long.extend(member("_go_.o", &long));
        // Its build ID's content half computed again, as `go` does for the bytes it writes.
        let placeholder = build_id(&with_long).unwrap();
        let content = content_id(&with_long, &placeholder);
        let id = format!("{}/{content}", hash_to_string(&action));
        replace_all(&mut with_long, placeholder.as_bytes(), id.as_bytes());
        assert_eq!(portable(&with_long, &action), Ok(()));
    }

    // covers: B-42
    #[test]
    fn native_code_and_another_packages_assembly_stay_here() {
        let action = [4u8; 32];
        let cgo = archive(
            &action,
            b"x",
            &[("_x001.o", b"\x7fELF\x02\x01\x01".to_vec())],
        );
        assert_eq!(portable(&cgo, &action), Err(Local::Native));
        let syso = archive(
            &action,
            b"x",
            &[("rsrc.syso", b"\xcf\xfa\xed\xfe".to_vec())],
        );
        assert_eq!(portable(&syso, &action), Err(Local::Native));
        // F1: assembly of a package outside the standard library may include a header Go
        // doesn't hash.
        let asm = archive(
            &action,
            b"x",
            &[("p_arm64.o", object(None, FROM_ASSEMBLY, b"asm"))],
        );
        assert_eq!(portable(&asm, &action), Err(Local::Assembly));
        // A `_go_.o` that says it came from assembly is no compiler's object.
        let mut odd = AR_MAGIC.to_vec();
        odd.extend(member("__.PKGDEF", b"go object x\n"));
        odd.extend(member("_go_.o", &object(None, FROM_ASSEMBLY, b"")));
        assert_eq!(portable(&odd, &action), Err(Local::Assembly));
    }

    // covers: B-42
    #[test]
    fn an_archive_for_another_action_or_with_a_forged_content_id_stays_here() {
        let action = [5u8; 32];
        let bytes = archive(&action, b"honest", &[]);
        // Put under another action id: Go would take it, the program doesn't.
        assert_eq!(portable(&bytes, &[6u8; 32]), Err(Local::BuildId));
        // F2: one byte changed, the build ID left as it was: the content id is no longer the
        // bytes'.
        let mut forged = bytes.clone();
        let last = forged.len() - 2;
        forged[last] ^= 1;
        assert_eq!(portable(&forged, &action), Err(Local::ContentId));
        // Not an archive at all: Go's other entries.
        assert_eq!(portable(b"go index v2\n", &action), Err(Local::NotArchive));
        assert_eq!(portable(b"!<arch>\nshort", &action), Err(Local::Malformed));
    }

    // covers: B-42
    #[test]
    fn the_build_id_is_read_where_go_reads_it() {
        let action = [8u8; 32];
        let bytes = archive(&action, b"b", &[]);
        let id = build_id(&bytes).unwrap();
        assert!(id.starts_with(&hash_to_string(&action)));
        // A build ID past the first 1024 bytes is none, as for Go.
        let mut late = AR_MAGIC.to_vec();
        let mut pkgdef = b"go object x\n".to_vec();
        pkgdef.extend(std::iter::repeat_n(b' ', 1100));
        pkgdef.extend_from_slice(b"\nbuild id \"a/b\"\n");
        late.extend(member("__.PKGDEF", &pkgdef));
        assert_eq!(build_id(&late), None);
        // An escape in the quoted string is not read.
        let mut escaped = AR_MAGIC.to_vec();
        escaped.extend(member("__.PKGDEF", b"go object x\nbuild id \"a\\x/b\"\n\n"));
        assert_eq!(build_id(&escaped), None);
    }
}

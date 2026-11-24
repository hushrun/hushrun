//! B-78, B-79: a Gradle build cache entry, as Gradle's HTTP build cache sends and takes it: a gzip
//! stream of one tar, whose first member Gradle writes is `METADATA` (Java properties: the
//! entry's `buildCacheKey`, the task's `identity`, `gradleVersion`, `executionTime`,
//! `creationTime` and `buildInvocationId`), and whose other members are the task's outputs, as
//! `tree-<property>/…` folders and files and `missing-<property>` markers.
//!
//! hushrun keeps an entry in its **canonical form**: the same members in the same order, each
//! with its name, type, mode and bytes, and nothing else (no date, owner or group); long names
//! in a PAX header; `METADATA` with its lines sorted and the three values that change from one
//! execution of a task to the next fixed (`buildInvocationId`, `creationTime`,
//! `executionTime`). Two executions of a task that made the same outputs make the same canonical
//! entry, so its digest of outputs is the same on every machine (B-27, B-33), and a block
//! compared with a fresh execution differs only where the outputs do. Gradle reads the canonical
//! form back: it dates every file it unpacks with the time it writes it, and reads nothing of
//! `METADATA` but its origin.
//!
//! Every check is here, and none trusts the bytes it reads: a gzip stream that isn't one
//! member, a CRC or a length that doesn't match, a tar header whose checksum fails, a name that
//! is absolute, holds `.`, `..`, a backslash or a NUL, a member of any type but a file or a
//! folder, a second `METADATA` or none, a `buildCacheKey` other than the key asked, or more bytes
//! than the limit, and the entry is refused.

use std::collections::BTreeMap;

use zlib_rs::{Inflate, InflateFlush, Status};

/// The entry's `METADATA` member, by its name in the tar.
pub const METADATA: &str = "METADATA";

/// What the canonical form's `METADATA` says for `buildInvocationId`: a fixed id in Gradle's
/// form (26 lowercase base-36 characters), so that the entry names no build.
pub const INVOCATION: &str = "hushrunhushrunhushrunhushr";

/// The most bytes an entry may unpack to: past it, it stays with Gradle's own cache.
pub const MOST_UNPACKED: u64 = 1 << 30;

/// The longest name a ustar header holds by itself; a longer one goes in a PAX header.
const NAME_FIELD: usize = 100;

/// One tar block.
const BLOCK: usize = 512;

/// Why an entry was refused, in one word each for the log and the spec.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    #[error("it is no gzip stream")]
    Gzip,
    #[error("its gzip stream doesn't inflate whole")]
    Inflate,
    #[error("it unpacks to more than {0} bytes")]
    TooLarge(u64),
    #[error("its tar is cut short or a header's checksum fails")]
    Tar,
    #[error("a member is neither a file nor a folder")]
    MemberType,
    #[error("a member's name is unsafe: {0:?}")]
    Name(String),
    #[error("it has no METADATA, or more than one")]
    Metadata,
    #[error("its METADATA names another key")]
    OtherKey,
}

/// One member of an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    /// A folder, else a file.
    pub folder: bool,
    /// The permission bits, as the tar gave them (`0o7777` at most).
    pub mode: u32,
    pub bytes: Vec<u8>,
}

/// An entry that passed every check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// Its members, in the order the tar gave them; `METADATA` already canonical.
    pub members: Vec<Member>,
    /// The key its `METADATA` names, as Gradle wrote it (lowercase hex).
    pub key: String,
    /// The task, as Gradle names it (`:app:compileDebugKotlin`), unescaped.
    pub identity: String,
    /// The Gradle that made it.
    pub gradle_version: String,
    /// What its `METADATA` said the task took to execute, in milliseconds: 0 in a canonical
    /// entry, whose cost is the block's record's.
    pub execution_ms: u64,
}

impl Checked {
    /// Reads an entry as Gradle sends it (a gzip stream of a tar) and checks it against `key`,
    /// the key it was sent under.
    pub fn from_gradle(body: &[u8], key: &str) -> Result<Checked, Refused> {
        let tar = gunzip(body, MOST_UNPACKED)?;
        Checked::from_tar(&tar, key)
    }

    /// Reads a tar (an entry gunzipped, or a canonical entry from the store) and checks it
    /// against `key`.
    pub fn from_tar(tar: &[u8], key: &str) -> Result<Checked, Refused> {
        let mut members = read_tar(tar)?;
        let mut metadata = None;
        for (i, member) in members.iter().enumerate() {
            if member.name == METADATA {
                if metadata.is_some() || member.folder {
                    return Err(Refused::Metadata);
                }
                metadata = Some(i);
            }
        }
        let at = metadata.ok_or(Refused::Metadata)?;
        let fields = properties(&members[at].bytes);
        if fields.get("buildCacheKey").map(String::as_str) != Some(key) {
            return Err(Refused::OtherKey);
        }
        let execution_ms = fields
            .get("executionTime")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let identity = fields.get("identity").cloned().unwrap_or_default();
        let gradle_version = fields.get("gradleVersion").cloned().unwrap_or_default();
        members[at].bytes = canonical_metadata(&members[at].bytes);
        members[at].mode = 0o644;
        Ok(Checked {
            members,
            key: key.to_string(),
            identity,
            gradle_version,
            execution_ms,
        })
    }

    /// The canonical form (B-77): a tar of the members and nothing else.
    pub fn canonical(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for member in &self.members {
            write_member(&mut out, member);
        }
        out.extend_from_slice(&[0u8; 2 * BLOCK]);
        out
    }

    /// The bytes of every member, `METADATA` aside, and their names: what B-78 searches for a
    /// machine's folders.
    pub fn outputs_hold(&self, needle: &[u8]) -> bool {
        !needle.is_empty()
            && self.members.iter().filter(|m| m.name != METADATA).any(|m| {
                memchr::memmem::find(m.name.as_bytes(), needle).is_some()
                    || memchr::memmem::find(&m.bytes, needle).is_some()
            })
    }

    /// The members' bytes, `METADATA` included.
    pub fn unpacked_bytes(&self) -> u64 {
        self.members.iter().map(|m| m.bytes.len() as u64).sum()
    }
}

/// The canonical tar `tar` as Gradle takes it: a gzip stream of stored deflate blocks, which
/// costs no compression on the loopback it travels over and is the same bytes on every machine.
pub fn to_gradle(tar: &[u8]) -> Vec<u8> {
    // Header: magic, deflate, no flags, no date, no extra flags, unknown OS.
    let mut out = Vec::with_capacity(tar.len() + tar.len() / 65_535 * 5 + 32);
    out.extend_from_slice(&[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255]);
    let mut chunks = tar.chunks(65_535).peekable();
    if chunks.peek().is_none() {
        out.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(chunk) = chunks.next() {
        let last = chunks.peek().is_none();
        let len = chunk.len() as u16;
        out.push(u8::from(last));
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&zlib_rs::crc32::crc32(0, tar).to_le_bytes());
    out.extend_from_slice(&(tar.len() as u32).to_le_bytes());
    out
}

/// The tar inside one gzip member, at most `most` bytes of it.
pub fn gunzip(body: &[u8], most: u64) -> Result<Vec<u8>, Refused> {
    let start = gzip_header(body).ok_or(Refused::Gzip)?;
    let mut inflate = Inflate::new(false, 15);
    let mut out: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 1 << 16];
    let deflated = &body[start..];
    loop {
        let read = usize::try_from(inflate.total_in()).unwrap_or(usize::MAX);
        let wrote = inflate.total_out();
        let input = deflated.get(read..).unwrap_or_default();
        let status = inflate
            .decompress(input, &mut chunk, InflateFlush::NoFlush)
            .map_err(|_| Refused::Inflate)?;
        let new = usize::try_from(inflate.total_out() - wrote).unwrap_or(usize::MAX);
        if out.len() as u64 + new as u64 > most {
            return Err(Refused::TooLarge(most));
        }
        out.extend_from_slice(chunk.get(..new).unwrap_or_default());
        if status == Status::StreamEnd {
            break;
        }
        let progressed = usize::try_from(inflate.total_in()).unwrap_or(usize::MAX) != read;
        if !progressed && new == 0 {
            return Err(Refused::Inflate);
        }
    }
    let end = start + usize::try_from(inflate.total_in()).map_err(|_| Refused::Inflate)?;
    // The trailer, and nothing after it: one member, as Gradle writes.
    let trailer = body.get(end..).ok_or(Refused::Inflate)?;
    if trailer.len() != 8 {
        return Err(Refused::Inflate);
    }
    let crc = u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    let size = u32::from_le_bytes([trailer[4], trailer[5], trailer[6], trailer[7]]);
    if crc != zlib_rs::crc32::crc32(0, &out) || size != out.len() as u32 {
        return Err(Refused::Inflate);
    }
    Ok(out)
}

/// Where a gzip member's deflate stream begins, past its header.
fn gzip_header(body: &[u8]) -> Option<usize> {
    if body.len() < 18 || body[0] != 0x1f || body[1] != 0x8b || body[2] != 8 {
        return None;
    }
    let flags = body[3];
    if flags & 0xe0 != 0 {
        return None;
    }
    let mut at = 10;
    if flags & 4 != 0 {
        let len = usize::from(u16::from_le_bytes([*body.get(at)?, *body.get(at + 1)?]));
        at += 2 + len;
    }
    for flag in [8u8, 16] {
        if flags & flag != 0 {
            at += body.get(at..)?.iter().position(|b| *b == 0)? + 1;
        }
    }
    if flags & 2 != 0 {
        at += 2;
    }
    (at < body.len()).then_some(at)
}

/// The members of a tar: ustar files and folders, with long names from a PAX header (`path`)
/// or a GNU long name; PAX's global headers are read past.
fn read_tar(tar: &[u8]) -> Result<Vec<Member>, Refused> {
    let mut members = Vec::new();
    let mut at = 0usize;
    let mut long_name: Option<String> = None;
    let mut total: u64 = 0;
    loop {
        let header = tar.get(at..at + BLOCK).ok_or(Refused::Tar)?;
        if header.iter().all(|b| *b == 0) {
            // The end: two zero blocks, or one where a writer stops early.
            break;
        }
        if !checksum_holds(header) {
            return Err(Refused::Tar);
        }
        let size = octal(&header[124..136]).ok_or(Refused::Tar)?;
        total = total.saturating_add(size);
        if total > MOST_UNPACKED {
            return Err(Refused::TooLarge(MOST_UNPACKED));
        }
        let size = usize::try_from(size).map_err(|_| Refused::Tar)?;
        let body_at = at + BLOCK;
        let body = tar.get(body_at..body_at + size).ok_or(Refused::Tar)?;
        at = body_at + size.div_ceil(BLOCK) * BLOCK;
        match header[156] {
            b'x' => {
                long_name = pax_path(body).or(long_name);
                continue;
            }
            b'g' => continue,
            b'L' => {
                let end = body.iter().position(|b| *b == 0).unwrap_or(body.len());
                long_name =
                    Some(String::from_utf8(body[..end].to_vec()).map_err(|_| Refused::Tar)?);
                continue;
            }
            b'0' | 0 | b'5' => {}
            _ => return Err(Refused::MemberType),
        }
        let folder = header[156] == b'5';
        let name = match long_name.take() {
            Some(name) => name,
            None => {
                let name = field(&header[0..100]);
                let prefix = if &header[257..262] == b"ustar" {
                    field(&header[345..500])
                } else {
                    String::new()
                };
                if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}/{name}")
                }
            }
        };
        let name = safe_name(&name, folder)?;
        let mode = octal(&header[100..108]).ok_or(Refused::Tar)? as u32 & 0o7777;
        if folder && size != 0 {
            return Err(Refused::Tar);
        }
        members.push(Member {
            name,
            folder,
            mode,
            bytes: body.to_vec(),
        });
    }
    Ok(members)
}

/// A member's name as hushrun keeps it: relative, its parts plain, a folder's without its
/// trailing `/`.
fn safe_name(name: &str, folder: bool) -> Result<String, Refused> {
    let trimmed = if folder {
        name.trim_end_matches('/')
    } else {
        name
    };
    let bad = trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.contains('\\')
        || trimmed.contains('\0')
        || trimmed
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..");
    if bad {
        return Err(Refused::Name(name.to_string()));
    }
    Ok(trimmed.to_string())
}

/// The `path` record of a PAX header's body.
fn pax_path(body: &[u8]) -> Option<String> {
    let mut rest = body;
    let mut path = None;
    while !rest.is_empty() {
        let space = rest.iter().position(|b| *b == b' ')?;
        let len: usize = std::str::from_utf8(&rest[..space]).ok()?.parse().ok()?;
        let record = rest.get(..len)?;
        let text = record.get(space + 1..len.checked_sub(1)?)?;
        if let Some(value) = text.strip_prefix(b"path=") {
            path = Some(String::from_utf8(value.to_vec()).ok()?);
        }
        rest = &rest[len..];
    }
    path
}

fn checksum_holds(header: &[u8]) -> bool {
    let Some(stated) = octal(&header[148..156]) else {
        return false;
    };
    let sum: u64 = header
        .iter()
        .enumerate()
        .map(|(i, b)| {
            if (148..156).contains(&i) {
                u64::from(b' ')
            } else {
                u64::from(*b)
            }
        })
        .sum();
    sum == stated
}

/// A tar number: octal digits, ended by a space or a NUL; or GNU's base-256 form for a size
/// too large for octal.
fn octal(field: &[u8]) -> Option<u64> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        let mut value: u64 = u64::from(field[0] & 0x7f);
        for b in &field[1..] {
            value = value.checked_mul(256)?.checked_add(u64::from(*b))?;
        }
        return Some(value);
    }
    let text: Vec<u8> = field
        .iter()
        .copied()
        .skip_while(|b| *b == b' ')
        .take_while(|b| *b != 0 && *b != b' ')
        .collect();
    if text.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(std::str::from_utf8(&text).ok()?, 8).ok()
}

fn field(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Java properties, as far as `METADATA` needs: `key=value` lines, comments and blanks read
/// past, a backslash escaping the character after it.
fn properties(bytes: &[u8]) -> BTreeMap<String, String> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            out.insert(unescape(key.trim_end()), unescape(value));
        }
    }
    out
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `METADATA` in the canonical form: its `key=value` lines as written, sorted, the values that
/// change from one execution to the next fixed, no comment.
fn canonical_metadata(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines: Vec<String> = text
        .lines()
        .map(str::trim_start)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('!'))
        .map(|l| {
            let key = l.split_once('=').map_or(l, |(k, _)| k).trim_end();
            match key {
                "buildInvocationId" => format!("buildInvocationId={INVOCATION}"),
                "creationTime" => "creationTime=0".to_string(),
                "executionTime" => "executionTime=0".to_string(),
                _ => l.to_string(),
            }
        })
        .collect();
    lines.sort();
    let mut out = lines.join("\n").into_bytes();
    out.push(b'\n');
    out
}

/// One member in the canonical form: a PAX header first when its name doesn't fit.
fn write_member(out: &mut Vec<u8>, member: &Member) {
    let name = if member.folder {
        format!("{}/", member.name)
    } else {
        member.name.clone()
    };
    if name.len() > NAME_FIELD {
        let record = pax_record("path", &name);
        write_header(out, "././@PaxHeader", b'x', 0o644, record.len() as u64);
        write_body(out, record.as_bytes());
    }
    let short: String = if name.len() > NAME_FIELD {
        // What a reader that ignores PAX sees: the name's end, which never holds a `/` first.
        let mut cut = name.len() - NAME_FIELD;
        while !name.is_char_boundary(cut) {
            cut += 1;
        }
        name[cut..].trim_start_matches('/').to_string()
    } else {
        name
    };
    let (kind, size) = if member.folder {
        (b'5', 0)
    } else {
        (b'0', member.bytes.len() as u64)
    };
    write_header(out, &short, kind, member.mode, size);
    if !member.folder {
        write_body(out, &member.bytes);
    }
}

/// A PAX record, `"<len> <key>=<value>\n"`, whose length counts its own digits.
fn pax_record(key: &str, value: &str) -> String {
    let rest = key.len() + value.len() + 3;
    let mut len = rest + 1;
    while len != rest + len.to_string().len() {
        len = rest + len.to_string().len();
    }
    format!("{len} {key}={value}\n")
}

fn write_header(out: &mut Vec<u8>, name: &str, kind: u8, mode: u32, size: u64) {
    let mut h = [0u8; BLOCK];
    let n = name.as_bytes();
    h[..n.len().min(NAME_FIELD)].copy_from_slice(&n[..n.len().min(NAME_FIELD)]);
    put_octal(&mut h[100..108], u64::from(mode & 0o7777));
    put_octal(&mut h[108..116], 0);
    put_octal(&mut h[116..124], 0);
    put_octal(&mut h[124..136], size);
    put_octal(&mut h[136..148], 0);
    h[156] = kind;
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
    let text = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(text.as_bytes());
    out.extend_from_slice(&h);
}

/// An octal number filling its field, NUL-ended; past 11 digits (8 GiB), GNU's base-256.
fn put_octal(field: &mut [u8], value: u64) {
    let digits = field.len() - 1;
    let text = format!("{value:0digits$o}");
    if text.len() > digits {
        field.fill(0);
        field[0] = 0x80;
        let bytes = value.to_be_bytes();
        let n = field.len();
        field[n - 8..].copy_from_slice(&bytes);
        return;
    }
    field[..digits].copy_from_slice(text.as_bytes());
    field[digits] = 0;
}

fn write_body(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(bytes);
    let pad = bytes.len().div_ceil(BLOCK) * BLOCK - bytes.len();
    out.extend(std::iter::repeat_n(0u8, pad));
}

#[cfg(test)]
mod tests;

//! B-71: a baked store as a read-only EROFS disk image, for a volume a sandbox mounts read-only.
//!
//! Like [`super::image`]'s layer bytes, the image is a pure function of an ordered list of files,
//! each a path and its bytes: [`chunks`] says every byte of it as metadata bytes, ranges of the
//! files' own bytes and zero padding, so the same store always makes the same image, on any
//! machine, with no `mkfs.erofs`, no root and no loop device. Nothing in it depends on the
//! machine: no time, no owner but root, no random UUID.
//!
//! The layout is the subset of EROFS (Linux's `fs/erofs/erofs_fs.h`) that `mkfs.erofs` writes for
//! an uncompressed image with no extended attributes, which every kernel with EROFS (5.4 and
//! later) and `fsck.erofs` read:
//!
//! | Part | Value |
//! |---|---|
//! | blocks | 4096 bytes (`blkszbits` 12) |
//! | superblock | at byte 1024, 128 bytes: magic `0xE0F5E1E2`, no checksum, no features, the root's nid, the inode count, the block count, the metadata at block 0, the UUID given, no volume name, every other field 0 |
//! | inodes | from byte 1152 (the root, nid 36), breadth first, each folder's entries by name as bytes; each at a multiple of 32 bytes, never across a block (moved to the next block when it and its inline tail wouldn't fit); compact (32 bytes) unless its size needs 64 bits (extended, 64 bytes); `i_ino` counts from 1 in that order |
//! | an inode's fields | mode `0100644` (a file) or `040755` (a folder), owner and group 0, every time 0, links 1 for a file and 2 plus its folders for a folder |
//! | data | a file's or folder's whole blocks after the inodes, in the inodes' order, each from a block boundary; the tail (its size modulo 4096) inline after the inode when it and the inode fit in a block (`FLAT_INLINE`, with `0xFFFFFFFF` as the start block when there is no whole block), else as one more block padded with zeros (`FLAT_PLAIN`); an empty file is `FLAT_PLAIN` at block 0 |
//! | a folder's bytes | `.`, `..` and its entries sorted by name as bytes, cut into blocks of 4096 bytes greedily; a block is its 12-byte entries (nid, name offset, type 1 for a file and 2 for a folder) then their names, unpadded; every block but the last padded to 4096 with zeros |
//!
//! A path that isn't relative printable ASCII without empty, `.` or `..` parts, a name longer
//! than 255 bytes, a path given twice or both a file and a folder, or an image of more than 2^32
//! blocks, is refused.
//!
//! Like the rest of this crate it does no IO.

use std::collections::BTreeMap;

use super::image::{File, Refused, valid_path};

/// An EROFS block: 4096 bytes.
pub const BLOCK: u64 = 4096;
/// `blkszbits`: 2^12 is [`BLOCK`].
const BLOCK_BITS: u8 = 12;
/// EROFS's magic number, at the superblock's start.
pub const MAGIC: u32 = 0xE0F5_E1E2;
/// Where the superblock begins.
const SUPER_AT: u64 = 1024;
/// The superblock's size with no extension slots.
const SUPER_SIZE: u64 = 128;
/// A compact inode's size.
const COMPACT: u64 = 32;
/// An extended inode's size.
const EXTENDED: u64 = 64;
/// A directory entry's size.
const DIRENT: u64 = 12;
/// The longest name an entry may have.
const NAME_MOST: usize = 255;
/// The start block of an inode with no whole block.
const NULL_ADDR: u32 = u32::MAX;
/// `i_format`'s data layouts.
const FLAT_PLAIN: u16 = 0;
const FLAT_INLINE: u16 = 2;
/// Directory entries' types.
const FT_REG_FILE: u8 = 1;
const FT_DIR: u8 = 2;

/// A piece of an image's bytes, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chunk {
    /// These bytes: the superblock, inodes and folders.
    Bytes(Vec<u8>),
    /// `len` bytes of the file at index `file` of the list given, from `offset`.
    Range { file: usize, offset: u64, len: u64 },
    /// This many NULs.
    Zeros(u64),
}

/// A node of the tree: the root, a folder or a file.
struct Node {
    parent: usize,
    /// The file's index in the list given; `None` for a folder.
    file: Option<usize>,
    /// A folder's entries, by name.
    children: BTreeMap<Vec<u8>, usize>,
}

/// Where an inode and its bytes go.
struct Placed {
    nid: u64,
    size: u64,
    inline: u64,
    layout: u16,
    extended: bool,
    /// Its first whole block, when it has one.
    start: Option<u32>,
    /// Its whole blocks, the padded tail of a `FLAT_PLAIN` one included.
    blocks: u64,
    nlink: u32,
}

/// The tree of `files`, root first (index 0). `Err` on a bad path, a name too long, a path given
/// twice, or a path that is a file and a folder.
fn tree(files: &[File]) -> Result<Vec<Node>, Refused> {
    let mut nodes = vec![Node {
        parent: 0,
        file: None,
        children: BTreeMap::new(),
    }];
    for (i, file) in files.iter().enumerate() {
        if !valid_path(&file.path) {
            return Err(Refused(format!("{:?} can't be an image's path", file.path)));
        }
        let parts: Vec<&str> = file.path.split('/').collect();
        let mut at = 0;
        for (n, part) in parts.iter().enumerate() {
            if part.len() > NAME_MOST {
                return Err(Refused(format!(
                    "{} has a name longer than {NAME_MOST} bytes",
                    file.path
                )));
            }
            let last = n + 1 == parts.len();
            let name = part.as_bytes().to_vec();
            match nodes[at].children.get(&name).copied() {
                Some(next) => {
                    if last || nodes[next].file.is_some() {
                        return Err(Refused(format!(
                            "{} is in the image twice, or both a file and a folder",
                            parts[..=n].join("/")
                        )));
                    }
                    at = next;
                }
                None => {
                    let next = nodes.len();
                    nodes.push(Node {
                        parent: at,
                        file: last.then_some(i),
                        children: BTreeMap::new(),
                    });
                    nodes[at].children.insert(name, next);
                    at = next;
                }
            }
        }
    }
    Ok(nodes)
}

/// The nodes breadth first from the root, each folder's entries by name as bytes.
fn breadth_first(nodes: &[Node]) -> Vec<usize> {
    let mut order = vec![0];
    let mut at = 0;
    while at < order.len() {
        order.extend(nodes[order[at]].children.values().copied());
        at += 1;
    }
    order
}

/// A folder's entries, `.` and `..` among them, sorted by name as bytes: (name, node).
fn dirents(nodes: &[Node], dir: usize) -> Vec<(Vec<u8>, usize)> {
    let mut all = vec![(b".".to_vec(), dir), (b"..".to_vec(), nodes[dir].parent)];
    all.extend(nodes[dir].children.iter().map(|(n, &c)| (n.clone(), c)));
    all.sort_by(|a, b| a.0.cmp(&b.0));
    all
}

/// A folder's entries cut into blocks, greedily: each block's entries and the bytes it uses.
fn dir_blocks(entries: &[(Vec<u8>, usize)]) -> Vec<(std::ops::Range<usize>, u64)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut used = 0;
    for (i, (name, _)) in entries.iter().enumerate() {
        let more = DIRENT + name.len() as u64;
        if used + more > BLOCK {
            out.push((start..i, used));
            start = i;
            used = 0;
        }
        used += more;
    }
    out.push((start..entries.len(), used));
    out
}

/// One block of a folder's entries, as many bytes as it uses.
fn dir_block(entries: &[(Vec<u8>, usize)], nodes: &[Node], placed: &[Placed]) -> Vec<u8> {
    let mut heads = Vec::new();
    let mut names = Vec::new();
    let names_at = DIRENT as usize * entries.len();
    for (name, node) in entries {
        heads.extend_from_slice(&placed[*node].nid.to_le_bytes());
        heads.extend_from_slice(&((names_at + names.len()) as u16).to_le_bytes());
        heads.push(if nodes[*node].file.is_some() {
            FT_REG_FILE
        } else {
            FT_DIR
        });
        heads.push(0);
        names.extend_from_slice(name);
    }
    heads.extend_from_slice(&names);
    heads
}

/// The bytes of an inode.
fn inode(p: &Placed, mode: u16, ino: u32) -> Vec<u8> {
    let format = u16::from(p.extended) | (p.layout << 1);
    let start = match (p.start, p.layout) {
        (Some(start), _) => start,
        (None, FLAT_INLINE) => NULL_ADDR,
        (None, _) => 0,
    };
    let mut out = Vec::with_capacity(EXTENDED as usize);
    out.extend_from_slice(&format.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // i_xattr_icount
    out.extend_from_slice(&mode.to_le_bytes());
    if p.extended {
        out.extend_from_slice(&0u16.to_le_bytes()); // i_nb
        out.extend_from_slice(&p.size.to_le_bytes());
        out.extend_from_slice(&start.to_le_bytes());
        out.extend_from_slice(&ino.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // uid
        out.extend_from_slice(&0u32.to_le_bytes()); // gid
        out.extend_from_slice(&0u64.to_le_bytes()); // mtime
        out.extend_from_slice(&0u32.to_le_bytes()); // mtime_nsec
        out.extend_from_slice(&p.nlink.to_le_bytes());
        out.extend_from_slice(&[0; 16]);
    } else {
        out.extend_from_slice(&(p.nlink as u16).to_le_bytes());
        out.extend_from_slice(&(p.size as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // mtime
        out.extend_from_slice(&start.to_le_bytes());
        out.extend_from_slice(&ino.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // uid
        out.extend_from_slice(&0u16.to_le_bytes()); // gid
        out.extend_from_slice(&0u32.to_le_bytes()); // reserved
    }
    out
}

/// The superblock's 128 bytes.
fn superblock(root: u16, inodes: u64, blocks: u32, uuid: [u8; 16]) -> Vec<u8> {
    let mut sb = vec![0u8; SUPER_SIZE as usize];
    sb[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    sb[12] = BLOCK_BITS;
    sb[14..16].copy_from_slice(&root.to_le_bytes());
    sb[16..24].copy_from_slice(&inodes.to_le_bytes());
    sb[36..40].copy_from_slice(&blocks.to_le_bytes());
    sb[48..64].copy_from_slice(&uuid);
    sb
}

/// Bytes being laid out: the metadata area, pending bytes flushed before a file's range.
struct Out {
    chunks: Vec<Chunk>,
    pending: Vec<u8>,
    at: u64,
}

impl Out {
    fn bytes(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        self.at += bytes.len() as u64;
    }
    fn zeros_to(&mut self, to: u64) {
        let n = to - self.at;
        if n == 0 {
            return;
        }
        if n <= BLOCK {
            self.pending.resize(self.pending.len() + n as usize, 0);
        } else {
            self.flush();
            self.chunks.push(Chunk::Zeros(n));
        }
        self.at = to;
    }
    fn range(&mut self, file: usize, offset: u64, len: u64) {
        if len == 0 {
            return;
        }
        self.flush();
        self.chunks.push(Chunk::Range { file, offset, len });
        self.at += len;
    }
    fn flush(&mut self) {
        if !self.pending.is_empty() {
            self.chunks
                .push(Chunk::Bytes(std::mem::take(&mut self.pending)));
        }
    }
}

/// B-71: every byte of the EROFS image of `files` (paths from the image's root), with `uuid` as
/// its UUID, in order. The same list and UUID always give the same bytes, whatever order the list
/// is in.
pub fn chunks(files: &[File], uuid: [u8; 16]) -> Result<Vec<Chunk>, Refused> {
    let nodes = tree(files)?;
    let order = breadth_first(&nodes);
    let entries: Vec<Vec<(Vec<u8>, usize)>> = (0..nodes.len())
        .map(|n| {
            if nodes[n].file.is_some() {
                Vec::new()
            } else {
                dirents(&nodes, n)
            }
        })
        .collect();
    let cuts: Vec<Vec<(std::ops::Range<usize>, u64)>> = entries
        .iter()
        .enumerate()
        .map(|(n, e)| {
            if nodes[n].file.is_some() {
                Vec::new()
            } else {
                dir_blocks(e)
            }
        })
        .collect();
    // Sizes, layouts and the inodes' places, before any nid is known: a folder's size depends
    // on its names alone.
    let mut placed: Vec<Placed> = (0..nodes.len())
        .map(|n| {
            let (size, nlink) = match nodes[n].file {
                Some(i) => (files[i].size, 1u32),
                None => {
                    let c = &cuts[n];
                    let size = (c.len() as u64 - 1) * BLOCK + c.last().map_or(0, |b| b.1);
                    let dirs = nodes[n]
                        .children
                        .values()
                        .filter(|&&c| nodes[c].file.is_none())
                        .count() as u32;
                    (size, 2 + dirs)
                }
            };
            let extended = size > u64::from(u32::MAX) || nlink > u32::from(u16::MAX);
            let isize = if extended { EXTENDED } else { COMPACT };
            let tail = size % BLOCK;
            let (layout, inline, blocks) = if size == 0 || tail == 0 {
                (FLAT_PLAIN, 0, size / BLOCK)
            } else if isize + tail <= BLOCK {
                (FLAT_INLINE, tail, size / BLOCK)
            } else {
                (FLAT_PLAIN, 0, size / BLOCK + 1)
            };
            Placed {
                nid: 0,
                size,
                inline,
                layout,
                extended,
                start: None,
                blocks,
                nlink,
            }
        })
        .collect();
    let mut at = SUPER_AT + SUPER_SIZE;
    for &n in &order {
        let p = &mut placed[n];
        let record = if p.extended { EXTENDED } else { COMPACT } + p.inline;
        at = at.div_ceil(32) * 32;
        if at % BLOCK + record > BLOCK {
            at = at.div_ceil(BLOCK) * BLOCK;
        }
        p.nid = at / 32;
        at += record;
    }
    let root = u16::try_from(placed[0].nid).map_err(|_| Refused("the root's nid".into()))?;
    let meta_end = at.div_ceil(BLOCK) * BLOCK;
    let mut block = meta_end / BLOCK;
    for &n in &order {
        let p = &mut placed[n];
        if p.blocks > 0 {
            p.start = Some(
                u32::try_from(block)
                    .map_err(|_| Refused("the image would have more than 2^32 blocks".into()))?,
            );
            block += p.blocks;
        }
    }
    let total = u32::try_from(block)
        .map_err(|_| Refused("the image would have more than 2^32 blocks".into()))?;
    // A folder's bytes, now that every nid is known.
    let dir_bytes = |n: usize| -> Vec<Vec<u8>> {
        cuts[n]
            .iter()
            .map(|(range, _)| dir_block(&entries[n][range.clone()], &nodes, &placed))
            .collect()
    };
    let mut out = Out {
        chunks: Vec::new(),
        pending: Vec::new(),
        at: 0,
    };
    out.zeros_to(SUPER_AT);
    out.bytes(&superblock(root, order.len() as u64, total, uuid));
    for (ino, &n) in order.iter().enumerate() {
        let p = &placed[n];
        out.zeros_to(p.nid * 32);
        let mode = if nodes[n].file.is_some() {
            0o100_644
        } else {
            0o040_755
        };
        out.bytes(&inode(p, mode, ino as u32 + 1));
        if p.inline > 0 {
            match nodes[n].file {
                Some(i) => out.range(i, p.size - p.inline, p.inline),
                None => {
                    let last = dir_bytes(n).pop().unwrap_or_default();
                    out.bytes(&last);
                }
            }
        }
    }
    out.zeros_to(meta_end);
    for &n in &order {
        let p = &placed[n];
        if p.blocks == 0 {
            continue;
        }
        let begin = out.at;
        match nodes[n].file {
            Some(i) => {
                let whole = if p.layout == FLAT_PLAIN {
                    p.size
                } else {
                    p.size - p.inline
                };
                out.range(i, 0, whole);
            }
            None => {
                let mut blocks = dir_bytes(n);
                if p.layout == FLAT_INLINE {
                    blocks.pop();
                }
                for bytes in blocks {
                    let from = out.at;
                    out.bytes(&bytes);
                    out.zeros_to(from + BLOCK);
                }
            }
        }
        out.zeros_to(begin + p.blocks * BLOCK);
    }
    out.flush();
    Ok(out.chunks)
}

/// The size of the image of `files`.
pub fn size(files: &[File]) -> Result<u64, Refused> {
    Ok(chunks(files, [0; 16])?
        .iter()
        .map(|chunk| match chunk {
            Chunk::Bytes(b) => b.len() as u64,
            Chunk::Range { len, .. } => *len,
            Chunk::Zeros(n) => *n,
        })
        .sum())
}

/// The image of `files` written whole into memory, with `bytes(i)` the bytes of file `i`. `Err`
/// when a file's bytes aren't its size.
pub fn image(
    files: &[File],
    bytes: &dyn Fn(usize) -> Vec<u8>,
    uuid: [u8; 16],
) -> Result<Vec<u8>, Refused> {
    let mut out = Vec::new();
    for chunk in chunks(files, uuid)? {
        match chunk {
            Chunk::Bytes(b) => out.extend_from_slice(&b),
            Chunk::Range { file, offset, len } => {
                let data = bytes(file);
                if data.len() as u64 != files[file].size {
                    return Err(Refused(format!(
                        "{} has {} bytes, not the {} its entry says",
                        files[file].path,
                        data.len(),
                        files[file].size
                    )));
                }
                out.extend_from_slice(&data[offset as usize..(offset + len) as usize]);
            }
            Chunk::Zeros(n) => out.resize(out.len() + n as usize, 0),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2_hw::{Digest as _, Sha256};

    fn file(path: &str, size: u64) -> File {
        File {
            path: path.into(),
            size,
        }
    }

    /// A reader of the subset [`chunks`] writes, apart from the writer: what a kernel would see.
    struct Reader<'a>(&'a [u8]);

    #[derive(Debug)]
    struct Ino {
        mode: u16,
        nlink: u32,
        size: u64,
        uid: u32,
        data: Vec<u8>,
    }

    impl Reader<'_> {
        fn u16(&self, at: usize) -> u16 {
            u16::from_le_bytes(self.0[at..at + 2].try_into().unwrap())
        }
        fn u32(&self, at: usize) -> u32 {
            u32::from_le_bytes(self.0[at..at + 4].try_into().unwrap())
        }
        fn u64(&self, at: usize) -> u64 {
            u64::from_le_bytes(self.0[at..at + 8].try_into().unwrap())
        }
        fn root(&self) -> u64 {
            assert_eq!(self.u32(1024), MAGIC);
            assert_eq!(self.0[1024 + 12], 12);
            assert_eq!(
                u64::from(self.u32(1024 + 36)) * BLOCK,
                self.0.len() as u64,
                "the block count is the image's size"
            );
            u64::from(self.u16(1024 + 14))
        }
        fn inode(&self, nid: u64) -> Ino {
            let at = (nid * 32) as usize;
            let format = self.u16(at);
            let extended = format & 1 == 1;
            let layout = (format >> 1) & 7;
            let mode = self.u16(at + 4);
            let (size, start, nlink, uid, isize) = if extended {
                (
                    self.u64(at + 8),
                    self.u32(at + 16),
                    self.u32(at + 44),
                    self.u32(at + 24),
                    64,
                )
            } else {
                (
                    u64::from(self.u32(at + 8)),
                    self.u32(at + 16),
                    u32::from(self.u16(at + 6)),
                    u32::from(self.u16(at + 24)),
                    32,
                )
            };
            let mut data = Vec::new();
            let whole = match layout {
                0 => size,
                2 => size - size % BLOCK,
                other => panic!("layout {other}"),
            };
            if whole > 0 {
                let from = u64::from(start) * BLOCK;
                data.extend_from_slice(&self.0[from as usize..(from + whole) as usize]);
            }
            if layout == 2 {
                let tail = (size % BLOCK) as usize;
                let from = at + isize;
                assert!(from % 4096 + tail <= 4096, "an inline tail crosses a block");
                data.extend_from_slice(&self.0[from..from + tail]);
            }
            Ino {
                mode,
                nlink,
                size,
                uid,
                data,
            }
        }
        /// A folder's entries, checking each block is sorted and every block follows the last.
        fn list(&self, dir: &Ino) -> Vec<(Vec<u8>, u64, u8)> {
            let mut out: Vec<(Vec<u8>, u64, u8)> = Vec::new();
            for block in dir.data.chunks(BLOCK as usize) {
                let first = u16::from_le_bytes([block[8], block[9]]) as usize;
                let n = first / 12;
                for i in 0..n {
                    let e = &block[i * 12..];
                    let nid = u64::from_le_bytes(e[..8].try_into().unwrap());
                    let off = u16::from_le_bytes([e[8], e[9]]) as usize;
                    let end = if i + 1 < n {
                        u16::from_le_bytes([block[(i + 1) * 12 + 8], block[(i + 1) * 12 + 9]])
                            as usize
                    } else {
                        block[off..]
                            .iter()
                            .position(|&b| b == 0)
                            .map_or(block.len(), |p| off + p)
                    };
                    let name = block[off..end].to_vec();
                    if let Some(last) = out.last() {
                        assert!(last.0 < name, "entries out of order");
                    }
                    out.push((name, nid, e[10]));
                }
            }
            out
        }
        /// Every file under the root, by path, with its bytes.
        fn walk(&self) -> BTreeMap<String, Vec<u8>> {
            let root = self.root();
            let mut out = BTreeMap::new();
            let mut todo = vec![(String::new(), root, root)];
            while let Some((path, nid, parent)) = todo.pop() {
                let dir = self.inode(nid);
                assert_eq!(dir.mode, 0o040_755);
                assert_eq!(dir.uid, 0);
                let list = self.list(&dir);
                let mut dirs = 0;
                for (name, child, kind) in list {
                    if name == b"." {
                        assert_eq!(child, nid);
                        continue;
                    }
                    if name == b".." {
                        assert_eq!(child, parent);
                        continue;
                    }
                    let name = String::from_utf8(name).unwrap();
                    let full = if path.is_empty() {
                        name
                    } else {
                        format!("{path}/{name}")
                    };
                    if kind == FT_DIR {
                        dirs += 1;
                        todo.push((full, child, nid));
                    } else {
                        let f = self.inode(child);
                        assert_eq!((f.mode, f.nlink, f.uid), (0o100_644, 1, 0));
                        assert_eq!(f.size as usize, f.data.len());
                        out.insert(full, f.data);
                    }
                }
                assert_eq!(dir.nlink, 2 + dirs);
            }
            out
        }
    }

    fn content(i: usize, size: u64) -> Vec<u8> {
        (0..size).map(|n| (n as usize * 7 + i * 13) as u8).collect()
    }

    // covers: B-71
    #[test]
    fn an_image_reads_back_as_the_files_given() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let mut files = vec![
            file("baked.json", 3),
            file("baked.sigstore.json", 0),
            file(&format!("packs/{a}.pack"), 10_000),
            file(&format!("packs/{b}.pack"), 8_100),
            file(&format!("packs/{}.pack", "c".repeat(64)), 4096 * 3),
            file(&format!("hints/{a}/{b}"), 300),
        ];
        // A folder of more entries than one block holds.
        for n in 0..120 {
            files.push(file(&format!("hints/{b}/{n:064x}"), 40 + n));
        }
        let data: Vec<Vec<u8>> = files
            .iter()
            .enumerate()
            .map(|(i, f)| content(i, f.size))
            .collect();
        let img = image(&files, &|i| data[i].clone(), [7; 16]).unwrap();
        assert_eq!(img.len() as u64 % BLOCK, 0);
        assert_eq!(img.len() as u64, size(&files).unwrap());
        let read = Reader(&img).walk();
        assert_eq!(read.len(), files.len());
        for (i, f) in files.iter().enumerate() {
            assert_eq!(read[&f.path], data[i], "{}", f.path);
        }
        assert_eq!(&img[1024 + 48..1024 + 64], &[7; 16]);
    }

    // covers: B-71
    // A folder's entries are sorted with `.` and `..` among them: names that sort before `.`
    // (`!`, `,`, `-a`) come before them, as the kernel's binary search of a folder needs.
    #[test]
    fn names_that_sort_before_the_dot_entries_come_before_them() {
        let files = vec![
            file("!", 1),
            file(",", 2),
            file("-a", 3),
            file("a", 4),
            file("hints/!/-x", 5),
            file("hints/!/z", 6),
        ];
        let data: Vec<Vec<u8>> = files
            .iter()
            .enumerate()
            .map(|(i, f)| content(i, f.size))
            .collect();
        let img = image(&files, &|i| data[i].clone(), [1; 16]).unwrap();
        let reader = Reader(&img);
        let root = reader.root();
        let names: Vec<Vec<u8>> = reader
            .list(&reader.inode(root))
            .into_iter()
            .map(|(name, _, _)| name)
            .collect();
        let expect: Vec<&[u8]> = vec![b"!", b",", b"-a", b".", b"..", b"a", b"hints"];
        assert_eq!(names, expect);
        let read = reader.walk();
        for (i, f) in files.iter().enumerate() {
            assert_eq!(read[&f.path], data[i], "{}", f.path);
        }
    }

    // covers: B-71
    #[test]
    fn the_same_files_give_the_same_image_in_any_order() {
        let files = vec![file("packs/x.pack", 5000), file("baked.json", 2)];
        let data = [content(0, 5000), b"{}".to_vec()];
        let one = image(&files, &|i| data[i].clone(), [0; 16]).unwrap();
        let reversed = vec![files[1].clone(), files[0].clone()];
        let back = image(&reversed, &|i| data[1 - i].clone(), [0; 16]).unwrap();
        assert_eq!(one, back);
        // A digest pinned, so that a change to the layout is a change to this test, checked
        // with fsck.erofs and a mount by Linux.
        let hex: String = Sha256::digest(&one)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            hex,
            "713c667e96fe0aeaa03b74b474e1a7735f0c75936ebc3cef74dc98fe49d6c990"
        );
    }

    // covers: B-71
    #[test]
    fn the_layout_is_the_one_documented() {
        let files = vec![file("baked.json", 2)];
        let img = image(&files, &|_| b"{}".to_vec(), [0; 16]).unwrap();
        let r = Reader(&img);
        assert_eq!(r.root(), 36);
        // The root: compact, inline, a folder of three entries.
        assert_eq!(r.u16(1152), FLAT_INLINE << 1);
        assert_eq!(r.u16(1152 + 4), 0o040_755);
        assert_eq!(r.u16(1152 + 6), 2);
        assert_eq!(r.u32(1152 + 8), 3 * 12 + 1 + 2 + 10);
        assert_eq!(r.u32(1152 + 16), NULL_ADDR);
        assert_eq!(r.u32(1152 + 20), 1);
        // Its entries: `.`, `..`, then baked.json, each 12 bytes, then the names.
        assert_eq!(&img[1184 + 36..1184 + 49], b"...baked.json");
        // baked.json's inode, the next multiple of 32 after the root and its tail.
        let next = (1184 + 49u64).div_ceil(32) * 32;
        assert_eq!(r.u64(1184 + 24), next / 32);
        assert_eq!(&img[next as usize + 32..next as usize + 34], b"{}");
        assert_eq!(img.len(), 4096);
        // No inline tail crosses a block: one that wouldn't fit starts the next block.
        let files = vec![file("a", 4000), file("b", 4000)];
        let img = image(&files, &|i| content(i, 4000), [0; 16]).unwrap();
        assert_eq!(Reader(&img).walk()["b"], content(1, 4000));
        // A tail too large to sit beside an inode is a padded block of its own.
        let files = vec![file("a", 4090)];
        let img = image(&files, &|_| content(0, 4090), [0; 16]).unwrap();
        assert_eq!(Reader(&img).walk()["a"], content(0, 4090));
    }

    // covers: B-71
    #[test]
    fn bad_paths_and_clashes_are_refused() {
        for bad in ["/abs", "a/../b", "a//b", "", "a b", "é"] {
            assert!(chunks(&[file(bad, 1)], [0; 16]).is_err(), "{bad:?}");
        }
        assert!(chunks(&[file(&"x".repeat(256), 1)], [0; 16]).is_err());
        assert!(chunks(&[file("a/b", 1), file("a/b", 1)], [0; 16]).is_err());
        assert!(chunks(&[file("a", 1), file("a/b", 1)], [0; 16]).is_err());
        assert!(chunks(&[file("a/b", 1), file("a", 1)], [0; 16]).is_err());
    }
}

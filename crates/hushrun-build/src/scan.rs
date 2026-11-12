//! The search of outputs for raw roots (B-10): whether a block may move to another folder.
//!
//! Every output of a compile is searched once for each root, in its given and resolved forms.
//! A cold build writes a few gigabytes of libraries, so a file is read in chunks, never whole,
//! and the same pass can give its SHA-256 and size for the store.

use std::io::{self, Read};
use std::path::Path;

use hushrun_core::Digest;
use hushrun_digest::hash::Hasher;
use memchr::memmem::Finder;

/// How much of a file is read at a time.
const CHUNK: usize = 1 << 20;

/// Where a needle was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    /// The index of the needle, in the order [`Needles::new`] kept them.
    pub needle: usize,
    /// The byte offset of the hit from the start of the bytes or the file.
    pub offset: u64,
}

/// The byte strings an output must not hold: the roots, each in its given and resolved form.
///
/// Empty and repeated needles are dropped when built; [`Found::needle`] indexes the needles
/// that remain, in their first order.
pub struct Needles {
    finders: Vec<Finder<'static>>,
    /// For each needle, whether a hit counts only where it ends a name ([`ends_name`]).
    bounded: Vec<bool>,
    /// The length of the longest needle, so a chunk keeps enough of the one before it.
    longest: usize,
    chunk: usize,
}

/// Whether a byte can be part of a file's name: a folder followed by one is another name.
fn name_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'~')
}

/// Whether a hit ending at `end` ends a name there: the bytes end, or no name character follows
/// (`/roots.rs` and `/rootfs` are not the folder `/root`; `/root/x`, `"/root"` and `/root\0`
/// are). Only the byte after counts. The byte before can be a name character in front of a real
/// path (`-I/root/include`, or the length rustc writes before a string in crate metadata), so it
/// never rules a hit out.
pub fn ends_name(bytes: &[u8], end: usize) -> bool {
    end >= bytes.len() || !name_char(bytes[end])
}

/// Whether `text` holds `needle` where it ends a name ([`ends_name`]).
pub fn holds_path(text: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && memchr::memmem::find_iter(text, needle)
            .any(|start| ends_name(text, start + needle.len()))
}

impl Needles {
    /// Builds one searcher per distinct, non-empty needle.
    pub fn new(needles: Vec<Vec<u8>>) -> Needles {
        Needles::with_bounds(needles.into_iter().map(|needle| (needle, false)).collect())
    }

    /// Builds one searcher per distinct, non-empty needle, each with whether a hit counts only
    /// where it ends a name ([`ends_name`]). [`Needles::all_in_file_hashed`] and
    /// [`Needles::all_in_bytes`] keep to it.
    pub fn with_bounds(needles: Vec<(Vec<u8>, bool)>) -> Needles {
        let mut kept: Vec<(Vec<u8>, bool)> = Vec::with_capacity(needles.len());
        for needle in needles {
            if !needle.0.is_empty() && !kept.contains(&needle) {
                kept.push(needle);
            }
        }
        let longest = kept.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
        let finders = kept
            .iter()
            .map(|(needle, _)| Finder::new(needle).into_owned())
            .collect();
        Needles {
            finders,
            bounded: kept.iter().map(|(_, bounded)| *bounded).collect(),
            longest,
            chunk: CHUNK,
        }
    }

    /// Whether needle `index` is in `bytes`, by its own rule. `at_end` says whether the bytes end
    /// where the file does; a bounded needle's hit at the end of bytes that don't is not judged
    /// here: the bytes kept for the next chunk hold it again, with the byte after it.
    fn hit(&self, index: usize, bytes: &[u8], at_end: bool) -> bool {
        let finder = &self.finders[index];
        if !self.bounded[index] {
            return finder.find(bytes).is_some();
        }
        let len = finder.needle().len();
        finder.find_iter(bytes).any(|start| {
            let end = start + len;
            (end < bytes.len() || at_end) && ends_name(bytes, end)
        })
    }

    /// The needles kept, in the order [`Found::needle`] counts them.
    pub fn needles(&self) -> impl Iterator<Item = &[u8]> {
        self.finders.iter().map(Finder::needle)
    }

    /// The earliest hit in `bytes`. On a tie, the needle kept first.
    pub fn in_bytes(&self, bytes: &[u8]) -> Option<Found> {
        self.search(bytes).map(|(needle, offset)| Found {
            needle,
            offset: offset as u64,
        })
    }

    /// A hit in the file at `path`, read in chunks and never whole. The search stops at the
    /// first chunk that holds a hit, so the hit is the earliest only within what was read.
    pub fn in_file(&self, path: &Path) -> io::Result<Option<Found>> {
        self.stream(path, None).map(|(found, _)| found)
    }

    /// A hit in the file at `path`, with the SHA-256 and size of all its bytes, in one pass.
    /// Hashing goes on to the end after a hit.
    pub fn in_file_hashed(&self, path: &Path) -> io::Result<(Option<Found>, Digest, u64)> {
        let mut hasher = Hasher::new();
        let (found, size) = self.stream(path, Some(&mut hasher))?;
        Ok((found, Digest::from_bytes(hasher.finish()), size))
    }

    /// Every needle found anywhere in the file at `path`, as one flag per needle in the order
    /// [`Needles::needles`] gives them, with the SHA-256 and size of all its bytes, in one pass.
    /// Unlike [`Needles::in_file_hashed`] the search doesn't stop at the first hit: each needle
    /// is looked for until it is found.
    pub fn all_in_file_hashed(&self, path: &Path) -> io::Result<(Vec<bool>, Digest, u64)> {
        let mut file = crate::regular::open(path)?;
        let mut hasher = Hasher::new();
        // A bounded needle's hit is judged with the byte after it: one more kept.
        let keep = if self.bounded.contains(&true) {
            self.longest
        } else {
            self.longest.saturating_sub(1)
        };
        let chunk = self.chunk.max(1);
        let mut buf = vec![0u8; keep + chunk];
        let mut have = 0usize;
        let mut size = 0u64;
        let mut found = vec![false; self.finders.len()];
        loop {
            let n = match file.read(&mut buf[have..have + chunk]) {
                Ok(0) => break,
                Ok(n) => n,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            };
            hasher.update(&buf[have..have + n]);
            size += n as u64;
            have += n;
            for (index, hit) in found.iter_mut().enumerate() {
                *hit = *hit || self.hit(index, &buf[..have], false);
            }
            let tail = keep.min(have);
            buf.copy_within(have - tail..have, 0);
            have = tail;
        }
        // The file's last bytes, now known to be its end.
        for (index, hit) in found.iter_mut().enumerate() {
            *hit = *hit || self.hit(index, &buf[..have], true);
        }
        Ok((found, Digest::from_bytes(hasher.finish()), size))
    }

    /// Every needle found anywhere in `bytes`, as one flag per needle in the order
    /// [`Needles::needles`] gives them: [`Needles::all_in_file_hashed`] for bytes already read.
    pub fn all_in_bytes(&self, bytes: &[u8]) -> Vec<bool> {
        (0..self.finders.len())
            .map(|index| self.hit(index, bytes, true))
            .collect()
    }

    /// The earliest (needle, offset) in `bytes`. Each later needle searches only up to where a
    /// hit would still be earlier.
    fn search(&self, bytes: &[u8]) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize)> = None;
        for (index, finder) in self.finders.iter().enumerate() {
            let end = match best {
                // A later needle wins only by starting before the best hit.
                Some((_, at)) => (at + finder.needle().len())
                    .saturating_sub(1)
                    .min(bytes.len()),
                None => bytes.len(),
            };
            if let Some(at) = finder.find(&bytes[..end])
                && best.is_none_or(|(_, b)| at < b)
            {
                best = Some((index, at));
            }
        }
        best
    }

    /// Reads the file chunk by chunk. The last `longest - 1` bytes of each chunk are kept in
    /// front of the next, so a needle across a boundary is found. Returns the hit and the size.
    fn stream(
        &self,
        path: &Path,
        mut hasher: Option<&mut Hasher>,
    ) -> io::Result<(Option<Found>, u64)> {
        let mut file = crate::regular::open(path)?;
        let keep = self.longest.saturating_sub(1);
        let chunk = self.chunk.max(1);
        let mut buf = vec![0u8; keep + chunk];
        // Bytes of `buf` that hold file data, and the file offset of `buf[0]`.
        let mut have = 0usize;
        let mut base = 0u64;
        let mut size = 0u64;
        let mut found = None;
        loop {
            let n = match file.read(&mut buf[have..have + chunk]) {
                Ok(0) => break,
                Ok(n) => n,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            };
            if let Some(hasher) = hasher.as_deref_mut() {
                hasher.update(&buf[have..have + n]);
            }
            size += n as u64;
            have += n;
            if found.is_none()
                && let Some((needle, at)) = self.search(&buf[..have])
            {
                found = Some(Found {
                    needle,
                    offset: base + at as u64,
                });
                if hasher.is_none() {
                    break;
                }
            }
            if found.is_none() {
                let tail = keep.min(have);
                buf.copy_within(have - tail..have, 0);
                base += (have - tail) as u64;
                have = tail;
            } else {
                have = 0;
            }
        }
        Ok((found, size))
    }

    /// The same needles, read `chunk` bytes at a time, to put hits on chunk boundaries.
    #[cfg(test)]
    fn with_chunk(mut self, chunk: usize) -> Needles {
        self.chunk = chunk;
        self
    }

    /// A copy of the same needles, read `chunk` bytes at a time.
    #[cfg(test)]
    fn clone_with(&self, chunk: usize) -> Needles {
        Needles::with_bounds(
            self.needles()
                .map(<[u8]>::to_vec)
                .zip(self.bounded.iter().copied())
                .collect(),
        )
        .with_chunk(chunk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn needles(list: &[&[u8]]) -> Needles {
        Needles::new(list.iter().map(|n| n.to_vec()).collect())
    }

    /// Bytes that hold no `/` and no letter, so no root can appear by chance.
    fn filler(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 7) as u8).collect()
    }

    // covers: B-10
    #[test]
    fn every_needle_is_found_wherever_it_lies_and_the_hash_is_of_all_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = filler(40);
        bytes.extend(b"/home/dev/.cargo/registry");
        bytes.extend(filler(37));
        bytes.extend(b"/home/dev/work");
        let path = write(dir.path(), "out", &bytes);
        let n = needles(&[
            b"/home/dev/work",
            b"/home/dev/.cargo",
            b"/home/dev",
            b"/nowhere",
        ]);
        for chunk in [1, 3, 16, 1 << 20] {
            let (found, sha, size) = n.clone_with(chunk).all_in_file_hashed(&path).unwrap();
            assert_eq!(found, [true, true, true, false], "chunk {chunk}");
            assert_eq!(sha, Digest::of(&bytes));
            assert_eq!(size, bytes.len() as u64);
        }
        // The first-hit search stops at the earliest needle and says nothing of the others.
        let (first, _, _) = n.in_file_hashed(&path).unwrap();
        assert_eq!(first.map(|f| f.needle), Some(1));
    }

    // covers: B-10
    #[test]
    fn a_bounded_needle_counts_only_where_it_ends_a_name_in_every_chunk_size() {
        let dir = tempfile::tempdir().unwrap();
        let n = Needles::with_bounds(vec![(b"/root".to_vec(), true), (b"/root".to_vec(), false)]);
        let cases: [(&[u8], bool); 10] = [
            (b"/root", true),
            (b"/root/.cargo/x", true),
            (b"\0\0\0\0\0\0\0\0\0\0/root", true),
            (b"x \"/root\" y", true),
            (b"/root\xc1", true),
            // The byte before never rules a hit out: a flag, or a length in crate metadata.
            (b"-I/root/include", true),
            (b"\x2e/root/project/src/lib.rs\xc1", true),
            (b"/rootfs/etc and /roots.rs", false),
            (b"/root2 /root.d /root_ /root- /root~", false),
            (b"/rootb then /root:", true),
        ];
        for (text, whole) in cases {
            let mut bytes = filler(13);
            bytes.extend_from_slice(text);
            for at_end in [false, true] {
                let bytes = if at_end {
                    bytes.clone()
                } else {
                    let mut b = bytes.clone();
                    b.extend(filler(11));
                    b
                };
                assert_eq!(n.all_in_bytes(&bytes), [whole, true], "{text:?}");
                assert_eq!(n.all_in_bytes(text), [whole, true], "{text:?} alone");
                let path = write(dir.path(), "out", &bytes);
                for chunk in [1, 2, 3, 5, 6, 7, 16, 1 << 20] {
                    let (found, sha, _) = n.clone_with(chunk).all_in_file_hashed(&path).unwrap();
                    assert_eq!(found, [whole, true], "{text:?} chunk {chunk}");
                    assert_eq!(sha, Digest::of(&bytes));
                }
            }
        }
        assert!(holds_path(b"a /root/b", b"/root"));
        assert!(!holds_path(b"a/rooted", b"/root"));
        assert!(!holds_path(b"anything", b""));
    }

    // covers: B-10
    #[test]
    fn finds_a_needle_at_the_start_and_at_the_very_end() {
        let dir = tempfile::tempdir().unwrap();
        let root = b"/home/dev/work/tree-a";
        let n = needles(&[root]).with_chunk(16);

        let mut start = root.to_vec();
        start.extend(filler(100));
        let path = write(dir.path(), "start", &start);
        let hit = Found {
            needle: 0,
            offset: 0,
        };
        assert_eq!(n.in_bytes(&start), Some(hit));
        assert_eq!(n.in_file(&path).unwrap(), Some(hit));

        let mut end = filler(100);
        end.extend_from_slice(root);
        let path = write(dir.path(), "end", &end);
        let hit = Found {
            needle: 0,
            offset: 100,
        };
        assert_eq!(n.in_bytes(&end), Some(hit));
        assert_eq!(n.in_file(&path).unwrap(), Some(hit));
        assert_eq!(n.in_file_hashed(&path).unwrap().0, Some(hit));
    }

    // covers: B-10
    #[test]
    fn finds_a_needle_across_every_position_of_a_chunk_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let root: &[u8] = b"/private/var/folders/xy/target";
        for chunk in [1usize, 2, 3, 7, 16, 29, 30, 31, 64] {
            let n = needles(&[root]).with_chunk(chunk);
            // Put the needle at every offset around the second boundary.
            for at in 0..chunk * 2 + root.len() + 2 {
                let mut bytes = filler(at);
                bytes.extend_from_slice(root);
                bytes.extend(filler(chunk + 3));
                let path = write(dir.path(), "f", &bytes);
                let want = Some(Found {
                    needle: 0,
                    offset: at as u64,
                });
                assert_eq!(n.in_file(&path).unwrap(), want, "chunk {chunk}, at {at}");
                let (found, _, size) = n.in_file_hashed(&path).unwrap();
                assert_eq!(found, want, "hashed, chunk {chunk}, at {at}");
                assert_eq!(size, bytes.len() as u64);
            }
        }
    }

    // covers: B-10
    #[test]
    fn a_needle_that_is_a_prefix_of_another_is_found_and_both_count() {
        let dir = tempfile::tempdir().unwrap();
        let short: &[u8] = b"/home/me/src/app";
        let long: &[u8] = b"/home/me/src/app-worktree";
        let n = needles(&[long, short]).with_chunk(5);

        // Only the short one: the long needle's searcher must not hide it.
        let mut bytes = filler(40);
        bytes.extend_from_slice(short);
        bytes.extend(filler(40));
        let path = write(dir.path(), "short", &bytes);
        let want = Some(Found {
            needle: 1,
            offset: 40,
        });
        assert_eq!(n.in_bytes(&bytes), want);
        assert_eq!(n.in_file(&path).unwrap(), want);

        // The long one: both match at the same offset. In memory the needle kept first wins;
        // a file read in chunks sees the short one whole first, and either is a hit.
        let mut bytes = filler(40);
        bytes.extend_from_slice(long);
        let path = write(dir.path(), "long", &bytes);
        let want = Some(Found {
            needle: 0,
            offset: 40,
        });
        assert_eq!(n.in_bytes(&bytes), want);
        assert_eq!(n.in_file(&path).unwrap().map(|f| f.offset), Some(40));
        // Read in one chunk, the file gives what memory gives.
        let whole = needles(&[long, short]);
        assert_eq!(whole.in_file(&path).unwrap(), want);
    }

    // covers: B-10
    #[test]
    fn a_needle_longer_than_the_chunk_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let root: Vec<u8> = b"/Volumes/Data/"
            .iter()
            .copied()
            .cycle()
            .take(150)
            .collect();
        let n = Needles::new(vec![root.clone()]).with_chunk(8);
        for at in [0usize, 1, 7, 8, 9, 151, 300] {
            let mut bytes = filler(at);
            bytes.extend_from_slice(&root);
            bytes.extend(filler(5));
            let path = write(dir.path(), "f", &bytes);
            assert_eq!(
                n.in_file(&path).unwrap(),
                Some(Found {
                    needle: 0,
                    offset: at as u64
                }),
                "at {at}"
            );
        }
    }

    // covers: B-10
    #[test]
    fn an_empty_file_holds_nothing_and_hashes_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "empty", b"");
        let n = needles(&[b"/a/b"]);
        assert_eq!(n.in_file(&path).unwrap(), None);
        let (found, digest, size) = n.in_file_hashed(&path).unwrap();
        assert_eq!((found, digest, size), (None, Digest::of(b""), 0));
    }

    // covers: B-10
    #[test]
    fn a_near_miss_is_not_a_hit() {
        let dir = tempfile::tempdir().unwrap();
        let root: &[u8] = b"/home/dev/work/tree-a";
        let n = needles(&[root]).with_chunk(4);
        // Each byte of the root changed in turn, and the root cut short by one at the end.
        for i in 0..root.len() {
            let mut near = root.to_vec();
            near[i] ^= 1;
            let mut bytes = filler(9);
            bytes.extend_from_slice(&near);
            bytes.extend(filler(9));
            let path = write(dir.path(), "f", &bytes);
            assert_eq!(n.in_bytes(&bytes), None, "byte {i}");
            assert_eq!(n.in_file(&path).unwrap(), None, "byte {i}");
        }
        let mut bytes = filler(9);
        bytes.extend_from_slice(&root[..root.len() - 1]);
        let path = write(dir.path(), "cut", &bytes);
        assert_eq!(n.in_file(&path).unwrap(), None);
    }

    #[test]
    fn empty_and_repeated_needles_are_dropped() {
        let n = Needles::new(vec![
            b"/a".to_vec(),
            Vec::new(),
            b"/b".to_vec(),
            b"/a".to_vec(),
        ]);
        let kept: Vec<&[u8]> = n.needles().collect();
        assert_eq!(kept, vec![b"/a".as_slice(), b"/b".as_slice()]);
        assert_eq!(
            n.in_bytes(b"xx/b"),
            Some(Found {
                needle: 1,
                offset: 2
            })
        );
        assert_eq!(Needles::new(Vec::new()).in_bytes(b"/a"), None);
    }

    #[test]
    fn the_earliest_hit_wins_whatever_the_needle_order() {
        let n = needles(&[b"/late", b"/early"]);
        assert_eq!(
            n.in_bytes(b"..../early..../late"),
            Some(Found {
                needle: 1,
                offset: 4
            })
        );
    }

    // covers: B-10
    #[test]
    fn the_one_pass_hash_equals_hash_file_and_runs_to_the_end_after_a_hit() {
        let dir = tempfile::tempdir().unwrap();
        let root: &[u8] = b"/home/dev/work/tree-a";
        for len in [1usize, 1000, (1 << 20) + 13, 3 * (1 << 20) + 5] {
            let mut bytes = root.to_vec();
            bytes.extend((0..len).map(|i| (i * 31 % 251) as u8));
            let path = write(dir.path(), "f", &bytes);
            let (want, want_size) = hushrun_digest::hash::hash_file(&path).unwrap();
            for chunk in [7usize, CHUNK] {
                let n = needles(&[root]).with_chunk(chunk);
                let (found, digest, size) = n.in_file_hashed(&path).unwrap();
                assert_eq!(found.map(|f| f.offset), Some(0));
                assert_eq!(digest, Digest::from_bytes(want), "len {len}, chunk {chunk}");
                assert_eq!(size, want_size);
            }
        }
    }

    #[test]
    fn a_missing_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let n = needles(&[b"/a"]);
        assert!(n.in_file(&dir.path().join("gone")).is_err());
        assert!(n.in_file_hashed(&dir.path().join("gone")).is_err());
    }

    /// Run by hand: `cargo test -p hushrun-build --release --locked --offline -- --ignored
    /// --nocapture scan::tests::throughput`. Prints MB/s; never fails on a slow machine.
    #[test]
    #[ignore]
    fn throughput() {
        use std::time::Instant;
        let dir = tempfile::tempdir().unwrap();
        let mb = 512usize;
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut bytes = vec![0u8; mb << 20];
        for word in bytes.chunks_mut(8) {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            word.copy_from_slice(&state.to_le_bytes()[..word.len()]);
        }
        let path = write(dir.path(), "big", &bytes);
        drop(bytes);
        let n = Needles::new(
            [
                "/home/dev/Documents/project/.wt/feature-branch",
                "/private/home/dev/Documents/project/.wt/feature-branch",
                "/home/dev/Documents/project/.wt/feature-branch/crates/app",
                "/private/home/dev/Documents/project/.wt/feature-branch/crates/app",
                "/home/dev/Documents/project/.wt/feature-branch/target/debug/deps",
                "/private/home/dev/Documents/project/.wt/feature-branch/target/debug/deps",
                "/home/dev/Documents/project/.wt/feature-branch/target/debug/build/app-0123456789abcdef/out",
                "/private/home/dev/Documents/project/.wt/feature-branch/target/debug/build/app-0123456789abcdef/out",
            ]
            .iter()
            .map(|s| s.as_bytes().to_vec())
            .collect(),
        );
        // Warm the page cache, so the numbers are the search and the hash, not the disk.
        let _ = hushrun_digest::hash::hash_file(&path).unwrap();
        let start = Instant::now();
        assert_eq!(n.in_file(&path).unwrap(), None);
        let search = start.elapsed().as_secs_f64();
        let start = Instant::now();
        let (found, _, _) = n.in_file_hashed(&path).unwrap();
        assert_eq!(found, None);
        let hashed = start.elapsed().as_secs_f64();
        let start = Instant::now();
        let _ = hushrun_digest::hash::hash_file(&path).unwrap();
        let hash_only = start.elapsed().as_secs_f64();
        println!(
            "{mb} MiB, 8 needles: search {:.0} MB/s, search + SHA-256 {:.0} MB/s, SHA-256 alone {:.0} MB/s",
            mb as f64 * 1.048_576 / search,
            mb as f64 * 1.048_576 / hashed,
            mb as f64 * 1.048_576 / hash_only,
        );
    }
}

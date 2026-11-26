//! K-6: a package store as one ustar archive, every entry under `hushrun-packages/`, so that
//! `tar -x -C /` puts it where a run reads one. The same store gives the same bytes on any
//! machine: entries sorted by path as bytes, each folder above a file once, folders `0755`, files
//! `0644` or `0755` when a row says it may run, links as ustar links (`typeflag` `2`), owner 0,
//! time 0.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use crate::store::{self, Depth, Kind, PACKAGES_JSON, PARTS, Packages, ROWS};

/// The folder every entry is under.
pub const ROOT: &str = "hushrun-packages";

enum Entry {
    Dir,
    File { from: std::path::PathBuf, exe: bool },
    Link(String),
}

/// Writes the package store at `folder` (checked by names, K-1) as an archive to `out`.
pub fn write(folder: &Path, out: &mut dyn Write) -> io::Result<()> {
    let (packages, _) = Packages::read_bytes(folder).map_err(|e| io::Error::other(e.0))?;
    let all_rows = store::check_contents(folder, Some(&packages), Depth::Names)
        .map_err(|e| io::Error::other(e.0))?;
    let mut entries: Vec<(String, Entry)> = Vec::new();
    let mut folders = std::collections::BTreeSet::new();
    let file = |path: String, from: std::path::PathBuf, exe: bool, entries: &mut Vec<_>| {
        entries.push((path, Entry::File { from, exe }));
    };
    file(
        format!("{ROOT}/{PACKAGES_JSON}"),
        folder.join(PACKAGES_JSON),
        false,
        &mut entries,
    );
    if folder.join(store::SIGNATURE).is_file() {
        file(
            format!("{ROOT}/{}", store::SIGNATURE),
            folder.join(store::SIGNATURE),
            false,
            &mut entries,
        );
    }
    for (part, rows) in packages.parts.iter().zip(&all_rows) {
        let name = format!("{}.jsonl", part.part);
        file(
            format!("{ROOT}/{ROWS}/{name}"),
            folder.join(ROWS).join(&name),
            false,
            &mut entries,
        );
        for row in rows {
            let path = format!("{ROOT}/{PARTS}/{}/{}", part.part, row.path);
            let from = store::part_dir(folder, &part.part).join(&row.path);
            match row.kind {
                Kind::Link => {
                    entries.push((path, Entry::Link(row.target.clone().unwrap_or_default())))
                }
                Kind::Exe => file(path, from, true, &mut entries),
                Kind::File => file(path, from, false, &mut entries),
            }
        }
    }
    for (path, _) in &entries {
        let mut at = 0;
        while let Some(slash) = path[at..].find('/') {
            folders.insert(path[..at + slash].to_string());
            at += slash + 1;
        }
    }
    entries.extend(folders.into_iter().map(|f| (f, Entry::Dir)));
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    for (path, entry) in entries {
        match entry {
            Entry::Dir => out.write_all(&header(&format!("{path}/"), 0o755, 0, b'5', "")?)?,
            Entry::Link(target) => out.write_all(&header(&path, 0o777, 0, b'2', &target)?)?,
            Entry::File { from, exe } => {
                let bytes = fs::read(&from)?;
                let mode = if exe { 0o755 } else { 0o644 };
                out.write_all(&header(&path, mode, bytes.len() as u64, b'0', "")?)?;
                out.write_all(&bytes)?;
                let pad = (512 - bytes.len() % 512) % 512;
                out.write_all(&vec![0u8; pad])?;
            }
        }
    }
    out.write_all(&[0u8; 1024])
}

/// One ustar header.
fn header(name: &str, mode: u32, size: u64, kind: u8, link: &str) -> io::Result<[u8; 512]> {
    let (prefix, base) = split(name)
        .ok_or_else(|| io::Error::other(format!("{name} is too long for a ustar header")))?;
    if link.len() > 100 {
        return Err(io::Error::other(format!(
            "{name}'s target is too long for a ustar header"
        )));
    }
    let mut h = [0u8; 512];
    h[..base.len()].copy_from_slice(base.as_bytes());
    let octal = |h: &mut [u8; 512], at: usize, width: usize, value: u64| {
        let text = format!("{value:0w$o}", w = width - 1);
        h[at..at + width - 1].copy_from_slice(text.as_bytes());
    };
    octal(&mut h, 100, 8, u64::from(mode));
    octal(&mut h, 108, 8, 0);
    octal(&mut h, 116, 8, 0);
    octal(&mut h, 124, 12, size);
    octal(&mut h, 136, 12, 0);
    h[148..156].copy_from_slice(b"        ");
    h[156] = kind;
    h[157..157 + link.len()].copy_from_slice(link.as_bytes());
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..154].copy_from_slice(format!("{sum:06o}").as_bytes());
    h[154] = 0;
    h[155] = b' ';
    Ok(h)
}

/// A name's ustar `prefix` and `name`, split at a `/` as GNU tar does.
fn split(name: &str) -> Option<(&str, &str)> {
    if name.len() <= 100 {
        return Some(("", name));
    }
    let body = name.strip_suffix('/').unwrap_or(name);
    let at = body
        .bytes()
        .enumerate()
        .find(|&(at, b)| b == b'/' && name.len() - at - 1 <= 100 && at <= 155)?
        .0;
    Some((&name[..at], &name[at + 1..]))
}

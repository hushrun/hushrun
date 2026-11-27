//! K-20: unpacking an npm tarball (a gzip stream of a ustar archive) the way a package manager's
//! cache holds it: the first component of every path dropped (npm's `package/`), regular files
//! and folders only. A link, a device, a path that climbs out or a header this reader doesn't
//! know refuses the whole tarball: the tool then downloads the package, as it does today.

use std::fs;
use std::io::{self, Read as _};
use std::path::Path;

/// Why a tarball can't be unpacked.
fn refused(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.into())
}

/// A header field: the bytes up to the first NUL.
fn field(block: &[u8]) -> &[u8] {
    let end = block.iter().position(|b| *b == 0).unwrap_or(block.len());
    &block[..end]
}

/// An octal header number, spaces and NULs around it allowed.
fn octal(block: &[u8]) -> io::Result<u64> {
    let text =
        std::str::from_utf8(field(block)).map_err(|_| refused("a size that isn't a number"))?;
    let text = text.trim();
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text, 8).map_err(|_| refused(format!("a number that isn't octal: {text}")))
}

/// The value of `path` in a pax extended header's records (`<len> path=<value>\n`), if any.
fn pax_path(records: &[u8]) -> io::Result<Option<String>> {
    let mut rest = records;
    let mut path = None;
    while !rest.is_empty() {
        let space = rest
            .iter()
            .position(|b| *b == b' ')
            .ok_or_else(|| refused("a pax record with no length"))?;
        let len: usize = std::str::from_utf8(&rest[..space])
            .ok()
            .and_then(|t| t.parse().ok())
            .filter(|len| *len > space && *len <= rest.len())
            .ok_or_else(|| refused("a pax record whose length is wrong"))?;
        let record = &rest[space + 1..len];
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        if let Some(value) = record.strip_prefix(b"path=") {
            path = Some(
                String::from_utf8(value.to_vec())
                    .map_err(|_| refused("a pax path that isn't UTF-8"))?,
            );
        }
        rest = &rest[len..];
    }
    Ok(path)
}

/// The path under the package's folder an entry named `name` goes to: `None` for the first
/// component alone. A path that climbs out, is absolute or has a backslash or a control character
/// refuses the tarball.
fn inner(name: &str) -> io::Result<Option<String>> {
    let name = name.trim_start_matches("./");
    if name.starts_with('/') || name.contains('\\') || name.chars().any(char::is_control) {
        return Err(refused(format!("an entry named {name:?}")));
    }
    let mut parts = name.split('/').filter(|c| !c.is_empty() && *c != ".");
    parts.next();
    let rest: Vec<&str> = parts.collect();
    if rest.contains(&"..") {
        return Err(refused(format!("an entry that climbs out: {name:?}")));
    }
    Ok((!rest.is_empty()).then(|| rest.join("/")))
}

/// Unpacks the gzip'd ustar archive `bytes` into the folder `into`, which must not exist yet: the
/// first component of every path dropped, files `0644`, or `0755` when the archive lets anyone
/// run them, folders `0755`.
pub fn unpack(bytes: &[u8], into: &Path) -> io::Result<()> {
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(bytes).read_to_end(&mut tar)?;
    fs::create_dir(into)?;
    let mut at = 0usize;
    let mut long_name: Option<String> = None;
    while at + 512 <= tar.len() {
        let header = &tar[at..at + 512];
        at += 512;
        if header.iter().all(|b| *b == 0) {
            break;
        }
        let size = usize::try_from(octal(&header[124..136])?)
            .map_err(|_| refused("an entry too large"))?;
        let data_end = at
            .checked_add(size)
            .filter(|end| *end <= tar.len())
            .ok_or_else(|| refused("an entry longer than the archive"))?;
        let data = &tar[at..data_end];
        at = data_end.div_ceil(512) * 512;
        let kind = header[156];
        let name = match long_name.take() {
            Some(name) => name,
            None => {
                let base = String::from_utf8(field(&header[0..100]).to_vec())
                    .map_err(|_| refused("a name that isn't UTF-8"))?;
                let prefix = field(&header[345..500]);
                if &header[257..262] == b"ustar" && !prefix.is_empty() {
                    let prefix = String::from_utf8(prefix.to_vec())
                        .map_err(|_| refused("a name that isn't UTF-8"))?;
                    format!("{prefix}/{base}")
                } else {
                    base
                }
            }
        };
        match kind {
            // A pax header for the next entry, or for them all: only its path matters here.
            b'x' => long_name = pax_path(data)?,
            b'g' => {}
            // GNU's long name for the next entry.
            b'L' => {
                long_name = Some(
                    String::from_utf8(field(data).to_vec())
                        .map_err(|_| refused("a name that isn't UTF-8"))?,
                );
            }
            b'0' | 0 | b'7' => {
                let Some(path) = inner(&name)? else {
                    continue;
                };
                let to = into.join(&path);
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&to, data)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    let exe = octal(&header[100..108])? & 0o111 != 0;
                    fs::set_permissions(
                        &to,
                        fs::Permissions::from_mode(if exe { 0o755 } else { 0o644 }),
                    )?;
                }
            }
            b'5' => {
                if let Some(path) = inner(&name)? {
                    fs::create_dir_all(into.join(path))?;
                }
            }
            other => {
                return Err(refused(format!(
                    "an entry of a kind a package's cache doesn't hold ({}): {name}",
                    char::from(other)
                )));
            }
        }
    }
    Ok(())
}

/// A gzip'd ustar archive of `entries` (name, typeflag, bytes, mode), for tests.
#[cfg(test)]
pub(crate) fn tgz(entries: &[(&str, u8, &[u8], u32)]) -> Vec<u8> {
    tests::tgz(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// A ustar header for `name`, of `kind`, `size` bytes, mode `mode`.
    fn header(name: &str, kind: u8, size: usize, mode: u32) -> [u8; 512] {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(format!("{mode:07o}").as_bytes());
        h[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
        h[156] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        h
    }

    pub(super) fn tgz(entries: &[(&str, u8, &[u8], u32)]) -> Vec<u8> {
        let mut tar = Vec::new();
        for (name, kind, data, mode) in entries {
            tar.extend_from_slice(&header(name, *kind, data.len(), *mode));
            tar.extend_from_slice(data);
            tar.resize(tar.len().div_ceil(512) * 512, 0);
        }
        tar.extend_from_slice(&[0u8; 1024]);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar).unwrap();
        gz.finish().unwrap()
    }

    // covers: K-20
    #[test]
    fn a_tarball_unpacks_without_its_first_folder_and_keeps_only_who_may_run_a_file() {
        let t = tempfile::tempdir().unwrap();
        let into = t.path().join("chalk@4.1.2@@@1");
        let long = format!("package/{}/x.js", "d".repeat(120));
        let body = "path=package/from-pax.txt\n";
        let pax = format!("{} {body}", body.len() + 3).into_bytes();
        let bytes = tgz(&[
            ("package/package.json", b'0', b"{}", 0o644),
            ("package/bin/cli.js", b'0', b"#!", 0o775),
            ("package/lib", b'5', b"", 0o755),
            ("././@LongLink", b'L', long.as_bytes(), 0o644),
            ("ignored-name", b'0', b"long", 0o600),
            ("PaxHeader", b'x', &pax, 0o644),
            ("package/short", b'0', b"pax", 0o644),
        ]);
        unpack(&bytes, &into).unwrap();
        assert_eq!(fs::read(into.join("package.json")).unwrap(), b"{}");
        assert!(into.join("lib").is_dir());
        assert_eq!(
            fs::read(into.join(format!("{}/x.js", "d".repeat(120)))).unwrap(),
            b"long"
        );
        assert_eq!(fs::read(into.join("from-pax.txt")).unwrap(), b"pax");
        assert!(!into.join("short").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |p: &str| fs::metadata(into.join(p)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode("bin/cli.js"), 0o755);
            assert_eq!(mode("package.json"), 0o644);
        }
    }

    // covers: K-20
    #[test]
    fn a_link_a_climb_or_a_broken_stream_refuses_the_tarball() {
        let t = tempfile::tempdir().unwrap();
        for (n, bytes) in [
            tgz(&[("package/a", b'2', b"", 0o644)]),
            tgz(&[("package/../../etc/x", b'0', b"x", 0o644)]),
            tgz(&[("/abs/x", b'0', b"x", 0o644)]),
            b"not gzip".to_vec(),
        ]
        .into_iter()
        .enumerate()
        {
            assert!(
                unpack(&bytes, &t.path().join(n.to_string())).is_err(),
                "{n}"
            );
        }
    }
}

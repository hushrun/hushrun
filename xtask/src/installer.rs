//! `cargo xtask installer-pins`: writes a release's archive checksums into
//! `recipes/gitlab/install-hushrun.sh`, between its `hushrun-pins` markers (plan §14, ADR 0019).
//!
//! The release workflow attaches the result to each release, and maintainers commit it after a
//! release, so the script in the repository lists every release. Adopters vendor a copy attached
//! to a release: it checks the releases it lists against these pins, and others, such as newer
//! ones, against the checksum published with them (ADR 0035).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const SCRIPT: &str = "recipes/gitlab/install-hushrun.sh";
const BEGIN: &str = "# hushrun-pins: begin";
const END: &str = "# hushrun-pins: end";
/// The archives the script installs: Linux's static musl builds and macOS's.
pub const TARGETS: [&str; 4] = [
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
];

pub fn run(root: &Path, args: &[String]) -> Result<bool> {
    let mut version = None;
    let mut from = None;
    let mut script = root.join(SCRIPT);
    let mut out = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().with_context(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--version" => version = Some(value("--version")?.clone()),
            "--from" => from = Some(PathBuf::from(value("--from")?)),
            "--script" => script = PathBuf::from(value("--script")?),
            "--out" => out = Some(PathBuf::from(value("--out")?)),
            other => bail!("unexpected argument {other:?}"),
        }
    }
    let version = version.context("--version VERSION names the release, such as 0.9.0")?;
    let from = from.context(
        "--from DIR holds the release's checksum files (gh release download vVERSION --repo hushrun/hushrun --pattern '*.sha256' --dir DIR)",
    )?;
    let checksums = read_checksums(&from)?;
    let text =
        fs::read_to_string(&script).with_context(|| format!("reading {}", script.display()))?;
    let pinned = pin(&text, &version, &checksums)?;
    let out = out.unwrap_or(script);
    fs::write(&out, pinned).with_context(|| format!("writing {}", out.display()))?;
    eprintln!(
        "pinned hushrun {version} for {} archives in {}",
        checksums.len(),
        out.display()
    );
    Ok(true)
}

/// The SHA-256 of each target's archive, from the release's checksum files
/// (`hushrun-<target>.tar.xz.sha256`, holding `<hex>  <name>`).
pub fn read_checksums(dir: &Path) -> Result<BTreeMap<&'static str, String>> {
    let mut checksums = BTreeMap::new();
    for target in TARGETS {
        let name = format!("hushrun-{target}.tar.xz");
        let path = dir.join(format!("{name}.sha256"));
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let mut fields = text.split_whitespace();
        let hex = fields.next().unwrap_or_default().to_ascii_lowercase();
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("{}: no SHA-256 at the start", path.display());
        }
        if let Some(file) = fields.next()
            && file.trim_start_matches('*') != name
        {
            bail!("{}: the checksum is for {file}, not {name}", path.display());
        }
        checksums.insert(target, hex);
    }
    Ok(checksums)
}

/// `script` with `version`'s pins replaced by `checksums`, first; other versions' pins kept.
pub fn pin(script: &str, version: &str, checksums: &BTreeMap<&str, String>) -> Result<String> {
    if !is_exact_version(version) {
        bail!("{version:?} is not an exact release such as 0.9.0");
    }
    let lines: Vec<&str> = script.lines().collect();
    let begin = lines
        .iter()
        .position(|l| l.trim_start().starts_with(BEGIN))
        .context("the script has no `# hushrun-pins: begin` marker")?;
    let end = lines
        .iter()
        .position(|l| l.trim_start().starts_with(END))
        .filter(|end| *end > begin)
        .context("the script has no `# hushrun-pins: end` marker after its begin marker")?;
    let indent: String = lines[begin]
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect();
    let mut out: Vec<String> = lines[..=begin].iter().map(|l| l.to_string()).collect();
    for target in TARGETS {
        if let Some(hex) = checksums.get(target) {
            out.push(format!("{indent}{version}/{target}) echo {hex} ;;"));
        }
    }
    let ours = format!("{version}/");
    out.extend(
        lines[begin + 1..end]
            .iter()
            .filter(|l| !l.trim_start().starts_with(&ours))
            .map(|l| l.to_string()),
    );
    out.extend(lines[end..].iter().map(|l| l.to_string()));
    let mut text = out.join("\n");
    if script.ends_with('\n') {
        text.push('\n');
    }
    Ok(text)
}

fn is_exact_version(version: &str) -> bool {
    let (core, rest) = match version.find(['-', '+']) {
        Some(at) => (&version[..at], Some(&version[at + 1..])),
        None => (version, None),
    };
    let numbers: Vec<&str> = core.split('.').collect();
    numbers.len() == 3
        && numbers
            .iter()
            .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        && rest.is_none_or(|r| {
            !r.is_empty()
                && r.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script() -> String {
        fs::read_to_string(crate::workspace_root().join(SCRIPT)).unwrap()
    }

    fn checksums(c: char) -> BTreeMap<&'static str, String> {
        TARGETS
            .iter()
            .map(|t| (*t, c.to_string().repeat(64)))
            .collect()
    }

    #[test]
    fn pins_go_between_the_markers_newest_first() {
        let once = pin(&script(), "0.9.0", &checksums('a')).unwrap();
        let twice = pin(&once, "0.10.0", &checksums('b')).unwrap();
        let begin = twice.find(BEGIN).unwrap();
        let end = twice.find(END).unwrap();
        let block = &twice[begin..end];
        let lines: Vec<&str> = block
            .lines()
            .skip(1)
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(lines.len(), 8, "{block}");
        assert_eq!(
            lines[0],
            format!(
                "0.10.0/x86_64-unknown-linux-musl) echo {} ;;",
                "b".repeat(64)
            )
        );
        assert!(lines[4].starts_with("0.9.0/x86_64-unknown-linux-musl) echo aaaa"));
        // Pinning a version again replaces its lines.
        let again = pin(&twice, "0.9.0", &checksums('c')).unwrap();
        assert_eq!(again.matches("0.9.0/").count(), 4);
        assert!(again.contains(&format!("echo {} ;;", "c".repeat(64))));
        assert!(!again.contains(&"a".repeat(64)));
        // The rest of the script is untouched.
        assert_eq!(again.lines().count(), script().lines().count() + 8);
        assert!(pin(&script(), "latest", &checksums('a')).is_err());
        assert!(pin("#!/bin/sh\n", "0.9.0", &checksums('a')).is_err());
    }

    #[test]
    fn checksum_files_must_name_their_archive() {
        let dir = tempfile::tempdir().unwrap();
        for target in TARGETS {
            fs::write(
                dir.path().join(format!("hushrun-{target}.tar.xz.sha256")),
                format!("{}  hushrun-{target}.tar.xz\n", "A".repeat(64)),
            )
            .unwrap();
        }
        let read = read_checksums(dir.path()).unwrap();
        assert_eq!(read.len(), 4);
        assert_eq!(read["aarch64-apple-darwin"], "a".repeat(64));
        fs::write(
            dir.path().join("hushrun-x86_64-apple-darwin.tar.xz.sha256"),
            format!("{} *hushrun-aarch64-apple-darwin.tar.xz\n", "a".repeat(64)),
        )
        .unwrap();
        assert!(
            read_checksums(dir.path())
                .unwrap_err()
                .to_string()
                .contains("not hushrun-x86_64-apple-darwin.tar.xz")
        );
        fs::write(
            dir.path().join("hushrun-x86_64-apple-darwin.tar.xz.sha256"),
            "abc\n",
        )
        .unwrap();
        assert!(read_checksums(dir.path()).is_err());
    }

    /// The script, run for real against a fake release on the loopback (Linux and macOS).
    #[cfg(unix)]
    mod script {
        use super::*;
        use sha2::Digest as _;
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        use std::process::Command;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};

        /// Serves `files` by path, and `redirects` as GitHub redirects `releases/latest` to the
        /// newest release's tag; counts the requests.
        struct Release {
            base: String,
            files: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
            redirects: Arc<Mutex<BTreeMap<String, String>>>,
            requests: Arc<AtomicUsize>,
        }

        impl Release {
            fn start() -> Release {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let base = format!("http://{}", listener.local_addr().unwrap());
                let files = Arc::new(Mutex::new(BTreeMap::<String, Vec<u8>>::new()));
                let redirects = Arc::new(Mutex::new(BTreeMap::<String, String>::new()));
                let requests = Arc::new(AtomicUsize::new(0));
                let (served, redirecting, counted) =
                    (files.clone(), redirects.clone(), requests.clone());
                std::thread::spawn(move || {
                    for stream in listener.incoming() {
                        let Ok(mut stream) = stream else { break };
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        let mut first = String::new();
                        if reader.read_line(&mut first).is_err() {
                            continue;
                        }
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).is_err()
                                || line == "\r\n"
                                || line.is_empty()
                            {
                                break;
                            }
                        }
                        counted.fetch_add(1, Ordering::SeqCst);
                        let path = first.split(' ').nth(1).unwrap_or("").to_string();
                        if let Some(location) = redirecting.lock().unwrap().get(&path) {
                            let _ = write!(
                                stream,
                                "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                            );
                            continue;
                        }
                        let body = served.lock().unwrap().get(&path).cloned();
                        let (status, body) = match body {
                            Some(body) => ("200 OK", body),
                            None => ("404 Not Found", b"not found".to_vec()),
                        };
                        let _ = write!(
                            stream,
                            "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(&body);
                    }
                });
                Release {
                    base,
                    files,
                    redirects,
                    requests,
                }
            }
        }

        fn target() -> &'static str {
            let os = match std::env::consts::OS {
                "linux" => "unknown-linux-musl",
                "macos" => "apple-darwin",
                other => panic!("no target for {other}"),
            };
            match (std::env::consts::ARCH, os) {
                ("x86_64", "unknown-linux-musl") => "x86_64-unknown-linux-musl",
                ("aarch64", "unknown-linux-musl") => "aarch64-unknown-linux-musl",
                ("x86_64", _) => "x86_64-apple-darwin",
                ("aarch64", _) => "aarch64-apple-darwin",
                (arch, _) => panic!("no target for {arch}"),
            }
        }

        /// A release archive whose `hushrun` prints `fake hushrun <version>`, and its SHA-256.
        fn archive(dir: &Path, version: &str) -> (Vec<u8>, String) {
            let name = format!("hushrun-{}", target());
            let inner = dir.join("pack").join(&name);
            fs::create_dir_all(&inner).unwrap();
            fs::write(
                inner.join("hushrun"),
                format!("#!/bin/sh\necho fake hushrun {version}\n"),
            )
            .unwrap();
            fs::write(inner.join("README.md"), "# hushrun\n").unwrap();
            let file = dir.join(format!("{name}.tar.xz"));
            let status = Command::new("tar")
                .arg("-cJf")
                .arg(&file)
                .arg("-C")
                .arg(dir.join("pack"))
                .arg(&name)
                .status()
                .unwrap();
            assert!(status.success(), "tar -cJf");
            let bytes = fs::read(&file).unwrap();
            let hex = hex::encode(sha2::Sha256::digest(&bytes));
            (bytes, hex)
        }

        fn install(script: &Path, vars: &[(&str, &str)]) -> (bool, String, String) {
            let mut cmd = Command::new("sh");
            cmd.arg(script);
            for var in [
                "HUSHRUN_VERSION",
                "HUSHRUN_SHA256",
                "HUSHRUN_DOWNLOAD_URL",
                "HUSHRUN_INSTALLER_GITHUB_BASE_URL",
                "HUSHRUN_INSTALL_DIR",
            ] {
                cmd.env_remove(var);
            }
            for (k, v) in vars {
                cmd.env(k, v);
            }
            let output = cmd.output().unwrap();
            (
                output.status.success(),
                String::from_utf8_lossy(&output.stdout).trim().to_string(),
                String::from_utf8_lossy(&output.stderr).into_owned(),
            )
        }

        #[test]
        fn installs_the_exact_release_it_can_verify() {
            let dir = tempfile::tempdir().unwrap();
            let (bytes, hex) = archive(dir.path(), "9.9.9");
            let release = Release::start();
            let archive_path = format!("/v9.9.9/hushrun-{}.tar.xz", target());
            release.files.lock().unwrap().insert(archive_path, bytes);
            let url = format!("{}/v9.9.9", release.base);
            let bin = dir.path().join("bin");
            let script = crate::workspace_root().join(SCRIPT);
            let vars = |sha: &str| {
                vec![
                    ("HUSHRUN_VERSION", "9.9.9".to_string()),
                    ("HUSHRUN_SHA256", sha.to_string()),
                    ("HUSHRUN_DOWNLOAD_URL", url.clone()),
                    ("HUSHRUN_INSTALL_DIR", bin.to_string_lossy().into_owned()),
                ]
            };
            let run = |vars: &[(&str, String)]| {
                let pairs: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
                install(&script, &pairs)
            };
            // With HUSHRUN_SHA256: installed, and its path printed.
            let (ok, path, stderr) = run(&vars(&hex));
            assert!(ok, "{stderr}");
            assert_eq!(path, bin.join("hushrun").to_string_lossy());
            let printed = Command::new(&path).output().unwrap();
            assert_eq!(
                String::from_utf8_lossy(&printed.stdout),
                "fake hushrun 9.9.9\n"
            );
            assert_eq!(release.requests.load(Ordering::SeqCst), 1);
            // Again: the cached archive is checked and used, with no download.
            let (ok, _, stderr) = run(&vars(&hex.to_uppercase()));
            assert!(ok, "{stderr}");
            assert_eq!(release.requests.load(Ordering::SeqCst), 1);
            // Another checksum: the cached archive doesn't match, and neither does the download.
            let wrong = "0".repeat(64);
            let (ok, _, stderr) = run(&vars(&wrong));
            assert!(!ok);
            assert!(
                stderr.contains(&format!("has SHA-256 {hex}, expected {wrong}")),
                "{stderr}"
            );
            assert!(
                !bin.join(format!("hushrun-{}.tar.xz.partial", target()))
                    .exists()
            );
            // [ADR 0035] A version this copy doesn't pin is checked against the checksum
            // published with the release, read again on every run.
            let archive_name = format!("hushrun-{}.tar.xz", target());
            let published = format!("/v9.9.9/{archive_name}.sha256");
            let unpinned = |sums: &str, into: &str| {
                release
                    .files
                    .lock()
                    .unwrap()
                    .insert(published.clone(), sums.as_bytes().to_vec());
                let mut vars = vars("");
                vars.retain(|(k, _)| *k != "HUSHRUN_SHA256");
                vars[2].1 = dir.path().join(into).to_string_lossy().into_owned();
                run(&vars)
            };
            let (ok, path, stderr) = unpinned(&format!("{hex} *{archive_name}\n"), "published");
            assert!(ok, "{stderr}");
            assert!(path.ends_with("published/hushrun"), "{path}");
            let (ok, _, stderr) = unpinned(&format!("{wrong} *{archive_name}\n"), "published");
            assert!(!ok);
            assert!(
                stderr.contains(&format!("has SHA-256 {hex}, expected {wrong}")),
                "{stderr}"
            );
            let (ok, _, stderr) = unpinned(&format!("{hex} *hushrun-other.tar.xz\n"), "other-sums");
            assert!(!ok);
            assert!(
                stderr.contains("is the checksum of hushrun-other.tar.xz, not of"),
                "{stderr}"
            );
            let (ok, _, stderr) = unpinned("nothing\n", "no-sums");
            assert!(
                !ok && stderr.contains("doesn't start with a SHA-256"),
                "{stderr}"
            );
            // A copy pinned by `installer-pins` needs nothing else.
            let pinned_script = dir.path().join("install-hushrun.sh");
            let mut sums: BTreeMap<&str, String> = checksums('f');
            sums.insert(target(), hex.clone());
            fs::write(&pinned_script, pin(&script_text(), "9.9.9", &sums).unwrap()).unwrap();
            let fresh = dir.path().join("fresh");
            let pairs = [
                ("HUSHRUN_VERSION", "9.9.9"),
                ("HUSHRUN_DOWNLOAD_URL", url.as_str()),
                ("HUSHRUN_INSTALL_DIR", fresh.to_str().unwrap()),
            ];
            let (ok, path, stderr) = install(&pinned_script, &pairs);
            assert!(ok, "{stderr}");
            assert!(path.ends_with("fresh/hushrun"), "{path}");
            // Inexact versions and malformed checksums are refused before any download, and so
            // is a checksum for whichever release is the newest.
            let requests = release.requests.load(Ordering::SeqCst);
            for (version, sha, needle) in [
                (
                    "latest",
                    hex.as_str(),
                    "HUSHRUN_SHA256 needs HUSHRUN_VERSION",
                ),
                (
                    "0.9",
                    hex.as_str(),
                    "must be a release such as 0.9.0, or latest",
                ),
                ("9.9.9", "abc", "64 hex digits"),
            ] {
                let mut bad = vars(sha);
                bad[0].1 = version.to_string();
                let (ok, _, stderr) = run(&bad);
                assert!(!ok && stderr.contains(needle), "{version} {sha}: {stderr}");
            }
            assert_eq!(release.requests.load(Ordering::SeqCst), requests);
            // A missing archive fails the download.
            let mut missing = vars(&hex);
            missing[2].1 = format!("{}/nowhere", release.base);
            missing[3].1 = dir.path().join("other").to_string_lossy().into_owned();
            let (ok, _, stderr) = run(&missing);
            assert!(!ok && stderr.contains("downloading"), "{stderr}");
        }

        // [ADR 0035] Without HUSHRUN_VERSION, the newest release: the one GitHub redirects
        // releases/latest to, whose archive and checksum come from its tag.
        #[test]
        fn installs_the_newest_release_by_default() {
            let dir = tempfile::tempdir().unwrap();
            let (bytes, hex) = archive(dir.path(), "9.9.9");
            let release = Release::start();
            let archive_name = format!("hushrun-{}.tar.xz", target());
            let download = "/hushrun/hushrun/releases/download/v9.9.9";
            release.files.lock().unwrap().extend([
                (format!("{download}/{archive_name}"), bytes),
                (
                    format!("{download}/{archive_name}.sha256"),
                    format!("{hex} *{archive_name}\n").into_bytes(),
                ),
            ]);
            let script = crate::workspace_root().join(SCRIPT);
            let into = |name: &str| dir.path().join(name).to_string_lossy().into_owned();
            let newest = |script: &Path, extra: &[(&str, &str)], name: &str| {
                let dir = into(name);
                let mut vars = vec![
                    ("HUSHRUN_INSTALLER_GITHUB_BASE_URL", release.base.as_str()),
                    ("HUSHRUN_INSTALL_DIR", dir.as_str()),
                ];
                vars.extend_from_slice(extra);
                install(script, &vars)
            };
            let requests = || release.requests.load(Ordering::SeqCst);

            // No redirect, no newest release to install.
            let (ok, _, stderr) = newest(&script, &[], "none");
            assert!(!ok);
            assert!(
                stderr.contains("can't tell the newest hushrun release from http://127.0.0.1:"),
                "{stderr}"
            );
            assert_eq!(requests(), 1);
            release.redirects.lock().unwrap().insert(
                "/hushrun/hushrun/releases/latest".into(),
                "https://github.com/hushrun/hushrun/releases/tag/v9.9.9".into(),
            );
            // The newest release, checked against the checksum published with it.
            for (extra, name) in [
                (&[][..], "default"),
                (&[("HUSHRUN_VERSION", "latest")][..], "latest"),
            ] {
                let before = requests();
                let (ok, path, stderr) = newest(&script, extra, name);
                assert!(ok, "{stderr}");
                assert!(path.ends_with(&format!("{name}/hushrun")), "{path}");
                assert!(stderr.contains("hushrun 9.9.9 for "), "{stderr}");
                let printed = Command::new(&path).output().unwrap();
                assert_eq!(
                    String::from_utf8_lossy(&printed.stdout),
                    "fake hushrun 9.9.9\n"
                );
                // The redirect, the checksum and the archive.
                assert_eq!(requests() - before, 3);
            }
            // A copy that pins the newest release checks it against its own pin.
            let pinned_script = dir.path().join("install-hushrun.sh");
            let mut sums: BTreeMap<&str, String> = checksums('f');
            sums.insert(target(), hex.clone());
            fs::write(&pinned_script, pin(&script_text(), "9.9.9", &sums).unwrap()).unwrap();
            let before = requests();
            let (ok, _, stderr) = newest(&pinned_script, &[], "pinned");
            assert!(ok, "{stderr}");
            assert_eq!(requests() - before, 2, "no checksum to download");
            // A mirror holds the releases it copied: it needs a version.
            let mirror = format!("{}/v9.9.9", release.base);
            let before = requests();
            let (ok, _, stderr) = newest(
                &script,
                &[("HUSHRUN_DOWNLOAD_URL", mirror.as_str())],
                "mirror",
            );
            assert!(
                !ok && stderr.contains("HUSHRUN_DOWNLOAD_URL needs HUSHRUN_VERSION"),
                "{stderr}"
            );
            // Over plain HTTP, only this machine is trusted to name a release.
            let (ok, _, stderr) = install(
                &script,
                &[
                    ("HUSHRUN_INSTALLER_GITHUB_BASE_URL", "http://github.com"),
                    ("HUSHRUN_INSTALL_DIR", into("plain").as_str()),
                ],
            );
            assert!(
                !ok && stderr.contains("must be an https:// URL, not 'http://github.com'"),
                "{stderr}"
            );
            assert_eq!(requests(), before);
        }

        fn script_text() -> String {
            super::script()
        }
    }
}

//! `cargo xtask jars [--check]`: builds the small JVM jars hushrun embeds (ADR 0088) from their
//! Java sources in this repository, byte for byte the same on every machine.
//!
//! Each jar's classes are compiled by `javac` [`JDK`] (from `JAVA_HOME`, else `PATH`) with
//! `--release 8`, so they load in any test or build JVM from Java 8 on, against API jars fetched
//! from Maven Central by exact version and checked against the SHA-256 pinned here. The API jars
//! are only compiled against: nothing of them goes into a jar, which holds our classes, the files
//! beside the sources (`META-INF/…`) and a manifest. The jar is a stored zip written here, not by
//! the `jar` tool: entries in byte order of their paths after the manifest, every timestamp
//! 1980-01-01 00:00, no extra fields, so the bytes depend on the sources and the compiler only.
//!
//! Without `--check` it writes each jar (whole: a temporary file, then a rename). With `--check` it
//! writes nothing, and fails when a jar in the repository isn't what this build makes: CI runs it
//! with the pinned JDK. The API jars are kept in `target/xtask-jars/` between runs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// The `javac -version` the checked-in jars were built with. Another javac may write other bytes.
pub const JDK: &str = "21.0.11";

/// A jar fetched from Maven Central to compile against: its path there and its SHA-256.
struct Api {
    path: &'static str,
    sha256: &'static str,
}

const CENTRAL: &str = "https://repo1.maven.org/maven2/";

const JUNIT_PLATFORM: &[Api] = &[
    Api {
        path: "org/junit/platform/junit-platform-launcher/1.9.3/junit-platform-launcher-1.9.3.jar",
        sha256: "8515e91808a09c8c9af5e359185b5991b5e76b8adcf185b9a3dab4ec8e5ba8ff",
    },
    Api {
        path: "org/junit/platform/junit-platform-engine/1.9.3/junit-platform-engine-1.9.3.jar",
        sha256: "0c39553d9a03510757227f5a1c6cc6530287b1a321ed6258450664874aa2a16a",
    },
    Api {
        path: "org/junit/platform/junit-platform-commons/1.9.3/junit-platform-commons-1.9.3.jar",
        sha256: "8519157df813c210e85fc1414b74109e3d85f43d7092563ed704c43c48f0d5e6",
    },
    Api {
        path: "org/opentest4j/opentest4j/1.2.0/opentest4j-1.2.0.jar",
        sha256: "58812de60898d976fb81ef3b62da05c6604c18fd4a249f5044282479fc286af2",
    },
    Api {
        path: "org/apiguardian/apiguardian-api/1.1.2/apiguardian-api-1.1.2.jar",
        sha256: "b509448ac506d607319f182537f0b35d71007582ec741832a1f111e5b5b70b38",
    },
    Api {
        path: "org/testng/testng/7.5.1/testng-7.5.1.jar",
        sha256: "a5ac92d2362ccb3a509abe68e385ca809a7c96fcbaf851b3ee8bacb2ac899e2f",
    },
];

const MAVEN: &[Api] = &[
    Api {
        path: "org/apache/maven/maven-core/3.6.3/maven-core-3.6.3.jar",
        sha256: "f572ed9e2a6ea3e30522d13448c5d9b3b80ce62a23dac652111952d4941e2a3b",
    },
    Api {
        path: "org/apache/maven/maven-model/3.6.3/maven-model-3.6.3.jar",
        sha256: "17cef1f58e146ef0d7d9e96b3b92d98a1d6fd7d2b3288ba538e8ff1e0d9160cf",
    },
    Api {
        path: "org/apache/maven/maven-artifact/3.6.3/maven-artifact-3.6.3.jar",
        sha256: "61b20da4e1e3db837630d0810a6a7a5ac9fb7ea31b0a55638062aad6e479c09c",
    },
    Api {
        path: "org/codehaus/plexus/plexus-utils/3.2.1/plexus-utils-3.2.1.jar",
        sha256: "8d07b497bb8deb167ee5329cae87ef2043833bf52e4f15a5a9379cec447a5b2b",
    },
    Api {
        path: "javax/inject/javax.inject/1/javax.inject-1.jar",
        sha256: "91c77044a50c481636c32d916fd89c9118a72195390452c81065080f957de7ff",
    },
];

/// A jar hushrun embeds: where its sources are, where it goes, and what it compiles against.
struct Jar {
    sources: &'static str,
    out: &'static str,
    api: &'static [Api],
    /// Lines its manifest adds, each ending in CRLF.
    manifest: &'static str,
}

const JARS: &[Jar] = &[
    // The test-JVM hook: a JUnit Platform filter and listener, and a TestNG interceptor.
    Jar {
        sources: "crates/hushrun/jvm/test-jvm-hook",
        out: "crates/hushrun/jvm/hushrun-test-jvm-hook.jar",
        api: JUNIT_PLATFORM,
        // Gradle's script also gives it to each test JVM as a Java agent.
        manifest: "Premain-Class: dev.hushrun.testjvm.Agent\r\n",
    },
    // The Maven core extension: the reactor's model, and the hook's list from hushrun.json.
    Jar {
        sources: "crates/hushrun/jvm/maven-extension",
        out: "crates/hushrun/jvm/hushrun-maven-extension.jar",
        api: MAVEN,
        manifest: "",
    },
];

pub fn run(root: &Path, args: &[String]) -> Result<bool> {
    let mut check = false;
    for arg in args {
        match arg.as_str() {
            "--check" => check = true,
            other => bail!("unexpected argument {other:?}"),
        }
    }
    let javac = javac()?;
    let work = root.join("target").join("xtask-jars");
    let mut same = true;
    for jar in JARS {
        let mut classpath = Vec::new();
        for api in jar.api {
            classpath.push(fetch(&work.join("api"), api)?);
        }
        let bytes = build(
            &javac,
            &root.join(jar.sources),
            &classpath,
            &work.join("build"),
            jar.manifest,
        )?;
        let out = root.join(jar.out);
        if check {
            let have = fs::read(&out).with_context(|| format!("reading {}", jar.out))?;
            if have == bytes {
                eprintln!("{}: the same bytes as its sources build", jar.out);
            } else {
                eprintln!(
                    "{}: not what its sources build (sha256 {} in the repository, {} built): run `cargo xtask jars` with JDK {JDK} and commit it",
                    jar.out,
                    hex(&Sha256::digest(&have)),
                    hex(&Sha256::digest(&bytes)),
                );
                same = false;
            }
        } else {
            write_whole(&out, &bytes)?;
            eprintln!(
                "{}: {} bytes, sha256 {}",
                jar.out,
                bytes.len(),
                hex(&Sha256::digest(&bytes))
            );
        }
    }
    Ok(same)
}

/// `javac` from `JAVA_HOME`, else `PATH`, when it is [`JDK`]'s.
fn javac() -> Result<PathBuf> {
    let javac = match std::env::var_os("JAVA_HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home).join("bin").join("javac"),
        _ => PathBuf::from("javac"),
    };
    let out = Command::new(&javac)
        .arg("-version")
        .output()
        .with_context(|| format!("starting {}: set JAVA_HOME to a JDK {JDK}", javac.display()))?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let version = said.trim().strip_prefix("javac ").unwrap_or(said.trim());
    if version != JDK {
        bail!(
            "{} is javac {version:?}: the jars are built with javac {JDK}, since another may write other bytes; set JAVA_HOME to a JDK {JDK}",
            javac.display()
        );
    }
    Ok(javac)
}

/// The API jar in `dir`, fetched from Maven Central when it isn't there, and checked.
fn fetch(dir: &Path, api: &Api) -> Result<PathBuf> {
    let name = api.path.rsplit('/').next().unwrap_or(api.path);
    let path = dir.join(name);
    if fs::read(&path).is_ok_and(|b| hex(&Sha256::digest(&b)) == api.sha256) {
        return Ok(path);
    }
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let part = dir.join(format!(".{name}.part"));
    let url = format!("{CENTRAL}{}", api.path);
    let status = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--output",
        ])
        .arg(&part)
        .arg(&url)
        .status()
        .context("starting curl")?;
    if !status.success() {
        bail!("fetching {url}: curl exited {status}");
    }
    let bytes = fs::read(&part)?;
    let got = hex(&Sha256::digest(&bytes));
    if got != api.sha256 {
        let _ = fs::remove_file(&part);
        bail!(
            "{url}: sha256 {got}, not the pinned {}: refusing it",
            api.sha256
        );
    }
    fs::rename(&part, &path)?;
    Ok(path)
}

/// The jar's bytes, compiled from `sources` in a fresh directory under `work`.
fn build(
    javac: &Path,
    sources: &Path,
    classpath: &[PathBuf],
    work: &Path,
    manifest: &str,
) -> Result<Vec<u8>> {
    let classes = work.join("classes");
    if classes.exists() {
        fs::remove_dir_all(&classes)?;
    }
    fs::create_dir_all(&classes)?;
    let mut files = Vec::new();
    walk(sources, sources, &mut files)?;
    files.sort();
    let java: Vec<&String> = files.iter().filter(|f| f.ends_with(".java")).collect();
    if java.is_empty() {
        bail!("{}: no Java sources", sources.display());
    }
    let sep = if cfg!(windows) { ";" } else { ":" };
    let cp = classpath
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(sep);
    let status = Command::new(javac)
        .args([
            "--release",
            "8",
            "-encoding",
            "UTF-8",
            "-g:source,lines",
            "-implicit:none",
            "-Xlint:all",
            "-Xlint:-options",
            "-Werror",
            "-classpath",
            &cp,
            "-d",
        ])
        .arg(&classes)
        .args(java.iter().map(|f| sources.join(f)))
        .status()
        .context("starting javac")?;
    if !status.success() {
        bail!("javac exited {status} for {}", sources.display());
    }
    let mut entries = Vec::new();
    let mut built = Vec::new();
    walk(&classes, &classes, &mut built)?;
    for f in built {
        entries.push((f.clone(), fs::read(classes.join(&f))?));
    }
    for f in files.iter().filter(|f| !f.ends_with(".java")) {
        entries.push((f.clone(), fs::read(sources.join(f))?));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    if entries.windows(2).any(|w| w[0].0 == w[1].0) {
        bail!("{}: two files at one path", sources.display());
    }
    let mut all = vec![(
        "META-INF/MANIFEST.MF".to_string(),
        format!("Manifest-Version: 1.0\r\nCreated-By: cargo xtask jars\r\n{manifest}\r\n")
            .into_bytes(),
    )];
    all.extend(entries);
    Ok(zip(&all))
}

/// Every file below `dir`, as `/`-separated paths from `base`.
fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            walk(base, &path, out)?;
        } else {
            let rel = path.strip_prefix(base)?;
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            if parts.iter().any(|p| p.starts_with('.')) {
                continue;
            }
            out.push(parts.join("/"));
        }
    }
    Ok(())
}

/// A zip of `entries`, stored, in their order, every timestamp 1980-01-01 00:00.
fn zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    const TIME: u16 = 0;
    const DATE: u16 = (1 << 5) | 1;
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let size = data.len() as u32;
        let name = name.as_bytes();
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&10u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0x0800u16.to_le_bytes()); // names in UTF-8
        out.extend_from_slice(&0u16.to_le_bytes()); // stored
        out.extend_from_slice(&TIME.to_le_bytes());
        out.extend_from_slice(&DATE.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(data);

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // made by
        central.extend_from_slice(&10u16.to_le_bytes());
        central.extend_from_slice(&0x0800u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&TIME.to_le_bytes());
        central.extend_from_slice(&DATE.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 8]); // extra, comment, disk, internal attributes
        central.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name);
    }
    let start = out.len() as u32;
    let count = entries.len() as u16;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// CRC-32 (IEEE), as zip has it.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn write_whole(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_file_name(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("jar")
    ));
    fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_is_zips() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn a_zip_lists_its_entries_in_order_with_fixed_times() {
        let z = zip(&[
            ("a".to_string(), b"x".to_vec()),
            ("b/c".to_string(), b"yz".to_vec()),
        ]);
        assert_eq!(&z[..4], &0x0403_4b50u32.to_le_bytes());
        // The end record: two entries.
        let end = z.len() - 22;
        assert_eq!(&z[end..end + 4], &0x0605_4b50u32.to_le_bytes());
        assert_eq!(u16::from_le_bytes([z[end + 10], z[end + 11]]), 2);
        assert_eq!(
            z,
            zip(&[("a".into(), b"x".to_vec()), ("b/c".into(), b"yz".to_vec())])
        );
    }
}

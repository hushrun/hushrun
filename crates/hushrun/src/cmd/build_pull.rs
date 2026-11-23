//! \[ADR 0107\] `hushrun build pull` (B-77): puts the repository's newest baked store where a run
//! reads one, for a sandbox whose image is fixed. It downloads the image the backend's registry
//! keeps for the repository and this platform (`<registry host>/<org>/<name>:hushrun-<platform>-latest`,
//! B-70), with a pull token (`HUSHRUN_REGISTRY_TOKEN`), the backend's credential here, or nothing
//! for a public repository; unpacks the store its layers hold under `hushrun/` into a folder beside
//! the destination; checks it (B-63's names and bytes, and, when `hushrun.yaml` names signers of
//! baked stores, its signature, B-67); and only then swaps it into place: `HUSHRUN_BUILD_LOWER`, or
//! `/hushrun`. A store that doesn't check out replaces nothing. Each run still verifies the store
//! as it begins, as it does any baked store.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use anyhow::anyhow;

use hushrun_build::baked::{self, Baked, Depth};
use hushrun_core::blocks::image::{self as tags, STORE_ROOT};

use crate::cli::{BuildPullArgs, GlobalOpts};
use crate::exit::{CmdResult, Exit, Failure};

/// The most bytes one layer may have: 8 GiB.
const MOST_LAYER: u64 = 8 << 30;

/// This machine's platform, in a tag's words (`linux-x64`).
fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "apple-darwin",
        "windows" => "pc-windows",
        os => os,
    };
    baked::platform_of(&format!("{}-unknown-{os}", std::env::consts::ARCH))
}

pub fn run(global: &GlobalOpts, args: &BuildPullArgs) -> CmdResult {
    let failed = |why: String| Failure::new(Exit::Unexpected, anyhow!("build pull: {why}"));
    let usage = |why: String| Failure::new(Exit::Usage, anyhow!("build pull: {why}"));
    let dest = args
        .to
        .clone()
        .or_else(|| baked::named(&|name| hushrun_ci::var_os(name)))
        .unwrap_or_else(|| PathBuf::from(baked::DEFAULT_LOWER));
    let root = crate::cmd::login::repository_root(global);
    let (image, credential) = image_and_credential(args, root.as_deref()).map_err(usage)?;
    let tag = args
        .tag
        .clone()
        .unwrap_or_else(|| tags::latest_tag(&args.platform.clone().unwrap_or_else(platform)));
    let mut insecure = Vec::new();
    if args.insecure {
        insecure.push(image.host.clone());
    }
    let artifacts = hushrun_artifact::Artifacts::connect_with(
        hushrun_artifact::credentials::DockerConfig::default(),
        &hushrun_artifact::Options {
            insecure_registries: insecure,
            credentials: vec![(image.host.clone(), credential)],
        },
    )
    .map_err(|e| failed(e.to_string()))?;
    let Some((digest, layers)) = artifacts
        .pull_layers(&image, &tag, MOST_LAYER)
        .map_err(|e| failed(format!("{image}:{tag}: {e}")))?
    else {
        return Err(failed(format!(
            "{image} has no {tag}: no baked store of this repository for this platform yet"
        )));
    };
    // Unpacked beside the destination, so the swap is a rename on one file system.
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|e| failed(format!("creating {}: {e}", parent.display())))?;
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "hushrun".into());
    let staging = parent.join(format!(".{name}.pull-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    let result = (|| -> Result<String, String> {
        for layer in &layers {
            untar(layer, &staging).map_err(|e| format!("unpacking {image}:{tag}: {e}"))?;
        }
        let store = staging.join(STORE_ROOT);
        let baked = Baked::read(&store).map_err(|why| format!("{tag}: {}", why.0))?;
        baked::check_contents(&store, Some(&baked), Depth::Bytes)
            .map_err(|why| format!("{tag}: {why}"))?;
        signature(global, &store)
    })();
    let said = match result {
        Ok(said) => said,
        Err(why) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(failed(why));
        }
    };
    swap(&staging.join(STORE_ROOT), &dest).map_err(|e| {
        let _ = fs::remove_dir_all(&staging);
        failed(format!("putting it at {}: {e}", dest.display()))
    })?;
    let _ = fs::remove_dir_all(&staging);
    if global.quiet == 0 {
        eprintln!(
            "hushrun: pulled {image}:{tag} ({}) into {}: {said}",
            short(&digest.to_string()),
            dest.display()
        );
    }
    Ok(Exit::Ok)
}

fn short(digest: &str) -> String {
    digest.chars().take(19).collect()
}

/// The image to pull and the credential to pull it with: `--image`, else the repository's on its
/// backend's registry; a pull token, else the backend's credential here, else none.
fn image_and_credential(
    args: &BuildPullArgs,
    root: Option<&Path>,
) -> Result<
    (
        hushrun_artifact::Repository,
        hushrun_artifact::credentials::Credential,
    ),
    String,
> {
    use hushrun_artifact::credentials::Credential;
    let basic = |token: String| Credential::Basic {
        username: "hushrun".into(),
        password: token,
    };
    let pull_token = hushrun_ci::var("HUSHRUN_REGISTRY_TOKEN")
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let backend = match crate::results::Backend::select(root) {
        Ok(crate::results::Selection::On(backend)) => Some(backend),
        _ => None,
    };
    let image = match &args.image {
        Some(image) => {
            hushrun_artifact::Repository::parse(image.trim()).map_err(|e| e.to_string())?
        }
        None => {
            let base = backend.as_ref().and_then(|b| b.build_place()).ok_or(
                "no backend names this repository's registry: pass --image <host>/<org>/<name>",
            )?;
            let repository = root
                .and_then(crate::cmd::setup::repository_id)
                .ok_or("this repository has no name the registry takes: pass --image")?;
            let name = hushrun_core::blocks::publish::image_name(&repository)
                .ok_or("this repository has no name the registry takes: pass --image")?;
            let host = crate::hosted::registry_host(&base.host);
            hushrun_artifact::Repository::parse(&format!("{host}/{}/{name}", base.org))
                .map_err(|e| e.to_string())?
        }
    };
    if let Some(token) = pull_token {
        return Ok((image, basic(token)));
    }
    // The backend's own credential goes only to its own registry.
    let token = backend
        .as_ref()
        .and_then(|b| b.build_access())
        .and_then(|access| {
            let registry = crate::hosted::registry_host(&access.host);
            match access.credential {
                Some(crate::results::BuildCredential::Token(token)) if registry == image.host => {
                    Some(token)
                }
                _ => None,
            }
        });
    Ok((image, token.map_or(Credential::Anonymous, basic)))
}

/// B-67: what the store's signature says, checked when `hushrun.yaml` names signers of baked
/// stores: one that doesn't verify is refused. Without signers, it says the store is unsigned
/// here, which a run then reads as B-63 says.
fn signature(global: &GlobalOpts, store: &Path) -> Result<String, String> {
    use crate::context::{Content, Source};
    let signers = Content::open(global, Source::Repo(hushrun_vcs::Snapshot::Worktree))
        .ok()
        .and_then(|content| content.policy().ok().map(|p| p.build_baked.clone()));
    match signers {
        Some(trust) if !trust.signers.is_empty() => {
            let root =
                crate::cmd::build_relay::bake_image::run_root(&|name| hushrun_ci::var_os(name));
            let verified = crate::cmd::build_relay::bake_image::verify_store(
                store,
                &trust.signers,
                root.as_ref(),
            )
            .map_err(|why| format!("its signature doesn't verify: {why}"))?;
            Ok(format!("signed by {}", verified.signer))
        }
        _ => Ok(
            "hushrun.yaml names no signer of baked stores, so a run reuses it as the backend vouches, or with HUSHRUN_BUILD_LOWER_UNTRUSTED=1".into(),
        ),
    }
}

/// Puts `from` at `to`: what was there goes aside, then away.
fn swap(from: &Path, to: &Path) -> io::Result<()> {
    let aside = to.with_extension(format!("old-{}", std::process::id()));
    let had = to.exists();
    if had {
        fs::rename(to, &aside)?;
    }
    if let Err(err) = fs::rename(from, to) {
        if had {
            let _ = fs::rename(&aside, to);
        }
        return Err(err);
    }
    if had {
        let _ = fs::remove_dir_all(&aside);
    }
    Ok(())
}

/// Unpacks the tar `bytes` into `into`: regular files and folders under `hushrun/` only, with no
/// `..`, no absolute path and nothing else (links, devices); ustar's prefix, GNU's long names and
/// PAX's `path` read.
fn untar(bytes: &[u8], into: &Path) -> io::Result<()> {
    let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, why.to_string());
    let mut at = 0usize;
    let mut long_name: Option<String> = None;
    while at + 512 <= bytes.len() {
        let header = &bytes[at..at + 512];
        if header.iter().all(|b| *b == 0) {
            break;
        }
        let field = |from: usize, len: usize| {
            let raw = &header[from..from + len];
            let end = raw.iter().position(|b| *b == 0).unwrap_or(len);
            String::from_utf8_lossy(&raw[..end]).into_owned()
        };
        let size = u64::from_str_radix(field(124, 12).trim(), 8).map_err(|_| bad("a size"))?;
        let size = usize::try_from(size).map_err(|_| bad("a size"))?;
        let kind = header[156];
        let data_at = at + 512;
        let data_end = data_at.checked_add(size).ok_or_else(|| bad("a size"))?;
        if data_end > bytes.len() {
            return Err(bad("a file runs past the layer"));
        }
        let data = &bytes[data_at..data_end];
        at = data_at + size.div_ceil(512) * 512;
        let name = match long_name.take() {
            Some(name) => name,
            None => {
                let prefix = field(345, 155);
                let name = field(0, 100);
                if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}/{name}")
                }
            }
        };
        match kind {
            // GNU's long name, for the next entry.
            b'L' => {
                long_name = Some(
                    String::from_utf8_lossy(data)
                        .trim_end_matches('\0')
                        .to_string(),
                );
                continue;
            }
            // PAX: its `path`, for the next entry; the rest isn't read.
            b'x' => {
                long_name = pax_path(data);
                continue;
            }
            b'g' => continue,
            _ => {}
        }
        let relative = safe(&name).ok_or_else(|| bad("a path outside the store"))?;
        if relative.components().next().map(|c| c.as_os_str()) != Some(STORE_ROOT.as_ref()) {
            // Anything else the image holds isn't the store's.
            continue;
        }
        let path = into.join(&relative);
        match kind {
            b'0' | 0 => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&path, data)?;
            }
            b'5' => fs::create_dir_all(&path)?,
            _ => return Err(bad("an entry that is neither a file nor a folder")),
        }
    }
    Ok(())
}

/// A PAX extended header's `path`.
fn pax_path(data: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(data);
    text.lines().find_map(|line| {
        let (_, record) = line.split_once(' ')?;
        record.strip_prefix("path=").map(str::to_string)
    })
}

/// `name` as a relative path with no `..`, root or prefix; `None` otherwise.
fn safe(name: &str) -> Option<PathBuf> {
    let path = Path::new(name.trim_start_matches("./"));
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ustar entry: its header and data, padded.
    fn entry(name: &str, kind: u8, data: &[u8]) -> Vec<u8> {
        let mut header = [0u8; 512];
        header[..name.len().min(100)].copy_from_slice(&name.as_bytes()[..name.len().min(100)]);
        header[100..107].copy_from_slice(b"0000644");
        header[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
        header[156] = kind;
        header[257..263].copy_from_slice(b"ustar\0");
        let mut out = header.to_vec();
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
        out
    }

    // covers: B-77
    #[test]
    fn a_layer_unpacks_the_store_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let long = format!("hushrun/hints/{}/{}", "a".repeat(64), "b".repeat(64));
        let mut tar = Vec::new();
        tar.extend(entry("hushrun/", b'5', b""));
        tar.extend(entry("hushrun/baked.json", b'0', b"{}"));
        tar.extend(entry("././@LongLink", b'L', long.as_bytes()));
        tar.extend(entry("ignored", b'0', b"hint"));
        tar.extend(entry("etc/passwd", b'0', b"not the store's"));
        tar.extend([0u8; 1024]);
        untar(&tar, dir.path()).unwrap();
        assert_eq!(
            fs::read(dir.path().join("hushrun/baked.json")).unwrap(),
            b"{}"
        );
        assert_eq!(fs::read(dir.path().join(&long)).unwrap(), b"hint");
        assert!(!dir.path().join("etc").exists());
        // A path out of the folder, or a link, stops it.
        for bad in [
            entry("hushrun/../../x", b'0', b"x"),
            entry("/hushrun/x", b'0', b"x"),
            entry("hushrun/link", b'2', b""),
        ] {
            assert!(untar(&bad, dir.path()).is_err());
        }
        assert_eq!(
            pax_path(b"30 path=hushrun/a/very/long/name\n"),
            Some("hushrun/a/very/long/name".into())
        );
        assert!(platform().contains('-'));
    }

    // covers: B-77
    #[test]
    fn a_swap_leaves_the_old_store_when_the_new_cant_go_in() {
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("hushrun");
        fs::create_dir_all(&to).unwrap();
        fs::write(to.join("old"), b"old").unwrap();
        let from = dir.path().join("new");
        fs::create_dir_all(&from).unwrap();
        fs::write(from.join("baked.json"), b"new").unwrap();
        swap(&from, &to).unwrap();
        assert_eq!(fs::read(to.join("baked.json")).unwrap(), b"new");
        assert!(!to.join("old").exists());
        // From nowhere: what was there stays.
        assert!(swap(&dir.path().join("missing"), &to).is_err());
        assert_eq!(fs::read(to.join("baked.json")).unwrap(), b"new");
    }
}

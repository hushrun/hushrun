//! \[ADR 0143\] `hushrun build pull` (B-240): puts the repository's newest baked store where a run
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
    let Some(manifest) = artifacts
        .tagged(&image, &tag)
        .map_err(|e| failed(format!("{image}:{tag}: {e}")))?
    else {
        return Err(failed(format!(
            "{image} has no {tag}: no baked store of this repository for this platform yet"
        )));
    };
    let digest = manifest.digest();
    let pulled = Pulled {
        image: image.to_string(),
        tag: tag.clone(),
        digest: digest.to_string(),
        to: dest.clone(),
    };
    if args.resolve {
        return said(global, &pulled, "resolved", None);
    }
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "hushrun".into());
    let record = parent.join(format!(".{name}.pulled"));
    // B-240: the store there is this image's, as the last pull put it: nothing is downloaded.
    // One whose signature no longer verifies (hushrun.yaml changed) is pulled again.
    if already(&record, &pulled, &dest).is_some()
        && let Ok(trust) = signature(global, &dest)
    {
        return said(global, &pulled, "unchanged", Some(&trust));
    }
    // Unpacked beside the destination, so the swap is a rename on one file system; the layers
    // go to files there as they arrive, and each goes once it is unpacked.
    fs::create_dir_all(&parent)
        .map_err(|e| failed(format!("creating {}: {e}", parent.display())))?;
    let staging = parent.join(format!(".{name}.pull-{}", std::process::id()));
    let downloads = parent.join(format!(".{name}.layers-{}", std::process::id()));
    let clean = || {
        let _ = fs::remove_dir_all(&staging);
        let _ = fs::remove_dir_all(&downloads);
    };
    clean();
    let result = (|| -> Result<String, String> {
        let layers = artifacts
            .pull_layers(&image, &manifest, MOST_LAYER, &downloads)
            .map_err(|e| format!("{image}:{tag}: {e}"))?;
        for layer in &layers {
            let file =
                fs::File::open(layer).map_err(|e| format!("reading {}: {e}", layer.display()))?;
            untar(io::BufReader::new(file), &staging)
                .map_err(|e| format!("unpacking {image}:{tag}: {e}"))?;
            let _ = fs::remove_file(layer);
        }
        let store = staging.join(STORE_ROOT);
        let baked = Baked::read(&store).map_err(|why| format!("{tag}: {}", why.0))?;
        baked::check_contents(&store, Some(&baked), Depth::Bytes)
            .map_err(|why| format!("{tag}: {why}"))?;
        signature(global, &store)
    })();
    let trust = match result {
        Ok(trust) => trust,
        Err(why) => {
            clean();
            return Err(failed(why));
        }
    };
    let _ = fs::remove_file(&record);
    swap(&staging.join(STORE_ROOT), &dest).map_err(|e| {
        clean();
        failed(format!("putting it at {}: {e}", dest.display()))
    })?;
    clean();
    // What a later pull compares with; without it, that pull downloads again.
    if let Some(baked) = baked_digest(&dest) {
        let _ = fs::write(&record, pulled.record(&baked).to_string());
    }
    said(global, &pulled, "pulled", Some(&trust))
}

/// What a pull read: the image, its tag, the digest the tag names, and where the store goes.
struct Pulled {
    image: String,
    tag: String,
    digest: String,
    to: PathBuf,
}

impl Pulled {
    /// What the pull keeps beside the store, `.<name>.pulled`.
    fn record(&self, baked: &str) -> serde_json::Value {
        serde_json::json!({
            "image": self.image,
            "tag": self.tag,
            "digest": self.digest,
            "baked": baked,
        })
    }
}

/// The digest of `store`'s `baked.json`.
fn baked_digest(store: &Path) -> Option<String> {
    let bytes = fs::read(store.join("baked.json")).ok()?;
    Some(hushrun_core::digest::Digest::of(&bytes).to_string())
}

/// Whether `dest` holds the store of the image `pulled` names already: the record the last pull
/// left says the same image and digest, `baked.json` is the one it put there, and the folder
/// still checks (B-63, names and bytes).
fn already(record: &Path, pulled: &Pulled, dest: &Path) -> Option<()> {
    let record: serde_json::Value = serde_json::from_slice(&fs::read(record).ok()?).ok()?;
    let baked = baked_digest(dest)?;
    if record != pulled.record(&baked) {
        return None;
    }
    let read = Baked::read(dest).ok()?;
    baked::check_contents(dest, Some(&read), Depth::Bytes).ok()
}

/// Says what happened (`pulled`, `unchanged`, or `resolved` with no download), as text on stderr
/// or `hushrun.build-pull/v1` on stdout.
fn said(global: &GlobalOpts, pulled: &Pulled, what: &str, trust: Option<&str>) -> CmdResult {
    use crate::output::Format;
    if global.format == Format::Json {
        crate::output::print_json(&serde_json::json!({
            "schema": "hushrun.build-pull/v1",
            "outcome": what,
            "image": pulled.image,
            "tag": pulled.tag,
            "digest": pulled.digest,
            "reference": format!("{}@{}", pulled.image, pulled.digest),
            "to": pulled.to.display().to_string(),
            "trust": trust,
        }))?;
        return Ok(Exit::Ok);
    }
    let (image, tag, digest, to) = (
        &pulled.image,
        &pulled.tag,
        &pulled.digest,
        pulled.to.display(),
    );
    match trust {
        // `--resolve` prints the reference, which is what it is asked for.
        None => crate::output::print(&format!("{image}@{digest}\n"))?,
        Some(_) if global.quiet > 0 => {}
        Some(trust) if what == "unchanged" => eprintln!(
            "hushrun: {to} holds {image}:{tag} ({digest}) already, so nothing was downloaded: {trust}"
        ),
        Some(trust) => eprintln!("hushrun: pulled {image}:{tag} ({digest}) into {to}: {trust}"),
    }
    Ok(Exit::Ok)
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
    // C-13: the environment's secret goes where someone named, never to a registry a
    // repository's file chose: `--image`, hushrun's own registry, or the registry of a backend the
    // environment names.
    let env_named = matches!(
        backend.as_ref().map(|b| b.place()),
        Some(crate::results::Place::Server {
            named_by: crate::results::NamedBy::Environment | crate::results::NamedBy::Agent,
            ..
        })
    );
    let hushruns = image.host == crate::hosted::registry_host(&crate::hosted::origin());
    if let Some(token) = pull_token {
        if args.image.is_some() || hushruns || env_named {
            return Ok((image, basic(token)));
        }
        return Err(format!(
            "HUSHRUN_REGISTRY_TOKEN goes only to a registry someone named, and {} is named by this repository's files: pass --image {image}, or set HUSHRUN_BACKEND",
            image.host
        ));
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

/// Unpacks the tar read from `from` into `into`, a file at a time: regular files and folders
/// under `hushrun/` only, with no `..`, no absolute path and nothing else (links, devices);
/// ustar's prefix, GNU's long names and PAX's `path` read.
fn untar(mut from: impl io::Read, into: &Path) -> io::Result<()> {
    let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, why.to_string());
    // A long name or a PAX header is read whole, and none is larger than this.
    const MOST_HEADER: u64 = 1 << 20;
    let mut long_name: Option<String> = None;
    let mut header = [0u8; 512];
    loop {
        match read_block(&mut from, &mut header)? {
            false => break,
            true if header.iter().all(|b| *b == 0) => break,
            true => {}
        }
        let field = |from: usize, len: usize| {
            let raw = &header[from..from + len];
            let end = raw.iter().position(|b| *b == 0).unwrap_or(len);
            String::from_utf8_lossy(&raw[..end]).into_owned()
        };
        let size = u64::from_str_radix(field(124, 12).trim(), 8).map_err(|_| bad("a size"))?;
        let padding = size.div_ceil(512) * 512 - size;
        let kind = header[156];
        let mut data = io::Read::take(&mut from, size);
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
        let small = |data: &mut dyn io::Read| -> io::Result<Vec<u8>> {
            if size > MOST_HEADER {
                return Err(bad("a header larger than 1 MiB"));
            }
            let mut bytes = Vec::new();
            data.read_to_end(&mut bytes)?;
            Ok(bytes)
        };
        let relative = match kind {
            // GNU's long name, for the next entry.
            b'L' => {
                let bytes = small(&mut data)?;
                long_name = Some(
                    String::from_utf8_lossy(&bytes)
                        .trim_end_matches('\0')
                        .to_string(),
                );
                None
            }
            // PAX: its `path`, for the next entry; the rest isn't read.
            b'x' => {
                long_name = pax_path(&small(&mut data)?);
                None
            }
            b'g' => None,
            _ => {
                let relative = safe(&name).ok_or_else(|| bad("a path outside the store"))?;
                // Anything else the image holds isn't the store's.
                (relative.components().next().map(|c| c.as_os_str()) == Some(STORE_ROOT.as_ref()))
                    .then_some(relative)
            }
        };
        match (kind, relative) {
            (b'0' | 0, Some(relative)) => {
                let path = into.join(&relative);
                if let Some(parent) = path.parent() {
                    make_dirs(parent)?;
                }
                let mut file = fs::File::create(&path)?;
                io::copy(&mut data, &mut file)?;
                // Whatever the umask: a baked store others could write isn't one (B-63).
                set_mode(&path, 0o644)?;
            }
            (b'5', Some(relative)) => make_dirs(&into.join(relative))?,
            (_, Some(_)) => return Err(bad("an entry that is neither a file nor a folder")),
            (_, None) => {}
        }
        // What wasn't read (an entry that isn't the store's), then the padding.
        io::copy(&mut data, &mut io::sink())?;
        if data.limit() > 0 {
            return Err(bad("a file runs past the layer"));
        }
        let skipped = io::copy(&mut io::Read::take(&mut from, padding), &mut io::sink())?;
        if skipped < padding {
            return Err(bad("a file runs past the layer"));
        }
    }
    Ok(())
}

/// Reads one 512-byte block; `false` at the end of the layer, an error part of the way in.
fn read_block(from: &mut impl io::Read, block: &mut [u8; 512]) -> io::Result<bool> {
    let mut read = 0;
    while read < block.len() {
        match from.read(&mut block[read..]) {
            Ok(0) if read == 0 => return Ok(false),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a header runs past the layer",
                ));
            }
            Ok(n) => read += n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(true)
}

/// `path` and its missing parents, each `0755` whatever the umask.
fn make_dirs(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        make_dirs(parent)?;
    }
    match fs::create_dir(path) {
        Ok(()) => set_mode(path, 0o755),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
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

    // covers: B-240
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
        untar(&tar[..], dir.path()).unwrap();
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
            assert!(untar(&bad[..], dir.path()).is_err());
        }
        // A layer that ends part of the way into an entry, or a header, stops it.
        let file = entry("hushrun/cut", b'0', &[7; 600]);
        assert!(untar(&file[..700], dir.path()).is_err());
        assert!(untar(&file[..100], dir.path()).is_err());
        assert!(untar(&file[..], dir.path()).is_ok());
        assert_eq!(fs::read(dir.path().join("hushrun/cut")).unwrap(), [7; 600]);
        assert_eq!(
            pax_path(b"30 path=hushrun/a/very/long/name\n"),
            Some("hushrun/a/very/long/name".into())
        );
        assert!(platform().contains('-'));
    }

    // covers: B-240
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

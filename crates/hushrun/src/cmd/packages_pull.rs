//! \[ADR 0163\] `hushrun packages pull` (K-15): puts the repository's newest package store where
//! a run reads one, for a sandbox with no Dockerfile, the way `hushrun build pull` (B-240) does
//! for compiled code, and with its code: the same image and credential rules, the same download
//! beside the destination, the same swap. What differs is the store: the tag is
//! `hushrun-packages-<platform>-latest` (K-14), the image is also the one
//! `HUSHRUN_PACKAGES_IMAGE` names, the unpack keeps a file's run bit and the links that stay in
//! their part, the check is K-1's by names and bytes, and the trust is K-7's.

use std::path::{Path, PathBuf};

use anyhow::anyhow;
use hushrun_core::blocks::image as tags;
use hushrun_packages::store::{self, Depth, Packages, VerifiedBy};

use crate::cli::{GlobalOpts, PackagesPullArgs};
use crate::cmd::build_pull::{self, Source, Unpack};
use crate::cmd::packages_image::Label;
use crate::exit::{CmdResult, Exit, Failure};

/// The variable that names the image when `--image` doesn't: set by whoever sets up the sandbox,
/// so it counts as named (C-13), as `--image` does.
pub(crate) const IMAGE_VARIABLE: &str = "HUSHRUN_PACKAGES_IMAGE";

pub fn run(global: &GlobalOpts, args: &PackagesPullArgs) -> CmdResult {
    let failed = |why: String| Failure::new(Exit::Unexpected, anyhow!("packages pull: {why}"));
    let usage = |why: String| Failure::new(Exit::Usage, anyhow!("packages pull: {why}"));
    let dest = destination(args.to.as_deref(), &|name| hushrun_ci::var_os(name));
    let root = crate::cmd::login::repository_root(global);
    let named = args.image.clone().or_else(|| {
        hushrun_ci::var(IMAGE_VARIABLE)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    });
    let flag = if args.image.is_some() || named.is_none() {
        "--image"
    } else {
        IMAGE_VARIABLE
    };
    let (image, credential) =
        build_pull::image_and_credential(named.as_deref(), flag, root.as_deref()).map_err(usage)?;
    let platform = args.platform.clone().unwrap_or_else(build_pull::platform);
    let tag = args
        .tag
        .clone()
        .unwrap_or_else(|| tags::packages_latest_tag(&platform));
    let source = Source::connect(image, credential, args.insecure).map_err(failed)?;
    let image = &source.image;
    let Some(manifest) = source.tagged(&tag).map_err(failed)? else {
        return Err(failed(format!(
            "{image} has no {tag}: no package store of this repository for this platform yet (`hushrun packages bake --oci` publishes one)"
        )));
    };
    let label = source
        .artifacts
        .config(image, &manifest)
        .map_err(|e| failed(source.refused(&tag, e)))?
        .and_then(|config| Label::of_config(&config));
    let mut pulled = Pulled {
        image: image.to_string(),
        tag: tag.clone(),
        digest: manifest.digest().to_string(),
        identity: label.as_ref().map(|l| l.identity.clone()),
        platform: label.as_ref().map(|l| l.platform.clone()),
        to: dest.clone(),
        state: State::Resolved,
        downloaded: 0,
        trust: Trust::NotChecked,
    };
    if args.resolve {
        return said(global, &pulled);
    }
    // K-15: the destination holds this store already: the label names the identity its
    // `packages.json` has, and it passes the check a run makes as it begins.
    if !args.force
        && let Some(identity) = &pulled.identity
        && holds(&dest, identity)
    {
        pulled.state = State::AlreadyThere;
        return said(global, &pulled);
    }
    let keeps = |path: &Path, target: &str| link_in_part(path, target);
    let unpack = Unpack {
        root: hushrun_packages::tar::ROOT,
        modes_and_links: Some(&keeps),
    };
    let wanted_platform = args.tag.is_none().then_some(platform.as_str());
    let (downloaded, trust) = source
        .pull_into(&tag, &manifest, &dest, &unpack, |folder| {
            let packages = checked(folder, label.as_ref(), wanted_platform)
                .map_err(|why| format!("{tag}: {why}"))?;
            trust(global, folder, &packages, args.require_signed, &dest)
        })
        .map_err(|why| failed(with_hint(why, &dest)))?;
    pulled.state = State::Pulled;
    pulled.downloaded = downloaded;
    pulled.trust = trust;
    said(global, &pulled)
}

/// K-15: where the store goes: `--to`, else `HUSHRUN_PACKAGES_LOWER` when set and not empty, else
/// `/hushrun-packages`.
fn destination(to: Option<&Path>, env: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> PathBuf {
    to.map(Path::to_path_buf)
        .or_else(|| {
            env(hushrun_packages::LOWER)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| PathBuf::from(hushrun_packages::DEFAULT_LOWER))
}

/// The default folder is `/`'s: a sandbox that doesn't run as root names another.
fn with_hint(why: String, dest: &Path) -> String {
    if dest == Path::new(hushrun_packages::DEFAULT_LOWER) && why.contains("ermission denied") {
        return format!(
            "{why}: set {} to a folder this user may write, or pass --to",
            hushrun_packages::LOWER
        );
    }
    why
}

/// K-15: whether a link at `path` (from the image's root) to `target` is one a package store may
/// hold: under `hushrun-packages/parts/<part>/`, and staying inside that part (K-3).
fn link_in_part(path: &Path, target: &str) -> bool {
    let parts: Vec<&str> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(p) => p.to_str(),
            _ => None,
        })
        .collect();
    match parts.as_slice() {
        [root, dir, part, row @ ..]
            if *root == hushrun_packages::tar::ROOT
                && *dir == store::PARTS
                && store::is_part_name(part)
                && !row.is_empty() =>
        {
            store::link_stays_inside(&row.join("/"), target)
        }
        _ => false,
    }
}

/// K-15: the store at `folder` is a package store (K-1, by names and bytes), of the identity the
/// image's label names, and of `platform` when the tag was this machine's.
fn checked(
    folder: &Path,
    label: Option<&Label>,
    platform: Option<&str>,
) -> Result<Packages, String> {
    let (packages, _) = Packages::read_bytes(folder).map_err(|why| why.0)?;
    store::check_contents(folder, Some(&packages), Depth::Bytes).map_err(|why| why.0)?;
    if let Some(label) = label
        && label.identity != packages.identity.to_string()
    {
        return Err(format!(
            "the image's label names the store {}, and its packages.json is {}",
            label.identity, packages.identity
        ));
    }
    if let Some(platform) = platform
        && packages.platform != platform
    {
        return Err(format!(
            "the store was baked on {}, not {platform}",
            packages.platform
        ));
    }
    Ok(packages)
}

/// Whether `dest` holds the store whose identity is `identity`: its `packages.json` says so, and
/// it passes K-1's check by names, as a run checks a store when it begins (K-8).
fn holds(dest: &Path, identity: &str) -> bool {
    Packages::read_bytes(dest).is_ok_and(|(packages, _)| {
        packages.identity.to_string() == identity
            && store::check_contents(dest, Some(&packages), Depth::Names).is_ok()
    })
}

/// What a pulled store's trust came to (K-7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Trust {
    /// A signer `hushrun.yaml` names signed it.
    Signed(String),
    /// Unsigned, and every part is one its lockfile checks: any run reads it.
    Lockfiles,
    /// `hushrun.yaml` names no signer: nothing was checked.
    NotChecked,
}

/// K-7, K-15: the store's trust, checked when `hushrun.yaml` names signers of baked stores. A
/// signature there must verify (and pass `not_before`); none is refused when a part needs one.
/// With `required` (`--require-signed`) only a verified signature passes. Refused, `dest` is as
/// it was.
fn trust(
    global: &GlobalOpts,
    folder: &Path,
    packages: &Packages,
    required: bool,
    dest: &Path,
) -> Result<Trust, String> {
    use crate::context::{Content, Source as ContentSource};
    let signers = Content::open(global, ContentSource::Repo(hushrun_vcs::Snapshot::Worktree))
        .ok()
        .and_then(|content| content.policy().ok().map(|p| p.build_baked.clone()))
        .unwrap_or_default();
    decide(
        &signers,
        packages,
        folder.join(store::SIGNATURE).is_file(),
        required,
        dest,
        &|| {
            use crate::cmd::build_relay::bake_image;
            let root = bake_image::run_root(&|name| hushrun_ci::var_os(name));
            let verified = bake_image::verify_packages(folder, &signers.signers, root.as_ref())?;
            bake_image::not_before_holds(
                signers.not_before,
                verified.signed_ms,
                i64::try_from(verified.baked_ms).ok(),
            )?;
            Ok(verified.signer)
        },
    )
}

/// [`trust`]'s rules, with `verify` the signature's check.
fn decide(
    signers: &hushrun_core::policy::BakedTrust,
    packages: &Packages,
    has_signature: bool,
    required: bool,
    dest: &Path,
    verify: &dyn Fn() -> Result<String, String>,
) -> Result<Trust, String> {
    if signers.signers.is_empty() {
        if required {
            return Err(format!(
                "hushrun.yaml names no signer of baked stores, and --require-signed was given: {} is as it was. hushrun init --bake prints the lines to add",
                dest.display()
            ));
        }
        return Ok(Trust::NotChecked);
    }
    let needing: Vec<&str> = packages
        .parts
        .iter()
        .filter(|p| p.verified_by == VerifiedBy::Signature)
        .map(|p| p.part.as_str())
        .collect();
    if has_signature || required || !needing.is_empty() {
        let signer = verify().map_err(|why| {
            if has_signature {
                format!("its signature doesn't verify: {why}")
            } else if required {
                format!(
                    "it is unsigned, and --require-signed was given: {} is as it was",
                    dest.display()
                )
            } else {
                format!(
                    "it is unsigned, and {} need a signature by a signer hushrun.yaml names (K-7): {} is as it was",
                    needing.join(", "),
                    dest.display()
                )
            }
        })?;
        return Ok(Trust::Signed(signer));
    }
    Ok(Trust::Lockfiles)
}

/// What a pull did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Pulled,
    AlreadyThere,
    Resolved,
}

/// What a pull read and did.
struct Pulled {
    image: String,
    tag: String,
    digest: String,
    /// The store's identity and platform, from the image's label.
    identity: Option<String>,
    platform: Option<String>,
    to: PathBuf,
    state: State,
    downloaded: u64,
    trust: Trust,
}

/// What the line says of a store when `hushrun.yaml` names no signer of baked stores.
const UNSIGNED: &str = "hushrun.yaml names no signer of baked stores, so a run reads the parts a lockfile checks, and the others only with HUSHRUN_PACKAGES_LOWER_UNTRUSTED=1, outside CI";

/// Says what happened: a line on stderr (the reference on stdout for `--resolve`), and with
/// `--format json` `hushrun.packages-pull/v1` on stdout; on GitHub Actions, the step's outputs.
fn said(global: &GlobalOpts, pulled: &Pulled) -> CmdResult {
    use crate::output::Format;
    let (image, tag, digest, to) = (
        &pulled.image,
        &pulled.tag,
        &pulled.digest,
        pulled.to.display(),
    );
    let mut outputs = crate::output::Outputs::new();
    outputs.set("hushrun-image-digest", digest);
    outputs.set(
        "hushrun-packages-identity",
        pulled.identity.clone().unwrap_or_default(),
    );
    crate::ci::Ci::from_process(global)
        .outputs(&outputs)
        .map_err(|e| anyhow!("packages pull: writing the GitHub step outputs: {e}"))?;
    let json = global.format == Format::Json;
    if json {
        crate::output::print_json(&json_of(pulled))?;
    }
    match pulled.state {
        State::Resolved if !json => crate::output::print(&format!("{image}@{digest}\n"))?,
        State::Resolved => {}
        _ if global.quiet > 0 => {}
        State::AlreadyThere => {
            eprintln!(
                "hushrun: {to} holds {image}:{tag} ({digest}) already: nothing was downloaded"
            )
        }
        State::Pulled => {
            let trust = match &pulled.trust {
                Trust::Signed(signer) => format!("signed by {signer}"),
                Trust::Lockfiles => {
                    "unsigned: each part is checked against its lockfile, so any run reads it"
                        .to_string()
                }
                Trust::NotChecked => UNSIGNED.to_string(),
            };
            eprintln!("hushrun: pulled {image}:{tag} ({digest}) into {to}: {trust}")
        }
    }
    Ok(Exit::Ok)
}

/// `hushrun.packages-pull/v1`.
fn json_of(pulled: &Pulled) -> serde_json::Value {
    let mut value = serde_json::json!({
        "schema": "hushrun.packages-pull/v1",
        "image": pulled.image,
        "tag": pulled.tag,
        "digest": pulled.digest,
        "identity": pulled.identity,
        "platform": pulled.platform,
        "state": match pulled.state {
            State::Pulled => "pulled",
            State::AlreadyThere => "already_there",
            State::Resolved => "resolved",
        },
    });
    if pulled.state != State::Resolved {
        value["to"] = pulled.to.display().to_string().into();
        value["downloaded_bytes"] = pulled.downloaded.into();
        let (word, signer) = match &pulled.trust {
            Trust::Signed(signer) => ("named", Some(signer.clone())),
            Trust::Lockfiles => ("lockfiles", None),
            Trust::NotChecked => ("not_checked", None),
        };
        value["signature"] = word.into();
        value["signed_by"] = signer.into();
    }
    value
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;

    use super::*;
    use crate::cmd::packages_image::{self, Target};

    fn lockfile_only(packages: &mut Packages) {
        for part in &mut packages.parts {
            part.verified_by = VerifiedBy::Lockfile;
        }
    }

    // covers: K-15
    #[test]
    fn the_destination_is_to_then_the_variable_then_the_default() {
        let none = |_: &str| None;
        assert_eq!(destination(None, &none), PathBuf::from("/hushrun-packages"));
        let set = |name: &str| (name == "HUSHRUN_PACKAGES_LOWER").then(|| OsString::from("/x"));
        assert_eq!(destination(None, &set), PathBuf::from("/x"));
        let empty = |name: &str| (name == "HUSHRUN_PACKAGES_LOWER").then(OsString::new);
        assert_eq!(
            destination(None, &empty),
            PathBuf::from("/hushrun-packages")
        );
        assert_eq!(
            destination(Some(Path::new("/y")), &set),
            PathBuf::from("/y")
        );
        let hinted = with_hint(
            "putting it at /hushrun-packages: Permission denied (os error 13)".into(),
            Path::new("/hushrun-packages"),
        );
        assert!(hinted.contains("HUSHRUN_PACKAGES_LOWER"), "{hinted}");
    }

    // covers: K-15
    #[test]
    fn a_link_is_kept_only_inside_its_part() {
        let at = |p: &str| PathBuf::from(p);
        assert!(link_in_part(&at("hushrun-packages/parts/npm/a/b"), "../c"));
        assert!(!link_in_part(
            &at("hushrun-packages/parts/npm/a"),
            "../../rows"
        ));
        assert!(!link_in_part(&at("hushrun-packages/parts/npm/a"), "/etc"));
        assert!(!link_in_part(&at("hushrun-packages/packages.json"), "x"));
        assert!(!link_in_part(&at("hushrun-packages/parts/npm"), "x"));
        assert!(!link_in_part(&at("hushrun-packages/parts/NPM/a"), "b"));
    }

    /// The archive of a store, as its image's one layer holds it.
    fn archive_of(folder: &Path) -> Vec<u8> {
        let mut out = Vec::new();
        hushrun_packages::tar::write(folder, &mut out).unwrap();
        out
    }

    // covers: K-15
    #[cfg(unix)]
    #[test]
    fn a_layer_unpacks_a_store_with_its_run_bits_and_links_and_checks_out() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("baked");
        let packages = packages_image::tests::store_at(
            &folder,
            "linux-x64",
            &[("cache/a.tgz", b"aaaa", false), ("bin/run", b"#!", true)],
            &[("cache/b.tgz", "a.tgz")],
        );
        let archive = archive_of(&folder);
        let into = dir.path().join("staging");
        let keeps = |path: &Path, target: &str| link_in_part(path, target);
        let unpack = Unpack {
            root: hushrun_packages::tar::ROOT,
            modes_and_links: Some(&keeps),
        };
        build_pull::untar(&archive[..], &into, &unpack).unwrap();
        let store_dir = into.join("hushrun-packages");
        let label = Label::of(&packages, &Packages::read_bytes(&folder).unwrap().1);
        let read = checked(&store_dir, Some(&label), Some("linux-x64")).unwrap();
        assert_eq!(read.identity, packages.identity);
        let run = fs::metadata(store_dir.join("parts/npm/bin/run")).unwrap();
        assert_eq!(run.permissions().mode() & 0o777, 0o755);
        let tgz = fs::metadata(store_dir.join("parts/npm/cache/a.tgz")).unwrap();
        assert_eq!(tgz.permissions().mode() & 0o777, 0o644);
        assert_eq!(
            fs::read_link(store_dir.join("parts/npm/cache/b.tgz")).unwrap(),
            PathBuf::from("a.tgz")
        );
        // Another platform's store, or another identity than the label's, doesn't check out.
        assert!(checked(&store_dir, Some(&label), Some("macos-arm64")).is_err());
        let mut other = label.clone();
        other.identity = "00".repeat(32);
        assert!(checked(&store_dir, Some(&other), None).is_err());
        // The no-op: what's there is this identity, by names.
        assert!(holds(&store_dir, &label.identity));
        assert!(!holds(&store_dir, &other.identity));
        fs::write(store_dir.join("parts/npm/cache/a.tgz"), b"aaaaa").unwrap();
        assert!(!holds(&store_dir, &label.identity));
    }

    /// A ustar entry: its header and data, padded.
    fn entry(name: &str, kind: u8, link: &str, data: &[u8]) -> Vec<u8> {
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..107].copy_from_slice(b"0000644");
        header[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
        header[156] = kind;
        header[157..157 + link.len()].copy_from_slice(link.as_bytes());
        header[257..263].copy_from_slice(b"ustar\0");
        let mut out = header.to_vec();
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
        out
    }

    // covers: K-15
    #[cfg(unix)]
    #[test]
    fn a_link_out_of_its_part_or_written_through_stops_the_unpack() {
        let dir = tempfile::tempdir().unwrap();
        let keeps = |path: &Path, target: &str| link_in_part(path, target);
        let unpack = Unpack {
            root: hushrun_packages::tar::ROOT,
            modes_and_links: Some(&keeps),
        };
        let cases: [Vec<u8>; 4] = [
            // A link out of its part.
            entry("hushrun-packages/parts/npm/x", b'2', "../../../etc", b""),
            // A link outside any part.
            entry("hushrun-packages/rows", b'2', "parts", b""),
            // A file written where a link goes: the link finds an entry there.
            [
                entry("hushrun-packages/parts/npm/a", b'2', "b", b""),
                entry("hushrun-packages/parts/npm/a/f", b'0', "", b"f"),
            ]
            .concat(),
            // A link under another link: lexically inside, but not where it lands.
            [
                entry("hushrun-packages/parts/npm/d/up", b'2', "..", b""),
                entry("hushrun-packages/parts/npm/d/up/esc", b'2', "../..", b""),
            ]
            .concat(),
        ];
        for (n, case) in cases.iter().enumerate() {
            let into = dir.path().join(format!("case-{n}"));
            assert!(
                build_pull::untar(&case[..], &into, &unpack).is_err(),
                "case {n}"
            );
            // Nothing is ever written outside the folder.
            assert!(!dir.path().join("etc").exists());
        }
        // A hard link stops it whatever unpack keeps.
        let hard = entry("hushrun-packages/parts/npm/h", b'1', "a", b"");
        assert!(build_pull::untar(&hard[..], &dir.path().join("hard"), &unpack).is_err());
    }

    // covers: K-15
    #[test]
    fn trust_follows_k7_and_require_signed() {
        let dir = tempfile::tempdir().unwrap();
        let mut packages =
            packages_image::tests::store_at(dir.path(), "linux-x64", &[("a", b"a", false)], &[]);
        let dest = Path::new("/hushrun-packages");
        let none = hushrun_core::policy::BakedTrust::default();
        let named = hushrun_core::policy::BakedTrust {
            signers: vec![hushrun_core::policy::BakedSigner::Identity {
                issuer: "https://token.actions.githubusercontent.com".into(),
                subject: "https://github.com/acme/shop/.github/workflows/bake.yml@refs/heads/main"
                    .into(),
            }],
            ..Default::default()
        };
        let good = || Ok("ci".to_string());
        let bad = || Err("no".to_string());
        // No signers: nothing checked, unless --require-signed.
        assert_eq!(
            decide(&none, &packages, false, false, dest, &bad),
            Ok(Trust::NotChecked)
        );
        let refused = decide(&none, &packages, true, true, dest, &good).unwrap_err();
        assert!(refused.contains("--require-signed"), "{refused}");
        assert!(refused.contains("is as it was"), "{refused}");
        // Signers: a signature there must verify.
        assert_eq!(
            decide(&named, &packages, true, false, dest, &good),
            Ok(Trust::Signed("ci".into()))
        );
        let refused = decide(&named, &packages, true, false, dest, &bad).unwrap_err();
        assert!(refused.contains("doesn't verify"), "{refused}");
        // Unsigned: refused while a part needs a signature, read when lockfiles check every part.
        let refused = decide(&named, &packages, false, false, dest, &bad).unwrap_err();
        assert!(refused.contains("npm need a signature"), "{refused}");
        lockfile_only(&mut packages);
        assert_eq!(
            decide(&named, &packages, false, false, dest, &bad),
            Ok(Trust::Lockfiles)
        );
        // --require-signed refuses even that.
        assert!(decide(&named, &packages, false, true, dest, &bad).is_err());
    }

    // covers: K-14, K-15
    #[cfg(unix)]
    #[test]
    fn a_published_layer_swaps_in_and_a_bad_one_leaves_what_was_there() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("baked");
        packages_image::tests::store_at(&folder, "linux-x64", &[("a", b"a", false)], &[]);
        let layout = dir.path().join("layout");
        packages_image::publish(&folder, &Target::Layout(layout.clone()), Ok(())).unwrap();
        // The layout's layer is the archive; unpacked and checked, it goes in place.
        let archive = archive_of(&folder);
        let dest = dir.path().join("hushrun-packages");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("old"), b"old").unwrap();
        let staging = dir.path().join("staging");
        let keeps = |path: &Path, target: &str| link_in_part(path, target);
        let unpack = Unpack {
            root: hushrun_packages::tar::ROOT,
            modes_and_links: Some(&keeps),
        };
        build_pull::untar(&archive[..], &staging, &unpack).unwrap();
        checked(&staging.join("hushrun-packages"), None, Some("linux-x64")).unwrap();
        build_pull::swap(&staging.join("hushrun-packages"), &dest).unwrap();
        assert!(dest.join("packages.json").is_file());
        assert!(!dest.join("old").exists());
        // A layer whose file differs from its row doesn't check out.
        let mut broken = archive.clone();
        let at = broken
            .windows(512)
            .position(|w| w.starts_with(b"hushrun-packages/parts/npm/a\0"))
            .unwrap();
        broken[at + 512] = b'b';
        let staging = dir.path().join("broken");
        build_pull::untar(&broken[..], &staging, &unpack).unwrap();
        assert!(checked(&staging.join("hushrun-packages"), None, None).is_err());
    }

    // covers: K-15
    #[test]
    fn the_json_is_the_schemas() {
        let schema = hushrun_testkit::schema::load("packages-pull.v1.schema.json");
        for (state, trust) in [
            (State::Pulled, Trust::Signed("ci".into())),
            (State::AlreadyThere, Trust::Lockfiles),
            (State::Pulled, Trust::NotChecked),
            (State::Resolved, Trust::NotChecked),
        ] {
            let value = json_of(&Pulled {
                image: "registry.hushrun.com/acme/shop".into(),
                tag: "hushrun-packages-linux-x64-latest".into(),
                digest: format!("sha256:{}", "ab".repeat(32)),
                identity: Some("cd".repeat(32)),
                platform: Some("linux-x64".into()),
                to: PathBuf::from("/hushrun-packages"),
                state,
                downloaded: 10,
                trust,
            });
            let wrong = hushrun_testkit::schema::violations(&schema, &value);
            assert!(wrong.is_empty(), "{wrong:?} in {value}");
        }
    }
}

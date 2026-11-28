//! \[ADR 0163\] K-14: a package store published as an image, beside the baked store's, and the
//! label `hushrun packages pull` reads (K-15).
//!
//! The image is the repository's own image (`<host>/<org>/<name>`) under tags of its own:
//! `hushrun-packages-<platform>-<first 16 hex of the store's identity>`, which never moves, and
//! `hushrun-packages-<platform>-latest`, which moves only from CI on the default branch (B-66's
//! rule). Its one layer is the archive `hushrun packages bake --tar` writes (K-6), byte for byte,
//! so the same store is the same layer digest and a registry that holds it uploads nothing again.
//! The signature is the store's own `packages.sigstore.json`, which that archive carries: it
//! covers `packages.json`, which covers every byte (K-7), so no referrer is needed.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use hushrun_artifact::image::{self as oci, Role};
use hushrun_artifact::{Artifacts, Repository};
use hushrun_core::blocks::Hex32;
use hushrun_core::blocks::image as tags;
use hushrun_packages::store::Packages;
use serde::{Deserialize, Serialize};

use crate::cli::PackagesBakeArgs;
use crate::exit::{Exit, Failure};

/// The image config's label that says what package store the image holds.
pub(crate) const LABEL: &str = "dev.hushrun.baked-packages";
/// The label's schema.
pub(crate) const LABEL_SCHEMA: &str = "hushrun.packages-image/v1";
/// The manifest's annotation that names the store's identity.
pub(crate) const IDENTITY_ANNOTATION: &str = "dev.hushrun.packages-identity";

/// K-14: what an image's label says of the package store it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Label {
    pub(crate) schema: String,
    /// `packages.json`'s `identity`.
    pub(crate) identity: String,
    pub(crate) platform: String,
    /// The SHA-256 of `packages.json`'s bytes.
    pub(crate) packages_json: String,
    /// The parts, by name.
    pub(crate) parts: Vec<String>,
}

impl Label {
    /// The label of the store `packages`, whose `packages.json` bytes hash to `packages_json`.
    pub(crate) fn of(packages: &Packages, packages_json: &Hex32) -> Label {
        Label {
            schema: LABEL_SCHEMA.into(),
            identity: packages.identity.to_string(),
            platform: packages.platform.clone(),
            packages_json: packages_json.to_string(),
            parts: packages.parts.iter().map(|p| p.part.clone()).collect(),
        }
    }

    /// The label an image config's bytes carry: `None` for none, or another schema.
    pub(crate) fn of_config(config: &[u8]) -> Option<Label> {
        let json: serde_json::Value = serde_json::from_slice(config).ok()?;
        let text = json["config"]["Labels"][LABEL].as_str()?;
        let label: Label = serde_json::from_str(text).ok()?;
        (label.schema == LABEL_SCHEMA).then_some(label)
    }
}

/// Where `packages bake` publishes.
pub(crate) enum Target {
    /// A registry's repository, with the credential settled before the fetch ran.
    Registry {
        repository: Repository,
        artifacts: Box<Artifacts>,
    },
    /// An OCI image layout in a folder.
    Layout(PathBuf),
}

/// Why the backend's registry is refused as a target: it takes images only through
/// `POST v0/build/images`, which carries a baked store of compiled code (B-70).
pub(crate) fn hosted_refusal(host: &str) -> String {
    format!(
        "{host} is the registry of this repository's backend, which takes only what `hushrun build bake` publishes there (B-70), not a package store yet: publish to a registry of your own with --oci <host>/<org>/<name>, and have sandboxes pull it with HUSHRUN_PACKAGES_IMAGE=<host>/<org>/<name> (ADR 0163)"
    )
}

/// K-14: the target `--oci` or `--oci-dir` names, settled before the fetch runs anything of the
/// repository's (ADR 0102): the registry's credential is the CI job's own, else the Docker
/// configuration's, read now. `None` when neither is given. The backend's registry is refused
/// ([`hosted_refusal`]).
pub(crate) fn target(args: &PackagesBakeArgs, folder: &Path) -> Result<Option<Target>, Failure> {
    let refused = |why: String| Failure::new(Exit::Usage, anyhow!("packages bake: {why}"));
    if let Some(dir) = &args.oci_dir {
        return Ok(Some(Target::Layout(dir.clone())));
    }
    let Some(text) = args.oci.as_deref().map(str::trim) else {
        return Ok(None);
    };
    let backend_registry = match crate::results::Backend::select(Some(folder)) {
        Ok(crate::results::Selection::On(backend)) => backend
            .build_access()
            .map(|a| crate::hosted::registry_host(&a.host)),
        _ => None,
    };
    if text == "hushrun" {
        return Err(refused(hosted_refusal(
            backend_registry
                .as_deref()
                .unwrap_or(crate::hosted::HOSTED_REGISTRY),
        )));
    }
    let reference = hushrun_artifact::ImageRef::parse(text).map_err(|e| refused(e.to_string()))?;
    if reference.tag.is_some() || reference.digest.is_some() {
        return Err(refused(format!(
            "--oci takes a repository, such as ghcr.io/acme/app, not {text}: hushrun names the tags"
        )));
    }
    let host = reference.repository.host.clone();
    if Some(host.as_str()) == backend_registry.as_deref() || host == crate::hosted::HOSTED_REGISTRY
    {
        return Err(refused(hosted_refusal(&host)));
    }
    let config =
        hushrun_artifact::credentials::DockerConfig::load().map_err(|e| refused(e.to_string()))?;
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let credential = hushrun_artifact::credentials::settled(&host, &config, &env).map_err(|e| {
        Failure::new(
            Exit::Unexpected,
            anyhow!("packages bake: the credential for {host}: {e}"),
        )
    })?;
    let artifacts = Artifacts::connect_with(
        hushrun_artifact::credentials::DockerConfig::default(),
        &hushrun_artifact::Options {
            insecure_registries: args.registry.insecure.clone(),
            credentials: vec![(host, credential)],
        },
    )
    .map_err(|e| refused(e.to_string()))?;
    Ok(Some(Target::Registry {
        repository: reference.repository,
        artifacts: Box::new(artifacts),
    }))
}

/// What a publish did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Published {
    /// `<repository>@<digest>`, or `<folder>@<digest>`.
    pub(crate) reference: String,
    pub(crate) digest: String,
    /// The tags it took.
    pub(crate) tags: Vec<String>,
    /// The layer's bytes, and whether they went up (else the destination held them).
    pub(crate) layer_bytes: u64,
    pub(crate) uploaded: bool,
    /// What the publish says of its tags.
    pub(crate) notes: Vec<String>,
}

/// K-14: publishes the package store at `folder` to `target`: its archive, written beside it and
/// removed after, as the one layer; the fixed tag, unless it names another image already (it
/// never moves); and the moving tag when `latest` is `Ok` (CI on the default branch), else a note
/// that says why not.
pub(crate) fn publish(
    folder: &Path,
    target: &Target,
    latest: Result<(), String>,
) -> Result<Published, String> {
    let (packages, packages_json) = Packages::read_bytes(folder).map_err(|why| why.0)?;
    if !tags::valid_platform(&packages.platform) {
        return Err(format!(
            "{} is no platform a tag can name",
            packages.platform
        ));
    }
    let layer = folder.with_file_name(format!(
        ".{}.image-{}.tar",
        folder
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
        hushrun_build::baked::unique("layer")
    ));
    let written = (|| -> std::io::Result<()> {
        let mut file = std::io::BufWriter::new(fs::File::create(&layer)?);
        hushrun_packages::tar::write(folder, &mut file)?;
        file.into_inner().map_err(|e| e.into_error())?.sync_all()
    })();
    let result = written
        .map_err(|e| format!("{}: {e}", layer.display()))
        .and_then(|()| publish_layer(&layer, &packages, &packages_json, target, latest));
    let _ = fs::remove_file(&layer);
    result
}

/// [`publish`] of the archive already at `layer`.
fn publish_layer(
    layer: &Path,
    packages: &Packages,
    packages_json: &Hex32,
    target: &Target,
    latest: Result<(), String>,
) -> Result<Published, String> {
    let label = Label::of(packages, packages_json);
    let identity = packages.identity.to_string();
    let labels = BTreeMap::from([(
        LABEL.to_string(),
        serde_json::to_string(&label).expect("labels serialize"),
    )]);
    let annotations = BTreeMap::from([(IDENTITY_ANNOTATION.to_string(), identity.clone())]);
    let image = oci::file_image(
        layer,
        Role::Packages,
        &packages.platform,
        &labels,
        &annotations,
    )
    .map_err(|e| e.to_string())?;
    let digest = image.digest();
    let fixed = tags::packages_fixed_tag(&packages.platform, &identity);
    let latest_tag = tags::packages_latest_tag(&packages.platform);
    let mut notes = Vec::new();
    let moving = match latest {
        Ok(()) => vec![latest_tag],
        Err(why) => {
            notes.push(format!(
                "{latest_tag} moves only from CI on the default branch, and this bake runs {why}"
            ));
            Vec::new()
        }
    };
    let (reference, pushed) = match target {
        Target::Registry {
            repository,
            artifacts,
        } => {
            // K-14: the fixed tag never moves. The same store signed again (a keyless signature
            // differs each time) is another image: it keeps its moving tag, and the fixed tag stays.
            let fixed_tags = match artifacts.tagged(repository, &fixed) {
                Ok(None) => vec![fixed.clone()],
                Ok(Some(m)) if m.digest() == digest => vec![fixed.clone()],
                Ok(Some(m)) => {
                    notes.push(format!(
                        "{fixed} names {} already, this store signed another time: fixed tags never move",
                        m.digest()
                    ));
                    Vec::new()
                }
                Err(e) => {
                    notes.push(format!(
                        "whether {fixed} names an image couldn't be read ({e}); the fixed tag never moves, so this image takes none"
                    ));
                    Vec::new()
                }
            };
            let pushed = artifacts
                .publish_file_image(repository, &image, layer, &fixed_tags, &moving)
                .map_err(|e| format!("{repository}: {e}"))?;
            (repository.at_digest(&digest), pushed)
        }
        Target::Layout(dir) => {
            let fixed_tags = match oci::layout_tagged(dir, &fixed) {
                Some(m) if m.digest() != digest => {
                    notes.push(format!(
                        "{fixed} names {} already, this store signed another time: fixed tags never move",
                        m.digest()
                    ));
                    Vec::new()
                }
                _ => vec![fixed.clone()],
            };
            let mut named = fixed_tags;
            named.extend(moving.iter().cloned());
            let pushed = oci::write_file_layout(dir, &image, layer, &named)
                .map_err(|e| format!("{}: {e}", dir.display()))?;
            (format!("{}@{digest}", dir.display()), pushed)
        }
    };
    let first = pushed.blobs.first();
    Ok(Published {
        reference,
        digest: digest.to_string(),
        tags: pushed.tags.clone(),
        layer_bytes: first.map_or(0, |b| b.size),
        uploaded: first.is_some_and(|b| b.uploaded),
        notes,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use hushrun_packages::store::{self, Class, Row, RowsFile, VerifiedBy};

    /// A package store of one part, `npm`, with `files` (path, bytes, runs) and `links` (path,
    /// target), written into `folder` as `packages bake` writes one.
    pub(crate) fn store_at(
        folder: &Path,
        platform: &str,
        files: &[(&str, &[u8], bool)],
        links: &[(&str, &str)],
    ) -> Packages {
        let part_dir = store::part_dir(folder, "npm");
        let mut rows = Vec::new();
        for (path, bytes, runs) in files {
            let at = part_dir.join(path);
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            fs::write(&at, bytes).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mode = if *runs { 0o755 } else { 0o644 };
                fs::set_permissions(&at, fs::Permissions::from_mode(mode)).unwrap();
            }
            rows.push(Row {
                path: path.to_string(),
                kind: if *runs {
                    store::Kind::Exe
                } else {
                    store::Kind::File
                },
                bytes: bytes.len() as u64,
                sha256: Hex32::sha256(bytes),
                target: None,
                package: None,
                locked: None,
            });
        }
        for (path, target) in links {
            let at = part_dir.join(path);
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &at).unwrap();
            rows.push(Row {
                path: path.to_string(),
                kind: store::Kind::Link,
                bytes: target.len() as u64,
                sha256: Hex32::sha256(target.as_bytes()),
                target: Some(target.to_string()),
                package: None,
                locked: None,
            });
        }
        rows.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        let rows_bytes = store::rows_bytes(&rows);
        fs::create_dir_all(folder.join(store::ROWS)).unwrap();
        fs::write(folder.join(store::ROWS).join("npm.jsonl"), &rows_bytes).unwrap();
        let identity = store::part_identity("npm", Class::Download, None, &rows);
        let packages = Packages {
            schema: store::SCHEMA.into(),
            repository: Some("acme/shop".into()),
            platform: platform.into(),
            identity: store::store_identity(&[("npm", identity)]),
            baked_ms: 1,
            parts: vec![store::PartEntry {
                part: "npm".into(),
                class: Class::Download,
                tool: "npm 10".into(),
                runtime: None,
                inputs: Hex32([0; 32]),
                manifests: vec!["package-lock.json".into()],
                identity,
                rows: RowsFile {
                    sha256: Hex32::sha256(&rows_bytes),
                    bytes: rows_bytes.len() as u64,
                    count: rows.len() as u64,
                },
                bytes: rows.iter().map(|r| r.bytes).sum(),
                verified_by: VerifiedBy::Signature,
            }],
        };
        fs::write(folder.join(store::PACKAGES_JSON), packages.to_bytes()).unwrap();
        store::check_contents(folder, Some(&packages), store::Depth::Bytes).unwrap();
        packages
    }

    // covers: K-14
    #[test]
    fn a_store_publishes_as_one_layer_under_its_own_tags() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("store");
        let packages = store_at(
            &folder,
            "linux-x64",
            &[("cache/a.tgz", b"aaaa", false), ("bin/run", b"#!", true)],
            &[("cache/b.tgz", "a.tgz")],
        );
        let layout = dir.path().join("layout");
        let target = Target::Layout(layout.clone());
        let identity = packages.identity.to_string();
        let fixed = tags::packages_fixed_tag("linux-x64", &identity);
        // Outside CI: the fixed tag only, and a note that says why.
        let first = publish(&folder, &target, Err("outside CI".into())).unwrap();
        assert_eq!(first.tags, std::slice::from_ref(&fixed));
        assert!(first.uploaded);
        assert!(first.notes[0].contains("outside CI"), "{:?}", first.notes);
        // The layer is the archive --tar writes, byte for byte.
        let mut archive = Vec::new();
        hushrun_packages::tar::write(&folder, &mut archive).unwrap();
        assert_eq!(first.layer_bytes, archive.len() as u64);
        let manifest = oci::layout_tagged(&layout, &fixed).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&manifest.bytes).unwrap();
        assert_eq!(
            json["layers"][0]["digest"],
            hushrun_core::digest::Digest::of(&archive).to_string()
        );
        assert_eq!(json["annotations"][IDENTITY_ANNOTATION], identity.as_str());
        // From CI on the default branch, the moving tag too; the same layer goes up once.
        let second = publish(&folder, &target, Ok(())).unwrap();
        assert_eq!(
            second.tags,
            [fixed.clone(), tags::packages_latest_tag("linux-x64")]
        );
        assert!(!second.uploaded);
        assert_eq!(second.digest, first.digest);
        // No archive is left beside the store.
        let left: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    // covers: K-14
    #[test]
    fn the_label_says_the_store_and_the_backends_registry_is_refused_plainly() {
        let dir = tempfile::tempdir().unwrap();
        let packages = store_at(dir.path(), "linux-x64", &[("a", b"a", false)], &[]);
        let (_, digest) = Packages::read_bytes(dir.path()).unwrap();
        let label = Label::of(&packages, &digest);
        let config = serde_json::json!({
            "config": {"Labels": {LABEL: serde_json::to_string(&label).unwrap()}},
        });
        let read = Label::of_config(&serde_json::to_vec(&config).unwrap()).unwrap();
        assert_eq!(read, label);
        assert_eq!(read.parts, ["npm"]);
        let other = serde_json::json!({"config": {"Labels": {LABEL: "{\"schema\":\"x\"}"}}});
        assert!(Label::of_config(&serde_json::to_vec(&other).unwrap()).is_none());
        let why = hosted_refusal("registry.hushrun.com");
        assert!(why.contains("--oci <host>/<org>/<name>"), "{why}");
        assert!(why.contains("HUSHRUN_PACKAGES_IMAGE"), "{why}");
    }
}

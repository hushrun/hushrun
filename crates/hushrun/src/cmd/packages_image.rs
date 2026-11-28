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
//!
//! The backend's registry (`--oci hushrun`, or a repository on its host) takes the image through a
//! push grant (ADR 0163, hushrun/cloud's `registry-v0` §10): the CI job's own credential, settled
//! before the fetch, asks `POST v0/packages/images/grant` for a password to that one repository
//! and its two tags; the blobs then go up in chunks of at most the grant's `chunk_bytes`, and the
//! manifest under the fixed tag, then the moving one.

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
    /// The backend's registry, through a push grant ([`Hosted`]).
    Hosted(Box<Hosted>),
    /// An OCI image layout in a folder.
    Layout(PathBuf),
}

/// The backend's push grant (`registry-v0` §10): its route under the org's URL.
pub(crate) const GRANT_PATH: &str = "v0/packages/images/grant";
/// The most bytes per request hushrun's registry takes, when a grant names none.
const CHUNK_BYTES: u64 = 16 << 20;
/// How long the grant's request waits on a backend that sends nothing.
const GRANT_SILENCE: std::time::Duration = std::time::Duration::from_secs(60);

/// K-14: the backend's registry as a target: the image `<registry host>/<org>/<name>` and the CI
/// job's credential, settled before the fetch ran (ADR 0102), that asks the backend for a push
/// grant once the store is baked.
pub(crate) struct Hosted {
    caller: crate::cmd::build_relay::caller::Caller,
    /// The image the backend's registry names this repository's.
    image: Repository,
    /// The commit checked out: the backend takes a grant only for the ID token's own.
    commit: String,
    /// `--insecure-registry`'s hosts.
    insecure: Vec<String>,
    /// The backend is reached over plain HTTP (tests): so is its registry.
    plain: bool,
}

/// What the backend granted: a password to push to one repository under two tags. Never
/// printed: its `Debug` leaves the password out.
#[derive(Deserialize)]
struct Grant {
    registry: String,
    repository: String,
    image: String,
    username: String,
    password: String,
    tags: GrantTags,
    #[serde(default)]
    chunk_bytes: u64,
    #[serde(default)]
    most_blob_bytes: u64,
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grant")
            .field("image", &self.image)
            .field("tags", &self.tags)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Deserialize)]
struct GrantTags {
    fixed: String,
    latest: String,
}

/// What a grant's request came to.
enum Granted {
    Grant(Grant),
    /// 409 `not_newer`: a newer run holds the moving tag, and nothing is granted.
    NotNewer(String),
}

impl Hosted {
    /// K-14: asks the backend for a push grant for the store of `identity` on `platform`. `Err`
    /// is the line to fail with.
    fn grant(&self, platform: &str, identity: &str, latest: &str) -> Result<Granted, String> {
        let body = serde_json::to_vec(&serde_json::json!({
            "platform": platform,
            "identity": identity,
            "commit": self.commit,
        }))
        .map_err(|e| e.to_string())?;
        let answer = self
            .caller
            .post(GRANT_PATH, body, GRANT_SILENCE)
            .map_err(|why| format!("the backend didn't answer the push grant's request: {why}"))?;
        if answer.status == 200 {
            return serde_json::from_slice(&answer.body)
                .map(Granted::Grant)
                .map_err(|e| format!("the backend's push grant isn't one: {e}"));
        }
        let refusal: serde_json::Value =
            serde_json::from_slice(&answer.body).unwrap_or(serde_json::Value::Null);
        let code = refusal["error"]["code"].as_str().unwrap_or_default();
        let message = refusal["error"]["message"].as_str().unwrap_or_default();
        if (answer.status, code) == (409, "not_newer") {
            return Ok(Granted::NotNewer(format!(
                "a newer run holds {latest}{}: nothing of this bake was published to {}",
                said(message),
                self.image
            )));
        }
        Err(grant_refused(
            answer.status,
            code,
            message,
            refusal["current"]["word"].as_str(),
        ))
    }
}

/// ` (message)`, or nothing.
fn said(message: &str) -> String {
    if message.is_empty() {
        String::new()
    } else {
        format!(" ({message})")
    }
}

/// K-14: the line a refused push grant fails with, in `build bake --oci hushrun`'s words for
/// the same refusals (B-70).
fn grant_refused(status: u16, code: &str, message: &str, word: Option<&str>) -> String {
    let said = said(message);
    match (status, code) {
        (401, _) => format!(
            "the backend took no credential for the push grant{said}: only CI publishes there, with its own ID token (`permissions: id-token: write` on GitHub, `id_tokens: HUSHRUN_ID_TOKEN` on GitLab)"
        ),
        (403, _) => format!(
            "only the repository's CI, with its own ID token, on its default branch, publishes to the backend's registry; this run doesn't ({}){said}. Elsewhere, write the image into a folder with --oci-dir, or push it to a registry of your own with --oci REPOSITORY",
            word.unwrap_or("no reason given")
        ),
        (404 | 405, _) => format!(
            "the backend has no registry, or the org doesn't share build blocks (answered {status}){said}: publish to a registry of your own with --oci REPOSITORY, or into a folder with --oci-dir"
        ),
        (409, "name_taken") => format!(
            "another repository of the org has this image's name in the backend's registry{said}: an owner settles it"
        ),
        (422, "bad_platform" | "bad_identity") => {
            format!("the backend refused the store's {code}{said}: report it")
        }
        (429, _) => format!(
            "the backend grants a repository only so many pushes an hour, and this one is past it{said}: bake again later"
        ),
        (400, _) => {
            format!("the backend refused the push grant's request as invalid{said}: report it")
        }
        (status, "") => format!("the backend answered the push grant's request {status}{said}"),
        (status, code) => {
            format!("the backend answered the push grant's request {status} {code}{said}")
        }
    }
}

/// K-14: the target `--oci` or `--oci-dir` names, settled before the fetch runs anything of the
/// repository's (ADR 0102): for a registry, its credential is the CI job's own, else the Docker
/// configuration's, read now; for the backend's (`--oci hushrun`, or a repository on its host),
/// the CI job's credential for the backend ([`hosted_target`]). `None` when neither is given.
/// `repository` is the repository's name (`github.com/acme/shop`), which names its image there.
pub(crate) fn target(
    args: &PackagesBakeArgs,
    folder: &Path,
    repository: Option<&str>,
) -> Result<Option<Target>, Failure> {
    let refused = |why: String| Failure::new(Exit::Usage, anyhow!("packages bake: {why}"));
    if let Some(dir) = &args.oci_dir {
        return Ok(Some(Target::Layout(dir.clone())));
    }
    let Some(text) = args.oci.as_deref().map(str::trim) else {
        return Ok(None);
    };
    let (access, place) = match crate::results::Backend::select(Some(folder)) {
        Ok(crate::results::Selection::On(backend)) => {
            (backend.build_access(), backend.build_place())
        }
        _ => (None, None),
    };
    let named = place.is_some();
    let backend_registry = access
        .as_ref()
        .map(|a| a.host.clone())
        .or(place.map(|p| p.host))
        .map(|host| crate::hosted::registry_host(&host));
    if text == "hushrun" {
        return hosted_target(args, None, access, named, folder, repository).map(Some);
    }
    let reference = hushrun_artifact::ImageRef::parse(text).map_err(|e| refused(e.to_string()))?;
    if reference.tag.is_some() || reference.digest.is_some() {
        return Err(refused(format!(
            "--oci takes a repository, such as ghcr.io/acme/app, not {text}: hushrun names the tags"
        )));
    }
    let host = reference.repository.host.clone();
    if Some(host.as_str()) == backend_registry.as_deref() {
        return hosted_target(
            args,
            Some(reference.repository),
            access,
            named,
            folder,
            repository,
        )
        .map(Some);
    }
    if host == crate::hosted::HOSTED_REGISTRY {
        return Err(refused(format!(
            "{} is the registry of hushrun's hosted backend, which this repository doesn't use: publish to its own backend's registry with --oci hushrun, or to a registry of your own",
            crate::hosted::HOSTED_REGISTRY
        )));
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

/// K-14: the backend's registry as a target: the image `<registry host>/<org>/<name>` (B-70's
/// naming), `given` if named the same, and the CI job's own credential for the backend, which
/// alone a push grant is had with (default-branch CI, as for B-70's publish). Without one, a line
/// says why, before anything bakes. `named`: a backend is named, whether or not this run can
/// reach it.
fn hosted_target(
    args: &PackagesBakeArgs,
    given: Option<Repository>,
    access: Option<crate::results::BuildAccess>,
    named: bool,
    folder: &Path,
    repository: Option<&str>,
) -> Result<Target, Failure> {
    use crate::results::{BuildCredential, GitlabCi};
    let refused = |why: String| Failure::new(Exit::Usage, anyhow!("packages bake: {why}"));
    let elsewhere =
        "publish to a registry of your own with --oci REPOSITORY, or into a folder with --oci-dir";
    if access.is_none() && named {
        return Err(refused(format!(
            "only CI publishes to the backend's registry, with the job's own ID token, and this run has no credential for the backend: {elsewhere}"
        )));
    }
    let access = access.ok_or_else(|| {
        refused(
            "--oci hushrun publishes to the registry of the repository's backend, and no backend is named here (hushrun.yaml's `backend`, or HUSHRUN_BACKEND)".into(),
        )
    })?;
    let name = repository
        .and_then(hushrun_core::blocks::publish::image_name)
        .ok_or_else(|| {
            refused(format!(
                "the backend's registry names an image after its repository, and {} has no name it takes",
                repository.unwrap_or("this repository")
            ))
        })?;
    let host = crate::hosted::registry_host(&access.host);
    let image = Repository::parse(&format!("{host}/{}/{name}", access.org))
        .map_err(|e| refused(e.to_string()))?;
    if let Some(given) = &given
        && given.path != image.path
    {
        return Err(refused(format!(
            "the backend's registry names this repository's image {image}, not {given}"
        )));
    }
    // Only a CI job's own ID token is granted a push (default-branch CI, as B-70's publish).
    match &access.credential {
        Some(
            BuildCredential::GithubOidc { .. } | BuildCredential::Gitlab(GitlabCi::Usable { .. }),
        ) => {}
        Some(BuildCredential::Gitlab(GitlabCi::Absent { declare })) => {
            return Err(refused(format!(
                "only CI publishes to the backend's registry, with the job's own ID token, and this job has none: {declare}; else {elsewhere}"
            )));
        }
        Some(BuildCredential::Gitlab(GitlabCi::Unusable(why))) => {
            return Err(refused(format!(
                "only CI publishes to the backend's registry, with the job's own ID token, and this job's can't be sent ({why}); else {elsewhere}"
            )));
        }
        Some(BuildCredential::Token(_)) => {
            return Err(refused(format!(
                "only CI publishes to the backend's registry, with the job's own ID token, on the default branch, and this bake has a person's or an agent's token: {elsewhere}"
            )));
        }
        None => {
            return Err(refused(format!(
                "only CI publishes to the backend's registry, with the job's own ID token, and this run has none (GitHub: `permissions: id-token: write`): {elsewhere}"
            )));
        }
    }
    let commit = crate::cmd::build::head_commit(folder).ok_or_else(|| {
        refused("the backend grants a push for the commit checked out, and no commit is checked out here".into())
    })?;
    let caller = crate::cmd::build_relay::caller::Caller::of(&access).map_err(|why| {
        refused(format!(
            "no credential for the backend's registry: {why}; {elsewhere}"
        ))
    })?;
    Ok(Target::Hosted(Box::new(Hosted {
        caller,
        image,
        commit,
        insecure: args.registry.insecure.clone(),
        plain: access.host.starts_with("http://"),
    })))
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
    /// K-14: nothing went up, since a newer run holds the moving tag (the backend's `not_newer`):
    /// what the notes say then is why.
    pub(crate) held_back: bool,
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
        Target::Hosted(hosted) => {
            return publish_hosted(
                hosted,
                layer,
                &image,
                packages,
                &fixed,
                moving.first().map(String::as_str),
                notes,
            );
        }
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
        held_back: false,
    })
}

/// K-14: publishes `image` (of the archive at `layer`) into the backend's registry with a push
/// grant: the layer and the config, in chunks of at most the grant's `chunk_bytes`, each unless
/// the registry holds it; then the manifest under the fixed tag, kept where it is when it names
/// another image (409), then under the moving one when `latest` names it (CI on the default
/// branch), kept where it is when a newer run holds it (409 `not_newer`).
fn publish_hosted(
    hosted: &Hosted,
    layer: &Path,
    image: &oci::Image,
    packages: &Packages,
    fixed: &str,
    latest: Option<&str>,
    mut notes: Vec<String>,
) -> Result<Published, String> {
    let digest = image.digest();
    let size = image.layers.first().map_or(0, |l| l.size);
    let identity = packages.identity.to_string();
    let latest_tag = tags::packages_latest_tag(&packages.platform);
    let grant = match hosted.grant(&packages.platform, &identity, &latest_tag)? {
        Granted::Grant(grant) => grant,
        Granted::NotNewer(why) => {
            notes.push(why);
            return Ok(Published {
                reference: hosted.image.at_digest(&digest),
                digest: digest.to_string(),
                tags: Vec::new(),
                layer_bytes: size,
                uploaded: false,
                notes,
                held_back: true,
            });
        }
    };
    if grant.tags.fixed != fixed || grant.tags.latest != latest_tag {
        return Err(format!(
            "the backend granted the tags {} and {}, not the ones K-14 names ({fixed} and {latest_tag}): report it",
            grant.tags.fixed, grant.tags.latest
        ));
    }
    if grant.repository != hosted.image.path {
        return Err(format!(
            "the backend granted a push to {}, not to {}: report it",
            grant.repository, hosted.image.path
        ));
    }
    if grant.most_blob_bytes > 0 && size > grant.most_blob_bytes {
        return Err(format!(
            "the store's layer is {size} bytes, more than the {} the backend's registry takes in one blob",
            grant.most_blob_bytes
        ));
    }
    let repository = Repository::parse(&grant.image).map_err(|e| e.to_string())?;
    if repository.host != grant.registry {
        return Err(format!(
            "the backend's push grant names the image {} on another registry than {}: report it",
            grant.image, grant.registry
        ));
    }
    let mut insecure = hosted.insecure.clone();
    if hosted.plain {
        insecure.push(repository.host.clone());
    }
    let chunk = if grant.chunk_bytes == 0 {
        CHUNK_BYTES
    } else {
        grant.chunk_bytes
    };
    let artifacts = Artifacts::connect_chunked(
        hushrun_artifact::credentials::DockerConfig::default(),
        &hushrun_artifact::Options {
            insecure_registries: insecure,
            credentials: vec![(
                repository.host.clone(),
                hushrun_artifact::credentials::Credential::Basic {
                    username: grant.username.clone(),
                    password: grant.password.clone(),
                },
            )],
        },
        chunk,
    )
    .map_err(|e| e.to_string())?;
    let blobs = artifacts
        .push_file_image_blobs(&repository, image, layer)
        .map_err(|e| format!("{repository}: {e}"))?;
    let mut taken = Vec::new();
    // The fixed tag never moves: one that names another image (the same store signed another
    // time) stays, and the moving tag still goes to this one.
    match artifacts.tag(&repository, fixed, &image.manifest) {
        Ok(()) => taken.push(fixed.to_string()),
        Err(hushrun_artifact::ArtifactError::Conflict { message, .. }) => notes.push(format!(
            "{fixed} names another image already, this store signed another time: fixed tags never move ({message})"
        )),
        Err(e) => return Err(format!("{repository}: {e}")),
    }
    if let Some(latest) = latest {
        match artifacts.tag(&repository, latest, &image.manifest) {
            Ok(()) => taken.push(latest.to_string()),
            Err(hushrun_artifact::ArtifactError::Conflict { message, .. })
                if message.starts_with("not_newer") =>
            {
                notes.push(format!(
                    "a newer run holds {latest}, which stays where it is ({message})"
                ));
            }
            Err(e) => return Err(format!("{repository}: {e}")),
        }
    }
    let first = blobs.first();
    Ok(Published {
        reference: repository.at_digest(&digest),
        digest: digest.to_string(),
        tags: taken,
        layer_bytes: first.map_or(0, |b| b.size),
        uploaded: first.is_some_and(|b| b.uploaded),
        notes,
        held_back: false,
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
    fn the_label_says_the_store() {
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
    }

    /// The grant's password, which nothing may print.
    const PASSWORD: &str = "hrg_0123456789abcdefghijABCDEFGHIJ0123456789";
    /// The CI job's token the fake backend takes.
    const JOB_TOKEN: &str = "job-id-token";
    /// The registry's bearer for the grant's password.
    const BEARER: &str = "registry-bearer";
    /// The fake registry's most bytes per request.
    const CHUNK: usize = 1024;

    /// What the fake backend and registry do and saw.
    #[derive(Default)]
    struct Seen {
        /// The grant's answer instead of a grant: status and body.
        refuse_grant: Option<(u16, serde_json::Value)>,
        /// The moving tag is held by a newer run.
        latest_newer: bool,
        grants: Vec<serde_json::Value>,
        /// Every registry request: method, path, body size.
        requests: Vec<(String, String, usize)>,
        blobs: BTreeMap<String, Vec<u8>>,
        uploads: BTreeMap<String, Vec<u8>>,
        tags: BTreeMap<String, String>,
    }

    /// A backend that grants pushes and the registry they go to, both on the loopback.
    struct Fake {
        backend: hushrun_testkit::http::Server,
        registry: hushrun_testkit::http::Server,
        seen: std::sync::Arc<std::sync::Mutex<Seen>>,
    }

    impl Fake {
        fn start() -> Fake {
            use hushrun_testkit::http::{Reply, Response, Server};
            use std::sync::{Arc, Mutex};
            let seen: Arc<Mutex<Seen>> = Arc::default();
            let at = seen.clone();
            let registry = Server::start(move |request| {
                use base64::Engine as _;
                let mut seen = at.lock().unwrap();
                seen.requests.push((
                    request.method.clone(),
                    request.path.clone(),
                    request.body.len(),
                ));
                let path = request.path.as_str();
                let host = request.header("host").unwrap_or_default().to_string();
                if path == "/v2/token" {
                    let basic = format!(
                        "Basic {}",
                        base64::engine::general_purpose::STANDARD
                            .encode(format!("hushrun-push:{PASSWORD}"))
                    );
                    if request.header("authorization") != Some(basic.as_str()) {
                        return Reply::Send(Response::new(401));
                    }
                    return Reply::Send(Response::json(200, &serde_json::json!({"token": BEARER})));
                }
                if request.header("authorization") != Some(format!("Bearer {BEARER}").as_str()) {
                    return Reply::Send(Response::new(401).header(
                        "www-authenticate",
                        format!("Bearer realm=\"http://{host}/v2/token\",service=\"{host}\""),
                    ));
                }
                if request.body.len() > CHUNK {
                    return Reply::Send(Response::new(413));
                }
                if path == "/v2/" {
                    return Reply::Send(Response::new(200));
                }
                let Some(rest) = path.strip_prefix("/v2/acme/shop/") else {
                    return Reply::Send(Response::new(404));
                };
                let location = |id: &str| format!("/v2/acme/shop/blobs/uploads/{id}");
                if let Some(id) = rest.strip_prefix("blobs/uploads/") {
                    return Reply::Send(match request.method.as_str() {
                        "POST" => {
                            let id = format!("u{}", seen.requests.len());
                            seen.uploads.insert(id.clone(), Vec::new());
                            Response::new(202).header("location", location(&id))
                        }
                        "PATCH" => match seen.uploads.get_mut(id) {
                            Some(bytes) => {
                                bytes.extend_from_slice(&request.body);
                                Response::new(202).header("location", location(id))
                            }
                            None => Response::new(404),
                        },
                        "PUT" => match seen.uploads.remove(id) {
                            Some(mut bytes) => {
                                bytes.extend_from_slice(&request.body);
                                let digest = request.query("digest").unwrap().to_string();
                                if hushrun_core::digest::Digest::of(&bytes).to_string() != digest {
                                    Response::new(400)
                                } else {
                                    seen.blobs.insert(digest.clone(), bytes);
                                    Response::new(201)
                                        .header("location", format!("/v2/acme/shop/blobs/{digest}"))
                                        .header("docker-content-digest", digest)
                                }
                            }
                            None => Response::new(404),
                        },
                        _ => Response::new(405),
                    });
                }
                if let Some(digest) = rest.strip_prefix("blobs/") {
                    return Reply::Send(match seen.blobs.get(digest) {
                        Some(b) => Response::new(200)
                            .header("content-length", b.len().to_string())
                            .header("docker-content-digest", digest),
                        None => Response::new(404),
                    });
                }
                if let Some(tag) = rest.strip_prefix("manifests/") {
                    let denied = |status: u16, message: &str| {
                        Response::json(
                            status,
                            &serde_json::json!({"errors": [{"code": "DENIED", "message": message}]}),
                        )
                    };
                    if request.method != "PUT" {
                        return Reply::Send(Response::new(404));
                    }
                    if !tag.starts_with("hushrun-packages-") {
                        return Reply::Send(denied(403, "only the granted tags"));
                    }
                    let digest = hushrun_core::digest::Digest::of(&request.body).to_string();
                    if tag.ends_with("-latest") && seen.latest_newer {
                        return Reply::Send(denied(409, "not_newer: run 9 holds it"));
                    }
                    if !tag.ends_with("-latest")
                        && seen.tags.get(tag).is_some_and(|held| *held != digest)
                    {
                        return Reply::Send(denied(409, "the fixed tag names another image"));
                    }
                    seen.tags.insert(tag.to_string(), digest.clone());
                    return Reply::Send(Response::new(201).header("docker-content-digest", digest));
                }
                Reply::Send(Response::new(404))
            });
            let registry_host = registry.addr().to_string();
            let at = seen.clone();
            let backend = Server::start(move |request| {
                let mut seen = at.lock().unwrap();
                if request.path != format!("/o/acme/{GRANT_PATH}")
                    || request.header("authorization")
                        != Some(format!("Bearer {JOB_TOKEN}").as_str())
                {
                    return Reply::Send(Response::new(404));
                }
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                seen.grants.push(body.clone());
                if let Some((status, refusal)) = &seen.refuse_grant {
                    return Reply::Send(Response::json(*status, refusal));
                }
                let platform = body["platform"].as_str().unwrap();
                let identity = body["identity"].as_str().unwrap();
                Reply::Send(Response::json(
                    200,
                    &serde_json::json!({
                        "registry": registry_host,
                        "repository": "acme/shop",
                        "image": format!("{registry_host}/acme/shop"),
                        "username": "hushrun-push",
                        "password": PASSWORD,
                        "expires_ms": 1,
                        "tags": {
                            "fixed": format!("hushrun-packages-{platform}-{}", &identity[..16]),
                            "latest": format!("hushrun-packages-{platform}-latest"),
                        },
                        "chunk_bytes": CHUNK,
                        "most_blob_bytes": 1u64 << 31,
                    }),
                ))
            });
            Fake {
                backend,
                registry,
                seen,
            }
        }

        fn target(&self) -> Target {
            Target::Hosted(Box::new(Hosted {
                caller: crate::cmd::build_relay::caller::Caller::with_token(
                    &format!("{}/o/acme", self.backend.url()),
                    JOB_TOKEN,
                ),
                image: Repository::parse(&format!("{}/acme/shop", self.registry.addr())).unwrap(),
                commit: "c".repeat(40),
                insecure: Vec::new(),
                plain: true,
            }))
        }

        fn seen(&self) -> std::sync::MutexGuard<'_, Seen> {
            self.seen.lock().unwrap()
        }
    }

    /// A store whose archive takes a few chunks of the fake registry's.
    fn big_store(folder: &Path) -> Packages {
        let bytes: Vec<u8> = (0..5000u32).map(|i| (i * 7 % 251) as u8).collect();
        store_at(folder, "linux-x64", &[("cache/a.tgz", &bytes, false)], &[])
    }

    /// Everything a publish says, which must never hold the grant's password.
    fn said(published: &Result<Published, String>) -> String {
        let text = match published {
            Ok(p) => format!(
                "{p:?} {}",
                crate::cmd::packages::tests_published_lines(p).join("\n")
            ),
            Err(why) => why.clone(),
        };
        assert!(!text.contains(PASSWORD), "{text}");
        text
    }

    // covers: K-14
    #[test]
    fn the_backends_registry_takes_a_store_with_a_push_grant_in_chunks_fixed_tag_first() {
        let fake = Fake::start();
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("store");
        let packages = big_store(&folder);
        let identity = packages.identity.to_string();
        let fixed = tags::packages_fixed_tag("linux-x64", &identity);
        let latest = tags::packages_latest_tag("linux-x64");
        let target = fake.target();
        let published = publish(&folder, &target, Ok(()));
        said(&published);
        let published = published.unwrap();
        assert_eq!(published.tags, [fixed.clone(), latest.clone()]);
        assert!(published.uploaded && !published.held_back);
        {
            let seen = fake.seen();
            // The grant was asked with the store's platform and identity and the commit.
            assert_eq!(
                seen.grants,
                [
                    serde_json::json!({"platform": "linux-x64", "identity": identity, "commit": "c".repeat(40)})
                ]
            );
            // No request carried more than a chunk, and the layer took several.
            assert!(
                seen.requests.iter().all(|(_, _, n)| *n <= CHUNK),
                "{:?}",
                seen.requests
            );
            let patches = seen
                .requests
                .iter()
                .filter(|(m, _, _)| m == "PATCH")
                .count();
            assert!(patches >= 5, "{:?}", seen.requests);
            // The fixed tag, then the moving one; never a digest.
            let manifests: Vec<&str> = seen
                .requests
                .iter()
                .filter(|(m, p, _)| m == "PUT" && p.contains("/manifests/"))
                .map(|(_, p, _)| p.rsplit('/').next().unwrap())
                .collect();
            assert_eq!(manifests, [fixed.as_str(), latest.as_str()]);
            assert_eq!(seen.tags.get(&fixed), Some(&published.digest));
        }
        // Outside default-branch CI the moving tag isn't asked for, and the blobs the registry
        // holds don't go up again.
        fake.seen().requests.clear();
        let again = publish(&folder, &target, Err("outside CI".into())).unwrap();
        assert_eq!(again.tags, std::slice::from_ref(&fixed));
        assert!(!again.uploaded);
        assert!(again.notes[0].contains("outside CI"), "{:?}", again.notes);
        let seen = fake.seen();
        assert!(
            !seen.requests.iter().any(|(m, _, _)| m == "PATCH"),
            "{:?}",
            seen.requests
        );
        assert!(!seen.requests.iter().any(|(_, p, _)| p.ends_with("-latest")));
    }

    // covers: K-14
    #[test]
    fn a_held_fixed_tag_stays_and_a_newer_runs_moving_tag_is_left_alone() {
        let fake = Fake::start();
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("store");
        let packages = big_store(&folder);
        let fixed = tags::packages_fixed_tag("linux-x64", &packages.identity.to_string());
        let latest = tags::packages_latest_tag("linux-x64");
        // The same store signed another time: its fixed tag names another image already.
        fake.seen()
            .tags
            .insert(fixed.clone(), "sha256:other".into());
        let published = publish(&folder, &fake.target(), Ok(()));
        said(&published);
        let published = published.unwrap();
        assert_eq!(published.tags, std::slice::from_ref(&latest));
        assert!(
            published
                .notes
                .iter()
                .any(|n| n.contains("fixed tags never move")),
            "{:?}",
            published.notes
        );
        assert_eq!(
            fake.seen().tags.get(&fixed).map(String::as_str),
            Some("sha256:other")
        );
        // A newer run holds the moving tag: it stays, and the publish still succeeds.
        fake.seen().tags.clear();
        fake.seen().latest_newer = true;
        let published = publish(&folder, &fake.target(), Ok(())).unwrap();
        assert_eq!(published.tags, std::slice::from_ref(&fixed));
        assert!(
            published
                .notes
                .iter()
                .any(|n| n.contains("a newer run holds")),
            "{:?}",
            published.notes
        );
    }

    // covers: K-14
    #[test]
    fn a_refused_grant_says_why_and_a_newer_runs_grant_publishes_nothing() {
        let fake = Fake::start();
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("store");
        big_store(&folder);
        fake.seen().refuse_grant = Some((
            403,
            serde_json::json!({"error": {"code": "not_publisher", "message": "not the default branch"}, "current": {"word": "default_branch"}}),
        ));
        let refused = publish(&folder, &fake.target(), Ok(()));
        let why = said(&refused);
        assert!(refused.is_err());
        assert!(
            why.contains("default_branch") && why.contains("--oci-dir"),
            "{why}"
        );
        fake.seen().refuse_grant = Some((
            409,
            serde_json::json!({"error": {"code": "not_newer", "message": "run 9"}, "current": {"run_id": "9", "run_attempt": "1", "digest": "sha256:x"}}),
        ));
        let held = publish(&folder, &fake.target(), Ok(()));
        said(&held);
        let held = held.unwrap();
        assert!(held.held_back && held.tags.is_empty());
        assert!(
            held.notes[0].contains("a newer run holds"),
            "{:?}",
            held.notes
        );
        // Nothing reached the registry.
        assert!(fake.seen().requests.is_empty());
        for (status, code, words) in [
            (404, "", "--oci REPOSITORY"),
            (429, "rate_limited", "bake again later"),
            (409, "name_taken", "an owner settles it"),
        ] {
            fake.seen().refuse_grant = Some((
                status,
                serde_json::json!({"error": {"code": code, "message": ""}}),
            ));
            let why = said(&publish(&folder, &fake.target(), Ok(())));
            assert!(why.contains(words), "{status}: {why}");
        }
    }
}

//! B-65 to B-67: a baked store published as a layered OCI image, its tags, and its signatures.
//!
//! [`publish`] turns the folder `hushrun build bake` wrote into an image (the layering is
//! `hushrun_artifact::image::plan`'s one decision), signs it, and pushes it to a registry or writes
//! it as an OCI image layout. [`verify_store`] is what a run checks, as it begins, of a baked
//! store's signature: the evidence `baked::Evidence::Signed` stands for.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use hushrun_artifact::image::{self as oci, Entry, Group, Previous, Referrer};
use hushrun_artifact::{Artifacts, Repository};
use hushrun_attest::bundle::Object;
use hushrun_attest::{PAYLOAD_TYPE, TrustedRoot};
use hushrun_build::baked::{self, BAKED_JSON, Baked, PackRow, SIGNATURE};
use hushrun_core::blocks::Hex32;
use hushrun_core::blocks::image::{self as tags, STORE_ROOT};
use hushrun_core::digest::Digest;
use hushrun_core::event::STATEMENT_TYPE;
use hushrun_core::policy::BakedSigner;
use serde_json::{Value, json};

use crate::signing::Signing;

/// The predicate type of a baked store's signature, over `baked.json` and its hints (B-67).
pub(crate) const STORE_PREDICATE: &str = "https://hushrun.com/attestations/baked-store/v1";
/// The predicate type of an image's signature, over its manifest (B-67).
pub(crate) const IMAGE_PREDICATE: &str = "https://hushrun.com/attestations/baked-image/v1";
/// A Sigstore bundle's media type, as a referrer's artifact type and layer.
pub(crate) const BUNDLE_MEDIA_TYPE: &str = "application/vnd.dev.sigstore.bundle.v0.3+json";
/// The most bytes a store's signature may have.
const MOST_SIGNATURE: u64 = 4 << 20;

/// Cargo.lock's packages, by where they come from: what tells a crate of the repository's own
/// (a path package) from a third-party one (a registry or git source).
#[derive(Debug, Clone, Default)]
pub(crate) struct ThirdParty {
    sourced: HashSet<String>,
    local: HashSet<String>,
}

impl ThirdParty {
    /// Reads `Cargo.lock` in `checkout`: each `[[package]]`'s `name`, and whether it has a
    /// `source`. `None` with no lockfile.
    pub(crate) fn read(checkout: &Path) -> Option<ThirdParty> {
        let text = fs::read_to_string(checkout.join("Cargo.lock")).ok()?;
        let mut out = ThirdParty::default();
        let mut name: Option<String> = None;
        let mut sourced = false;
        let mut finish = |name: &mut Option<String>, sourced: &mut bool| {
            if let Some(name) = name.take() {
                if *sourced {
                    out.sourced.insert(name);
                } else {
                    out.local.insert(name);
                }
            }
            *sourced = false;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                finish(&mut name, &mut sourced);
                continue;
            }
            if let Some(value) = line.strip_prefix("name = ") {
                name = Some(value.trim_matches('"').to_string());
            } else if line.starts_with("source = ") {
                sourced = true;
            }
        }
        finish(&mut name, &mut sourced);
        Some(out)
    }

    /// The nearest `Cargo.lock` from `folder` up to `checkout`, read as [`ThirdParty::read`]
    /// reads one: a Cargo workspace may sit in a folder of the checkout.
    pub(crate) fn read_near(folder: &Path, checkout: &Path) -> Option<ThirdParty> {
        for dir in folder.ancestors() {
            if dir.join("Cargo.lock").is_file() {
                return ThirdParty::read(dir);
            }
            if dir == checkout {
                break;
            }
        }
        ThirdParty::read(checkout)
    }

    /// B-72: the lockfile's path packages that no registry or git source names: the repository's
    /// own crates, which a dependencies-only build compiles as stubs.
    pub(crate) fn own_only(&self) -> Vec<String> {
        let mut own: Vec<String> = self
            .local
            .iter()
            .filter(|p| !self.sourced.contains(*p))
            .cloned()
            .collect();
        own.sort();
        own
    }

    /// The group of a Rust block of `package` (`CARGO_PKG_NAME`): third-party only when the
    /// lockfile names it with a source and never as a path package; anything it can't place is
    /// the repository's own, which changes more often.
    pub(crate) fn group(&self, package: Option<&str>) -> Group {
        match package {
            Some(p) if self.sourced.contains(p) && !self.local.contains(p) => Group::Deps,
            _ => Group::Code,
        }
    }
}

/// Where an image goes.
pub(crate) enum Target {
    /// A registry's repository.
    Registry {
        repository: Repository,
        artifacts: Box<Artifacts>,
    },
    /// An OCI image layout in a folder.
    Layout(PathBuf),
    /// B-70: the backend's registry, read-only to the outside: the previous image is read from it
    /// as from any registry, and the image is published by one request to the backend.
    Hosted(Box<Hosted>),
}

/// B-70: the backend's registry, and what publishes there.
pub(crate) struct Hosted {
    /// The image's repository there, `<registry host>/<org>/<name>`.
    pub(crate) repository: Repository,
    /// A client of it, with the job's own credential for that host.
    pub(crate) artifacts: Box<Artifacts>,
    /// The bake's relay, whose client sends the publish with the job's credential.
    pub(crate) relay: std::sync::Arc<super::Relay>,
    /// The workspace's commit, which the job's ID token must name.
    pub(crate) commit: Option<String>,
}

/// What reads images where a bake publishes.
enum Reading<'a> {
    Registry(&'a Repository, &'a Artifacts),
    Layout(&'a Path),
}

impl Target {
    fn reading(&self) -> Reading<'_> {
        match self {
            Target::Registry {
                repository,
                artifacts,
            } => Reading::Registry(repository, artifacts),
            Target::Hosted(hosted) => Reading::Registry(&hosted.repository, &hosted.artifacts),
            Target::Layout(dir) => Reading::Layout(dir),
        }
    }
}

impl Reading<'_> {
    /// The image `tag` names, when a bake published it (its label read).
    fn previous(&self, tag: &str) -> Result<Option<Previous>, String> {
        match self {
            Reading::Registry(repository, artifacts) => artifacts
                .previous(repository, tag)
                .map_err(|e| e.to_string()),
            Reading::Layout(dir) => Ok(oci::layout_previous(dir, tag)),
        }
    }

    /// The manifest `tag` names, when it names one.
    fn tagged(&self, tag: &str) -> Result<Option<hushrun_artifact::Manifest>, String> {
        match self {
            Reading::Registry(repository, artifacts) => {
                artifacts.tagged(repository, tag).map_err(|e| e.to_string())
            }
            Reading::Layout(dir) => Ok(oci::layout_tagged(dir, tag)),
        }
    }

    /// The signature bundles beside the image `digest`.
    fn bundles(&self, digest: &Digest) -> Result<Vec<Vec<u8>>, String> {
        match self {
            Reading::Registry(repository, artifacts) => artifacts
                .referrer_layers(repository, digest, BUNDLE_MEDIA_TYPE, MOST_SIGNATURE)
                .map_err(|e| e.to_string()),
            Reading::Layout(dir) => Ok(oci::layout_referrer_layers(
                dir,
                digest,
                BUNDLE_MEDIA_TYPE,
                MOST_SIGNATURE,
            )),
        }
    }

    /// Whether it holds the blob `digest`.
    fn has(&self, digest: &Digest) -> Result<bool, String> {
        match self {
            Reading::Registry(repository, artifacts) => artifacts
                .has_blob(repository, digest)
                .map_err(|e| e.to_string()),
            Reading::Layout(dir) => Ok(oci::layout_has(dir, digest)),
        }
    }
}

/// What a publish did.
pub(crate) struct Published {
    pub(crate) digest: Digest,
    pub(crate) reference: String,
    pub(crate) tags: Vec<String>,
    pub(crate) pushed: oci::Pushed,
    pub(crate) layers: Vec<(oci::Role, Digest, u64, usize, bool)>,
    pub(crate) stale: usize,
    pub(crate) compacted: Option<String>,
    pub(crate) signed: Option<String>,
    /// Published to a registry, not written into a folder.
    pub(crate) registry: bool,
    /// What the bake says of its tags and of the image it built on.
    pub(crate) notes: Vec<String>,
    /// B-70: what the backend's registry answered, when published there.
    pub(crate) hosted: Option<HostedAnswer>,
    /// What the bake says loudly, on stderr whatever the format: a fixed tag taken by an image
    /// that doesn't count (B-66).
    pub(crate) warnings: Vec<String>,
    /// B-73: why nothing was published, when the re-bake limit held: the tags stay where they
    /// are.
    pub(crate) skipped: Option<String>,
}

impl Published {
    /// B-73: a bake that published nothing, the re-bake limit holding for `why`.
    fn skipped(reference: String, registry: bool, why: String) -> Published {
        Published {
            digest: Digest::of(b""),
            reference,
            tags: Vec::new(),
            pushed: oci::Pushed::default(),
            layers: Vec::new(),
            stale: 0,
            compacted: None,
            signed: None,
            registry,
            notes: Vec::new(),
            hosted: None,
            warnings: Vec::new(),
            skipped: Some(why),
        }
    }
}

/// B-70: what a publish to the backend's registry came to.
#[derive(Debug, Clone)]
pub(crate) struct HostedAnswer {
    /// The image's name there, `<registry host>/<org>/<name>`.
    pub(crate) image: String,
    /// `published`, `unchanged`, or `not_newer` (a newer run moved the moving tag: nothing of
    /// this bake was published).
    pub(crate) state: &'static str,
    /// This store was published before: the moving tag went to that image.
    pub(crate) before: bool,
    /// The bytes of the request sent.
    pub(crate) sent: u64,
    /// Every request's: how many, all their bytes, and whether a stale answer made it plan again.
    pub(crate) tries: super::hosted::Tries,
}

/// B-67: the in-toto statement a store's signature covers: `baked.json` by its SHA-256, and the
/// hint files the store holds, each named for the SHA-256 of its bytes.
pub(crate) fn store_statement(baked: &Baked, baked_json: &Hex32, hints: &[String]) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "_type": STATEMENT_TYPE,
        "subject": [{"name": BAKED_JSON, "digest": {"sha256": baked_json.to_string()}}],
        "predicateType": STORE_PREDICATE,
        "predicate": {
            "repository": baked.repository,
            "manifest": baked.manifest.to_string(),
            "platform": baked.platform,
            "hints": hints,
        },
    }))
    .expect("statements serialize")
}

/// B-67: the in-toto statement an image's signature covers: the image's manifest by its
/// digest, as cosign checks it, and the `baked.json` it holds, with what [`image_counts`] reads
/// of that store: when it was baked, its source and its claim.
fn image_statement(
    repository: &str,
    image: &Digest,
    baked: &Baked,
    baked_json: &Hex32,
    tags: &[String],
) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "_type": STATEMENT_TYPE,
        "subject": [{"name": repository, "digest": {"sha256": image.hex()}}],
        "predicateType": IMAGE_PREDICATE,
        "predicate": {
            "repository": baked.repository,
            "manifest": baked.manifest.to_string(),
            "platform": baked.platform,
            "baked_json": {"sha256": baked_json.to_string()},
            "tags": tags,
            "baked_ms": baked.baked_ms,
            "source": baked.source,
            "trust": baked.trust,
        },
    }))
    .expect("statements serialize")
}

/// The files of a baked store's folder, as the image's entries: `baked.json` and its signature
/// in the recent layer, each pack and hint in its block's group.
fn entries(
    folder: &Path,
    baked: &Baked,
    packs: &HashMap<Hex32, Group>,
    hints: &HashMap<Hex32, Group>,
) -> Result<Vec<Entry>, String> {
    let at = |e: std::io::Error, p: &Path| format!("{}: {e}", p.display());
    let mut out = Vec::new();
    let file = |path: &Path, group: Group, name: String| -> Result<Entry, String> {
        let bytes = fs::read(path).map_err(|e| at(e, path))?;
        Ok(Entry {
            path: name,
            size: bytes.len() as u64,
            sha256: Digest::of(&bytes),
            group,
            source: path.to_path_buf(),
        })
    };
    for name in [BAKED_JSON, SIGNATURE] {
        let path = folder.join(name);
        if path.is_file() {
            out.push(file(&path, Group::Store, format!("{STORE_ROOT}/{name}"))?);
        }
    }
    for row in &baked.packs {
        let path = baked::pack_path(folder, &row.address);
        out.push(Entry {
            path: format!("{STORE_ROOT}/{}/{}.pack", baked::PACKS, row.address),
            size: row.bytes,
            sha256: Digest::from_bytes(row.sha256.0),
            // B-65: a program the repository links goes up with every merge that relinks it.
            group: match packs.get(&row.address).copied() {
                Some(Group::Deps) => Group::Deps,
                _ if row.class == hushrun_core::blocks::Class::Program => Group::Program,
                Some(group) => group,
                None => Group::Code,
            },
            source: path,
        });
    }
    for hint in baked::hint_files(folder).ok_or("the hints can't be read")? {
        let step = hint
            .split('/')
            .nth(1)
            .and_then(Hex32::parse)
            .ok_or_else(|| format!("{hint} is not a hint"))?;
        let group = hints.get(&step).copied().unwrap_or(Group::Code);
        out.push(file(
            &folder.join(&hint),
            group,
            format!("{STORE_ROOT}/{hint}"),
        )?);
    }
    Ok(out)
}

/// Rows of `baked.json`, by their pack's path in the image.
fn rows_by_path(baked: &Baked) -> BTreeMap<String, Value> {
    baked
        .packs
        .iter()
        .map(|row| {
            (
                format!("{STORE_ROOT}/{}/{}.pack", baked::PACKS, row.address),
                serde_json::to_value(row).expect("rows serialize"),
            )
        })
        .collect()
}

/// The image's `baked.json`: the bake's, with the rows `carried` of the packs the workspace layer
/// keeps unused (B-65), read back as one.
fn image_baked(baked: &Baked, carried: &[PackRow]) -> Option<Baked> {
    if carried.is_empty() {
        return Some(baked.clone());
    }
    let mut out = baked.clone();
    out.packs.extend(carried.iter().cloned());
    Baked::parse(&serde_json::to_vec_pretty(&out).ok()?).ok()
}

/// What tells which rows the backend vouches for now (outputs, pack SHA-256, size), of those asked; `None` when it
/// can't say (B-65).
pub(crate) type Vouch<'a> = &'a dyn Fn(&[PackRow]) -> Option<Vec<PackRow>>;

/// What decides which tags a bake moves and what verifies the image it builds on (B-66, B-67).
pub(crate) struct Publishing<'a> {
    /// The moving tag moves: CI on the repository's default branch ([`moves_latest`]).
    pub(crate) latest: bool,
    /// The branch whose tag moves (`--branch-tag`), when asked.
    pub(crate) branch: Option<String>,
    /// The store is complete: only a complete store takes its fixed tag.
    pub(crate) complete: bool,
    /// The repository's `build: baked: signers:`, which a previous image must be signed by for a
    /// bake to build on it, and the trust root a certificate is checked with.
    pub(crate) signers: &'a [BakedSigner],
    pub(crate) root: Option<&'a TrustedRoot>,
    /// The repository's `build: baked: not_before`, in milliseconds: an image of a store baked
    /// before it doesn't count ([`image_counts`]).
    pub(crate) not_before: Option<i64>,
    /// For a store baked from a backend: the rows of those asked that the backend vouches for
    /// now (outputs, pack SHA-256, size) in the repository's trusted scope; `None` when it can't
    /// say.
    pub(crate) vouch: Option<Vouch<'a>>,
    /// B-65: the workspace layer is written again once this old (`build: baked: compact_every`,
    /// a week by default); `None`, never for its age.
    pub(crate) compact_every_ms: Option<u64>,
    /// B-73: the re-bake limit; `None` with `--force` or turned off.
    pub(crate) rebake: Option<Rebake>,
    /// B-72: the store is a dependencies-only build's: its image's label says so.
    pub(crate) deps_only: bool,
}

/// B-72: whether a dependencies-only bake publishes nothing, because a tag it would move (`tags`)
/// names an image whose label doesn't say it is dependencies only: a full image, which a store of
/// third-party code alone would set back. Asked before the build, so a job whose repository has
/// its first full image already compiles nothing. `Ok(None)`: every tag names nothing or another
/// dependencies-only image. An image that can't be read is an error.
pub(crate) fn deps_only_waits(target: &Target, tags: &[String]) -> Result<Option<String>, String> {
    let reading = target.reading();
    for tag in tags {
        if reading.tagged(tag)?.is_none() {
            continue;
        }
        match reading.previous(tag)? {
            Some(previous) if previous.label.deps_only => {}
            _ => {
                return Ok(Some(format!(
                    "{tag} names a full image already, which a dependencies-only store would set back"
                )));
            }
        }
    }
    Ok(None)
}

/// B-73: how often a bake publishes over the image its tag names: not while that image is younger
/// than `every_ms` and fewer than `blocks` blocks changed since.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rebake {
    pub(crate) every_ms: u64,
    pub(crate) blocks: u64,
}

/// The re-bake limit's defaults (B-73): an hour, and 50 blocks.
pub(crate) const REBAKE_EVERY_MS: u64 = 60 * 60 * 1000;
pub(crate) const REBAKE_BLOCKS: u64 = 50;

impl Rebake {
    /// B-73: whether a bake of `store` publishes over the image its tag names, `previous`, baked
    /// at `previous_ms` (its signed statement's `baked_ms`): `Some(why)` when it doesn't, that
    /// image younger than `every_ms` by the bake's own clock (`store`'s `baked_ms`) and fewer than
    /// `blocks` of the store's packs not in it. The one decision, evaluated as the bake runs from
    /// the time that image records; an image with no recorded time, one past the bake's clock
    /// (never "recent"), or a dependencies-only image under a full bake (`deps_only`: this
    /// bake's) is no reason to wait.
    pub(crate) fn holds(
        &self,
        store: &Baked,
        deps_only: bool,
        previous: &Previous,
        previous_ms: Option<i64>,
    ) -> Option<String> {
        // B-72: a full store replaces a dependencies-only image whatever its age.
        if previous.label.deps_only && !deps_only {
            return None;
        }
        let previous_ms = u64::try_from(previous_ms?).ok()?;
        let age = store.baked_ms.checked_sub(previous_ms)?;
        if age >= self.every_ms {
            return None;
        }
        let held: HashSet<&str> = previous
            .label
            .layers
            .iter()
            .flat_map(|l| &l.files)
            .map(|f| f.path.as_str())
            .collect();
        let changed = store
            .packs
            .iter()
            .filter(|row| {
                !held.contains(
                    format!("{STORE_ROOT}/{}/{}.pack", baked::PACKS, row.address).as_str(),
                )
            })
            .count() as u64;
        (changed < self.blocks).then(|| {
            format!(
                "the image it would replace was baked {} ago and only {changed} {} changed since (a new image waits {} unless {} blocks changed; the relay fetches the rest meanwhile). --force publishes anyway",
                minutes(age),
                if changed == 1 { "block" } else { "blocks" },
                minutes(self.every_ms),
                self.blocks
            )
        })
    }
}

/// A span of milliseconds as minutes, or seconds below one.
fn minutes(ms: u64) -> String {
    match ms / 60_000 {
        0 => format!("{} s", ms / 1000),
        1 => "1 minute".into(),
        m => format!("{m} minutes"),
    }
}

/// B-66: whether a bake in this environment moves the moving tag: CI, not a pull request, on a
/// branch that is the repository's default branch as the CI provider names it. `Err` says why
/// not, for the line.
pub(crate) fn moves_latest(context: &hushrun_ci::RunContext) -> Result<(), String> {
    if !context.in_ci {
        return Err("outside CI".into());
    }
    if context.pull_request.is_some() {
        return Err("in a pull request's run".into());
    }
    let default = context
        .source
        .as_ref()
        .and_then(|s| s.default_branch.as_deref());
    match (context.branch.as_deref(), default) {
        (Some(branch), Some(default)) if branch == default => Ok(()),
        (Some(branch), Some(default)) => {
            Err(format!("on {branch}, not the default branch {default}"))
        }
        (None, _) => Err("on no branch".into()),
        (_, None) => Err("the CI provider doesn't name the default branch".into()),
    }
}

/// B-65, B-67: the image a bake builds on, when it may: the moving tag's (`tag`), read without
/// error and signed, as its manifest, by a signer the repository names. Otherwise `None`, and why.
fn verified_previous(
    reading: &Reading<'_>,
    tag: &str,
    store: &Baked,
    publishing: &Publishing<'_>,
) -> (Option<Counted>, Option<String>) {
    let previous = match reading.previous(tag) {
        Ok(Some(previous)) => previous,
        Ok(None) => return (None, None),
        Err(why) => {
            return (
                None,
                Some(format!("the previous image couldn't be read ({why})")),
            );
        }
    };
    let bundles = reading
        .bundles(&previous.manifest_digest)
        .unwrap_or_default();
    let mut why = Vec::new();
    for bundle in &bundles {
        match image_counts(bundle, &previous.manifest_digest, store, publishing) {
            Ok(_) => {
                let baked_ms = statement_payload(bundle)
                    .ok()
                    .and_then(|p| serde_json::from_slice::<Value>(&p).ok())
                    .and_then(|s| s["predicate"]["baked_ms"].as_i64());
                return (Some(Counted { previous, baked_ms }), None);
            }
            Err(e) => why.push(e),
        }
    }
    let why = if bundles.is_empty() {
        "the previous image has no signature".to_string()
    } else {
        format!(
            "the previous image's signature isn't evidence: {}",
            why.join("; ")
        )
    };
    (None, Some(why))
}

/// The image a bake builds on, once it counts, and when it was baked, as its signed statement
/// says.
pub(crate) struct Counted {
    pub(crate) previous: Previous,
    pub(crate) baked_ms: Option<i64>,
}

/// B-73: whether a bake of `store` waits instead of publishing over the image `base` names where
/// `reading` reads: the limit only holds over an image that counts ([`verified_previous`]).
fn rebake_waits(
    reading: &Reading<'_>,
    base: &str,
    store: &Baked,
    publishing: &Publishing<'_>,
) -> Option<String> {
    let rebake = publishing.rebake?;
    let (counted, _) = verified_previous(reading, base, store, publishing);
    let counted = counted?;
    rebake.holds(
        store,
        publishing.deps_only,
        &counted.previous,
        counted.baked_ms,
    )
}

/// B-67: whether the bundle `bytes` is an image signature over the manifest `subject` by one of
/// `signers`: who signed it, or why not. The same signer check as a store's ([`verify_store`]).
pub(crate) fn verify_image(
    bytes: &[u8],
    signers: &[BakedSigner],
    root: Option<&TrustedRoot>,
    subject: &Digest,
) -> Result<(String, Option<i64>), String> {
    if signers.is_empty() {
        return Err("hushrun.yaml names no signer of baked stores (build: baked: signers:)".into());
    }
    let signed = signed_by(bytes, signers, root)?;
    let payload = statement_payload(bytes)?;
    let statement: Value =
        serde_json::from_slice(&payload).map_err(|_| "its statement isn't JSON".to_string())?;
    if statement["_type"] != STATEMENT_TYPE || statement["predicateType"] != IMAGE_PREDICATE {
        return Err("it is no signature of a baked image".into());
    }
    let subjects = statement["subject"].as_array().cloned().unwrap_or_default();
    let names_it = subjects.len() == 1 && subjects[0]["digest"]["sha256"] == subject.hex().as_str();
    if !names_it {
        return Err("it was made for another image".into());
    }
    Ok(signed)
}

/// B-65 to B-67: whether the image `subject` counts for a bake of `store`, by its signature
/// `bundle`: the one check of "this image counts", which the bake's fixed-tag shortcut and the
/// image it builds on both make. An image signature by a signer the repository names
/// ([`verify_image`]), over that image, whose statement names this store's repository and platform,
/// the same source and claim (a laptop's store never stands for a backend's trusted one), and a
/// baking time no earlier than `build: baked: not_before` (an image from before this field was
/// signed names none, and doesn't count once `not_before` is set).
pub(crate) fn image_counts(
    bundle: &[u8],
    subject: &Digest,
    store: &Baked,
    publishing: &Publishing<'_>,
) -> Result<String, String> {
    let (signer, trusted_ms) = verify_image(bundle, publishing.signers, publishing.root, subject)?;
    let payload = statement_payload(bundle)?;
    let statement: Value =
        serde_json::from_slice(&payload).map_err(|_| "its statement isn't JSON".to_string())?;
    predicate_counts(
        &statement["predicate"],
        store,
        publishing.not_before,
        trusted_ms,
    )?;
    Ok(signer)
}

/// B-67: whether a signature made at `trusted_ms` (a keyless signature's timestamp, which its
/// certificate's validity holds and the signer can't choose), else claiming `claimed_ms` (the
/// baking time a key's signature covers, which the key's holder chose), counts from `not_before`
/// on. The one rule for stores and images. A leaked key can sign any claimed time: `not_before` is
/// no defence against it; taking the key out of `build: baked: signers` is.
pub(crate) fn not_before_holds(
    not_before: Option<i64>,
    trusted_ms: Option<i64>,
    claimed_ms: Option<i64>,
) -> Result<(), String> {
    let Some(not_before) = not_before else {
        return Ok(());
    };
    match (trusted_ms, claimed_ms) {
        (Some(at), _) if at < not_before => Err(format!(
            "its timestamp is from before build: baked: not_before ({at} < {not_before} ms)"
        )),
        (Some(_), _) => Ok(()),
        (None, Some(baked_ms)) if baked_ms < not_before => Err(format!(
            "it was baked before build: baked: not_before ({baked_ms} < {not_before} ms)"
        )),
        (None, Some(_)) => Ok(()),
        (None, None) => {
            Err("its statement names no baking time, and build: baked: not_before is set".into())
        }
    }
}

/// [`image_counts`]'s rules for a verified image statement's `predicate`, for a bake of `store`.
fn predicate_counts(
    predicate: &Value,
    store: &Baked,
    not_before: Option<i64>,
    trusted_ms: Option<i64>,
) -> Result<(), String> {
    let named = |field: &str, ours: Value| {
        if predicate[field] == ours {
            Ok(())
        } else {
            Err(format!(
                "its statement names another {field} ({}) than this bake's ({ours})",
                predicate[field]
            ))
        }
    };
    named("repository", json!(store.repository))?;
    named("platform", json!(store.platform))?;
    named("source", json!(store.source))?;
    named("trust", json!(store.trust))?;
    not_before_holds(not_before, trusted_ms, predicate["baked_ms"].as_i64())
}

/// B-65 to B-67: publishes the baked store in `folder` (whose `baked.json` is `baked`) to
/// `target`: the layers [`oci::plan`] decides from the previous image (the tag this bake moves,
/// else the moving tag's), only once that image is signed by a signer the repository names, and
/// with the rows of the packs its workspace layer keeps unused only as far as the backend vouches
/// for them now (a store baked from this machine carries none); the image signed by `signing`
/// when there is one; and the tags: the fixed one only for a complete store and only when it names
/// nothing yet (it never moves: a complete store it names already is published, signed, and the
/// moving tags go to it), then the signature, then the moving tags `publishing` allows. To the
/// backend's registry (B-70) the same image is described in one request instead
/// ([`publish_hosted`]). The folder's own signature, `baked.sigstore.json`, is the caller's to
/// make first ([`sign_folder`]): the recent layer holds it as the folder has it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn publish(
    folder: &Path,
    baked: &Baked,
    packs: &HashMap<Hex32, Group>,
    hints: &HashMap<Hex32, Group>,
    target: &Target,
    signing: Option<&Signing>,
    compact: bool,
    staging: &Path,
    publishing: &Publishing<'_>,
) -> Result<Published, String> {
    if !tags::valid_platform(&baked.platform) {
        return Err(format!("{} is no platform a tag can name", baked.platform));
    }
    let store = Store {
        folder,
        baked,
        packs,
        hints,
    };
    if let Target::Hosted(hosted) = target {
        return publish_hosted(&store, hosted, signing, compact, staging, publishing);
    }
    let reading = target.reading();
    let manifest_hex = baked.manifest.to_string();
    let fixed = tags::fixed_tag(&baked.platform, &manifest_hex);
    let latest = tags::latest_tag(&baked.platform);
    let mut moving = Vec::new();
    if publishing.latest {
        moving.push(latest.clone());
    }
    if let Some(branch) = &publishing.branch {
        let tag = tags::branch_tag(&baked.platform, branch);
        if !moving.contains(&tag) {
            moving.push(tag);
        }
    }
    let mut notes = Vec::new();
    let registry = matches!(target, Target::Registry { .. });
    // B-66: the fixed tag never moves. A complete store whose fixed tag names an image signed by
    // a signer the repository names is published: the moving tags go to that image.
    // An error reading the fixed tag is no answer: the bake publishes, but takes no fixed tag,
    // which never moves.
    let (existing, unread) = match reading.tagged(&fixed) {
        Ok(existing) => (existing, None),
        Err(why) => (None, Some(why)),
    };
    let mut warnings = Vec::new();
    let mut existing_counts = None;
    if publishing.complete
        && let Some(manifest) = &existing
    {
        let signed = verified_previous_at(&reading, manifest, baked, publishing);
        existing_counts = Some(signed.clone());
        if let Ok(Counts::Yes(signer)) = signed {
            let signed: Result<String, String> = Ok(signer);
            // B-72: asked again as the tags move, not only before the build.
            if publishing.deps_only
                && let Some(why) = deps_only_waits(target, &moving).unwrap_or_else(Some)
            {
                return Ok(Published::skipped(
                    target_name(target),
                    registry,
                    format!("{why}: no tag moved"),
                ));
            }
            for tag in &moving {
                match target {
                    Target::Registry {
                        repository,
                        artifacts,
                    } => artifacts
                        .tag(repository, tag, manifest)
                        .map_err(|e| e.to_string())?,
                    Target::Layout(dir) => {
                        oci::layout_tag(dir, tag, manifest).map_err(|e| e.to_string())?
                    }
                    Target::Hosted(_) => unreachable!("published by its own path"),
                }
            }
            let digest = manifest.digest();
            let mut tags = vec![fixed.clone()];
            tags.extend(moving.iter().cloned());
            return Ok(Published {
                digest,
                reference: match target {
                    Target::Registry { repository, .. } => repository.at_digest(&digest),
                    _ => format!("{}@{digest}", target_name(target)),
                },
                tags,
                pushed: oci::Pushed {
                    blobs: Vec::new(),
                    tags: moving.clone(),
                },
                layers: Vec::new(),
                stale: 0,
                compacted: None,
                signed: signed.ok(),
                registry,
                notes: vec![format!(
                    "{fixed} names this store already, signed: nothing new was uploaded"
                )],
                hosted: None,
                warnings: Vec::new(),
                skipped: None,
            });
        }
    }
    // B-73: a new image waits while the one its tag names is young and little changed.
    let base = moving.first().cloned().unwrap_or_else(|| latest.clone());
    if let Some(why) = rebake_waits(&reading, &base, baked, publishing) {
        return Ok(Published::skipped(target_name(target), registry, why));
    }
    let takes_fixed = publishing.complete && existing.is_none() && unread.is_none();
    if let Some(why) = &unread {
        notes.push(format!(
            "whether {fixed} names an image couldn't be read ({why}); the fixed tag never moves, so this image takes none"
        ));
    } else if !publishing.complete {
        notes.push(format!(
            "the store is partial, so it takes no fixed tag ({fixed} names a complete store only)"
        ));
    } else {
        match existing_counts {
            // B-66: the fixed tag never moves, and a bake never takes it over: whoever can push
            // to the repository could have put an image there first. Said loudly.
            Some(Ok(Counts::No(why))) => warnings.push(format!(
                "{fixed} names an image that doesn't count for this store ({why}); the fixed tag never moves, so this image has none, though its moving tags move. Anyone who can push to {} could have put it there: look at who did",
                target_name(target)
            )),
            Some(Err(why)) => notes.push(format!(
                "{fixed} names an image whose signature couldn't be read ({why}); the fixed tag never moves, so this image takes none"
            )),
            _ => {}
        }
    }
    // B-65, B-67: what the bake builds on: the tag it moves, else the moving tag's image (`base`).
    // The folder's own signature, for a store used as a folder, is the caller's: a bake signs
    // the folder once, before it publishes anything of it (B-67, B-71).
    let signer_words = signing.map(Signing::describe);
    let fixed_tags: Vec<String> = if takes_fixed {
        vec![fixed.clone()]
    } else {
        Vec::new()
    };
    let mut named = fixed_tags.clone();
    named.extend(moving.iter().cloned());
    let assembled = assemble(
        &store,
        &reading,
        &base,
        signing,
        compact,
        staging,
        publishing,
        &target_name(target),
        &named,
    )?;
    let image = &assembled.image;
    let digest = image.digest();
    let reference = match target {
        Target::Registry { repository, .. } => repository.at_digest(&digest),
        _ => format!("{}@{digest}", target_name(target)),
    };
    // B-72: a dependencies-only image's moving tags move only once a read made after its layers
    // went up says no full image took them meanwhile (a plain registry has no compare-and-set).
    let moving_now: Vec<String> = if publishing.deps_only {
        Vec::new()
    } else {
        moving.clone()
    };
    let mut named_now = fixed_tags.clone();
    named_now.extend(moving_now.iter().cloned());
    let mut pushed = match target {
        Target::Registry {
            repository,
            artifacts,
        } => artifacts
            .publish(
                repository,
                image,
                &fixed_tags,
                &assembled.referrers,
                &moving_now,
            )
            .map_err(|e| e.to_string())?,
        Target::Layout(dir) => oci::write_layout(dir, image, &named_now, &assembled.referrers)
            .map_err(|e| e.to_string())?,
        Target::Hosted(_) => unreachable!("published by its own path"),
    };
    if publishing.deps_only && !moving.is_empty() {
        match deps_only_waits(target, &moving).unwrap_or_else(Some) {
            None => {
                for tag in &moving {
                    match target {
                        Target::Registry {
                            repository,
                            artifacts,
                        } => artifacts
                            .tag(repository, tag, &image.manifest)
                            .map_err(|e| e.to_string())?,
                        Target::Layout(dir) => {
                            oci::layout_tag(dir, tag, &image.manifest).map_err(|e| e.to_string())?
                        }
                        Target::Hosted(_) => unreachable!("published by its own path"),
                    }
                    pushed.tags.push(tag.clone());
                    named_now.push(tag.clone());
                }
            }
            Some(why) => notes.push(format!("{why}: the moving tags didn't move")),
        }
    }
    let named = named_now;
    Ok(Published {
        digest,
        reference,
        tags: named,
        layers: layer_words(image),
        stale: assembled.plan.stale.len(),
        compacted: assembled.plan.compacted.clone(),
        pushed,
        signed: signer_words,
        registry,
        notes,
        hosted: None,
        warnings,
        skipped: None,
    })
}

/// The baked store a bake publishes: its folder, its `baked.json`, and the layer group of each
/// pack and hint.
struct Store<'a> {
    folder: &'a Path,
    baked: &'a Baked,
    packs: &'a HashMap<Hex32, Group>,
    hints: &'a HashMap<Hex32, Group>,
}

/// An image planned, built and signed, not yet published.
struct Assembled {
    image: oci::Image,
    referrers: Vec<Referrer>,
    plan: oci::Plan,
    /// The image's `baked.json`: the bake's, with the rows it carries.
    image_store: Baked,
}

/// A target's name, as an image statement's subject and a reference say it.
fn target_name(target: &Target) -> String {
    match target {
        Target::Registry { repository, .. } => repository.to_string(),
        Target::Hosted(hosted) => hosted.repository.to_string(),
        Target::Layout(dir) => dir.display().to_string(),
    }
}

/// Each layer of `image` as a bake says it: role, digest, size, files, kept.
fn layer_words(image: &oci::Image) -> Vec<(oci::Role, Digest, u64, usize, bool)> {
    image
        .layers
        .iter()
        .map(|l| (l.role, l.digest, l.size, l.files.len(), l.reused))
        .collect()
}

/// B-65, B-67: the image of `store`, planned on the image `base` names where `reading` reads
/// (only once a signer the repository names signed it), its workspace layer's unused rows carried
/// as the backend vouches for them, built, and its image signature made by `signing` for the
/// repository `name` and the tags `named`. The folder's own signature is the caller's.
#[allow(clippy::too_many_arguments)]
fn assemble(
    store: &Store<'_>,
    reading: &Reading<'_>,
    base: &str,
    signing: Option<&Signing>,
    compact: bool,
    staging: &Path,
    publishing: &Publishing<'_>,
    name: &str,
    named: &[String],
) -> Result<Assembled, String> {
    let Store {
        folder,
        baked,
        packs,
        hints,
    } = *store;
    let manifest_hex = baked.manifest.to_string();
    let (counted, unverified) = verified_previous(reading, base, baked, publishing);
    let previous = counted.map(|c| c.previous);
    let compaction = oci::Compaction {
        asked: compact,
        now_ms: i64::try_from(baked.baked_ms).unwrap_or(i64::MAX),
        every_ms: publishing.compact_every_ms,
    };
    // One answer per digest for the bake: a registry asked twice could answer twice.
    let presence: std::cell::RefCell<HashMap<Digest, bool>> = Default::default();
    let present_error: std::cell::RefCell<Option<String>> = Default::default();
    let present = |digest: &Digest| {
        if let Some(known) = presence.borrow().get(digest) {
            return *known;
        }
        let held = match reading.has(digest) {
            Ok(held) => held,
            Err(e) => {
                *present_error.borrow_mut() = Some(e);
                false
            }
        };
        presence.borrow_mut().insert(*digest, held);
        held
    };
    let mut entries = entries(folder, baked, packs, hints)?;
    let mut plan = oci::plan_at(&entries, previous.as_ref(), &present, &compaction);
    if let Some(why) = present_error.borrow().clone() {
        return Err(format!(
            "the registry couldn't say which layers it holds: {why}"
        ));
    }
    if plan.compacted.as_deref() == Some("there is no previous image")
        && let Some(why) = &unverified
    {
        plan.compacted = Some(why.clone());
    }
    // B-65: the rows of the packs the workspace layer keeps unused, from the verified previous
    // image's label, only as the backend vouches for them now: a withdrawn block is never
    // carried, and a store baked from this machine carries none.
    let carried = carried_rows(&plan.stale, previous.as_ref(), publishing.vouch);
    let carried = match carried {
        Ok(rows) => rows,
        Err(why) => {
            plan = oci::plan_at(
                &entries,
                previous.as_ref(),
                &present,
                &oci::Compaction {
                    asked: true,
                    ..compaction
                },
            );
            plan.compacted = Some(why);
            Vec::new()
        }
    };
    let image_store = image_baked(baked, &carried)
        .ok_or_else(|| "the carried rows don't read back as this store's baked.json".to_string())?;
    if image_store != *baked {
        // The image's baked.json names the packs kept unused too: written, and signed, apart.
        fs::create_dir_all(staging).map_err(|e| format!("{}: {e}", staging.display()))?;
        let json = serde_json::to_vec_pretty(&image_store).map_err(|e| e.to_string())?;
        fs::write(staging.join(BAKED_JSON), &json)
            .map_err(|e| format!("{}: {e}", staging.display()))?;
        let _ = fs::remove_file(staging.join(SIGNATURE));
        if let Some(signing) = signing {
            let hints = baked::hint_files(folder).unwrap_or_default();
            let statement = store_statement(&image_store, &Hex32::sha256(&json), &hints);
            let bundle = sign(signing, &statement)?;
            fs::write(staging.join(SIGNATURE), bundle)
                .map_err(|e| format!("{}: {e}", staging.display()))?;
        }
        for entry in &mut entries {
            if entry.group == Group::Store {
                let name = entry
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let path = staging.join(&name);
                let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                entry.size = bytes.len() as u64;
                entry.sha256 = Digest::of(&bytes);
                entry.source = path;
            }
        }
        if signing.is_none() {
            entries.retain(|e| !e.path.ends_with(SIGNATURE));
        }
        let compacted = plan.compacted.clone();
        let again = oci::plan_at(
            &entries,
            previous.as_ref(),
            &present,
            &oci::Compaction {
                asked: compacted.is_some() && compact,
                ..compaction
            },
        );
        if again.stale != plan.stale {
            return Err("the layers changed between two plans of the same bake".into());
        }
        plan = again;
        plan.compacted = compacted;
    }
    let rows = {
        let mut rows = rows_by_path(&image_store);
        for (path, row) in rows_by_path(baked) {
            rows.entry(path).or_insert(row);
        }
        rows
    };
    plan.deps_only = publishing.deps_only;
    let image =
        oci::build(&plan, &baked.platform, &manifest_hex, &rows).map_err(|e| e.to_string())?;
    let digest = image.digest();
    let mut referrers = Vec::new();
    if let Some(signing) = signing {
        let baked_json_sha =
            Hex32::sha256(&serde_json::to_vec_pretty(&image_store).map_err(|e| e.to_string())?);
        let baked_json_sha = if image_store == *baked {
            baked::baked_json_digest(folder).unwrap_or(baked_json_sha)
        } else {
            baked_json_sha
        };
        let statement = image_statement(name, &digest, &image_store, &baked_json_sha, named);
        let bundle = sign(signing, &statement)?;
        referrers.push(Referrer {
            artifact_type: BUNDLE_MEDIA_TYPE.into(),
            layer_media_type: BUNDLE_MEDIA_TYPE.into(),
            layer: bundle,
            annotations: BTreeMap::from([
                ("dev.sigstore.bundle.content".into(), "dsse-envelope".into()),
                (
                    "dev.sigstore.bundle.predicateType".into(),
                    IMAGE_PREDICATE.into(),
                ),
            ]),
        });
    }
    Ok(Assembled {
        image,
        referrers,
        plan,
        image_store,
    })
}

/// B-70: publishes `store` into the backend's registry: the image planned on the moving tag's
/// (verified as for any registry), signed for the fixed and moving tags, then described in one
/// request ([`super::Relay::publish_image`]); the backend computes the layers from the blocks it
/// holds and moves the moving tag by compare-and-set on the digest read here. A stale answer reads
/// the previous image again and plans again, once; a newer run's win publishes nothing and says
/// so.
fn publish_hosted(
    store: &Store<'_>,
    hosted: &Hosted,
    signing: Option<&Signing>,
    compact: bool,
    staging: &Path,
    publishing: &Publishing<'_>,
) -> Result<Published, String> {
    let baked = store.baked;
    let fixed = tags::fixed_tag(&baked.platform, &baked.manifest.to_string());
    let latest = tags::latest_tag(&baked.platform);
    let name = hosted.repository.to_string();
    let reading = Reading::Registry(&hosted.repository, &hosted.artifacts);
    let signer_words = signing.map(Signing::describe);
    let named = vec![fixed.clone(), latest.clone()];
    // B-73: as on any registry, a new image waits while the moving tag's is young.
    if let Some(why) = rebake_waits(&reading, &latest, baked, publishing) {
        return Ok(Published::skipped(name, true, why));
    }
    // The compare-and-set's expected value: the moving tag's image as read here, whatever it is
    // (an image this bake can't build on still is the one the tag names).
    let mut expected = reading
        .tagged(&latest)
        .map_err(|e| format!("the backend's registry couldn't be read: {e}"))?
        .map(|m| m.digest().to_string());
    let mut last: Option<Assembled> = None;
    let sent = hosted.relay.publish_image(&mut |stale| {
        if let Some(now) = stale {
            expected = now;
        }
        let assembled = assemble(
            store, &reading, &latest, signing, compact, staging, publishing, &name, &named,
        )?;
        let request = publish_request(&assembled, baked, expected.clone(), hosted.commit.clone())?;
        let prepared = super::hosted::Prepared {
            request,
            digest: assembled.image.digest(),
            layers: assembled
                .image
                .layers
                .iter()
                .map(|l| (l.digest, l.size))
                .collect(),
        };
        last = Some(assembled);
        Ok(prepared)
    })?;
    let assembled = last.ok_or("nothing was prepared to publish")?;
    let mut notes = Vec::new();
    let (digest, reference, tags, hosted_answer) = match sent {
        super::hosted::Sent::Published {
            answer,
            before,
            bytes,
            tries,
        } => {
            let digest = Digest::parse(&answer.digest)
                .map_err(|_| format!("the backend named the image {}", answer.digest))?;
            if before {
                notes.push(format!(
                    "this store was published before: {} names {}, and the moving tag went to it, not to the image built now",
                    answer.tags.fixed, answer.digest
                ));
            }
            if tries.replanned {
                notes.push(format!(
                    "the moving tag moved while this bake planned on it: it read the image the backend named and planned again, then published ({} requests, {} bytes sent in all)",
                    tries.requests, tries.bytes
                ));
            }
            let state = match answer.state {
                hushrun_core::blocks::PublishState::Published => "published",
                hushrun_core::blocks::PublishState::Unchanged => "unchanged",
                hushrun_core::blocks::PublishState::Unknown => "unknown",
            };
            (
                digest,
                format!("{}@{}", answer.image, answer.digest),
                vec![answer.tags.fixed.clone(), answer.tags.latest.clone()],
                HostedAnswer {
                    image: answer.image,
                    state,
                    before,
                    sent: bytes,
                    tries,
                },
            )
        }
        super::hosted::Sent::NotNewer(message) => {
            notes.push(format!(
                "a newer run moved {latest} already ({message}): nothing of this bake was published"
            ));
            let digest = assembled.image.digest();
            (
                digest,
                format!("{name}@{digest}"),
                Vec::new(),
                HostedAnswer {
                    image: name.clone(),
                    state: "not_newer",
                    before: false,
                    sent: 0,
                    tries: super::hosted::Tries::default(),
                },
            )
        }
    };
    Ok(Published {
        digest,
        reference,
        tags,
        pushed: oci::Pushed::default(),
        layers: layer_words(&assembled.image),
        stale: assembled.plan.stale.len(),
        compacted: assembled.plan.compacted.clone(),
        signed: signer_words,
        registry: true,
        notes,
        hosted: Some(hosted_answer),
        warnings: Vec::new(),
        skipped: None,
    })
}

/// B-70: the request that describes `assembled` (of the store whose `baked.json` is `baked`) to
/// the backend: each layer's files in order, a pack by its block (its kind from the image's
/// `baked.json` rows), any other file by its bytes, read from this bake's own files and checked
/// against their SHA-256 (a file a reused layer keeps from the previous image is one the backend
/// holds, and is left out); the manifest, config and signature; `previous`, the moving tag's
/// digest the plan read; and the commit.
fn publish_request(
    assembled: &Assembled,
    baked: &Baked,
    previous: Option<String>,
    commit: Option<String>,
) -> Result<hushrun_core::blocks::PublishRequest, String> {
    use hushrun_core::blocks::publish::MOST_BLOB_BYTES;
    use hushrun_core::blocks::{
        Base64, PublishFile, PublishLayer, PublishPack, PublishRequest, PublishSignature,
    };
    let kinds: HashMap<Hex32, &str> = assembled
        .image_store
        .packs
        .iter()
        .chain(baked.packs.iter())
        .map(|row| (row.address, row.kind.as_str()))
        .collect();
    let packs = format!("{STORE_ROOT}/{}/", baked::PACKS);
    let mut blobs = Vec::new();
    let mut sent = HashSet::new();
    let mut layers = Vec::new();
    for layer in &assembled.image.layers {
        let mut files = Vec::new();
        for file in &layer.files {
            let address = file
                .path
                .strip_prefix(&packs)
                .and_then(|n| n.strip_suffix(".pack"))
                .and_then(Hex32::parse);
            let pack = match address {
                Some(address) => Some(PublishPack {
                    kind: kinds
                        .get(&address)
                        .ok_or_else(|| format!("no row of baked.json names the pack {address}"))?
                        .to_string(),
                    address,
                }),
                None => None,
            };
            if pack.is_none() && sent.insert(file.sha256) {
                if file.size > MOST_BLOB_BYTES as u64 {
                    return Err(format!(
                        "{} is {} bytes, more than the {MOST_BLOB_BYTES} a backend takes for a file that isn't a pack",
                        file.path, file.size
                    ));
                }
                if let Some(source) = &file.source {
                    let bytes =
                        fs::read(source).map_err(|e| format!("{}: {e}", source.display()))?;
                    if Digest::of(&bytes) != file.sha256 {
                        return Err(format!("{} changed while it was published", file.path));
                    }
                    blobs.push(Base64(bytes));
                }
            }
            files.push(PublishFile {
                path: file.path.clone(),
                size: file.size,
                sha256: Hex32(*file.sha256.as_bytes()),
                pack,
            });
        }
        layers.push(PublishLayer {
            role: layer.role.word().to_string(),
            files,
        });
    }
    let signature = assembled.referrers.first().map(|r| PublishSignature {
        manifest: Base64(r.manifest(&assembled.image.manifest).bytes),
        bundle: Base64(r.layer.clone()),
    });
    Ok(PublishRequest {
        platform: baked.platform.clone(),
        manifest: baked.manifest,
        image: Base64(assembled.image.manifest.bytes.clone()),
        config: Base64(assembled.image.config.clone()),
        layers,
        blobs,
        signature,
        previous,
        commit,
    })
}

/// Whether an image counts ([`image_counts`]), as the fixed-tag shortcut asks it.
#[derive(Debug, Clone)]
enum Counts {
    /// It counts: who signed it.
    Yes(String),
    /// It doesn't, and why.
    No(String),
}

/// Whether the image `manifest` `reading` reads counts for a bake of `store` ([`image_counts`]):
/// `Err` when its signatures couldn't be read.
fn verified_previous_at(
    reading: &Reading<'_>,
    manifest: &hushrun_artifact::Manifest,
    store: &Baked,
    publishing: &Publishing<'_>,
) -> Result<Counts, String> {
    let digest = manifest.digest();
    let bundles = reading.bundles(&digest)?;
    let mut why = vec!["it has no signature".to_string()];
    for bundle in &bundles {
        match image_counts(bundle, &digest, store, publishing) {
            Ok(signer) => return Ok(Counts::Yes(signer)),
            Err(e) => why.push(e),
        }
    }
    Ok(Counts::No(why.join("; ")))
}

/// B-65: the rows of the packs `stale` (paths the reused workspace layer keeps that this bake
/// doesn't use), from the verified previous image's label, each one the backend vouches for now,
/// whole. `Err` (the workspace layer is written again) when any can't be had that way: no
/// backend to ask (a store baked from this machine), no answer, or a row it no longer vouches for.
fn carried_rows(
    stale: &[String],
    previous: Option<&Previous>,
    vouch: Option<Vouch<'_>>,
) -> Result<Vec<PackRow>, String> {
    let packs: Vec<&String> = stale.iter().filter(|p| p.ends_with(".pack")).collect();
    if packs.is_empty() {
        return Ok(Vec::new());
    }
    let previous = previous.ok_or("there is no previous image to carry from")?;
    let Some(vouch) = vouch else {
        return Err(format!(
            "{} blocks it keeps are no longer used, and a store baked from this machine carries none",
            packs.len()
        ));
    };
    let by_path: HashMap<String, PackRow> = previous
        .label
        .rows
        .iter()
        .filter_map(|row| serde_json::from_value::<PackRow>(row.clone()).ok())
        .map(|row| {
            (
                format!("{STORE_ROOT}/{}/{}.pack", baked::PACKS, row.address),
                row,
            )
        })
        .collect();
    let wanted: Vec<PackRow> = packs
        .iter()
        .map(|path| {
            by_path
                .get(path.as_str())
                .cloned()
                .ok_or_else(|| format!("the previous image's label has no row for {path}"))
        })
        .collect::<Result<_, _>>()?;
    let vouched = vouch(&wanted).ok_or("the backend didn't answer for the blocks it keeps")?;
    if let Some(row) = wanted.iter().find(|row| !vouched.contains(row)) {
        return Err(format!(
            "the backend no longer vouches for {}, which it keeps",
            row.address
        ));
    }
    Ok(wanted)
}

/// B-67: `statement` signed into a bundle that cosign reads too: its envelope's signature names no
/// key id (the bundle's verification material does), since cosign's DSSE check skips a signature
/// whose key id isn't its own form of the key's. The signature and its timestamp are over the
/// envelope's PAE and the signature's bytes, which the key id is no part of.
fn sign(signing: &Signing, statement: &[u8]) -> Result<Vec<u8>, String> {
    let bundle = signing
        .sign_statement(statement)
        .map_err(|f| format!("{:#}", f.error))?;
    let mut value: Value = serde_json::from_slice(&bundle).map_err(|e| e.to_string())?;
    if let Some(signatures) = value["dsseEnvelope"]["signatures"].as_array_mut() {
        for signature in signatures {
            signature["keyid"] = Value::String(String::new());
        }
    }
    serde_json::to_vec(&value).map_err(|e| e.to_string())
}

/// B-67: signs the baked store in `folder` as it is: `baked.sigstore.json` beside its
/// `baked.json`, over that file's bytes and the hint files there.
pub(crate) fn sign_folder(folder: &Path, baked: &Baked, signing: &Signing) -> Result<(), String> {
    let digest = baked::baked_json_digest(folder).ok_or("baked.json can't be read")?;
    let hints = baked::hint_files(folder).ok_or("the hints can't be read")?;
    let statement = store_statement(baked, &digest, &hints);
    let bundle = sign(signing, &statement)?;
    let path = folder.join(SIGNATURE);
    let temp = folder.join(baked::unique(&format!(".{SIGNATURE}.tmp")));
    let written = (|| {
        use std::io::Write as _;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&bundle)?;
        file.sync_all()
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(format!("{}: {e}", temp.display()));
    }
    if let Err(e) = fs::rename(&temp, &path) {
        let _ = fs::remove_file(&temp);
        return Err(format!("{}: {e}", path.display()));
    }
    // Whole on the disk, under its name, before anything is made of the folder.
    if let Ok(dir) = fs::File::open(folder) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// What a verified store signature says (B-67).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Verified {
    /// The signer, in words.
    pub(crate) signer: String,
    /// The SHA-256 of the `baked.json` it covers.
    pub(crate) baked_json: Hex32,
    /// The hint files it covers.
    pub(crate) hints: Vec<String>,
    /// When the `baked.json` it covers says it was baked, read from the bytes it covers.
    pub(crate) baked_ms: u64,
    /// A keyless signature's timestamp, in milliseconds (B-67: what `not_before` compares for
    /// it); `None` for a key's.
    pub(crate) signed_ms: Option<i64>,
}

/// B-67: the one check of a baked store's signature, as a run makes it when it begins: the
/// bundle beside `baked.json` verifies, by one of `signers` (a key's signature over the
/// envelope, or a certificate `root` issued, valid at its RFC 3161 timestamp, whose issuer and
/// subject are a signer's, and not a run a `pull_request_target` started), and its statement
/// is the store statement for exactly this `baked.json`. `Err` says why not.
pub(crate) fn verify_store(
    folder: &Path,
    signers: &[BakedSigner],
    root: Option<&TrustedRoot>,
) -> Result<Verified, String> {
    if signers.is_empty() {
        return Err("hushrun.yaml names no signer of baked stores (build: baked: signers:)".into());
    }
    let path = folder.join(SIGNATURE);
    let meta = fs::symlink_metadata(&path).map_err(|_| format!("there is no {SIGNATURE}"))?;
    if !meta.is_file() || meta.len() > MOST_SIGNATURE {
        return Err(format!("{SIGNATURE} is not a signature"));
    }
    let bytes = fs::read(&path).map_err(|e| format!("{SIGNATURE}: {e}"))?;
    let (signer, signed_ms) = signed_by(&bytes, signers, root)?;
    let payload = statement_payload(&bytes)?;
    let statement: Value =
        serde_json::from_slice(&payload).map_err(|_| "its statement isn't JSON".to_string())?;
    if statement["_type"] != STATEMENT_TYPE || statement["predicateType"] != STORE_PREDICATE {
        return Err("it is no signature of a baked store".into());
    }
    // baked.json read once: its digest and its baking time from the same bytes.
    let (store, digest) = Baked::read_bytes(folder).map_err(|why| why.0)?;
    let subjects = statement["subject"].as_array().cloned().unwrap_or_default();
    let names_it = subjects.len() == 1
        && subjects[0]["name"] == BAKED_JSON
        && subjects[0]["digest"]["sha256"] == digest.to_string().as_str();
    if !names_it {
        return Err("it was made for another baked.json".into());
    }
    let hints: Vec<String> = statement["predicate"]["hints"]
        .as_array()
        .ok_or("its statement lists no hints")?
        .iter()
        .map(|h| h.as_str().map(str::to_string).ok_or("a hint isn't a name"))
        .collect::<Result<_, _>>()?;
    Ok(Verified {
        signer,
        baked_json: digest,
        hints,
        baked_ms: store.baked_ms,
        signed_ms,
    })
}

/// B-67: the events a keyless signature's run may have been started by for it to count: a push,
/// a schedule, a run by hand (GitHub's `workflow_dispatch`, GitLab's `web`), and the run after
/// another workflow (`workflow_run`: the job `hushrun init --bake` writes, whose condition, in the
/// default branch's own file the subject names, admits only a push to that branch of this
/// repository). A certificate's subject names the workflow file and its branch; anything else that
/// starts it (an issue comment, `pull_request_target`, another workflow's call, a fork's request)
/// runs that file with inputs others choose. A certificate that names no event never counts.
const SIGNING_TRIGGERS: &[&str] = &[
    "push",
    "schedule",
    "workflow_dispatch",
    "workflow_run",
    "web",
];

/// B-67: whether a bake started by GitHub's `workflow_run` may sign keylessly. A certificate of
/// such a run names the default branch's own workflow file and the event `workflow_run`, never
/// what started the run it follows, so the bake checks that itself, in the event GitHub gives the
/// job (`GITHUB_EVENT_PATH`): the run it follows was a push, succeeded, of this repository (not a
/// fork's pull request whose branch has the default branch's name), to the default branch, at the
/// commit this job checked out (`GITHUB_SHA`, the default branch's head: a tag named like the
/// branch is another commit). The condition `hushrun init --bake` writes guards the same, and a
/// hand-written job need not. Any other event signs as before. `Err` says why not.
pub(crate) fn workflow_run_signs(env: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
    if env("GITHUB_EVENT_NAME").as_deref() != Some("workflow_run") {
        return Ok(());
    }
    let path = env("GITHUB_EVENT_PATH")
        .ok_or("the job names no event file (GITHUB_EVENT_PATH) to read what started it")?;
    let bytes = fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    let event: Value = serde_json::from_slice(&bytes).map_err(|_| format!("{path} isn't JSON"))?;
    follows_a_push_here(&event, env("GITHUB_SHA").as_deref())
}

/// [`workflow_run_signs`]'s rules for the event `event`, in a job at commit `sha`.
fn follows_a_push_here(event: &Value, sha: Option<&str>) -> Result<(), String> {
    let run = &event["workflow_run"];
    let word = |v: &Value| v.as_str().unwrap_or("nothing").to_string();
    if run["event"] != "push" {
        return Err(format!(
            "the run it follows was started by {}, not a push",
            word(&run["event"])
        ));
    }
    if run["conclusion"] != "success" {
        return Err(format!(
            "the run it follows ended {}, not in success",
            word(&run["conclusion"])
        ));
    }
    let repository = &event["repository"]["full_name"];
    if repository.is_null() || run["head_repository"]["full_name"] != *repository {
        return Err(format!(
            "the run it follows built {}, not this repository ({})",
            word(&run["head_repository"]["full_name"]),
            word(repository)
        ));
    }
    let default = &event["repository"]["default_branch"];
    if default.is_null() || run["head_branch"] != *default {
        return Err(format!(
            "the run it follows built {}, not the default branch {}",
            word(&run["head_branch"]),
            word(default)
        ));
    }
    if sha.is_none() || run["head_sha"].as_str() != sha {
        return Err(format!(
            "the run it follows built commit {}, not the one this job checked out ({})",
            word(&run["head_sha"]),
            sha.unwrap_or("none")
        ));
    }
    Ok(())
}

/// Whether a run started by `trigger` signs baked stores ([`SIGNING_TRIGGERS`]).
fn trigger_counts(trigger: Option<&str>) -> bool {
    trigger.is_some_and(|t| SIGNING_TRIGGERS.contains(&t))
}

/// Which of `signers` signed the bundle `bytes`, in words, and, for a keyless signature, the
/// time its timestamp authority gave it (milliseconds), which lies within its certificate's
/// validity: a time the signer can't choose. A key's signature has none: what it signed says when
/// it was made, which is the key holder's claim.
fn signed_by(
    bytes: &[u8],
    signers: &[BakedSigner],
    root: Option<&TrustedRoot>,
) -> Result<(String, Option<i64>), String> {
    let keys: Vec<_> = signers
        .iter()
        .filter_map(|s| match s {
            BakedSigner::Key(key) => Some(*key),
            BakedSigner::Identity { .. } => None,
        })
        .collect();
    let mut why = Vec::new();
    if !keys.is_empty() {
        match key_signed(bytes, &keys) {
            Ok(keyid) => return Ok((format!("key {keyid}"), None)),
            Err(e) => why.push(e),
        }
    }
    let identities: Vec<(&str, &str)> = signers
        .iter()
        .filter_map(|s| match s {
            BakedSigner::Identity { issuer, subject } => Some((issuer.as_str(), subject.as_str())),
            BakedSigner::Key(_) => None,
        })
        .collect();
    if !identities.is_empty() {
        let root = root.ok_or("no Sigstore trust root to check a certificate with")?;
        match hushrun_attest::certified_identity_at(bytes, root) {
            Ok((cert, _))
                if !trigger_counts(cert.trigger.as_deref())
                    && identities.iter().any(|(issuer, subject)| {
                        cert.issuer == *issuer && cert.subject == *subject
                    }) =>
            {
                why.push(format!(
                    "its certificate is of a run started by {}, which signs no baked store",
                    cert.trigger
                        .as_deref()
                        .unwrap_or("an event it doesn't name")
                ));
            }
            Ok((cert, at))
                if identities.iter().any(|(issuer, subject)| {
                    cert.issuer == *issuer && cert.subject == *subject
                }) =>
            {
                return Ok((
                    format!("issuer {}, subject {}", cert.issuer, cert.subject),
                    Some(at.millis()),
                ));
            }
            Ok((cert, _)) => why.push(format!(
                "its certificate is {}, which hushrun.yaml doesn't name",
                hushrun_attest::verify::describe(&cert)
            )),
            Err(e) => why.push(e.to_string()),
        }
    }
    Err(why.join("; "))
}

/// Which of `keys` made the one signature of the bundle `bytes`, over its envelope's PAE: its key
/// id. The envelope's own key id isn't read (a cosign-readable bundle names none).
fn key_signed(bytes: &[u8], keys: &[hushrun_core::signers::PublicKey]) -> Result<String, String> {
    let object = Object::parse(bytes).map_err(|e| e.to_string())?;
    let Object::Bundle(bundle) = &object else {
        return Err("a plain envelope, not a bundle".into());
    };
    let envelope = object.envelope().ok_or("it holds no envelope")?;
    if envelope.signatures.len() != 1 {
        return Err(format!(
            "its envelope has {} signatures, not one",
            envelope.signatures.len()
        ));
    }
    if !matches!(bundle.material(), Ok(hushrun_attest::Material::Key(_))) {
        return Err("signed keylessly, not by a key".into());
    }
    let sig = hushrun_attest::dsse::signature_bytes(&envelope.signatures[0])
        .ok_or("the signature is not standard padded base64")?;
    let payload = envelope.payload_bytes().map_err(|e| e.to_string())?;
    let message = hushrun_attest::dsse::pae(&envelope.payload_type, &payload);
    keys.iter()
        .find(|key| hushrun_attest::keys::verify_signature(key, &message, &sig))
        .map(|key| key.keyid())
        .ok_or_else(|| "it isn't signed by a key hushrun.yaml names".to_string())
}

/// The payload of the bundle (or plain envelope) `bytes`, once its payload type is in-toto's.
fn statement_payload(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let object = Object::parse(bytes).map_err(|e| e.to_string())?;
    let envelope = object.envelope().ok_or("it holds no envelope")?;
    if envelope.payload_type != PAYLOAD_TYPE {
        return Err(format!(
            "its payload is {}, not an in-toto statement",
            envelope.payload_type
        ));
    }
    envelope.payload_bytes().map_err(|e| e.to_string())
}

/// The Sigstore trust root a run checks a certificate with: the file
/// `HUSHRUN_SIGSTORE_TRUSTED_ROOT` names, else the one in the binary.
pub(crate) fn run_root(env: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Option<TrustedRoot> {
    match env("HUSHRUN_SIGSTORE_TRUSTED_ROOT").filter(|v| !v.is_empty()) {
        Some(path) => {
            let bytes = fs::read(&path).ok()?;
            TrustedRoot::parse(&bytes, &path.to_string_lossy()).ok()
        }
        None => Some(TrustedRoot::public_good().clone()),
    }
}

impl super::Relay {
    /// B-68: for each of `checks` (`None`: the repository's newest), the newest manifest of
    /// each kind and platform of `wanted`, as `open` answers, downloading nothing: `rustc/1`'s
    /// from the check's own `open`, each other's from an `open` of its own. The error is the
    /// line to say.
    #[allow(clippy::type_complexity)]
    pub(crate) fn newest_parts(
        self: &std::sync::Arc<super::Relay>,
        checks: &[Option<Hex32>],
        wanted: &[(hushrun_build::model::Kind, String)],
    ) -> Result<
        Vec<(
            Option<Hex32>,
            hushrun_build::model::Kind,
            String,
            Vec<hushrun_core::blocks::Pair>,
        )>,
        String,
    > {
        use hushrun_build::model::Kind;
        use hushrun_core::blocks::{self, Policy};
        let relay = self.clone();
        let wanted = wanted.to_vec();
        let checks = checks.to_vec();
        self.runtime.block_on(async move {
            let mut out = Vec::new();
            for check in checks {
                relay.clone().open(check).await;
                if let super::Phase::Stopped(stop) = relay.state().phase.clone() {
                    return Err(match stop {
                        super::Stop::Off(why)
                        | super::Stop::Offline(why)
                        | super::Stop::Refused(why) => why,
                    });
                }
                let rustc = relay
                    .state()
                    .policy
                    .as_ref()
                    .map(|p| p.manifest.clone())
                    .unwrap_or_default();
                let platform = relay.platform().await;
                out.push((check, Kind::Rustc, platform, rustc));
                for (kind, platform) in wanted.iter().filter(|(k, _)| *k != Kind::Rustc) {
                    let body = blocks::OpenRequest {
                        kind: kind.word().to_string(),
                        repository: relay.config.repository.clone(),
                        commit: relay.config.commit.clone(),
                        check,
                        platform: platform.clone(),
                    };
                    if let Ok(answered) = relay
                        .client
                        .post::<_, Policy>(blocks::OPEN_PATH, &body, relay.config.tuning.open, None)
                        .await
                    {
                        out.push((check, *kind, platform.clone(), answered.body.manifest));
                    }
                }
            }
            Ok(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-65, B-66, B-67
    #[test]
    fn an_image_counts_only_for_the_same_repository_source_claim_and_from_not_before() {
        use hushrun_build::baked::{Source, Trust};
        let store = Baked {
            schema: baked::SCHEMA.into(),
            trust: Trust::Trusted,
            manifest: Hex32([1; 32]),
            source: Source::Backend {
                host: "https://api.example.com".into(),
                org: "acme".into(),
            },
            repository: Some("github.com/acme/shop".into()),
            platform: "linux-x64".into(),
            checks: Vec::new(),
            toolchains: Vec::new(),
            packs: Vec::new(),
            baked_ms: 5_000,
            paths: None,
        };
        let statement: Value = serde_json::from_slice(&image_statement(
            "r",
            &Digest::of(b"i"),
            &store,
            &Hex32([2; 32]),
            &[],
        ))
        .unwrap();
        let predicate = &statement["predicate"];
        assert!(predicate_counts(predicate, &store, None, None).is_ok());
        assert!(predicate_counts(predicate, &store, Some(5_000), None).is_ok());
        let late = predicate_counts(predicate, &store, Some(5_001), None).unwrap_err();
        assert!(late.contains("not_before"), "{late}");
        // A laptop's store, signed by the same key, never stands for a backend's trusted one.
        let mut laptop = store.clone();
        laptop.source = Source::Store;
        laptop.trust = Trust::Untrusted {
            because: hushrun_build::baked::Untrusted::LocalStore,
        };
        let theirs: Value = serde_json::from_slice(&image_statement(
            "r",
            &Digest::of(b"i"),
            &laptop,
            &Hex32([2; 32]),
            &[],
        ))
        .unwrap();
        let err = predicate_counts(&theirs["predicate"], &store, None, None).unwrap_err();
        assert!(err.contains("source"), "{err}");
        let mut other = store.clone();
        other.repository = Some("github.com/acme/other".into());
        assert!(predicate_counts(predicate, &other, None, None).is_err());
        let mut claim = store.clone();
        claim.source = laptop.source.clone();
        assert!(
            predicate_counts(&theirs["predicate"], &claim, None, None)
                .unwrap_err()
                .contains("trust")
        );
        // A statement from before these fields counts only while not_before is unset.
        let mut old = predicate.clone();
        old.as_object_mut().unwrap().remove("baked_ms");
        assert!(predicate_counts(&old, &store, None, None).is_ok());
        assert!(predicate_counts(&old, &store, Some(1), None).is_err());
        // A keyless signature: its timestamp, never the claimed baking time, meets not_before.
        // A compromised workflow could sign a far-future baked_ms; it can't move the authority's
        // clock.
        let mut future = predicate.clone();
        future["baked_ms"] = json!(9_000_000_000_000_i64);
        assert!(predicate_counts(&future, &store, Some(5_000), None).is_ok());
        let said = predicate_counts(&future, &store, Some(5_000), Some(4_999)).unwrap_err();
        assert!(said.contains("timestamp"), "{said}");
        assert!(predicate_counts(predicate, &store, Some(5_000), Some(5_000)).is_ok());
        assert!(predicate_counts(&old, &store, Some(1), Some(2)).is_ok());
    }

    /// A store folder of one pack (`seed`), and its `baked.json`.
    fn one_pack_store(dir: &Path, seed: u8) -> (PathBuf, Baked, HashMap<Hex32, Group>) {
        use hushrun_build::baked::{BakedCheck, Part, Source, Trust};
        let folder = dir.join(format!("store-{seed}"));
        fs::create_dir_all(folder.join(baked::PACKS)).unwrap();
        let bytes = vec![seed; 900];
        let address = Hex32([seed; 32]);
        fs::write(baked::pack_path(&folder, &address), &bytes).unwrap();
        let checks = vec![BakedCheck {
            check: None,
            parts: vec![Part {
                kind: "rustc/1".into(),
                platform: "x86_64-unknown-linux-gnu".into(),
                manifest: Hex32([seed + 100; 32]),
            }],
        }];
        let store = Baked {
            schema: baked::SCHEMA.into(),
            trust: Trust::Untrusted {
                because: hushrun_build::baked::Untrusted::LocalStore,
            },
            manifest: baked::store_manifest(&checks),
            source: Source::Store,
            repository: Some("github.com/acme/shop".into()),
            platform: "linux-x64".into(),
            checks,
            toolchains: Vec::new(),
            packs: vec![PackRow {
                kind: "rustc/1".into(),
                address,
                bytes: bytes.len() as u64,
                sha256: Hex32::sha256(&bytes),
                outputs: Hex32([seed + 10; 32]),
                class: hushrun_core::blocks::Class::Library,
                cost_ms: 5,
                scope: None,
                link_environment: None,
            }],
            baked_ms: 1_000_000,
            paths: None,
        };
        fs::write(
            folder.join(BAKED_JSON),
            serde_json::to_vec_pretty(&store).unwrap(),
        )
        .unwrap();
        let groups = HashMap::from([(address, Group::Deps)]);
        (folder, store, groups)
    }

    // covers: B-72, B-73
    // A full image that lands while a dependencies-only bake builds (its first read said the tag
    // was free) keeps the moving tag: the bake asks again as the tags move. And a full bake right
    // after a dependencies-only image publishes, however young that image and few its changes.
    #[test]
    fn a_dependencies_only_image_never_takes_a_moving_tag_from_a_full_one() {
        let dir = tempfile::tempdir().unwrap();
        let layout = dir.path().join("layout");
        let target = Target::Layout(layout.clone());
        let publishing = |deps_only: bool| Publishing {
            latest: true,
            branch: None,
            complete: true,
            signers: &[],
            root: None,
            not_before: None,
            vouch: None,
            compact_every_ms: None,
            rebake: None,
            deps_only,
        };
        let latest = "hushrun-linux-x64-latest";
        let (folder, full, groups) = one_pack_store(dir.path(), 1);
        let published = publish(
            &folder,
            &full,
            &groups,
            &HashMap::new(),
            &target,
            None,
            false,
            &dir.path().join("staging-1"),
            &publishing(false),
        )
        .unwrap();
        assert_eq!(
            target.reading().tagged(latest).unwrap().unwrap().digest(),
            published.digest
        );
        let (folder, deps, groups) = one_pack_store(dir.path(), 2);
        let late = publish(
            &folder,
            &deps,
            &groups,
            &HashMap::new(),
            &target,
            None,
            false,
            &dir.path().join("staging-2"),
            &publishing(true),
        )
        .unwrap();
        assert!(
            late.notes
                .iter()
                .any(|n| n.contains("the moving tags didn't move")),
            "{:?}",
            late.notes
        );
        assert!(!late.tags.iter().any(|t| t == latest), "{:?}", late.tags);
        assert_eq!(
            target.reading().tagged(latest).unwrap().unwrap().digest(),
            published.digest
        );

        // The re-bake limit: a dependencies-only image is no reason for a full bake to wait, and
        // an image dated past the bake's clock is never "recent".
        let rebake = Rebake {
            every_ms: REBAKE_EVERY_MS,
            blocks: REBAKE_BLOCKS,
        };
        let mut previous = target.reading().previous(latest).unwrap().unwrap();
        let young = Some(i64::try_from(full.baked_ms).unwrap() - 1);
        assert!(rebake.holds(&full, false, &previous, young).is_some());
        previous.label.deps_only = true;
        assert!(rebake.holds(&full, false, &previous, young).is_none());
        assert!(rebake.holds(&deps, true, &previous, young).is_some());
        previous.label.deps_only = false;
        let ahead = Some(i64::try_from(full.baked_ms).unwrap() + 60_000);
        assert!(rebake.holds(&full, false, &previous, ahead).is_none());
    }

    // covers: B-70
    #[test]
    fn a_hosted_publish_describes_the_image_it_built_file_by_file() {
        use hushrun_build::baked::{BakedCheck, Part, Source, Trust};
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("hushrun");
        fs::create_dir_all(folder.join(baked::PACKS)).unwrap();
        let mut rows = Vec::new();
        let mut groups = HashMap::new();
        for (seed, kind, group) in [
            (1u8, "rustc/1", Group::Deps),
            (2, "rustc/1", Group::Code),
            (3, "go/1", Group::Code),
        ] {
            let bytes = vec![seed; 700 + usize::from(seed)];
            let address = Hex32([seed; 32]);
            fs::write(baked::pack_path(&folder, &address), &bytes).unwrap();
            groups.insert(address, group);
            rows.push(PackRow {
                kind: kind.into(),
                address,
                bytes: bytes.len() as u64,
                sha256: Hex32::sha256(&bytes),
                outputs: Hex32([seed + 10; 32]),
                class: hushrun_core::blocks::Class::Library,
                cost_ms: 5,
                scope: Some("protected/1".into()),
                link_environment: None,
            });
        }
        let hint_dir = folder.join(baked::HINTS).join(Hex32([1; 32]).to_string());
        fs::create_dir_all(&hint_dir).unwrap();
        let hint = b"sealed hint".to_vec();
        fs::write(hint_dir.join(Hex32::sha256(&hint).to_string()), &hint).unwrap();
        let checks = vec![BakedCheck {
            check: None,
            parts: vec![Part {
                kind: "rustc/1".into(),
                platform: "x86_64-unknown-linux-gnu".into(),
                manifest: Hex32([9; 32]),
            }],
        }];
        let store = Baked {
            schema: baked::SCHEMA.into(),
            trust: Trust::Trusted,
            manifest: baked::store_manifest(&checks),
            source: Source::Backend {
                host: "https://api.example.com".into(),
                org: "acme".into(),
            },
            repository: Some("github.com/acme/shop".into()),
            platform: "linux-x64".into(),
            checks,
            toolchains: Vec::new(),
            packs: rows.clone(),
            baked_ms: 1,
            paths: None,
        };
        let json = serde_json::to_vec_pretty(&store).unwrap();
        fs::write(folder.join(BAKED_JSON), &json).unwrap();
        let hints = HashMap::from([(Hex32([1; 32]), Group::Deps)]);
        let layout = dir.path().join("layout");
        let publishing = Publishing {
            latest: true,
            branch: None,
            complete: true,
            signers: &[],
            root: None,
            not_before: None,
            vouch: None,
            compact_every_ms: None,
            rebake: None,
            deps_only: false,
        };
        let assembled = assemble(
            &Store {
                folder: &folder,
                baked: &store,
                packs: &groups,
                hints: &hints,
            },
            &Reading::Layout(&layout),
            "hushrun-linux-x64-latest",
            None,
            false,
            &dir.path().join("staging"),
            &publishing,
            "registry.example/acme/shop",
            &["hushrun-linux-x64-latest".to_string()],
        )
        .unwrap();
        let previous = format!("sha256:{}", Hex32([4; 32]));
        let request = publish_request(
            &assembled,
            &store,
            Some(previous.clone()),
            Some("b".repeat(40)),
        )
        .unwrap();
        assert_eq!(request.platform, "linux-x64");
        assert_eq!(request.manifest, store.manifest);
        assert_eq!(request.previous, Some(previous));
        assert_eq!(request.image.0, assembled.image.manifest.bytes);
        assert_eq!(request.config.0, assembled.image.config);
        assert!(request.signature.is_none());
        // Each layer, bottom first, as the image holds it: the same files, path for path.
        let roles: Vec<&str> = request.layers.iter().map(|l| l.role.as_str()).collect();
        let built: Vec<&str> = assembled
            .image
            .layers
            .iter()
            .map(|l| l.role.word())
            .collect();
        assert_eq!(roles, built);
        assert_eq!(roles, ["deps", "workspace", "recent"]);
        for (sent, layer) in request.layers.iter().zip(&assembled.image.layers) {
            let paths: Vec<&str> = sent.files.iter().map(|f| f.path.as_str()).collect();
            let held: Vec<&str> = layer.files.iter().map(|f| f.path.as_str()).collect();
            assert_eq!(paths, held);
        }
        // A pack names its block, with the row's kind and SHA-256; nothing else is a pack.
        let files: Vec<&hushrun_core::blocks::PublishFile> =
            request.layers.iter().flat_map(|l| &l.files).collect();
        for row in &rows {
            let file = files
                .iter()
                .find(|f| f.path == format!("hushrun/packs/{}.pack", row.address))
                .unwrap();
            let pack = file.pack.as_ref().unwrap();
            assert_eq!(
                (pack.kind.as_str(), pack.address),
                (row.kind.as_str(), row.address)
            );
            assert_eq!((file.size, file.sha256), (row.bytes, row.sha256));
        }
        // Every other file's bytes go along once, and no pack's.
        let others: Vec<&&hushrun_core::blocks::PublishFile> =
            files.iter().filter(|f| f.pack.is_none()).collect();
        assert_eq!(others.len(), 2, "baked.json and the hint");
        assert_eq!(request.blobs.len(), 2);
        assert!(request.blobs.iter().any(|b| b.0 == json));
        assert!(request.blobs.iter().any(|b| b.0 == hint));
        for other in others {
            assert!(
                request
                    .blobs
                    .iter()
                    .any(|b| Hex32::sha256(&b.0) == other.sha256)
            );
        }
        // It reads back with the wire type the backend uses.
        let bytes = serde_json::to_vec(&request).unwrap();
        let back: hushrun_core::blocks::PublishRequest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, request);
    }

    // covers: B-67
    #[test]
    fn only_a_push_a_schedule_a_run_by_hand_or_the_bake_job_s_trigger_signs() {
        for trigger in [
            "push",
            "schedule",
            "workflow_dispatch",
            "workflow_run",
            "web",
        ] {
            assert!(trigger_counts(Some(trigger)), "{trigger}");
        }
        for trigger in [
            "pull_request_target",
            "pull_request",
            "issue_comment",
            "workflow_call",
            "merge_request_event",
            "trigger",
            "",
        ] {
            assert!(!trigger_counts(Some(trigger)), "{trigger}");
        }
        assert!(!trigger_counts(None));
    }

    // covers: B-67
    #[test]
    fn a_workflow_run_bake_signs_only_after_a_push_to_this_repository_s_default_branch() {
        let sha = "a".repeat(40);
        let event = |change: &dyn Fn(&mut Value)| {
            let mut event = json!({
                "repository": {"full_name": "acme/shop", "default_branch": "main"},
                "workflow_run": {
                    "event": "push",
                    "conclusion": "success",
                    "head_branch": "main",
                    "head_sha": sha,
                    "head_repository": {"full_name": "acme/shop"},
                },
            });
            change(&mut event);
            event
        };
        assert!(follows_a_push_here(&event(&|_| {}), Some(&sha)).is_ok());
        type Change<'a> = &'a dyn Fn(&mut Value);
        let refused: [(&str, Change<'_>); 6] = [
            ("pull_request", &|e| {
                e["workflow_run"]["event"] = json!("pull_request")
            }),
            ("failure", &|e| {
                e["workflow_run"]["conclusion"] = json!("failure")
            }),
            ("fork/shop", &|e| {
                e["workflow_run"]["head_repository"]["full_name"] = json!("fork/shop");
            }),
            ("feature", &|e| {
                e["workflow_run"]["head_branch"] = json!("feature")
            }),
            ("bbbb", &|e| {
                e["workflow_run"]["head_sha"] = json!("b".repeat(40))
            }),
            ("nothing", &|e| e["repository"] = json!(null)),
        ];
        for (said, change) in refused {
            let err = follows_a_push_here(&event(change), Some(&sha)).unwrap_err();
            assert!(err.contains(said), "{said}: {err}");
        }
        // No commit to compare with: refused.
        assert!(follows_a_push_here(&event(&|_| {}), None).is_err());
        // Another event signs as before; a workflow_run with no event file doesn't.
        let env = |pairs: Vec<(&'static str, String)>| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| v.clone())
            }
        };
        assert!(workflow_run_signs(&env(vec![("GITHUB_EVENT_NAME", "push".into())])).is_ok());
        assert!(
            workflow_run_signs(&env(vec![("GITHUB_EVENT_NAME", "workflow_run".into())])).is_err()
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("event.json");
        fs::write(&path, serde_json::to_vec(&event(&|_| {})).unwrap()).unwrap();
        let ok = env(vec![
            ("GITHUB_EVENT_NAME", "workflow_run".into()),
            ("GITHUB_EVENT_PATH", path.display().to_string()),
            ("GITHUB_SHA", sha.clone()),
        ]);
        assert!(workflow_run_signs(&ok).is_ok());
    }

    // covers: B-66
    #[test]
    fn the_moving_tag_moves_only_from_ci_on_the_default_branch() {
        let context = |in_ci: bool, branch: Option<&str>, default: Option<&str>, pr: bool| {
            let mut c = hushrun_ci::context(&hushrun_ci::Env::from_pairs([]));
            c.in_ci = in_ci;
            c.branch = branch.map(str::to_string);
            c.source = Some(hushrun_ci::SourceRepository {
                url: "https://github.com/acme/app".into(),
                default_branch: default.map(str::to_string),
                id: None,
            });
            c.pull_request = pr.then_some(hushrun_ci::PullRequest { base: None });
            c
        };
        assert!(moves_latest(&context(true, Some("main"), Some("main"), false)).is_ok());
        assert!(moves_latest(&context(false, Some("main"), Some("main"), false)).is_err());
        assert!(moves_latest(&context(true, Some("feature"), Some("main"), false)).is_err());
        assert!(moves_latest(&context(true, Some("main"), None, false)).is_err());
        assert!(moves_latest(&context(true, None, Some("main"), false)).is_err());
        assert!(moves_latest(&context(true, Some("main"), Some("main"), true)).is_err());
    }

    // covers: B-73
    // B-73's way out is a daily `schedule` run of the bake job. GitHub's event file for a
    // scheduled run holds the cron line, not the repository (there is no webhook behind it), and
    // GitHub runs it only on the default branch: its ref is the default branch, so `-latest`
    // moves and the bake may build.
    #[test]
    fn a_scheduled_bake_on_the_default_branch_moves_latest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("event.json");
        fs::write(&path, br#"{"schedule": "17 4 * * *"}"#).unwrap();
        let path = path.display().to_string();
        let env = hushrun_ci::Env::from_pairs([
            ("CI", "true"),
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_REPOSITORY", "acme/app"),
            ("GITHUB_EVENT_NAME", "schedule"),
            ("GITHUB_EVENT_PATH", path.as_str()),
            ("GITHUB_REF", "refs/heads/main"),
            ("GITHUB_REF_NAME", "main"),
        ]);
        let context = hushrun_ci::context(&env);
        assert_eq!(context.trigger, Some(hushrun_core::run::Trigger::Schedule));
        assert!(
            moves_latest(&context).is_ok(),
            "{:?}",
            moves_latest(&context)
        );
    }

    // covers: B-65
    #[test]
    fn the_lockfile_tells_third_party_crates_from_the_repository_s_own() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"x\"\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"serde\",\n]\n\n[[package]]\nname = \"both\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"both\"\nversion = \"2.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
        )
        .unwrap();
        let tp = ThirdParty::read(dir.path()).unwrap();
        assert_eq!(tp.group(Some("serde")), Group::Deps);
        assert_eq!(tp.group(Some("app")), Group::Code);
        // A name that is a path package too, or none: the repository's own.
        assert_eq!(tp.group(Some("both")), Group::Code);
        assert_eq!(tp.group(None), Group::Code);
        assert!(ThirdParty::read(&dir.path().join("none")).is_none());
    }
}

//! B-65: a baked store as a layered OCI image, built without `docker build`, and published to a
//! registry or written as an OCI image layout.
//!
//! [`plan`] is the one place that decides which file goes in which layer, from the files of this
//! bake and what the previous image (the moving tag's) holds. [`build`] turns a plan into the
//! image's bytes: each layer's tar ([`hushrun_core::blocks::image::chunks`]), the config with
//! the label that says what each layer holds ([`LABEL`]), and the manifest. [`Artifacts::publish`](crate::Artifacts::publish)
//! uploads what the registry lacks and moves the tags; [`write_layout`] writes the same image into
//! a folder.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use bytes::Bytes;
use futures_util::StreamExt as _;
use hushrun_core::blocks::image::{self as layout, Chunk};
use hushrun_core::digest::Digest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::registry::{ArtifactError, BlobStream, Manifest, OCI_INDEX, OCI_MANIFEST, Repository};

/// The image config's media type.
pub const CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.image.config.v1+json";
/// The config label that says what each layer holds, as JSON ([`Label`]).
pub const LABEL: &str = "dev.hushrun.baked-image";
/// The schema of [`LABEL`]'s value.
pub const LABEL_SCHEMA: &str = "hushrun.baked-image/v1";
/// Each layer descriptor's annotation naming its role.
pub const LAYER_ANNOTATION: &str = "dev.hushrun.layer";
/// The manifest's annotation naming the store's manifest identity.
pub const MANIFEST_ANNOTATION: &str = "dev.hushrun.baked-manifest";
/// The annotation an OCI image layout's index gives each tag.
pub const REF_NAME: &str = "org.opencontainers.image.ref.name";
/// The most bytes of a previous image's config hushrun reads.
pub const MOST_CONFIG: u64 = 64 << 20;

/// How much a workspace layer may hold of blocks no longer used before a bake compacts it: half
/// its bytes.
pub const STALE_SHARE: u64 = 2;
/// Below this many bytes the recent layer is never folded into the workspace layer for its size:
/// re-uploading it costs less than writing the workspace layer again.
pub const RECENT_FLOOR: u64 = 1 << 20;
/// The recent layer is folded into the workspace layer once it holds more than this share of the
/// workspace layer's bytes: one in four.
pub const RECENT_SHARE: u64 = 4;
/// Or once it holds more than this many bytes, whatever the workspace layer holds: what every bake
/// would upload again.
pub const RECENT_MOST: u64 = 64 << 20;
/// How long a workspace layer is kept before a bake writes it again whatever its size, by default:
/// a week since the bake that wrote it (`build: baked: compact_every`).
pub const COMPACT_EVERY_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// A workspace layer's recorded time this far past the bake's own clock is no time: the clocks of
/// two CI runners (synced over the network) disagree by less, and a time further ahead would keep
/// a layer past its age for as long.
const CLOCK_SLACK_MS: i64 = 10 * 60 * 1000;

/// A layer's role (B-65), in the image's order, bottom first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// Third-party code: changes with the lockfile or the toolchain.
    Deps,
    /// The repository's own code as of the last compaction, with what it held then.
    Workspace,
    /// The repository's own code since, and `baked.json`: changes with every bake.
    Recent,
    /// \[ADR 0163\] A package store's one layer (K-14): the bytes `hushrun packages bake --tar`
    /// writes.
    Packages,
}

impl Role {
    /// The role's word, as the annotation and the label write it.
    pub fn word(self) -> &'static str {
        match self {
            Role::Deps => "deps",
            Role::Workspace => "workspace",
            Role::Recent => "recent",
            Role::Packages => "packages",
        }
    }
}

/// Which group a file of a bake belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// A block of third-party code, and its hints.
    Deps,
    /// A block of the repository's own code, or of code the bake can't place, and its hints.
    Code,
    /// `baked.json` and its signature: always in the recent layer.
    Store,
    /// A program the repository links (a binary, a test executable): it links everything below
    /// it, so most merges change it. Always in the recent layer, never in the workspace layer, and
    /// not counted against the recent layer's bound: it would compact the workspace layer at
    /// every merge, which every pull would then take again.
    Program,
}

/// A file of this bake: its path in the image, size, SHA-256, group, and where its bytes are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub sha256: Digest,
    pub group: Group,
    pub source: PathBuf,
}

/// A file a layer holds, as [`LABEL`] lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listed {
    pub path: String,
    pub size: u64,
    pub sha256: Digest,
}

/// One layer as [`LABEL`] lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelLayer {
    pub role: Role,
    pub files: Vec<Listed>,
}

/// [`LABEL`]'s value: what each layer holds, in the manifest's order, and the `baked.json` rows of
/// the workspace layer's packs, which a later bake carries while they stay there unused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Label {
    pub schema: String,
    /// The store's manifest identity.
    pub manifest: String,
    /// The platform, in a tag's words.
    pub platform: String,
    pub layers: Vec<LabelLayer>,
    /// The workspace layer's packs' rows, as `baked.json` has them.
    pub rows: Vec<Value>,
    /// When the workspace layer was last written (compacted), in milliseconds since the epoch: the
    /// baking time of the bake that wrote it, carried by each bake that keeps it. What the weekly
    /// compaction compares with the bake's own clock; absent in an image with no workspace layer
    /// time recorded, whose next bake compacts when an age is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted_ms: Option<i64>,
    /// B-72: the store holds only third-party code, from a dependencies-only build: what a later
    /// dependencies-only bake may publish over, and a full one replaces.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deps_only: bool,
}

impl Label {
    /// The label an image's config bytes carry, when a bake published it: `None` for no label or
    /// another schema.
    pub fn of_config(config: &[u8]) -> Option<Label> {
        let config_json: Value = serde_json::from_slice(config).ok()?;
        let text = config_json["config"]["Labels"][LABEL].as_str()?;
        let label: Label = serde_json::from_str(text).ok()?;
        (label.schema == LABEL_SCHEMA).then_some(label)
    }
}

/// The image a moving tag points to, as a bake reads it.
#[derive(Debug, Clone)]
pub struct Previous {
    pub manifest_digest: Digest,
    pub label: Label,
    /// Each layer's descriptor, by role.
    pub layers: BTreeMap<Role, (Digest, u64)>,
}

impl Previous {
    /// Reads a previous image from its manifest and config bytes. `None` when it isn't one a bake
    /// published (no label, another schema, layers that don't match it).
    pub fn read(manifest: &[u8], config: &[u8]) -> Option<Previous> {
        let manifest_json: Value = serde_json::from_slice(manifest).ok()?;
        let label = Label::of_config(config)?;
        let descriptors = manifest_json["layers"].as_array()?;
        if descriptors.len() != label.layers.len() {
            return None;
        }
        let mut layers = BTreeMap::new();
        for (descriptor, layer) in descriptors.iter().zip(&label.layers) {
            if descriptor["annotations"][LAYER_ANNOTATION].as_str() != Some(layer.role.word()) {
                return None;
            }
            let digest = Digest::parse(descriptor["digest"].as_str()?).ok()?;
            let size = descriptor["size"].as_u64()?;
            layers.insert(layer.role, (digest, size));
        }
        Some(Previous {
            manifest_digest: Digest::of(manifest),
            label,
            layers,
        })
    }

    fn files(&self, role: Role) -> &[Listed] {
        self.label
            .layers
            .iter()
            .find(|l| l.role == role)
            .map_or(&[][..], |l| &l.files)
    }
}

/// A file of a planned layer: from this bake (`source`), or kept from the previous image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub path: String,
    pub size: u64,
    pub sha256: Digest,
    pub source: Option<PathBuf>,
}

/// A planned layer: written from its files, or the previous image's, reused by digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedLayer {
    pub role: Role,
    pub files: Vec<Planned>,
    /// The previous image's layer of the same files, reused as it is.
    pub reused: Option<(Digest, u64)>,
}

/// What [`plan`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub layers: Vec<PlannedLayer>,
    /// Packs the workspace layer keeps that this bake doesn't use, by path.
    pub stale: Vec<String>,
    /// Why the workspace layer was written again, when it was.
    pub compacted: Option<String>,
    /// When the workspace layer was written, as the label records it: this bake's time when it
    /// compacted, else the previous image's.
    pub compacted_ms: Option<i64>,
    /// B-72: the label says the store is dependencies only. The caller's to set: [`plan_at`]
    /// leaves it false.
    pub deps_only: bool,
}

/// What may make a bake write the workspace layer again besides what the files say: the person
/// asking (`--compact`), and the layer's age, against the bake's own clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Compaction {
    /// `--compact`.
    pub asked: bool,
    /// The bake's time, in milliseconds since the epoch: `baked.json`'s `baked_ms`.
    pub now_ms: i64,
    /// The workspace layer is written again once it is this old ([`COMPACT_EVERY_MS`] by
    /// default); `None`, never for its age.
    pub every_ms: Option<u64>,
}

impl Compaction {
    /// Only when asked: no age rule.
    pub fn asked(asked: bool) -> Compaction {
        Compaction {
            asked,
            ..Compaction::default()
        }
    }

    /// Why a workspace layer written at `written` (the previous label's `compacted_ms`) is too old
    /// to keep, when it is: `every_ms` or more before `now_ms`, unrecorded, or recorded past the
    /// bake's clock by more than ten minutes (another machine's clock, or a forged time). Evaluated as
    /// the bake runs, never stored as a verdict.
    pub fn too_old(&self, written: Option<i64>) -> Option<String> {
        let every = self.every_ms?;
        let Some(written) = written else {
            return Some("its writing time isn't recorded".into());
        };
        if written > self.now_ms.saturating_add(CLOCK_SLACK_MS) {
            return Some(format!(
                "its recorded writing time ({written} ms) is past this bake's clock ({} ms)",
                self.now_ms
            ));
        }
        let age = self.now_ms.saturating_sub(written);
        (age >= 0 && age as u64 >= every).then(|| {
            format!(
                "it was written {} ago, and it is written again every {}",
                words_of(age as u64),
                words_of(every)
            )
        })
    }
}

/// A span of milliseconds in a person's words: days, hours or minutes.
fn words_of(ms: u64) -> String {
    let minutes = ms / 60_000;
    match minutes {
        m if m >= 2 * 24 * 60 => format!("{} days", m / (24 * 60)),
        m if m >= 120 => format!("{} hours", m / 60),
        1 => "1 minute".into(),
        m => format!("{m} minutes"),
    }
}

/// B-65: the one decision of which file goes in which layer.
///
/// - **deps**: this bake's third-party blocks (and their hints), except those the workspace layer
///   kept holds: a pure function of them, so it changes only when they do.
/// - **workspace**: the previous image's workspace layer, reused by its digest, with any of its
///   blocks this bake no longer uses (stale), until it is **compacted**: then this bake's own
///   code. It is compacted when there is no previous image, when `compact` asks, when it is
///   older than `compact`'s age (its label's `compacted_ms` against the bake's clock), when the previous
///   layer is missing at the destination (`present` says no), when a path it holds has other
///   bytes now, when stale blocks are half its bytes or more, or when the recent layer, which every
///   bake uploads again, would hold more than [`RECENT_MOST`], or more than [`RECENT_FLOOR`] and
///   more than a [`RECENT_SHARE`]th of what the workspace layer holds.
/// - **recent**: this bake's own code the workspace layer doesn't hold, its programs (which
///   don't count against the recent layer's bound), and `baked.json` with its signature; always
///   present.
///
/// No path is in two layers, and an empty deps or workspace layer is left out.
pub fn plan_at(
    entries: &[Entry],
    previous: Option<&Previous>,
    present: &dyn Fn(&Digest) -> bool,
    compact: &Compaction,
) -> Plan {
    let current: HashMap<&str, &Entry> = entries.iter().map(|e| (e.path.as_str(), e)).collect();
    let mut why = None;
    let kept: Option<(&[Listed], (Digest, u64))> = match previous {
        None => {
            why = Some("there is no previous image".to_string());
            None
        }
        Some(_) if compact.asked => {
            why = Some("asked to".to_string());
            None
        }
        Some(previous)
            if previous.layers.contains_key(&Role::Workspace)
                && compact.too_old(previous.label.compacted_ms).is_some() =>
        {
            why = compact.too_old(previous.label.compacted_ms);
            None
        }
        Some(previous) => match previous.layers.get(&Role::Workspace) {
            None => Some((&[][..], (Digest::of(b""), 0))),
            Some(&(digest, size)) => Some((previous.files(Role::Workspace), (digest, size))),
        },
    };
    let mut workspace: Vec<Planned> = Vec::new();
    let mut reused = None;
    let mut stale = Vec::new();
    if let Some((files, descriptor)) = kept {
        let held: u64 = files.iter().map(|f| f.size).sum();
        let stale_bytes: u64 = files
            .iter()
            .filter(|f| !current.contains_key(f.path.as_str()))
            .map(|f| f.size)
            .sum();
        let changed = files.iter().find(|f| {
            current
                .get(f.path.as_str())
                .is_some_and(|e| e.sha256 != f.sha256 || e.size != f.size)
        });
        let in_place: BTreeSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
        let recent_bytes: u64 = entries
            .iter()
            .filter(|e| e.group == Group::Code && !in_place.contains(e.path.as_str()))
            .map(|e| e.size)
            .sum();
        if !files.is_empty() && !present(&descriptor.0) {
            why = Some("the previous workspace layer isn't at the destination".into());
        } else if let Some(file) = changed {
            why = Some(format!("{} has other bytes now", file.path));
        } else if held > 0 && stale_bytes * STALE_SHARE >= held {
            why = Some(format!(
                "{stale_bytes} of its {held} bytes are blocks no longer used"
            ));
        } else if recent_bytes > RECENT_MOST
            || (recent_bytes > RECENT_FLOOR && recent_bytes * RECENT_SHARE > held)
        {
            why = Some(format!(
                "the recent layer would hold {recent_bytes} bytes, against its {held}"
            ));
        } else {
            for f in files {
                let source = current.get(f.path.as_str()).map(|e| e.source.clone());
                if source.is_none() {
                    stale.push(f.path.clone());
                }
                workspace.push(Planned {
                    path: f.path.clone(),
                    size: f.size,
                    sha256: f.sha256,
                    source,
                });
            }
            if !files.is_empty() {
                reused = Some(descriptor);
            }
        }
    }
    let compacted = why.is_some();
    if compacted {
        workspace = entries
            .iter()
            .filter(|e| e.group == Group::Code)
            .map(planned)
            .collect();
        stale.clear();
    }
    let in_workspace: BTreeSet<String> = workspace.iter().map(|f| f.path.clone()).collect();
    let deps: Vec<Planned> = entries
        .iter()
        .filter(|e| e.group == Group::Deps && !in_workspace.contains(&e.path))
        .map(planned)
        .collect();
    // A program goes in the recent layer, but one a kept workspace layer holds already with these
    // very bytes (an image baked before programs had a group of their own): no path is in two
    // layers. One whose bytes changed there compacted the workspace layer above.
    let recent: Vec<Planned> = entries
        .iter()
        .filter(|e| {
            e.group == Group::Store
                || (matches!(e.group, Group::Program | Group::Code)
                    && !in_workspace.contains(&e.path))
        })
        .map(planned)
        .collect();
    let mut layers = Vec::new();
    if !deps.is_empty() {
        layers.push(PlannedLayer {
            role: Role::Deps,
            files: deps,
            reused: None,
        });
    }
    if !workspace.is_empty() {
        layers.push(PlannedLayer {
            role: Role::Workspace,
            files: workspace,
            reused,
        });
    }
    layers.push(PlannedLayer {
        role: Role::Recent,
        files: recent,
        reused: None,
    });
    for layer in &mut layers {
        layer
            .files
            .sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    }
    let compacted_ms = if compacted {
        Some(compact.now_ms)
    } else {
        previous.and_then(|p| p.label.compacted_ms)
    };
    Plan {
        layers,
        stale,
        compacted: why,
        compacted_ms,
        deps_only: false,
    }
}

/// [`plan_at`] with no age rule: compacted only by what the files say, or when `compact` asks.
pub fn plan(
    entries: &[Entry],
    previous: Option<&Previous>,
    present: &dyn Fn(&Digest) -> bool,
    compact: bool,
) -> Plan {
    plan_at(entries, previous, present, &Compaction::asked(compact))
}

fn planned(entry: &Entry) -> Planned {
    Planned {
        path: entry.path.clone(),
        size: entry.size,
        sha256: entry.sha256,
        source: Some(entry.source.clone()),
    }
}

/// A layer of a built image.
#[derive(Debug, Clone)]
pub struct Layer {
    pub role: Role,
    pub digest: Digest,
    pub size: u64,
    pub files: Vec<Planned>,
    /// Kept from the previous image as it was.
    pub reused: bool,
}

/// An image ready to publish: its layers, config and manifest.
#[derive(Debug, Clone)]
pub struct Image {
    pub layers: Vec<Layer>,
    pub config: Vec<u8>,
    pub manifest: Manifest,
}

impl Image {
    /// The manifest's digest: the image's identity.
    pub fn digest(&self) -> Digest {
        self.manifest.digest()
    }
}

/// The OS and CPU of an image config for a platform in a tag's words (`linux-x64`).
pub fn os_arch(platform: &str) -> (String, String) {
    let (os, arch) = platform.split_once('-').unwrap_or((platform, ""));
    let os = match os {
        "macos" => "darwin",
        other => other,
    };
    let arch = match arch {
        "x64" => "amd64",
        other => other,
    };
    (os.to_string(), arch.to_string())
}

/// What a layer's bytes are, chunk by chunk, read from its files' sources.
fn chunks_of(files: &[Planned]) -> Result<(Vec<Chunk>, Vec<layout::File>), ArtifactError> {
    let listed: Vec<layout::File> = files
        .iter()
        .map(|f| layout::File {
            path: f.path.clone(),
            size: f.size,
        })
        .collect();
    let chunks = layout::chunks(&listed).map_err(|e| ArtifactError::Invalid(e.0))?;
    Ok((chunks, listed))
}

/// Writes the layer of `files` into `out`, reading each file from its source and checking it is
/// as the plan says; returns its digest and size.
pub fn write_layer(files: &[Planned], out: &mut dyn Write) -> Result<(Digest, u64), ArtifactError> {
    let (chunks, _) = chunks_of(files)?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut put = |bytes: &[u8], out: &mut dyn Write| -> Result<(), ArtifactError> {
        hasher.update(bytes);
        size += bytes.len() as u64;
        out.write_all(bytes)
            .map_err(|e| ArtifactError::Network(format!("writing a layer: {e}")))
    };
    let zeros = [0u8; 512];
    for chunk in chunks {
        match chunk {
            Chunk::Header(h) => put(&h[..], out)?,
            Chunk::Zeros(mut n) => {
                while n > 0 {
                    let k = n.min(512) as usize;
                    put(&zeros[..k], out)?;
                    n -= k as u64;
                }
            }
            Chunk::File(i) => {
                let file = &files[i];
                let source = file.source.as_ref().ok_or_else(|| {
                    ArtifactError::Invalid(format!(
                        "{} is kept from the previous image, and its bytes aren't here",
                        file.path
                    ))
                })?;
                let mut reader = fs::File::open(source)
                    .map_err(|e| ArtifactError::Invalid(format!("{}: {e}", source.display())))?;
                let mut check = Sha256::new();
                let mut buf = vec![0u8; 1 << 20];
                let mut seen = 0u64;
                loop {
                    let n = reader.read(&mut buf).map_err(|e| {
                        ArtifactError::Invalid(format!("{}: {e}", source.display()))
                    })?;
                    if n == 0 {
                        break;
                    }
                    check.update(&buf[..n]);
                    seen += n as u64;
                    put(&buf[..n], out)?;
                }
                let actual = Digest::from_bytes(check.finalize().into());
                if seen != file.size || actual != file.sha256 {
                    return Err(ArtifactError::DigestMismatch {
                        what: file.path.clone(),
                        expected: file.sha256.to_string(),
                        actual: actual.to_string(),
                    });
                }
            }
        }
    }
    Ok((Digest::from_bytes(hasher.finalize().into()), size))
}

/// The layer of `files` as a stream, for an upload: the same bytes [`write_layer`] writes.
fn layer_stream(files: Vec<Planned>) -> Result<BlobStream, ArtifactError> {
    let (chunks, _) = chunks_of(&files)?;
    enum Piece {
        Bytes(Vec<u8>),
        File(PathBuf),
    }
    let pieces: Vec<Piece> = chunks
        .into_iter()
        .map(|chunk| match chunk {
            Chunk::Header(h) => Piece::Bytes(h.to_vec()),
            Chunk::Zeros(n) => Piece::Bytes(vec![0; n as usize]),
            Chunk::File(i) => Piece::File(files[i].source.clone().unwrap_or_default()),
        })
        .collect();
    let stream = futures_util::stream::iter(pieces).flat_map(|piece| match piece {
        Piece::Bytes(bytes) => {
            futures_util::stream::once(async move { Ok(Bytes::from(bytes)) }).boxed()
        }
        Piece::File(path) => file_stream(path),
    });
    Ok(stream.boxed())
}

/// The bytes of the file at `path` as a stream, a MiB at a time.
fn file_stream(path: PathBuf) -> BlobStream {
    let file = fs::File::open(&path);
    futures_util::stream::unfold(Some(file), |state| async move {
        let mut file = match state? {
            Ok(file) => file,
            Err(e) => return Some((Err(e), None)),
        };
        let mut buf = vec![0u8; 1 << 20];
        match file.read(&mut buf) {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(Bytes::from(buf)), Some(Ok(file))))
            }
            Err(e) => Some((Err(e), None)),
        }
    })
    .boxed()
}

/// B-65: builds the image of `plan` for `platform`: each layer's digest (a reused one's from the
/// previous image, the others by reading their files once), the config with [`LABEL`], and the
/// manifest. `rows` are the `baked.json` rows of the workspace layer's packs, by path.
pub fn build(
    plan: &Plan,
    platform: &str,
    store_manifest: &str,
    rows: &BTreeMap<String, Value>,
) -> Result<Image, ArtifactError> {
    let mut layers = Vec::new();
    for planned in &plan.layers {
        let (digest, size, reused) = match planned.reused {
            Some((digest, size)) => (digest, size, true),
            None => {
                let (digest, size) = write_layer(&planned.files, &mut io::sink())?;
                (digest, size, false)
            }
        };
        layers.push(Layer {
            role: planned.role,
            digest,
            size,
            files: planned.files.clone(),
            reused,
        });
    }
    let label = Label {
        schema: LABEL_SCHEMA.into(),
        manifest: store_manifest.into(),
        platform: platform.into(),
        layers: layers
            .iter()
            .map(|l| LabelLayer {
                role: l.role,
                files: l
                    .files
                    .iter()
                    .map(|f| Listed {
                        path: f.path.clone(),
                        size: f.size,
                        sha256: f.sha256,
                    })
                    .collect(),
            })
            .collect(),
        rows: layers
            .iter()
            .filter(|l| l.role == Role::Workspace)
            .flat_map(|l| &l.files)
            .filter_map(|f| rows.get(&f.path).cloned())
            .collect(),
        compacted_ms: plan.compacted_ms,
        deps_only: plan.deps_only,
    };
    let (os, arch) = os_arch(platform);
    let config = serde_json::to_vec(&json!({
        "architecture": arch,
        "os": os,
        "config": {
            "Labels": {
                LABEL: serde_json::to_string(&label).expect("labels serialize"),
            },
        },
        "rootfs": {
            "type": "layers",
            "diff_ids": layers.iter().map(|l| l.digest.to_string()).collect::<Vec<_>>(),
        },
    }))
    .expect("configs serialize");
    let manifest = serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST,
        "config": {
            "mediaType": CONFIG_MEDIA_TYPE,
            "digest": Digest::of(&config).to_string(),
            "size": config.len(),
        },
        "layers": layers.iter().map(|l| json!({
            "mediaType": layout::LAYER_MEDIA_TYPE,
            "digest": l.digest.to_string(),
            "size": l.size,
            "annotations": { LAYER_ANNOTATION: l.role.word() },
        })).collect::<Vec<_>>(),
        "annotations": { MANIFEST_ANNOTATION: store_manifest },
    }))
    .expect("manifests serialize");
    Ok(Image {
        layers,
        config,
        manifest: Manifest {
            media_type: OCI_MANIFEST.into(),
            bytes: manifest,
        },
    })
}

/// \[ADR 0163\] K-14: the image of one layer already written whole at `layer` (a package
/// store's archive): the file is hashed once; the config is for `platform`, with `labels`; the
/// manifest carries `annotations`, and names the layer with `role`'s word. The layer counts as
/// reused: [`Artifacts::publish_file_image`](crate::Artifacts::publish_file_image) and
/// [`write_file_layout`] put the file where it goes first.
pub fn file_image(
    layer: &Path,
    role: Role,
    platform: &str,
    labels: &BTreeMap<String, String>,
    annotations: &BTreeMap<String, String>,
) -> Result<Image, ArtifactError> {
    let at = |e: io::Error| ArtifactError::Invalid(format!("{}: {e}", layer.display()));
    let mut from = fs::File::open(layer).map_err(at)?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = from.read(&mut buf).map_err(at)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    let digest = Digest::from_bytes(hasher.finalize().into());
    let (os, arch) = os_arch(platform);
    let config = serde_json::to_vec(&json!({
        "architecture": arch,
        "os": os,
        "config": { "Labels": labels },
        "rootfs": { "type": "layers", "diff_ids": [digest.to_string()] },
    }))
    .expect("configs serialize");
    let manifest = serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST,
        "config": {
            "mediaType": CONFIG_MEDIA_TYPE,
            "digest": Digest::of(&config).to_string(),
            "size": config.len(),
        },
        "layers": [{
            "mediaType": layout::LAYER_MEDIA_TYPE,
            "digest": digest.to_string(),
            "size": size,
            "annotations": { LAYER_ANNOTATION: role.word() },
        }],
        "annotations": annotations,
    }))
    .expect("manifests serialize");
    Ok(Image {
        layers: vec![Layer {
            role,
            digest,
            size,
            files: Vec::new(),
            reused: true,
        }],
        config,
        manifest: Manifest {
            media_type: OCI_MANIFEST.into(),
            bytes: manifest,
        },
    })
}

/// \[K-14\] Writes `image`, made by [`file_image`] of the file at `layer`, into the OCI image
/// layout at `dir` with `tags`.
pub fn write_file_layout(
    dir: &Path,
    image: &Image,
    layer: &Path,
    tags: &[String],
) -> Result<Pushed, ArtifactError> {
    let digest = image
        .layers
        .first()
        .map(|l| l.digest)
        .ok_or_else(|| ArtifactError::Invalid("an image of no layer".into()))?;
    let copied = layout_put_file(dir, layer, &digest)?;
    let mut pushed = write_layout(dir, image, tags, &[])?;
    if let Some(blob) = pushed.blobs.first_mut() {
        blob.uploaded = copied;
    }
    Ok(pushed)
}

/// What a publish uploaded, blob by blob.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Pushed {
    /// Each blob: its role (`deps`, `workspace`, `recent`, `config`, `signature`…), digest, size,
    /// and whether it was uploaded (else the destination had it).
    pub blobs: Vec<PushedBlob>,
    /// The tags moved, in order.
    pub tags: Vec<String>,
}

/// One blob of a publish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PushedBlob {
    pub role: String,
    pub digest: String,
    pub size: u64,
    pub uploaded: bool,
}

impl Pushed {
    /// The bytes uploaded.
    pub fn uploaded_bytes(&self) -> u64 {
        self.blobs
            .iter()
            .filter(|b| b.uploaded)
            .map(|b| b.size)
            .sum()
    }
}

/// A referrer to attach to the image (B-67): an artifact manifest with one layer, the image as
/// its subject.
#[derive(Debug, Clone)]
pub struct Referrer {
    pub artifact_type: String,
    pub layer_media_type: String,
    pub layer: Vec<u8>,
    pub annotations: BTreeMap<String, String>,
}

/// The OCI empty descriptor's bytes and media type, a referrer's config.
pub const EMPTY_MEDIA_TYPE: &str = "application/vnd.oci.empty.v1+json";
const EMPTY: &[u8] = b"{}";

impl Referrer {
    /// The referrer's manifest, about `subject` (the image's manifest).
    pub fn manifest(&self, subject: &Manifest) -> Manifest {
        let mut annotations = serde_json::Map::new();
        for (k, v) in &self.annotations {
            annotations.insert(k.clone(), Value::String(v.clone()));
        }
        let bytes = serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST,
            "artifactType": self.artifact_type,
            "config": {
                "mediaType": EMPTY_MEDIA_TYPE,
                "digest": Digest::of(EMPTY).to_string(),
                "size": EMPTY.len(),
            },
            "layers": [{
                "mediaType": self.layer_media_type,
                "digest": Digest::of(&self.layer).to_string(),
                "size": self.layer.len(),
            }],
            "subject": {
                "mediaType": subject.media_type,
                "digest": subject.digest().to_string(),
                "size": subject.bytes.len(),
            },
            "annotations": annotations,
        }))
        .expect("manifests serialize");
        Manifest {
            media_type: OCI_MANIFEST.into(),
            bytes,
        }
    }
}

/// The tag under which a registry with no referrers API keeps an image's referrers (the OCI
/// distribution spec's tag schema): `sha256-<hex>`.
pub fn referrers_tag(subject: &Digest) -> String {
    format!("sha256-{}", subject.hex())
}

/// `index` (an image index, or none) with `referrer` added once.
pub fn add_referrer(index: Option<&[u8]>, referrer: &Manifest, artifact_type: &str) -> Manifest {
    let mut manifests: Vec<Value> = index
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
        .and_then(|v| v["manifests"].as_array().cloned())
        .unwrap_or_default();
    let digest = referrer.digest().to_string();
    if !manifests.iter().any(|m| m["digest"] == digest.as_str()) {
        let annotations = serde_json::from_slice::<Value>(&referrer.bytes)
            .ok()
            .and_then(|v| v.get("annotations").cloned())
            .unwrap_or_else(|| json!({}));
        manifests.push(json!({
            "mediaType": OCI_MANIFEST,
            "digest": digest,
            "size": referrer.bytes.len(),
            "artifactType": artifact_type,
            "annotations": annotations,
        }));
    }
    Manifest {
        media_type: OCI_INDEX.into(),
        bytes: serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "mediaType": OCI_INDEX,
            "manifests": manifests,
        }))
        .expect("indexes serialize"),
    }
}

impl crate::Artifacts {
    /// The image `tag` points to in `repository`, when it is one a bake published, with its
    /// config read; `None` when there is none or it isn't one.
    pub fn previous(
        &self,
        repository: &Repository,
        tag: &str,
    ) -> Result<Option<Previous>, ArtifactError> {
        self.runtime.block_on(async {
            let manifest = match self.registry.manifest(repository, tag).await {
                Ok(manifest) => manifest,
                Err(ArtifactError::NotFound(_)) => return Ok(None),
                Err(e) => return Err(e),
            };
            let Ok(json) = serde_json::from_slice::<Value>(&manifest.bytes) else {
                return Ok(None);
            };
            let Some(config) = json["config"]["digest"]
                .as_str()
                .and_then(|d| Digest::parse(d).ok())
            else {
                return Ok(None);
            };
            let config = self
                .registry
                .pull_blob(repository, &config, MOST_CONFIG)
                .await?;
            Ok(Previous::read(&manifest.bytes, &config))
        })
    }

    /// \[B-240\] The label of the image `manifest` (of `repository`) names, read from its config
    /// (at most [`MOST_CONFIG`] bytes, checked against its digest): `None` when it carries none.
    pub fn label(
        &self,
        repository: &Repository,
        manifest: &Manifest,
    ) -> Result<Option<Label>, ArtifactError> {
        Ok(self
            .config(repository, manifest)?
            .and_then(|bytes| Label::of_config(&bytes)))
    }

    /// \[B-240, K-15\] The config bytes of the image `manifest` (of `repository`) names, at most
    /// [`MOST_CONFIG`] bytes and checked against its digest: `None` when the manifest names none.
    pub fn config(
        &self,
        repository: &Repository,
        manifest: &Manifest,
    ) -> Result<Option<Vec<u8>>, ArtifactError> {
        let json: Value = serde_json::from_slice(&manifest.bytes).map_err(|e| {
            ArtifactError::Invalid(format!("{}'s manifest: {e}", manifest.digest()))
        })?;
        let Some(config) = json["config"]["digest"]
            .as_str()
            .and_then(|d| Digest::parse(d).ok())
        else {
            return Ok(None);
        };
        let bytes =
            self.runtime
                .block_on(self.registry.pull_blob(repository, &config, MOST_CONFIG))?;
        if Digest::of(&bytes) != config {
            return Err(ArtifactError::Invalid(format!(
                "{config}: the config isn't its digest"
            )));
        }
        Ok(Some(bytes))
    }

    /// \[K-14\] Publishes `image`, made by [`file_image`] of the file at `layer`, into
    /// `repository`: the layer (unless the registry holds it), then as [`Self::publish`] does,
    /// the `fixed` tags before the `moving` ones.
    pub fn publish_file_image(
        &self,
        repository: &Repository,
        image: &Image,
        layer: &Path,
        fixed: &[String],
        moving: &[String],
    ) -> Result<Pushed, ArtifactError> {
        let first = image
            .layers
            .first()
            .ok_or_else(|| ArtifactError::Invalid("an image of no layer".into()))?;
        let uploaded = self.push_file(repository, layer, &first.digest, first.size)?;
        let mut pushed = self.publish(repository, image, fixed, &[], moving)?;
        if let Some(blob) = pushed.blobs.first_mut() {
            blob.uploaded = uploaded;
        }
        Ok(pushed)
    }

    /// \[K-14\] Uploads the file at `path`, whose SHA-256 and size are `digest` and `size`, as a
    /// blob of `repository`, unless the registry holds it already (a HEAD first): whether it was
    /// uploaded. A layer written whole beforehand goes up this way, then [`Self::publish`] takes
    /// it as a reused layer.
    pub fn push_file(
        &self,
        repository: &Repository,
        path: &Path,
        digest: &Digest,
        size: u64,
    ) -> Result<bool, ArtifactError> {
        self.runtime.block_on(async {
            let registry = self.registry.as_ref();
            if registry.blob_exists(repository, digest).await? {
                return Ok(false);
            }
            registry
                .push_blob(repository, digest, size, file_stream(path.to_path_buf()))
                .await?;
            Ok(true)
        })
    }

    /// \[B-240\] The layers `manifest` (an image of `repository`) names, each written into `dir`
    /// as it arrives, under its digest's hex, checked against its digest, and refused past
    /// `most` bytes; in the manifest's order. No layer is ever held in memory whole.
    pub fn pull_layers(
        &self,
        repository: &Repository,
        manifest: &Manifest,
        most: u64,
        dir: &Path,
    ) -> Result<Vec<PathBuf>, ArtifactError> {
        let what = repository.at_digest(&manifest.digest());
        let json: Value = serde_json::from_slice(&manifest.bytes)
            .map_err(|e| ArtifactError::Invalid(format!("{what}'s manifest: {e}")))?;
        let layers = json["layers"]
            .as_array()
            .ok_or_else(|| ArtifactError::Invalid(format!("{what} names no layers")))?;
        let digests = layers
            .iter()
            .map(|layer| {
                layer["digest"]
                    .as_str()
                    .and_then(|d| Digest::parse(d).ok())
                    .ok_or_else(|| ArtifactError::Invalid(format!("{what}: a layer's digest")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let io = |path: &Path, e: io::Error| {
            ArtifactError::Invalid(format!("writing {}: {e}", path.display()))
        };
        fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
        self.runtime.block_on(async {
            let mut out = Vec::new();
            for digest in digests {
                let path = dir.join(digest.hex());
                let mut file =
                    io::BufWriter::new(fs::File::create(&path).map_err(|e| io(&path, e))?);
                self.registry
                    .pull_blob_to(repository, &digest, most, &mut file)
                    .await?;
                file.flush().map_err(|e| io(&path, e))?;
                out.push(path);
            }
            Ok(out)
        })
    }

    /// The manifest `tag` names in `repository`, checked against the digest it is served under;
    /// `None` when the tag names nothing.
    pub fn tagged(
        &self,
        repository: &Repository,
        tag: &str,
    ) -> Result<Option<Manifest>, ArtifactError> {
        self.runtime.block_on(async {
            match self.registry.manifest(repository, tag).await {
                Ok(manifest) => Ok(Some(manifest)),
                Err(ArtifactError::NotFound(_)) => Ok(None),
                Err(e) => Err(e),
            }
        })
    }

    /// Moves `tag` in `repository` to `manifest`, an image the repository holds already.
    pub fn tag(
        &self,
        repository: &Repository,
        tag: &str,
        manifest: &Manifest,
    ) -> Result<(), ArtifactError> {
        self.runtime.block_on(put_manifest(
            self.registry.as_ref(),
            repository,
            tag,
            manifest,
        ))
    }

    /// B-67: the one layer of each referrer of `subject` in `repository` whose artifact type is
    /// `artifact_type` (the referrers API's list, else the tag schema's index), each read whole,
    /// checked, and refused past `most` bytes. A referrer that can't be read is left out.
    pub fn referrer_layers(
        &self,
        repository: &Repository,
        subject: &Digest,
        artifact_type: &str,
        most: u64,
    ) -> Result<Vec<Vec<u8>>, ArtifactError> {
        self.runtime.block_on(async {
            let registry = self.registry.as_ref();
            let index = match registry.referrers(repository, subject).await? {
                Some(index) => index,
                None => match registry.manifest(repository, &referrers_tag(subject)).await {
                    Ok(index) => index.bytes,
                    Err(ArtifactError::NotFound(_)) => return Ok(Vec::new()),
                    Err(e) => return Err(e),
                },
            };
            let mut out = Vec::new();
            for digest in referrer_digests(&index, artifact_type) {
                let Ok(manifest) = registry.manifest(repository, &digest.to_string()).await else {
                    continue;
                };
                if manifest.digest() != digest {
                    continue;
                }
                let Some(layer) = first_layer(&manifest.bytes) else {
                    continue;
                };
                if let Ok(bytes) = registry.pull_blob(repository, &layer, most).await {
                    out.push(bytes);
                }
            }
            Ok(out)
        })
    }

    /// Whether `repository` holds the blob `digest`. An error is an error, never "absent".
    pub fn has_blob(
        &self,
        repository: &Repository,
        digest: &Digest,
    ) -> Result<bool, ArtifactError> {
        self.runtime
            .block_on(self.registry.blob_exists(repository, digest))
    }

    /// B-65: publishes `image` into `repository`: every blob the registry doesn't hold (a HEAD
    /// first), the manifest by its digest, then `fixed` tags, then each referrer (B-67), and the
    /// `moving` tags last, so that a moving tag never points at an image whose signature isn't
    /// there yet. Every step can be repeated: a retry uploads only what is still missing.
    pub fn publish(
        &self,
        repository: &Repository,
        image: &Image,
        fixed: &[String],
        referrers: &[Referrer],
        moving: &[String],
    ) -> Result<Pushed, ArtifactError> {
        self.runtime.block_on(async {
            let registry = self.registry.as_ref();
            let mut pushed = Pushed::default();
            let blob = async |role: &str,
                              digest: Digest,
                              size: u64,
                              stream: Option<BlobStream>|
                   -> Result<PushedBlob, ArtifactError> {
                let uploaded = if registry.blob_exists(repository, &digest).await? {
                    false
                } else {
                    let stream = stream.ok_or_else(|| {
                        ArtifactError::NotFound(format!(
                            "{role} layer {digest} isn't at {repository}, and its bytes aren't here"
                        ))
                    })?;
                    registry
                        .push_blob(repository, &digest, size, stream)
                        .await?;
                    true
                };
                Ok(PushedBlob {
                    role: role.into(),
                    digest: digest.to_string(),
                    size,
                    uploaded,
                })
            };
            for layer in &image.layers {
                let stream = if layer.reused {
                    None
                } else {
                    Some(layer_stream(layer.files.clone())?)
                };
                pushed
                    .blobs
                    .push(blob(layer.role.word(), layer.digest, layer.size, stream).await?);
            }
            let config = Bytes::from(image.config.clone());
            pushed.blobs.push(
                blob(
                    "config",
                    Digest::of(&image.config),
                    image.config.len() as u64,
                    Some(futures_util::stream::once(async move { Ok(config) }).boxed()),
                )
                .await?,
            );
            let digest = image.digest();
            put_manifest(registry, repository, &digest.to_string(), &image.manifest).await?;
            for tag in fixed {
                put_manifest(registry, repository, tag, &image.manifest).await?;
                pushed.tags.push(tag.clone());
            }
            for referrer in referrers {
                let layer = Bytes::from(referrer.layer.clone());
                pushed.blobs.push(
                    blob(
                        "signature",
                        Digest::of(&referrer.layer),
                        referrer.layer.len() as u64,
                        Some(futures_util::stream::once(async move { Ok(layer) }).boxed()),
                    )
                    .await?,
                );
                pushed.blobs.push(
                    blob(
                        "signature-config",
                        Digest::of(EMPTY),
                        EMPTY.len() as u64,
                        Some(
                            futures_util::stream::once(async { Ok(Bytes::from_static(EMPTY)) })
                                .boxed(),
                        ),
                    )
                    .await?,
                );
                let manifest = referrer.manifest(&image.manifest);
                put_manifest(
                    registry,
                    repository,
                    &manifest.digest().to_string(),
                    &manifest,
                )
                .await?;
                // A registry with the referrers API lists it already; one without keeps an index
                // under the tag schema's tag, which this read-modify-write updates.
                if registry.referrers(repository, &digest).await?.is_none() {
                    let tag = referrers_tag(&digest);
                    let index = match registry.manifest(repository, &tag).await {
                        Ok(index) => Some(index.bytes),
                        Err(ArtifactError::NotFound(_)) => None,
                        Err(e) => return Err(e),
                    };
                    let index = add_referrer(index.as_deref(), &manifest, &referrer.artifact_type);
                    put_manifest(registry, repository, &tag, &index).await?;
                }
            }
            for tag in moving {
                put_manifest(registry, repository, tag, &image.manifest).await?;
                pushed.tags.push(tag.clone());
            }
            Ok(pushed)
        })
    }
}

async fn put_manifest(
    registry: &dyn crate::Registry,
    repository: &Repository,
    reference: &str,
    manifest: &Manifest,
) -> Result<(), ArtifactError> {
    let reported = registry
        .push_manifest(repository, reference, manifest)
        .await?;
    if let Some(reported) = reported
        && reported != manifest.digest()
    {
        return Err(ArtifactError::DigestMismatch {
            what: format!("{repository}:{reference}"),
            expected: manifest.digest().to_string(),
            actual: reported.to_string(),
        });
    }
    Ok(())
}

/// The previous image in the OCI image layout at `dir`, under `tag`, when it is one a bake wrote.
pub fn layout_previous(dir: &Path, tag: &str) -> Option<Previous> {
    let index: Value = serde_json::from_slice(&fs::read(dir.join("index.json")).ok()?).ok()?;
    let entry = index["manifests"]
        .as_array()?
        .iter()
        .find(|m| m["annotations"][REF_NAME] == tag)?;
    let manifest = read_blob(dir, &Digest::parse(entry["digest"].as_str()?).ok()?)?;
    let json: Value = serde_json::from_slice(&manifest).ok()?;
    let config = read_blob(
        dir,
        &Digest::parse(json["config"]["digest"].as_str()?).ok()?,
    )?;
    Previous::read(&manifest, &config)
}

/// The digests of an image index's manifests of `artifact_type`.
fn referrer_digests(index: &[u8], artifact_type: &str) -> Vec<Digest> {
    serde_json::from_slice::<Value>(index)
        .ok()
        .and_then(|v| v["manifests"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|m| m["artifactType"] == artifact_type)
        .filter_map(|m| Digest::parse(m["digest"].as_str()?).ok())
        .collect()
}

/// The digest of a manifest's first layer.
fn first_layer(manifest: &[u8]) -> Option<Digest> {
    let json: Value = serde_json::from_slice(manifest).ok()?;
    Digest::parse(json["layers"][0]["digest"].as_str()?).ok()
}

/// The manifest `tag` names in the OCI image layout at `dir`, its bytes checked against its
/// digest; `None` when the tag names nothing there.
pub fn layout_tagged(dir: &Path, tag: &str) -> Option<Manifest> {
    let index: Value = serde_json::from_slice(&fs::read(dir.join("index.json")).ok()?).ok()?;
    let entry = index["manifests"]
        .as_array()?
        .iter()
        .find(|m| m["annotations"][REF_NAME] == tag)?;
    let bytes = read_blob(dir, &Digest::parse(entry["digest"].as_str()?).ok()?)?;
    Some(Manifest {
        media_type: entry["mediaType"]
            .as_str()
            .unwrap_or(OCI_MANIFEST)
            .to_string(),
        bytes,
    })
}

/// B-67: [`crate::Artifacts::referrer_layers`] in the OCI image layout at `dir`, from the tag
/// schema's index there.
pub fn layout_referrer_layers(
    dir: &Path,
    subject: &Digest,
    artifact_type: &str,
    most: u64,
) -> Vec<Vec<u8>> {
    let Some(index) = layout_tagged(dir, &referrers_tag(subject)) else {
        return Vec::new();
    };
    referrer_digests(&index.bytes, artifact_type)
        .iter()
        .filter_map(|digest| read_blob(dir, digest))
        .filter_map(|manifest| first_layer(&manifest))
        .filter_map(|layer| read_blob(dir, &layer))
        .filter(|bytes| bytes.len() as u64 <= most)
        .collect()
}

/// Names `manifest` (an image the layout at `dir` holds already) `tag` in its `index.json`,
/// which is read, changed and written whole under its lock.
pub fn layout_tag(dir: &Path, tag: &str, manifest: &Manifest) -> Result<(), ArtifactError> {
    let at = |e: io::Error, p: &Path| ArtifactError::Invalid(format!("{}: {e}", p.display()));
    if !layout_has(dir, &manifest.digest()) {
        return Err(ArtifactError::NotFound(format!(
            "{} isn't in {}",
            manifest.digest(),
            dir.display()
        )));
    }
    let lock_path = dir.join(".index.lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| at(e, &lock_path))?;
    lock.lock().map_err(|e| at(e, &lock_path))?;
    let index_path = dir.join("index.json");
    let mut manifests: Vec<Value> = fs::read(&index_path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v["manifests"].as_array().cloned())
        .unwrap_or_default();
    manifests.retain(|m| m["annotations"][REF_NAME] != tag);
    manifests.push(json!({
        "mediaType": manifest.media_type,
        "digest": manifest.digest().to_string(),
        "size": manifest.bytes.len(),
        "annotations": { REF_NAME: tag },
    }));
    let index = serde_json::to_vec_pretty(&json!({
        "schemaVersion": 2,
        "mediaType": OCI_INDEX,
        "manifests": manifests,
    }))
    .expect("indexes serialize");
    let temp = dir.join(format!(".index.json.{}.tmp", std::process::id()));
    fs::write(&temp, &index).map_err(|e| at(e, &temp))?;
    fs::rename(&temp, &index_path).map_err(|e| at(e, &index_path))?;
    drop(lock);
    Ok(())
}

fn blob_path(dir: &Path, digest: &Digest) -> PathBuf {
    dir.join("blobs").join("sha256").join(digest.hex())
}

fn read_blob(dir: &Path, digest: &Digest) -> Option<Vec<u8>> {
    let bytes = fs::read(blob_path(dir, digest)).ok()?;
    (Digest::of(&bytes) == *digest).then_some(bytes)
}

/// Whether the layout at `dir` holds the blob `digest`.
pub fn layout_has(dir: &Path, digest: &Digest) -> bool {
    blob_path(dir, digest).is_file()
}

/// \[K-14\] Copies the file at `path`, whose SHA-256 is `digest`, into the layout at `dir` as a
/// blob, unless it holds it already: whether it was copied. [`write_layout`] then takes it as a
/// reused layer. The copy is checked against `digest`.
pub fn layout_put_file(dir: &Path, path: &Path, digest: &Digest) -> Result<bool, ArtifactError> {
    put_blob(dir, digest, |file| {
        let mut from = fs::File::open(path)
            .map_err(|e| ArtifactError::Invalid(format!("{}: {e}", path.display())))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = from
                .read(&mut buf)
                .map_err(|e| ArtifactError::Invalid(format!("{}: {e}", path.display())))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])
                .map_err(|e| ArtifactError::Invalid(format!("writing a blob: {e}")))?;
        }
        let actual = Digest::from_bytes(hasher.finalize().into());
        if actual != *digest {
            return Err(ArtifactError::DigestMismatch {
                what: path.display().to_string(),
                expected: digest.to_string(),
                actual: actual.to_string(),
            });
        }
        Ok(())
    })
}

/// A blob written whole into the layout: under a temporary name, then renamed to its digest.
fn put_blob(
    dir: &Path,
    digest: &Digest,
    write: impl FnOnce(&mut fs::File) -> Result<(), ArtifactError>,
) -> Result<bool, ArtifactError> {
    let path = blob_path(dir, digest);
    if path.is_file() {
        return Ok(false);
    }
    let at = |e: io::Error, p: &Path| ArtifactError::Invalid(format!("{}: {e}", p.display()));
    let parent = path.parent().unwrap_or(dir);
    fs::create_dir_all(parent).map_err(|e| at(e, parent))?;
    let temp = parent.join(format!(".{}.{}.tmp", digest.hex(), std::process::id()));
    let mut file = fs::File::create(&temp).map_err(|e| at(e, &temp))?;
    let written = write(&mut file).and_then(|()| file.sync_all().map_err(|e| at(e, &temp)));
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    fs::rename(&temp, &path).map_err(|e| at(e, &path))?;
    Ok(true)
}

/// B-65: writes `image` into the OCI image layout at `dir` (made when missing): every blob it
/// lacks, then `index.json`, read, changed and written whole under its lock, naming the image
/// under each of `tags` (fixed first, moving last) and each referrer under the tag schema's tag.
pub fn write_layout(
    dir: &Path,
    image: &Image,
    tags: &[String],
    referrers: &[Referrer],
) -> Result<Pushed, ArtifactError> {
    let at = |e: io::Error, p: &Path| ArtifactError::Invalid(format!("{}: {e}", p.display()));
    fs::create_dir_all(dir).map_err(|e| at(e, dir))?;
    let mut pushed = Pushed::default();
    for layer in &image.layers {
        let uploaded = if layer.reused {
            if !layout_has(dir, &layer.digest) {
                return Err(ArtifactError::NotFound(format!(
                    "{} layer {} isn't in {}",
                    layer.role.word(),
                    layer.digest,
                    dir.display()
                )));
            }
            false
        } else {
            put_blob(dir, &layer.digest, |file| {
                let mut out = io::BufWriter::new(file);
                let (digest, _) = write_layer(&layer.files, &mut out)?;
                out.flush()
                    .map_err(|e| ArtifactError::Invalid(format!("writing a layer: {e}")))?;
                if digest != layer.digest {
                    return Err(ArtifactError::DigestMismatch {
                        what: format!("{} layer", layer.role.word()),
                        expected: layer.digest.to_string(),
                        actual: digest.to_string(),
                    });
                }
                Ok(())
            })?
        };
        pushed.blobs.push(PushedBlob {
            role: layer.role.word().into(),
            digest: layer.digest.to_string(),
            size: layer.size,
            uploaded,
        });
    }
    let mut small = vec![
        ("config".to_string(), image.config.clone()),
        ("manifest".to_string(), image.manifest.bytes.clone()),
    ];
    let mut referrer_manifests = Vec::new();
    for referrer in referrers {
        let manifest = referrer.manifest(&image.manifest);
        small.push(("signature".into(), referrer.layer.clone()));
        small.push(("signature-config".into(), EMPTY.to_vec()));
        small.push(("signature-manifest".into(), manifest.bytes.clone()));
        referrer_manifests.push((manifest, referrer.artifact_type.clone()));
    }
    for (role, bytes) in small {
        let digest = Digest::of(&bytes);
        let uploaded = put_blob(dir, &digest, |file| {
            file.write_all(&bytes)
                .map_err(|e| ArtifactError::Invalid(format!("writing a blob: {e}")))
        })?;
        pushed.blobs.push(PushedBlob {
            role,
            digest: digest.to_string(),
            size: bytes.len() as u64,
            uploaded,
        });
    }
    // index.json: load, change, save whole, under its lock.
    let lock_path = dir.join(".index.lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| at(e, &lock_path))?;
    lock.lock().map_err(|e| at(e, &lock_path))?;
    let index_path = dir.join("index.json");
    let mut manifests: Vec<Value> = fs::read(&index_path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v["manifests"].as_array().cloned())
        .unwrap_or_default();
    let mut name = |tag: &str, manifest: &Manifest| {
        manifests.retain(|m| m["annotations"][REF_NAME] != tag);
        manifests.push(json!({
            "mediaType": manifest.media_type,
            "digest": manifest.digest().to_string(),
            "size": manifest.bytes.len(),
            "annotations": { REF_NAME: tag },
        }));
    };
    for (manifest, artifact_type) in &referrer_manifests {
        let tag = referrers_tag(&image.digest());
        let existing = fs::read(&index_path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| {
                v["manifests"].as_array().and_then(|ms| {
                    ms.iter()
                        .find(|m| m["annotations"][REF_NAME] == tag.as_str())
                        .and_then(|m| m["digest"].as_str().and_then(|d| Digest::parse(d).ok()))
                })
            })
            .and_then(|d| read_blob(dir, &d));
        let index = add_referrer(existing.as_deref(), manifest, artifact_type);
        let digest = index.digest();
        put_blob(dir, &digest, |file| {
            file.write_all(&index.bytes)
                .map_err(|e| ArtifactError::Invalid(format!("writing a blob: {e}")))
        })?;
        name(&tag, &index);
    }
    for tag in tags {
        name(tag, &image.manifest);
        pushed.tags.push(tag.clone());
    }
    let index = serde_json::to_vec_pretty(&json!({
        "schemaVersion": 2,
        "mediaType": OCI_INDEX,
        "manifests": manifests,
    }))
    .expect("indexes serialize");
    let temp = dir.join(format!(".index.json.{}.tmp", std::process::id()));
    fs::write(&temp, &index).map_err(|e| at(e, &temp))?;
    fs::rename(&temp, &index_path).map_err(|e| at(e, &index_path))?;
    let layout = dir.join("oci-layout");
    if !layout.is_file() {
        fs::write(&layout, br#"{"imageLayoutVersion":"1.0.0"}"#).map_err(|e| at(e, &layout))?;
    }
    drop(lock);
    Ok(pushed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Registry as _;
    use crate::fake::{FakeRegistry, repo};
    use std::sync::Arc;

    struct Bake {
        dir: tempfile::TempDir,
    }

    impl Bake {
        fn new() -> Bake {
            Bake {
                dir: tempfile::tempdir().unwrap(),
            }
        }

        fn entry(&self, path: &str, bytes: &[u8], group: Group) -> Entry {
            let source = self.dir.path().join(path.replace('/', "_"));
            fs::write(&source, bytes).unwrap();
            Entry {
                path: path.into(),
                size: bytes.len() as u64,
                sha256: Digest::of(bytes),
                group,
                source,
            }
        }
    }

    fn pack(n: u8) -> String {
        format!("hushrun/packs/{}.pack", format!("{n:02x}").repeat(32))
    }

    fn all_present(_: &Digest) -> bool {
        true
    }

    fn roles(plan: &Plan) -> Vec<(Role, usize, bool)> {
        plan.layers
            .iter()
            .map(|l| (l.role, l.files.len(), l.reused.is_some()))
            .collect()
    }

    fn publish_once(
        fake: &Arc<FakeRegistry>,
        entries: &[Entry],
        previous: Option<&Previous>,
    ) -> (Image, Pushed, Plan) {
        let artifacts = crate::Artifacts::with_registry(fake.clone()).unwrap();
        let r = repo("registry.example/acme/shop");
        let present = |d: &Digest| artifacts.has_blob(&r, d).unwrap();
        let plan = plan(entries, previous, &present, false);
        let image = build(&plan, "linux-x64", &"ab".repeat(32), &BTreeMap::new()).unwrap();
        let pushed = artifacts
            .publish(
                &r,
                &image,
                &["hushrun-linux-x64-abababababababab".into()],
                &[],
                &["hushrun-linux-x64-latest".into()],
            )
            .unwrap();
        (image, pushed, plan)
    }

    // covers: B-65
    #[test]
    fn a_first_bake_is_compacted_into_deps_and_workspace() {
        let bake = Bake::new();
        let entries = vec![
            bake.entry(&pack(1), b"serde", Group::Deps),
            bake.entry(&pack(2), b"app", Group::Code),
            bake.entry("hushrun/baked.json", b"{}", Group::Store),
        ];
        let plan = plan(&entries, None, &all_present, false);
        assert_eq!(
            plan.compacted.as_deref(),
            Some("there is no previous image")
        );
        assert_eq!(
            roles(&plan),
            [
                (Role::Deps, 1, false),
                (Role::Workspace, 1, false),
                (Role::Recent, 1, false)
            ]
        );
    }

    // covers: B-65
    #[test]
    fn a_code_only_change_uploads_only_the_recent_layer() {
        let fake = Arc::new(FakeRegistry::default());
        let bake = Bake::new();
        let first = vec![
            bake.entry(&pack(1), &[1; 3000], Group::Deps),
            bake.entry(&pack(2), &[2; 2000], Group::Code),
            bake.entry(&pack(3), &[3; 2000], Group::Code),
            bake.entry(&pack(6), &[6; 2000], Group::Code),
            bake.entry("hushrun/baked.json", b"{\"v\":1}", Group::Store),
        ];
        let (image, pushed, _) = publish_once(&fake, &first, None);
        assert_eq!(pushed.blobs.iter().filter(|b| b.uploaded).count(), 4);
        let previous = Previous::read(&image.manifest.bytes, &image.config).unwrap();
        // Pack 3 changed: its new block is pack 4; pack 3 stays in the workspace layer, unused.
        let second = vec![
            first[0].clone(),
            first[1].clone(),
            first[3].clone(),
            bake.entry(&pack(4), &[4; 2100], Group::Code),
            bake.entry("hushrun/baked.json", b"{\"v\":2}", Group::Store),
        ];
        let (_, pushed, plan) = publish_once(&fake, &second, Some(&previous));
        assert_eq!(plan.compacted, None);
        assert_eq!(plan.stale, [pack(3)]);
        let uploaded: Vec<&str> = pushed
            .blobs
            .iter()
            .filter(|b| b.uploaded)
            .map(|b| b.role.as_str())
            .collect();
        assert_eq!(uploaded, ["recent", "config"]);
        // The registry got exactly those bytes, and the moving tag moved.
        let r = repo("registry.example/acme/shop");
        assert_eq!(fake.pushed().len(), 6);
        let latest = fake.manifest_at(&r, "hushrun-linux-x64-latest").unwrap();
        let again = Previous::read(&latest.bytes, &{
            let json: Value = serde_json::from_slice(&latest.bytes).unwrap();
            let d = Digest::parse(json["config"]["digest"].as_str().unwrap()).unwrap();
            crate::Artifacts::with_registry(fake.clone())
                .unwrap()
                .runtime
                .block_on(fake.pull_blob(&r, &d, MOST_CONFIG))
                .unwrap()
        })
        .unwrap();
        assert_eq!(again.label.layers.len(), 3);
    }

    // covers: B-65
    #[test]
    fn a_lockfile_change_rewrites_the_deps_layer_alone() {
        let fake = Arc::new(FakeRegistry::default());
        let bake = Bake::new();
        let first = vec![
            bake.entry(&pack(1), &[1; 3000], Group::Deps),
            bake.entry(&pack(2), &[2; 2000], Group::Code),
            bake.entry("hushrun/baked.json", b"{\"v\":1}", Group::Store),
        ];
        let (image, _, _) = publish_once(&fake, &first, None);
        let previous = Previous::read(&image.manifest.bytes, &image.config).unwrap();
        let second = vec![
            bake.entry(&pack(5), &[5; 3100], Group::Deps),
            first[1].clone(),
            bake.entry("hushrun/baked.json", b"{\"v\":3}", Group::Store),
        ];
        let (_, pushed, _) = publish_once(&fake, &second, Some(&previous));
        let uploaded: Vec<&str> = pushed
            .blobs
            .iter()
            .filter(|b| b.uploaded)
            .map(|b| b.role.as_str())
            .collect();
        assert_eq!(uploaded, ["deps", "recent", "config"]);
    }

    // covers: B-65
    #[test]
    fn the_workspace_layer_is_compacted_when_mostly_stale_or_gone() {
        let bake = Bake::new();
        let a = bake.entry(&pack(2), &[2; 1000], Group::Code);
        let b = bake.entry(&pack(3), &[3; 1000], Group::Code);
        let store = bake.entry("hushrun/baked.json", b"{}", Group::Store);
        let first = plan(
            &[a.clone(), b.clone(), store.clone()],
            None,
            &all_present,
            false,
        );
        let image = build(&first, "linux-x64", "m", &BTreeMap::new()).unwrap();
        let previous = Previous::read(&image.manifest.bytes, &image.config).unwrap();
        // Half its bytes no longer used.
        let c = bake.entry(&pack(4), &[4; 10], Group::Code);
        let next = plan(
            &[a.clone(), c.clone(), store.clone()],
            Some(&previous),
            &all_present,
            false,
        );
        assert!(next.compacted.unwrap().contains("no longer used"));
        // Missing at the destination.
        let none = |_: &Digest| false;
        let next = plan(
            &[a.clone(), b.clone(), c.clone(), store.clone()],
            Some(&previous),
            &none,
            false,
        );
        assert!(next.compacted.unwrap().contains("isn't at the destination"));
        // A path with other bytes.
        let b2 = bake.entry(&pack(3), &[9; 1000], Group::Code);
        let next = plan(
            &[a.clone(), b2, store.clone()],
            Some(&previous),
            &all_present,
            false,
        );
        assert!(next.compacted.unwrap().contains("other bytes"));
        // Asked.
        let next = plan(&[a, b, store], Some(&previous), &all_present, true);
        assert_eq!(next.compacted.as_deref(), Some("asked to"));
    }

    // covers: B-65
    #[test]
    fn the_workspace_layer_is_written_again_once_a_week_by_the_time_its_label_records() {
        let day = 24 * 60 * 60 * 1000_i64;
        let week = 7 * day as u64;
        let bake = Bake::new();
        let a = bake.entry(&pack(2), &[2; 1000], Group::Code);
        let b = bake.entry(&pack(3), &[3; 100], Group::Code);
        let store = bake.entry("hushrun/baked.json", b"{}", Group::Store);
        let at = |now_ms: i64| Compaction {
            asked: false,
            now_ms,
            every_ms: Some(week),
        };
        let t0 = 1_800_000_000_000_i64;
        // The first bake writes the workspace layer, and its label records when.
        let first = plan_at(&[a.clone(), store.clone()], None, &all_present, &at(t0));
        assert_eq!(first.compacted_ms, Some(t0));
        let image = build(&first, "linux-x64", "m", &BTreeMap::new()).unwrap();
        let previous = Previous::read(&image.manifest.bytes, &image.config).unwrap();
        assert_eq!(previous.label.compacted_ms, Some(t0));
        // Six days on, a code change keeps it, and the time it was written goes on.
        let kept = plan_at(
            &[a.clone(), b.clone(), store.clone()],
            Some(&previous),
            &all_present,
            &at(t0 + 6 * day),
        );
        assert!(kept.compacted.is_none(), "{:?}", kept.compacted);
        assert_eq!(kept.compacted_ms, Some(t0));
        let image = build(&kept, "linux-x64", "m", &BTreeMap::new()).unwrap();
        let previous = Previous::read(&image.manifest.bytes, &image.config).unwrap();
        assert_eq!(previous.label.compacted_ms, Some(t0));
        // A week after it was written, the next bake writes it again, whatever its size.
        let weekly = plan_at(
            &[a.clone(), b.clone(), store.clone()],
            Some(&previous),
            &all_present,
            &at(t0 + 7 * day),
        );
        assert!(
            weekly
                .compacted
                .as_deref()
                .is_some_and(|w| w.contains("7 days ago") && w.contains("every 7 days")),
            "{:?}",
            weekly.compacted
        );
        assert_eq!(weekly.compacted_ms, Some(t0 + 7 * day));
        assert_eq!(roles(&weekly)[0], (Role::Workspace, 2, false));
        // No age rule: kept; an unrecorded time with one: written again; a time past the
        // bake's clock by more than ten minutes: written again.
        let no_age = plan(
            &[a.clone(), b.clone(), store.clone()],
            Some(&previous),
            &all_present,
            false,
        );
        assert!(no_age.compacted.is_none());
        let mut unrecorded = previous.clone();
        unrecorded.label.compacted_ms = None;
        let again = plan_at(
            &[a.clone(), store.clone()],
            Some(&unrecorded),
            &all_present,
            &at(t0),
        );
        assert!(again.compacted.unwrap().contains("isn't recorded"));
        let mut future = previous.clone();
        future.label.compacted_ms = Some(t0 + day / 24);
        let again = plan_at(
            &[a.clone(), store.clone()],
            Some(&future),
            &all_present,
            &at(t0),
        );
        assert!(again.compacted.unwrap().contains("past this bake's clock"));
        // A clock a few minutes ahead is a clock, not a forged time.
        future.label.compacted_ms = Some(t0 + 60_000);
        let kept = plan_at(&[a, store], Some(&future), &all_present, &at(t0));
        assert!(kept.compacted.is_none(), "{:?}", kept.compacted);
    }

    // covers: B-65
    // An image baked before programs had a group of their own holds them in its workspace layer.
    // The first bake after the upgrade keeps that layer (nothing changed, nothing stale), and the
    // same, unchanged program isn't put in the recent layer too: no path is in two layers.
    #[test]
    fn a_program_an_older_workspace_layer_holds_is_in_one_layer_only() {
        let bake = Bake::new();
        let code = bake.entry(&pack(2), &vec![2; 4 << 20], Group::Code);
        let store = bake.entry("hushrun/baked.json", b"{}", Group::Store);
        let program_bytes = vec![7; 1 << 20];
        // The older bake: the program was Code, and went in the workspace layer.
        let old_program = bake.entry(&pack(7), &program_bytes, Group::Code);
        let first = plan(
            &[code.clone(), old_program, store.clone()],
            None,
            &all_present,
            false,
        );
        let image = build(&first, "linux-x64", "m", &BTreeMap::new()).unwrap();
        let previous = Previous::read(&image.manifest.bytes, &image.config).unwrap();
        // The bake after the upgrade: the same program, now a Program.
        let program = bake.entry(&pack(7), &program_bytes, Group::Program);
        let next = plan(
            &[code, program, store],
            Some(&previous),
            &all_present,
            false,
        );
        let mut seen = BTreeSet::new();
        for layer in &next.layers {
            for file in &layer.files {
                assert!(
                    seen.insert(file.path.clone()),
                    "{} is in two layers: {:?}",
                    file.path,
                    roles(&next)
                );
            }
        }
    }

    // covers: B-65
    // A merge relinks the repository's programs: they go up again in the recent layer, and the
    // workspace layer stays, however large they are beside it.
    #[test]
    fn programs_that_change_at_every_merge_leave_the_workspace_layer_alone() {
        let bake = Bake::new();
        let code = bake.entry(&pack(2), &vec![2; 4 << 20], Group::Code);
        let store = bake.entry("hushrun/baked.json", b"{}", Group::Store);
        let program = bake.entry(&pack(7), &vec![7; 3 << 20], Group::Program);
        let first = plan(
            &[code.clone(), program, store.clone()],
            None,
            &all_present,
            false,
        );
        assert_eq!(
            roles(&first),
            [(Role::Workspace, 1, false), (Role::Recent, 2, false)]
        );
        let image = build(&first, "linux-x64", "m", &BTreeMap::new()).unwrap();
        let previous = Previous::read(&image.manifest.bytes, &image.config).unwrap();
        // The merge: a new program, three quarters of the workspace layer's size.
        let relinked = bake.entry(&pack(8), &vec![8; 3 << 20], Group::Program);
        let merge = plan(
            &[code, relinked, store],
            Some(&previous),
            &all_present,
            false,
        );
        assert!(merge.compacted.is_none(), "{:?}", merge.compacted);
        assert_eq!(
            roles(&merge),
            [(Role::Workspace, 1, true), (Role::Recent, 2, false)]
        );
    }

    // covers: B-65
    #[test]
    fn the_recent_layer_every_bake_uploads_again_stays_small_beside_the_workspace_layer() {
        // A workspace layer of 8 MiB. Every bake uploads the recent layer again: past a quarter
        // of the workspace layer (and past 1 MiB), the workspace layer is written again instead.
        let bake = Bake::new();
        let big = 8usize << 20;
        let a = bake.entry(&pack(2), &vec![2; big], Group::Code);
        let store = bake.entry("hushrun/baked.json", b"{}", Group::Store);
        let first = plan(&[a.clone(), store.clone()], None, &all_present, false);
        let image = build(&first, "linux-x64", "m", &BTreeMap::new()).unwrap();
        let previous = Previous::read(&image.manifest.bytes, &image.config).unwrap();
        let small = bake.entry(&pack(3), &vec![3; 1 << 20], Group::Code);
        let kept = plan(
            &[a.clone(), small, store.clone()],
            Some(&previous),
            &all_present,
            false,
        );
        assert!(kept.compacted.is_none(), "{:?}", kept.compacted);
        let large = bake.entry(&pack(4), &vec![4; (big / 4) + 1], Group::Code);
        let folded = plan(&[a, large, store], Some(&previous), &all_present, false);
        assert!(
            folded
                .compacted
                .as_deref()
                .is_some_and(|w| w.contains("recent layer")),
            "{:?}",
            folded.compacted
        );
    }

    // covers: B-65
    #[test]
    fn no_path_is_in_two_layers_and_there_are_never_more_than_three() {
        let bake = Bake::new();
        let mut entries: Vec<Entry> = (0..50u8)
            .map(|n| {
                bake.entry(
                    &pack(n),
                    &[n; 10],
                    if n % 2 == 0 { Group::Deps } else { Group::Code },
                )
            })
            .collect();
        entries.push(bake.entry("hushrun/baked.json", b"{}", Group::Store));
        let plan = plan(&entries, None, &all_present, false);
        assert!(plan.layers.len() <= 3);
        let mut seen = BTreeSet::new();
        for layer in &plan.layers {
            for file in &layer.files {
                assert!(seen.insert(file.path.clone()), "{} twice", file.path);
            }
        }
        assert_eq!(seen.len(), entries.len());
    }

    // covers: B-65
    #[test]
    fn a_layout_holds_the_same_image_and_a_second_write_adds_only_what_changed() {
        let bake = Bake::new();
        let out = tempfile::tempdir().unwrap();
        let entries = vec![
            bake.entry(&pack(1), &[1; 3000], Group::Deps),
            bake.entry(&pack(2), &[2; 2000], Group::Code),
            bake.entry("hushrun/baked.json", b"{\"v\":1}", Group::Store),
        ];
        let present = |d: &Digest| layout_has(out.path(), d);
        let first = plan(&entries, None, &present, false);
        let image = build(&first, "linux-arm64", "m1", &BTreeMap::new()).unwrap();
        let tags = ["hushrun-linux-arm64-latest".to_string()];
        let pushed = write_layout(out.path(), &image, &tags, &[]).unwrap();
        assert!(pushed.blobs.iter().all(|b| b.uploaded));
        // The layer on disk is the one the manifest names, and tar reads it.
        for layer in &image.layers {
            let bytes = fs::read(blob_path(out.path(), &layer.digest)).unwrap();
            assert_eq!(Digest::of(&bytes), layer.digest);
        }
        let previous = layout_previous(out.path(), &tags[0]).unwrap();
        assert_eq!(previous.manifest_digest, image.digest());
        let second = vec![
            entries[0].clone(),
            entries[1].clone(),
            bake.entry("hushrun/baked.json", b"{\"v\":2}", Group::Store),
        ];
        let next = plan(&second, Some(&previous), &present, false);
        let image2 = build(&next, "linux-arm64", "m2", &BTreeMap::new()).unwrap();
        let pushed = write_layout(out.path(), &image2, &tags, &[]).unwrap();
        let uploaded: Vec<&str> = pushed
            .blobs
            .iter()
            .filter(|b| b.uploaded)
            .map(|b| b.role.as_str())
            .collect();
        assert_eq!(uploaded, ["recent", "config", "manifest"]);
        let config: Value = serde_json::from_slice(&image2.config).unwrap();
        assert_eq!(config["architecture"], "arm64");
        assert_eq!(config["os"], "linux");
    }

    // covers: B-67
    #[test]
    fn a_referrer_is_kept_under_the_tag_schema_when_there_is_no_api() {
        let fake = Arc::new(FakeRegistry::default());
        let bake = Bake::new();
        let entries = vec![bake.entry("hushrun/baked.json", b"{}", Group::Store)];
        let artifacts = crate::Artifacts::with_registry(fake.clone()).unwrap();
        let r = repo("registry.example/acme/shop");
        let image = build(
            &plan(&entries, None, &all_present, false),
            "linux-x64",
            "m",
            &BTreeMap::new(),
        )
        .unwrap();
        let referrer = Referrer {
            artifact_type: "application/vnd.dev.sigstore.bundle.v0.3+json".into(),
            layer_media_type: "application/vnd.dev.sigstore.bundle.v0.3+json".into(),
            layer: b"{\"bundle\":1}".to_vec(),
            annotations: BTreeMap::new(),
        };
        artifacts
            .publish(
                &r,
                &image,
                &[],
                &[referrer.clone(), referrer],
                &["t".into()],
            )
            .unwrap();
        let index = fake
            .manifest_at(&r, &referrers_tag(&image.digest()))
            .unwrap();
        let json: Value = serde_json::from_slice(&index.bytes).unwrap();
        // Twice the same signature is one referrer.
        assert_eq!(json["manifests"].as_array().unwrap().len(), 1);
        let child = Digest::parse(json["manifests"][0]["digest"].as_str().unwrap()).unwrap();
        let child = fake.manifest_at(&r, &child.to_string()).unwrap();
        let child: Value = serde_json::from_slice(&child.bytes).unwrap();
        assert_eq!(child["subject"]["digest"], image.digest().to_string());
    }

    // covers: K-14
    #[test]
    fn a_file_image_publishes_once_and_reads_back() {
        let fake = Arc::new(FakeRegistry::default());
        let dir = tempfile::tempdir().unwrap();
        let layer = dir.path().join("store.tar");
        fs::write(&layer, vec![7u8; 3000]).unwrap();
        let labels = BTreeMap::from([("dev.example".to_string(), "{}".to_string())]);
        let image = file_image(
            &layer,
            Role::Packages,
            "linux-x64",
            &labels,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(image.layers[0].digest, Digest::of(&[7u8; 3000]));
        let artifacts = crate::Artifacts::with_registry(fake.clone()).unwrap();
        let r = repo("registry.example/acme/shop");
        let fixed = vec!["hushrun-packages-linux-x64-0123".to_string()];
        let moving = vec!["hushrun-packages-linux-x64-latest".to_string()];
        let pushed = artifacts
            .publish_file_image(&r, &image, &layer, &fixed, &moving)
            .unwrap();
        assert!(pushed.blobs[0].uploaded);
        assert_eq!(pushed.blobs[0].role, "packages");
        assert_eq!(pushed.tags, [fixed[0].clone(), moving[0].clone()]);
        // The same layer again uploads nothing.
        let again = artifacts
            .publish_file_image(&r, &image, &layer, &[], &moving)
            .unwrap();
        assert!(!again.blobs[0].uploaded);
        let manifest = artifacts.tagged(&r, &moving[0]).unwrap().unwrap();
        assert_eq!(manifest.digest(), image.digest());
        let config = artifacts.config(&r, &manifest).unwrap().unwrap();
        let config: Value = serde_json::from_slice(&config).unwrap();
        assert_eq!(config["config"]["Labels"]["dev.example"], "{}");
        assert!(artifacts.label(&r, &manifest).unwrap().is_none());
        let pulled = artifacts
            .pull_layers(&r, &manifest, 1 << 20, &dir.path().join("pulled"))
            .unwrap();
        assert_eq!(fs::read(&pulled[0]).unwrap(), vec![7u8; 3000]);
        // As a layout, the same image.
        let out = dir.path().join("layout");
        let written = write_file_layout(&out, &image, &layer, &moving).unwrap();
        assert!(written.blobs[0].uploaded);
        assert_eq!(
            layout_tagged(&out, &moving[0]).unwrap().digest(),
            image.digest()
        );
    }
}

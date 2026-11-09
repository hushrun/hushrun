//! The OCI copy by digest (A-2).
//!
//! 1. Fetch the source manifest by digest and verify its SHA-256.
//! 2. For an index, copy every child manifest by digest first, for all platforms, so the
//!    index digest is preserved.
//! 3. Copy each blob: skip non-distributable layers and blobs the destination has; on the same
//!    host try a cross-repository mount; otherwise stream it, verifying the digest.
//! 4. Push the manifest bytes unchanged, by digest and then by tag, check the digest the
//!    registry reports, and HEAD the destination by digest.

use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::future::BoxFuture;
use futures_util::stream::{self, TryStreamExt};
use hushrun_core::digest::Digest;
use serde::Deserialize;

use crate::registry::{
    ArtifactError, DOCKER_LIST, DOCKER_MANIFEST, Descriptor, MANIFEST_TYPES, Manifest, Mount,
    OCI_INDEX, OCI_MANIFEST, Registry, Repository,
};

/// Blobs copied at once (A-2).
pub const DEFAULT_CONCURRENCY: usize = 4;

/// What a copy did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CopyReport {
    /// The digest of the top manifest, unchanged by the copy.
    pub digest: Option<Digest>,
    pub manifests: usize,
    pub blobs_copied: usize,
    pub blobs_mounted: usize,
    /// Already at the destination.
    pub blobs_present: usize,
    /// Non-distributable layers, which stay where their `urls` say.
    pub blobs_foreign: usize,
}

#[derive(Debug, Default)]
struct Counters {
    manifests: AtomicUsize,
    copied: AtomicUsize,
    mounted: AtomicUsize,
    present: AtomicUsize,
    foreign: AtomicUsize,
}

/// Copies the manifest `digest` of `source`, and everything it refers to, to `destination`,
/// tagging it `tag` there.
pub async fn copy(
    registry: &dyn Registry,
    source: &Repository,
    digest: &Digest,
    destination: &Repository,
    tag: Option<&str>,
    concurrency: usize,
) -> Result<CopyReport, ArtifactError> {
    let counters = Counters::default();
    let manifest = fetch(registry, source, digest).await?;
    copy_tree(
        registry,
        source,
        destination,
        &manifest,
        concurrency.max(1),
        &counters,
    )
    .await?;
    push(registry, destination, &digest.to_string(), &manifest).await?;
    if let Some(tag) = tag {
        push(registry, destination, tag, &manifest).await?;
    }
    // The destination serves the digest that was copied (A-2 step 4).
    match registry.head(destination, &digest.to_string()).await? {
        Some(found) if found == *digest => {}
        found => {
            return Err(ArtifactError::DigestMismatch {
                what: destination.at_digest(digest),
                expected: digest.to_string(),
                actual: found.map_or("nothing".into(), |d| d.to_string()),
            });
        }
    }
    Ok(CopyReport {
        digest: Some(*digest),
        manifests: counters.manifests.load(Ordering::SeqCst) + 1,
        blobs_copied: counters.copied.load(Ordering::SeqCst),
        blobs_mounted: counters.mounted.load(Ordering::SeqCst),
        blobs_present: counters.present.load(Ordering::SeqCst),
        blobs_foreign: counters.foreign.load(Ordering::SeqCst),
    })
}

/// Fetches the manifest `digest` and checks that its bytes hash to it.
async fn fetch(
    registry: &dyn Registry,
    repository: &Repository,
    digest: &Digest,
) -> Result<Manifest, ArtifactError> {
    let manifest = registry.manifest(repository, &digest.to_string()).await?;
    let actual = manifest.digest();
    if actual != *digest {
        return Err(ArtifactError::DigestMismatch {
            what: repository.at_digest(digest),
            expected: digest.to_string(),
            actual: actual.to_string(),
        });
    }
    if !MANIFEST_TYPES.contains(&manifest.media_type.as_str()) {
        return Err(ArtifactError::Unsupported(manifest.media_type));
    }
    Ok(manifest)
}

/// Pushes `manifest` under `reference` and checks the digest the registry reports.
async fn push(
    registry: &dyn Registry,
    repository: &Repository,
    reference: &str,
    manifest: &Manifest,
) -> Result<(), ArtifactError> {
    let expected = manifest.digest();
    match registry
        .push_manifest(repository, reference, manifest)
        .await?
    {
        Some(reported) if reported != expected => Err(ArtifactError::DigestMismatch {
            what: format!("{repository}:{reference} (Docker-Content-Digest)"),
            expected: expected.to_string(),
            actual: reported.to_string(),
        }),
        _ => Ok(()),
    }
}

/// Copies what `manifest` refers to, children before parents.
fn copy_tree<'a>(
    registry: &'a dyn Registry,
    source: &'a Repository,
    destination: &'a Repository,
    manifest: &'a Manifest,
    concurrency: usize,
    counters: &'a Counters,
) -> BoxFuture<'a, Result<(), ArtifactError>> {
    Box::pin(async move {
        match parse(manifest)? {
            Parsed::Index(children) => {
                for child in children {
                    let child_manifest = fetch(registry, source, &child.digest).await?;
                    copy_tree(
                        registry,
                        source,
                        destination,
                        &child_manifest,
                        concurrency,
                        counters,
                    )
                    .await?;
                    push(
                        registry,
                        destination,
                        &child.digest.to_string(),
                        &child_manifest,
                    )
                    .await?;
                    counters.manifests.fetch_add(1, Ordering::SeqCst);
                }
                Ok(())
            }
            Parsed::Image(blobs) => {
                stream::iter(blobs.iter().map(Ok))
                    .try_for_each_concurrent(concurrency, |blob| {
                        copy_blob(registry, source, destination, blob, counters)
                    })
                    .await
            }
        }
    })
}

async fn copy_blob(
    registry: &dyn Registry,
    source: &Repository,
    destination: &Repository,
    blob: &Descriptor,
    counters: &Counters,
) -> Result<(), ArtifactError> {
    if !blob.urls.is_empty() {
        counters.foreign.fetch_add(1, Ordering::SeqCst);
        return Ok(());
    }
    if registry.blob_exists(destination, &blob.digest).await? {
        counters.present.fetch_add(1, Ordering::SeqCst);
        return Ok(());
    }
    if source.host == destination.host
        && registry.mount(source, destination, &blob.digest).await? == Mount::Mounted
    {
        counters.mounted.fetch_add(1, Ordering::SeqCst);
        return Ok(());
    }
    registry.copy_blob(source, destination, blob).await?;
    counters.copied.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

enum Parsed {
    Index(Vec<Descriptor>),
    Image(Vec<Descriptor>),
}

#[derive(Deserialize)]
struct RawManifest {
    #[serde(default)]
    manifests: Option<Vec<RawDescriptor>>,
    #[serde(default)]
    config: Option<RawDescriptor>,
    #[serde(default)]
    layers: Option<Vec<RawDescriptor>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDescriptor {
    #[serde(default)]
    media_type: String,
    digest: String,
    size: u64,
    #[serde(default)]
    urls: Vec<String>,
}

fn parse(manifest: &Manifest) -> Result<Parsed, ArtifactError> {
    let raw: RawManifest = serde_json::from_slice(&manifest.bytes)
        .map_err(|e| ArtifactError::Invalid(format!("the manifest isn't valid JSON: {e}")))?;
    let descriptor = |d: RawDescriptor| -> Result<Descriptor, ArtifactError> {
        let digest = Digest::parse(&d.digest).map_err(|_| {
            ArtifactError::Unsupported(format!("digest {} (only sha256 is supported)", d.digest))
        })?;
        Ok(Descriptor {
            media_type: d.media_type,
            digest,
            size: d.size,
            urls: d.urls,
        })
    };
    match manifest.media_type.as_str() {
        OCI_INDEX | DOCKER_LIST => Ok(Parsed::Index(
            raw.manifests
                .unwrap_or_default()
                .into_iter()
                .map(descriptor)
                .collect::<Result<_, _>>()?,
        )),
        OCI_MANIFEST | DOCKER_MANIFEST => {
            let config = raw
                .config
                .ok_or_else(|| ArtifactError::Invalid("the manifest has no config".into()))?;
            let mut blobs = vec![descriptor(config)?];
            for layer in raw.layers.unwrap_or_default() {
                blobs.push(descriptor(layer)?);
            }
            Ok(Parsed::Image(blobs))
        }
        other => Err(ArtifactError::Unsupported(other.to_string())),
    }
}

/// The media type of manifest bytes: the `mediaType` field, else what the fields say (OCI
/// manifests and indexes may leave it out).
pub fn media_type_of(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if let Some(media_type) = value.get("mediaType").and_then(|m| m.as_str()) {
        return Some(media_type.to_string());
    }
    if value.get("manifests").is_some() {
        Some(OCI_INDEX.to_string())
    } else if value.get("config").is_some() && value.get("layers").is_some() {
        Some(OCI_MANIFEST.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeRegistry, descriptor_json, repo};

    fn run<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    // covers: A-2, A-4
    #[test]
    fn copies_an_image_across_hosts_by_digest() {
        let fake = FakeRegistry::default();
        let src = repo("staging.example.com/back/go");
        let dst = repo("prod.example.com/back/go");
        let digest = fake.image(&src, &[b"layer one", b"layer two"]);
        let report = run(copy(&fake, &src, &digest, &dst, Some("abc123"), 4)).unwrap();
        assert_eq!(report.digest, Some(digest));
        assert_eq!((report.blobs_copied, report.blobs_mounted), (3, 0));
        // The bytes, and so the digest, are unchanged; the tag points at the same manifest.
        assert_eq!(
            fake.manifest_at(&dst, &digest.to_string()),
            fake.manifest_at(&src, &digest.to_string())
        );
        assert_eq!(fake.manifest_at(&dst, "abc123").unwrap().digest(), digest);
        // Copying again finds every blob present.
        let again = run(copy(&fake, &src, &digest, &dst, None, 4)).unwrap();
        assert_eq!((again.blobs_copied, again.blobs_present), (0, 3));
    }

    // covers: A-2
    #[test]
    fn mounts_on_the_same_host_and_uploads_when_declined() {
        let fake = FakeRegistry::default();
        let src = repo("us-docker.pkg.dev/staging/back/go");
        let dst = repo("us-docker.pkg.dev/prod/back/go");
        let digest = fake.image(&src, &[b"layer"]);
        let report = run(copy(&fake, &src, &digest, &dst, None, 4)).unwrap();
        assert_eq!((report.blobs_mounted, report.blobs_copied), (2, 0));
        assert_eq!((fake.mounts(), fake.copies()), (2, 0));

        // 202 Accepted means "upload instead", not an error.
        let declining = FakeRegistry {
            decline_mounts: true,
            ..FakeRegistry::default()
        };
        let digest = declining.image(&src, &[b"layer"]);
        let report = run(copy(&declining, &src, &digest, &dst, None, 4)).unwrap();
        assert_eq!((report.blobs_mounted, report.blobs_copied), (0, 2));
    }

    // covers: A-2
    #[test]
    fn copies_every_child_of_an_index_first() {
        let fake = FakeRegistry::default();
        let src = repo("a.example.com/app");
        let dst = repo("b.example.com/app");
        let amd64 = fake.image(&src, &[b"amd64 layer"]);
        let arm64 = fake.image(&src, &[b"arm64 layer"]);
        let index = fake.index(&src, &[amd64, arm64]);
        let report = run(copy(&fake, &src, &index, &dst, Some("v1"), 4)).unwrap();
        assert_eq!(report.manifests, 3);
        for child in [amd64, arm64] {
            assert!(fake.manifest_at(&dst, &child.to_string()).is_some());
        }
        assert_eq!(fake.manifest_at(&dst, "v1").unwrap().digest(), index);
        // The config blob is shared: copied once, then present.
        assert_eq!(report.blobs_copied + report.blobs_present, 4);
    }

    // covers: A-2
    #[test]
    fn skips_non_distributable_layers() {
        let fake = FakeRegistry::default();
        let src = repo("a.example.com/win");
        let dst = repo("b.example.com/win");
        let config = fake.put_blob(&src, b"{}");
        let layer = fake.put_blob(&src, b"local layer");
        let mut foreign = descriptor_json(&fake.put_blob(&src, b"foreign"));
        foreign["urls"] = serde_json::json!(["https://mcr.microsoft.com/foreign"]);
        let json = serde_json::json!({
            "schemaVersion": 2, "mediaType": DOCKER_MANIFEST,
            "config": descriptor_json(&config),
            "layers": [descriptor_json(&layer), foreign],
        });
        let digest = fake.put_manifest(
            &src,
            None,
            Manifest {
                media_type: DOCKER_MANIFEST.into(),
                bytes: serde_json::to_vec(&json).unwrap(),
            },
        );
        let report = run(copy(&fake, &src, &digest, &dst, None, 4)).unwrap();
        assert_eq!((report.blobs_copied, report.blobs_foreign), (2, 1));
    }

    // covers: A-2
    #[test]
    fn mismatches_fail_the_copy() {
        let src = repo("a.example.com/app");
        let dst = repo("b.example.com/app");
        // A blob whose bytes don't hash to its digest.
        let corrupt = FakeRegistry {
            corrupt_blobs: true,
            ..FakeRegistry::default()
        };
        let digest = corrupt.image(&src, &[b"layer"]);
        assert!(matches!(
            run(copy(&corrupt, &src, &digest, &dst, None, 4)),
            Err(ArtifactError::DigestMismatch { .. })
        ));
        // A registry reporting another digest for the pushed manifest.
        let liar = FakeRegistry {
            misreport_digest: Some(Digest::of(b"other")),
            ..FakeRegistry::default()
        };
        let digest = liar.image(&src, &[b"layer"]);
        let err = run(copy(&liar, &src, &digest, &dst, None, 4)).unwrap_err();
        assert!(err.to_string().contains("Docker-Content-Digest"), "{err}");
        // A source manifest whose bytes don't match the digest asked for.
        let fake = FakeRegistry::default();
        let real = fake.image(&src, &[b"layer"]);
        let manifest = fake.manifest_at(&src, &real.to_string()).unwrap();
        let wrong = Digest::of(b"something else");
        fake.put_manifest(&src, Some(&wrong.to_string()), manifest);
        assert!(matches!(
            run(copy(&fake, &src, &wrong, &dst, None, 4)),
            Err(ArtifactError::DigestMismatch { .. })
        ));
    }

    // covers: A-2
    #[test]
    fn concurrency_is_bounded() {
        let fake = FakeRegistry::default();
        let src = repo("a.example.com/app");
        let dst = repo("b.example.com/app");
        let layers: Vec<Vec<u8>> = (0..20).map(|i| format!("layer {i}").into_bytes()).collect();
        let refs: Vec<&[u8]> = layers.iter().map(Vec::as_slice).collect();
        let digest = fake.image(&src, &refs);
        run(copy(&fake, &src, &digest, &dst, None, 4)).unwrap();
        let max = fake.max_in_flight.load(Ordering::SeqCst);
        assert!((1..=4).contains(&max), "{max}");
    }

    #[test]
    fn unsupported_media_types_are_refused() {
        let fake = FakeRegistry::default();
        let src = repo("a.example.com/app");
        let digest = fake.put_manifest(
            &src,
            None,
            Manifest {
                media_type: "application/vnd.docker.distribution.manifest.v1+prettyjws".into(),
                bytes: b"{}".to_vec(),
            },
        );
        assert!(matches!(
            run(copy(
                &fake,
                &src,
                &digest,
                &repo("b.example.com/app"),
                None,
                4
            )),
            Err(ArtifactError::Unsupported(_))
        ));
    }

    #[test]
    fn media_types_come_from_the_bytes() {
        assert_eq!(media_type_of(br#"{"mediaType":"x"}"#).as_deref(), Some("x"));
        assert_eq!(
            media_type_of(br#"{"manifests":[]}"#).as_deref(),
            Some(OCI_INDEX)
        );
        assert_eq!(
            media_type_of(br#"{"config":{},"layers":[]}"#).as_deref(),
            Some(OCI_MANIFEST)
        );
        assert_eq!(media_type_of(b"{}"), None);
    }
}

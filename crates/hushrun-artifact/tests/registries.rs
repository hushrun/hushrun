//! OCI copies against real registries (`--features it`): CNCF Distribution and zot in CI.
//! `HUSHRUN_IT_REGISTRIES` names two registry hosts, separated by spaces, reached over plain
//! HTTP; each test works in fresh repositories.
#![cfg(feature = "it")]

use std::time::{SystemTime, UNIX_EPOCH};

use oci_client::client::{ClientConfig, ClientProtocol};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference};
use std::sync::Arc;

use hushrun_artifact::credentials::DockerConfig;
use hushrun_artifact::{
    ArtifactError, Artifacts, ImageRef, OCI_INDEX, OCI_MANIFEST, OciRegistry, Repository,
};
use hushrun_core::digest::Digest;

fn hosts() -> Option<(String, String)> {
    let Ok(text) = std::env::var("HUSHRUN_IT_REGISTRIES") else {
        assert!(
            std::env::var_os("CI").is_none(),
            "HUSHRUN_IT_REGISTRIES must name two registries in CI"
        );
        eprintln!("HUSHRUN_IT_REGISTRIES is not set: nothing to test");
        return None;
    };
    let mut hosts = text.split_whitespace().map(str::to_string);
    let a = hosts.next().expect("two registry hosts");
    let b = hosts.next().unwrap_or_else(|| a.clone());
    Some((a, b))
}

fn unique(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("hushrun-it/{prefix}-{nanos}")
}

struct Seeder {
    client: Client,
    runtime: tokio::runtime::Runtime,
}

impl Seeder {
    fn new() -> Seeder {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Seeder {
            client: Client::new(ClientConfig {
                protocol: ClientProtocol::Http,
                ..ClientConfig::default()
            }),
            runtime: tokio::runtime::Runtime::new().unwrap(),
        }
    }

    fn blob(&self, repository: &Repository, bytes: &[u8]) -> serde_json::Value {
        let digest = Digest::of(bytes);
        let image = Reference::with_digest(
            repository.host.clone(),
            repository.path.clone(),
            digest.to_string(),
        );
        self.runtime.block_on(async {
            self.client
                .store_auth_if_needed(image.resolve_registry(), &RegistryAuth::Anonymous)
                .await;
            self.client
                .push_blob(&image, bytes.to_vec(), &digest.to_string())
                .await
                .unwrap();
        });
        serde_json::json!({"mediaType": "application/vnd.oci.image.layer.v1.tar", "digest": digest.to_string(), "size": bytes.len()})
    }

    fn manifest(
        &self,
        repository: &Repository,
        tag: &str,
        media_type: &str,
        json: &serde_json::Value,
    ) -> Digest {
        let bytes = serde_json::to_vec_pretty(json).unwrap();
        let image = Reference::with_tag(
            repository.host.clone(),
            repository.path.clone(),
            tag.to_string(),
        );
        self.runtime
            .block_on(self.client.push_manifest_raw(
                &image,
                bytes.clone(),
                media_type.parse().unwrap(),
            ))
            .unwrap();
        Digest::of(&bytes)
    }

    fn image(&self, repository: &Repository, tag: &str, layers: &[&[u8]]) -> (Digest, usize) {
        let mut config = self.blob(repository, format!("{{\"tag\":\"{tag}\"}}").as_bytes());
        config["mediaType"] = "application/vnd.oci.image.config.v1+json".into();
        let layers: Vec<_> = layers.iter().map(|l| self.blob(repository, l)).collect();
        let json = serde_json::json!({
            "schemaVersion": 2, "mediaType": OCI_MANIFEST, "config": config, "layers": layers,
        });
        let size = serde_json::to_vec_pretty(&json).unwrap().len();
        (self.manifest(repository, tag, OCI_MANIFEST, &json), size)
    }
}

/// A client with no Docker configuration: the tests never run the user's credential helpers.
fn artifacts_for(hosts: &[&str]) -> Artifacts {
    let registry = OciRegistry::new(
        DockerConfig::default(),
        hosts.iter().map(|h| h.to_string()).collect(),
    )
    .unwrap();
    Artifacts::with_registry(Arc::new(registry)).unwrap()
}

fn artifacts(a: &str, b: &str) -> Artifacts {
    artifacts_for(&[a, b])
}

fn repository(host: &str, path: &str) -> Repository {
    Repository::parse(&format!("{host}/{path}")).unwrap()
}

// covers: A-2, A-4
#[test]
fn copies_across_registries_and_resolves_tags() {
    let Some((a, b)) = hosts() else { return };
    let seeder = Seeder::new();
    let source = repository(&a, &unique("src"));
    let (digest, _) = seeder.image(&source, "v1", &[b"layer one", b"layer two"]);
    let artifacts = artifacts(&a, &b);
    // Tag resolution (attest --artifact, artifact digest).
    let tagged = ImageRef::parse(&source.at_tag("v1")).unwrap();
    assert_eq!(artifacts.digest(&tagged).unwrap(), digest);

    let destination = repository(&b, &unique("dst"));
    let pinned = ImageRef::parse(&source.at_digest(&digest)).unwrap();
    let report = artifacts.copy(&pinned, &destination, Some("rev1")).unwrap();
    assert_eq!(report.digest, Some(digest));
    if a != b {
        assert_eq!(report.blobs_copied, 3, "{report:?}");
    }
    assert_eq!(
        artifacts
            .digest(&ImageRef::parse(&destination.at_tag("rev1")).unwrap())
            .unwrap(),
        digest
    );
    // A second copy finds everything present.
    let again = artifacts.copy(&pinned, &destination, None).unwrap();
    assert_eq!(again.blobs_copied + again.blobs_mounted, 0, "{again:?}");
}

// covers: A-2
#[test]
fn mounts_within_one_registry() {
    let Some((a, b)) = hosts() else { return };
    let seeder = Seeder::new();
    let source = repository(&a, &unique("mount-src"));
    let (digest, _) = seeder.image(&source, "v1", &[b"mounted layer"]);
    let destination = repository(&a, &unique("mount-dst"));
    let report = artifacts(&a, &b)
        .copy(
            &ImageRef::parse(&source.at_digest(&digest)).unwrap(),
            &destination,
            None,
        )
        .unwrap();
    // A registry may decline (202) and the blob is uploaded instead; either way it's there.
    assert_eq!(report.blobs_mounted + report.blobs_copied, 2, "{report:?}");
    eprintln!("same-host copy: {report:?}");
}

// covers: A-2
#[test]
fn copies_an_index_and_every_platform() {
    let Some((a, b)) = hosts() else { return };
    let seeder = Seeder::new();
    let source = repository(&a, &unique("index-src"));
    let (amd64, amd64_size) = seeder.image(&source, "amd64", &[b"amd64"]);
    let (arm64, arm64_size) = seeder.image(&source, "arm64", &[b"arm64"]);
    let index = seeder.manifest(
        &source,
        "multi",
        OCI_INDEX,
        &serde_json::json!({
            "schemaVersion": 2, "mediaType": OCI_INDEX,
            "manifests": [
                {"mediaType": OCI_MANIFEST, "digest": amd64.to_string(), "size": amd64_size, "platform": {"architecture": "amd64", "os": "linux"}},
                {"mediaType": OCI_MANIFEST, "digest": arm64.to_string(), "size": arm64_size, "platform": {"architecture": "arm64", "os": "linux"}},
            ]
        }),
    );
    let destination = repository(&b, &unique("index-dst"));
    let artifacts = artifacts(&a, &b);
    let report = artifacts
        .copy(
            &ImageRef::parse(&source.at_digest(&index)).unwrap(),
            &destination,
            Some("multi"),
        )
        .unwrap();
    assert_eq!(report.manifests, 3);
    for child in [amd64, arm64, index] {
        let at = ImageRef::parse(&destination.at_digest(&child)).unwrap();
        assert_eq!(artifacts.digest(&at).unwrap(), child);
    }
}

#[test]
fn an_unreachable_registry_is_a_network_error() {
    let Some((a, b)) = hosts() else { return };
    let seeder = Seeder::new();
    let source = repository(&a, &unique("unreachable"));
    let (digest, _) = seeder.image(&source, "v1", &[b"x"]);
    let err = artifacts_for(&[&a, &b, "127.0.0.1:9"])
        .copy(
            &ImageRef::parse(&source.at_digest(&digest)).unwrap(),
            &Repository::parse("127.0.0.1:9/nowhere").unwrap(),
            None,
        )
        .unwrap_err();
    assert!(matches!(err, ArtifactError::Network(_)), "{err:?}");
}

/// Seeds an image with one large generated layer and prints its pinned reference, for the
/// memory check of A-2 (`cargo test … -- --ignored seed_a_large_layer`, then
/// `hushrun artifact copy` under `/usr/bin/time`). `HUSHRUN_IT_LARGE_LAYER_MB` sets the size
/// [default: 2048].
#[test]
#[ignore = "pushes gigabytes; run on demand"]
fn seed_a_large_layer() {
    use futures_util::stream;
    let Some((a, _)) = hosts() else { return };
    let megabytes: u64 = std::env::var("HUSHRUN_IT_LARGE_LAYER_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2048);
    let seeder = Seeder::new();
    let repository = repository(&a, &unique("large"));
    // xorshift bytes, one MiB at a time: nothing compressible, nothing held in memory.
    let chunks = stream::unfold(
        (0u64, 0x9E37_79B9_7F4A_7C15u64),
        move |(i, mut x)| async move {
            if i == megabytes {
                return None;
            }
            let mut chunk = vec![0u8; 1 << 20];
            for word in chunk.chunks_exact_mut(8) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                word.copy_from_slice(&x.to_le_bytes());
            }
            Some((Ok(bytes::Bytes::from(chunk)), (i + 1, x)))
        },
    );
    let image = Reference::with_tag(
        repository.host.clone(),
        repository.path.clone(),
        "large".into(),
    );
    let pushed = seeder.runtime.block_on(async {
        seeder
            .client
            .store_auth_if_needed(image.resolve_registry(), &RegistryAuth::Anonymous)
            .await;
        seeder
            .client
            .push_blob_stream_chunked(&image, chunks)
            .await
            .unwrap()
    });
    let mut config = seeder.blob(&repository, b"{\"large\":true}");
    config["mediaType"] = "application/vnd.oci.image.config.v1+json".into();
    let layer = serde_json::json!({
        "mediaType": "application/vnd.oci.image.layer.v1.tar",
        "digest": pushed.blob_digest,
        "size": pushed.size,
    });
    let json = serde_json::json!({"schemaVersion": 2, "mediaType": OCI_MANIFEST, "config": config, "layers": [layer]});
    let digest = seeder.manifest(&repository, "large", OCI_MANIFEST, &json);
    println!("SEEDED {}", repository.at_digest(&digest));
}

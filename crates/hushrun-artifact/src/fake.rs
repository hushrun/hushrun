//! An in-memory registry for tests: several hosts, repositories of manifests and blobs, and
//! knobs for the failures a copy must handle.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use hushrun_core::digest::Digest;

use crate::registry::{
    ArtifactError, Descriptor, Manifest, Mount, OCI_INDEX, OCI_MANIFEST, Registry, Repository,
};

#[derive(Debug, Default)]
pub struct FakeRegistry {
    pub(crate) state: Mutex<State>,
    /// Mounts answer `202 Accepted` (declined).
    pub decline_mounts: bool,
    /// Blob copies deliver bytes that don't match the digest.
    pub corrupt_blobs: bool,
    /// Pushes report this digest instead of the real one.
    pub misreport_digest: Option<Digest>,
    /// Pushes to this host are denied.
    pub deny_host: Option<String>,
    pub(crate) in_flight: AtomicUsize,
    pub max_in_flight: AtomicUsize,
}

#[derive(Debug, Default)]
pub(crate) struct State {
    /// (repository, reference) → manifest. References are tags and digests.
    manifests: BTreeMap<(Repository, String), Manifest>,
    blobs: BTreeMap<Repository, BTreeMap<Digest, Vec<u8>>>,
    mounts: BTreeSet<(Repository, Repository, Digest)>,
    copies: Vec<(Repository, Repository, Digest)>,
}

pub fn repo(text: &str) -> Repository {
    Repository::parse(text).unwrap()
}

impl FakeRegistry {
    pub fn put_blob(&self, repository: &Repository, bytes: &[u8]) -> Descriptor {
        let digest = Digest::of(bytes);
        self.state
            .lock()
            .unwrap()
            .blobs
            .entry(repository.clone())
            .or_default()
            .insert(digest, bytes.to_vec());
        Descriptor {
            media_type: "application/octet-stream".into(),
            digest,
            size: bytes.len() as u64,
            urls: Vec::new(),
        }
    }

    pub fn put_manifest(
        &self,
        repository: &Repository,
        reference: Option<&str>,
        manifest: Manifest,
    ) -> Digest {
        let digest = manifest.digest();
        let mut state = self.state.lock().unwrap();
        state
            .manifests
            .insert((repository.clone(), digest.to_string()), manifest.clone());
        if let Some(tag) = reference {
            state
                .manifests
                .insert((repository.clone(), tag.to_string()), manifest);
        }
        digest
    }

    /// An image manifest with a config and `layers` blobs, stored in `repository`.
    pub fn image(&self, repository: &Repository, layers: &[&[u8]]) -> Digest {
        let config = self.put_blob(repository, b"{\"architecture\":\"amd64\"}");
        let layers: Vec<Descriptor> = layers
            .iter()
            .map(|l| self.put_blob(repository, l))
            .collect();
        let json = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST,
            "config": descriptor_json(&config),
            "layers": layers.iter().map(descriptor_json).collect::<Vec<_>>(),
        });
        self.put_manifest(
            repository,
            None,
            Manifest {
                media_type: OCI_MANIFEST.into(),
                bytes: serde_json::to_vec_pretty(&json).unwrap(),
            },
        )
    }

    pub fn index(&self, repository: &Repository, children: &[Digest]) -> Digest {
        let state = self.state.lock().unwrap();
        let entries: Vec<_> = children
            .iter()
            .map(|d| {
                let size = state.manifests[&(repository.clone(), d.to_string())].bytes.len();
                serde_json::json!({"mediaType": OCI_MANIFEST, "digest": d.to_string(), "size": size})
            })
            .collect();
        drop(state);
        let json =
            serde_json::json!({"schemaVersion": 2, "mediaType": OCI_INDEX, "manifests": entries});
        self.put_manifest(
            repository,
            None,
            Manifest {
                media_type: OCI_INDEX.into(),
                bytes: serde_json::to_vec(&json).unwrap(),
            },
        )
    }

    pub fn has_blob(&self, repository: &Repository, digest: &Digest) -> bool {
        self.state
            .lock()
            .unwrap()
            .blobs
            .get(repository)
            .is_some_and(|b| b.contains_key(digest))
    }

    pub fn manifest_at(&self, repository: &Repository, reference: &str) -> Option<Manifest> {
        self.state
            .lock()
            .unwrap()
            .manifests
            .get(&(repository.clone(), reference.to_string()))
            .cloned()
    }

    pub fn mounts(&self) -> usize {
        self.state.lock().unwrap().mounts.len()
    }

    pub fn copies(&self) -> usize {
        self.state.lock().unwrap().copies.len()
    }

    fn denied(&self, repository: &Repository) -> Result<(), ArtifactError> {
        match &self.deny_host {
            Some(host) if *host == repository.host => Err(ArtifactError::Denied {
                host: host.clone(),
                detail: format!("push to {repository} denied"),
            }),
            _ => Ok(()),
        }
    }
}

pub fn descriptor_json(d: &Descriptor) -> serde_json::Value {
    serde_json::json!({"mediaType": d.media_type, "digest": d.digest.to_string(), "size": d.size})
}

#[async_trait]
impl Registry for FakeRegistry {
    async fn manifest(
        &self,
        repository: &Repository,
        reference: &str,
    ) -> Result<Manifest, ArtifactError> {
        self.manifest_at(repository, reference)
            .ok_or_else(|| ArtifactError::NotFound(format!("{repository}:{reference}")))
    }

    async fn head(
        &self,
        repository: &Repository,
        reference: &str,
    ) -> Result<Option<Digest>, ArtifactError> {
        Ok(self.manifest_at(repository, reference).map(|m| m.digest()))
    }

    async fn blob_exists(
        &self,
        repository: &Repository,
        digest: &Digest,
    ) -> Result<bool, ArtifactError> {
        Ok(self.has_blob(repository, digest))
    }

    async fn mount(
        &self,
        from: &Repository,
        to: &Repository,
        digest: &Digest,
    ) -> Result<Mount, ArtifactError> {
        assert_eq!(from.host, to.host, "mounts stay on one host");
        self.denied(to)?;
        if self.decline_mounts || !self.has_blob(from, digest) {
            return Ok(Mount::Declined);
        }
        let mut state = self.state.lock().unwrap();
        let bytes = state.blobs[from][digest].clone();
        state
            .blobs
            .entry(to.clone())
            .or_default()
            .insert(*digest, bytes);
        state.mounts.insert((from.clone(), to.clone(), *digest));
        Ok(Mount::Mounted)
    }

    async fn copy_blob(
        &self,
        from: &Repository,
        to: &Repository,
        blob: &Descriptor,
    ) -> Result<(), ArtifactError> {
        self.denied(to)?;
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        tokio::task::yield_now().await;
        let result = (|| {
            let mut state = self.state.lock().unwrap();
            let mut bytes = state
                .blobs
                .get(from)
                .and_then(|b| b.get(&blob.digest))
                .cloned()
                .ok_or_else(|| ArtifactError::NotFound(from.at_digest(&blob.digest)))?;
            if self.corrupt_blobs {
                bytes.push(b'!');
            }
            // What the real client does while streaming: hash and compare.
            let actual = Digest::of(&bytes);
            if actual != blob.digest {
                return Err(ArtifactError::DigestMismatch {
                    what: from.at_digest(&blob.digest),
                    expected: blob.digest.to_string(),
                    actual: actual.to_string(),
                });
            }
            state
                .blobs
                .entry(to.clone())
                .or_default()
                .insert(blob.digest, bytes);
            state.copies.push((from.clone(), to.clone(), blob.digest));
            Ok(())
        })();
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        result
    }

    async fn push_manifest(
        &self,
        repository: &Repository,
        reference: &str,
        manifest: &Manifest,
    ) -> Result<Option<Digest>, ArtifactError> {
        self.denied(repository)?;
        let digest = manifest.digest();
        let mut state = self.state.lock().unwrap();
        state.manifests.insert(
            (repository.clone(), reference.to_string()),
            manifest.clone(),
        );
        state
            .manifests
            .insert((repository.clone(), digest.to_string()), manifest.clone());
        Ok(Some(self.misreport_digest.unwrap_or(digest)))
    }
}

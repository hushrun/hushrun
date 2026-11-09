//! Artifact promotion (§9, §11.5): OCI copy by digest, tag resolution and registry
//! credentials.
#![forbid(unsafe_code)]

mod copy;
pub mod credentials;
#[cfg(test)]
mod fake;
pub mod hints;
mod oci;
mod registry;

use std::sync::{Arc, Once};

use hushrun_core::digest::Digest;

pub use copy::{CopyReport, DEFAULT_CONCURRENCY, copy, media_type_of};
pub use oci::OciRegistry;
pub use registry::{
    ArtifactError, DOCKER_LIST, DOCKER_MANIFEST, Descriptor, ImageRef, MANIFEST_TYPES, Manifest,
    Mount, OCI_INDEX, OCI_MANIFEST, Registry, Repository, valid_tag,
};

/// How to reach registries.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Hosts reached over plain HTTP (`--insecure-registry`, tests only).
    pub insecure_registries: Vec<String>,
}

/// A registry client and the runtime it runs on: the synchronous API commands use.
pub struct Artifacts {
    runtime: tokio::runtime::Runtime,
    registry: Arc<dyn Registry>,
}

impl std::fmt::Debug for Artifacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Artifacts")
            .field("registry", &self.registry)
            .finish_non_exhaustive()
    }
}

impl Artifacts {
    /// Connects with the Docker configuration's credentials (§11.5).
    pub fn connect(options: &Options) -> Result<Artifacts, ArtifactError> {
        let config = credentials::DockerConfig::load()?;
        let registry = OciRegistry::new(config, options.insecure_registries.clone())?;
        Self::with_registry(Arc::new(registry))
    }

    pub fn with_registry(registry: Arc<dyn Registry>) -> Result<Artifacts, ArtifactError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("hushrun-registry")
            .enable_all()
            .build()
            .map_err(|e| ArtifactError::Network(format!("starting the I/O runtime: {e}")))?;
        Ok(Artifacts { runtime, registry })
    }

    /// The digest `reference` points to. A digest-pinned reference is checked to exist.
    pub fn digest(&self, reference: &ImageRef) -> Result<Digest, ArtifactError> {
        let found = self
            .runtime
            .block_on(
                self.registry
                    .head(&reference.repository, &reference.reference()),
            )?
            .ok_or_else(|| {
                ArtifactError::NotFound(format!(
                    "{}:{}",
                    reference.repository,
                    reference.reference()
                ))
            })?;
        if let Some(pinned) = reference.digest
            && pinned != found
        {
            return Err(ArtifactError::DigestMismatch {
                what: format!("{}@{pinned}", reference.repository),
                expected: pinned.to_string(),
                actual: found.to_string(),
            });
        }
        Ok(found)
    }

    /// Whether the credentials can read `repository` (`doctor`).
    pub fn check_access(&self, repository: &Repository) -> Result<(), ArtifactError> {
        self.runtime
            .block_on(self.registry.check_access(repository))
    }

    /// Copies the digest-pinned `source` to `destination`, tagged `tag` there (A-2).
    pub fn copy(
        &self,
        source: &ImageRef,
        destination: &Repository,
        tag: Option<&str>,
    ) -> Result<CopyReport, ArtifactError> {
        let digest = source.digest.ok_or_else(|| {
            ArtifactError::Invalid(format!(
                "{}: a copy source must be pinned by digest",
                source.repository
            ))
        })?;
        if let Some(tag) = tag
            && !valid_tag(tag)
        {
            return Err(ArtifactError::Invalid(format!(
                "{tag:?} is not a valid tag"
            )));
        }
        self.runtime.block_on(copy(
            self.registry.as_ref(),
            &source.repository,
            &digest,
            destination,
            tag,
            DEFAULT_CONCURRENCY,
        ))
    }
}

/// rustls uses ring (plan §12.3); reqwest is built without a provider, so one must be the
/// process default before the first client.
pub(crate) fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

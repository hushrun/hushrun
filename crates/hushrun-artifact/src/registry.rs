//! The registry operations a copy needs (A-2), behind a trait so the client can be replaced
//! and the copy tested without a network.

use std::fmt;

use async_trait::async_trait;
use hushrun_core::digest::Digest;

/// OCI and Docker manifest media types hushrun copies (A-2).
pub const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
pub const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
pub const DOCKER_LIST: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
pub const DOCKER_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";
pub const MANIFEST_TYPES: [&str; 4] = [OCI_INDEX, OCI_MANIFEST, DOCKER_LIST, DOCKER_MANIFEST];

/// A repository: `host/path`, such as `us-central1-docker.pkg.dev/project/repo/image`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Repository {
    pub host: String,
    pub path: String,
}

impl Repository {
    /// Parses `host/path`. A first segment without `.` or `:` that isn't `localhost` is a
    /// Docker Hub path, as Docker reads it (`nginx` is `docker.io/library/nginx`).
    pub fn parse(text: &str) -> Result<Repository, ArtifactError> {
        let invalid =
            |why: &str| ArtifactError::Invalid(format!("{text:?} is not a repository: {why}"));
        if text.is_empty() || text.contains(['@', ' ']) || text.ends_with('/') {
            return Err(invalid("expected host/path"));
        }
        let (first, rest) = match text.split_once('/') {
            Some((first, rest)) => (first, Some(rest)),
            None => (text, None),
        };
        let is_host = first.contains(['.', ':']) || first == "localhost";
        let (host, path) = match (is_host, rest) {
            (true, Some(rest)) => (first.to_string(), rest.to_string()),
            (true, None) => return Err(invalid("no repository path after the host")),
            (false, Some(_)) => ("docker.io".to_string(), text.to_string()),
            (false, None) => ("docker.io".to_string(), format!("library/{text}")),
        };
        if path
            .split('/')
            .any(|s| s.is_empty() || s.chars().any(|c| c.is_ascii_uppercase()))
        {
            return Err(invalid("path segments are lowercase and non-empty"));
        }
        Ok(Repository { host, path })
    }

    /// `host/path@sha256:…`.
    pub fn at_digest(&self, digest: &Digest) -> String {
        format!("{self}@{digest}")
    }

    /// `host/path:tag`.
    pub fn at_tag(&self, tag: &str) -> String {
        format!("{self}:{tag}")
    }
}

impl fmt::Display for Repository {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.host, self.path)
    }
}

/// A parsed image reference: a repository and a tag, a digest, or both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    pub repository: Repository,
    pub tag: Option<String>,
    pub digest: Option<Digest>,
}

impl ImageRef {
    pub fn parse(text: &str) -> Result<ImageRef, ArtifactError> {
        let (name, digest) = match text.rsplit_once('@') {
            Some((name, _)) => (
                name,
                Some(hushrun_core::event::pinned_digest(text).ok_or_else(|| {
                    ArtifactError::Invalid(format!(
                        "{text}: only sha256 digests are supported, as @sha256:<64 hex>"
                    ))
                })?),
            ),
            None => (text, None),
        };
        let repository = hushrun_core::event::reference_repository(name);
        let tag = name
            .strip_prefix(repository)
            .and_then(|rest| rest.strip_prefix(':'))
            .map(str::to_string);
        if let Some(tag) = &tag
            && !valid_tag(tag)
        {
            return Err(ArtifactError::Invalid(format!(
                "{text}: invalid tag {tag:?}"
            )));
        }
        Ok(ImageRef {
            repository: Repository::parse(repository)?,
            tag,
            digest,
        })
    }

    /// What to ask the registry for: the digest when there is one, else the tag, else `latest`.
    pub fn reference(&self) -> String {
        match (&self.digest, &self.tag) {
            (Some(digest), _) => digest.to_string(),
            (None, Some(tag)) => tag.clone(),
            (None, None) => "latest".to_string(),
        }
    }
}

/// OCI tags: `[A-Za-z0-9_][A-Za-z0-9._-]{0,127}`.
pub fn valid_tag(tag: &str) -> bool {
    let mut chars = tag.chars();
    tag.len() <= 128
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// A manifest exactly as the registry served it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub media_type: String,
    pub bytes: Vec<u8>,
}

impl Manifest {
    /// The SHA-256 of the bytes: the manifest's identity.
    pub fn digest(&self) -> Digest {
        Digest::of(&self.bytes)
    }
}

/// A blob or child manifest a manifest refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    pub media_type: String,
    pub digest: Digest,
    pub size: u64,
    /// Where non-distributable layers live; such layers aren't copied (A-2).
    pub urls: Vec<String>,
}

/// What a cross-repository mount did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mount {
    Mounted,
    /// `202 Accepted`: the registry declined to mount; upload instead (A-2).
    Declined,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArtifactError {
    #[error("{host}: access denied: {detail}")]
    Denied { host: String, detail: String },
    #[error("{0} was not found")]
    NotFound(String),
    #[error("digest mismatch for {what}: expected {expected}, got {actual}")]
    DigestMismatch {
        what: String,
        expected: String,
        actual: String,
    },
    #[error("unsupported media type {0:?}")]
    Unsupported(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Network(String),
}

/// The operations a copy needs. Every method works on one repository or, for `mount` and
/// `copy_blob`, two.
#[async_trait]
pub trait Registry: Send + Sync + fmt::Debug {
    /// The manifest at `reference` (a tag or `sha256:…`), as the registry serves it.
    async fn manifest(
        &self,
        repository: &Repository,
        reference: &str,
    ) -> Result<Manifest, ArtifactError>;

    /// The digest `reference` points to (a HEAD request), or `None` if there's nothing there.
    async fn head(
        &self,
        repository: &Repository,
        reference: &str,
    ) -> Result<Option<Digest>, ArtifactError>;

    async fn blob_exists(
        &self,
        repository: &Repository,
        digest: &Digest,
    ) -> Result<bool, ArtifactError>;

    /// A cross-repository mount on the same host.
    async fn mount(
        &self,
        from: &Repository,
        to: &Repository,
        digest: &Digest,
    ) -> Result<Mount, ArtifactError>;

    /// Streams `blob` from `from` to `to`, hashing while copying; fails on a mismatch.
    async fn copy_blob(
        &self,
        from: &Repository,
        to: &Repository,
        blob: &Descriptor,
    ) -> Result<(), ArtifactError>;

    /// Whether the credentials can read `repository` (`doctor`). An empty or missing
    /// repository is fine; refused access is not.
    async fn check_access(&self, repository: &Repository) -> Result<(), ArtifactError> {
        let _ = repository;
        Ok(())
    }

    /// Pushes the manifest bytes unchanged under `reference` (a tag or `sha256:…`); returns
    /// the digest the registry reported (`Docker-Content-Digest`), if it reported one.
    async fn push_manifest(
        &self,
        repository: &Repository,
        reference: &str,
        manifest: &Manifest,
    ) -> Result<Option<Digest>, ArtifactError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repositories_parse_like_docker() {
        let r = Repository::parse("us-central1-docker.pkg.dev/p/back/go").unwrap();
        assert_eq!(
            (r.host.as_str(), r.path.as_str()),
            ("us-central1-docker.pkg.dev", "p/back/go")
        );
        assert_eq!(
            Repository::parse("localhost:5000/a").unwrap().host,
            "localhost:5000"
        );
        assert_eq!(Repository::parse("localhost/a").unwrap().host, "localhost");
        assert_eq!(
            Repository::parse("nginx").unwrap().to_string(),
            "docker.io/library/nginx"
        );
        assert_eq!(
            Repository::parse("acme/app").unwrap().to_string(),
            "docker.io/acme/app"
        );
        for bad in [
            "",
            "ghcr.io",
            "ghcr.io/",
            "ghcr.io//a",
            "ghcr.io/Acme",
            "a@b",
        ] {
            assert!(Repository::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn image_refs_split_tag_and_digest() {
        let hex = "a".repeat(64);
        let r = ImageRef::parse(&format!("localhost:5000/a/b:v1@sha256:{hex}")).unwrap();
        assert_eq!(r.repository.to_string(), "localhost:5000/a/b");
        assert_eq!(r.tag.as_deref(), Some("v1"));
        assert_eq!(r.reference(), format!("sha256:{hex}"));
        let r = ImageRef::parse("ghcr.io/acme/app:1.2.3").unwrap();
        assert_eq!((r.tag.as_deref(), r.digest), (Some("1.2.3"), None));
        assert_eq!(r.reference(), "1.2.3");
        assert_eq!(
            ImageRef::parse("ghcr.io/acme/app").unwrap().reference(),
            "latest"
        );
        assert!(ImageRef::parse("ghcr.io/acme/app@sha512:00").is_err());
        assert!(ImageRef::parse("ghcr.io/acme/app:-bad").is_err());
    }
}

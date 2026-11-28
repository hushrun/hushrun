//! The registry client (§11.5): `oci-client` for manifests, blob checks and blob streaming,
//! and hushrun's own requests where `oci-client` falls short:
//!
//! - cross-repository mounts, whose token needs pull scope on the source repository, and
//!   whose `202 Accepted` means "upload instead" (A-2);
//! - manifest pushes, to check the `Docker-Content-Digest` the registry reports;
//! - \[K-14\] chunked blob uploads (POST, PATCH each chunk, PUT with the digest), for a registry
//!   that takes at most so many bytes per request, as hushrun's own does
//!   ([`OciRegistry::chunked`]). Every other registry gets one streamed PUT per blob, as before.
//!
//! HTTPS only, except for hosts given as insecure (tests). reqwest drops credentials when a
//! redirect leaves the host, as blob downloads do when they redirect to storage.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use hushrun_core::digest::Digest;
use oci_client::client::{ClientConfig, ClientProtocol};
use oci_client::errors::OciDistributionError;
use oci_client::manifest::OciDescriptor;
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference};
use sha2::{Digest as _, Sha256};

use crate::copy::media_type_of;
use crate::credentials::{Credential, DockerConfig};
use crate::registry::{
    ArtifactError, BlobStream, Descriptor, MANIFEST_TYPES, Manifest, Mount, Registry, Repository,
};

pub struct OciRegistry {
    client: Client,
    http: reqwest::Client,
    config: DockerConfig,
    insecure: Vec<String>,
    credentials: Mutex<BTreeMap<String, Credential>>,
    /// Bearer tokens of hushrun's own requests, by host and scopes.
    tokens: Mutex<BTreeMap<(String, String), String>>,
    /// \[K-14\] Blobs go up in requests of at most this many bytes, else in one PUT.
    chunk_bytes: Option<usize>,
}

impl std::fmt::Debug for OciRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OciRegistry")
            .field("insecure", &self.insecure)
            .finish_non_exhaustive()
    }
}

impl OciRegistry {
    pub fn new(config: DockerConfig, insecure: Vec<String>) -> Result<OciRegistry, ArtifactError> {
        crate::install_crypto_provider();
        let client = Client::new(ClientConfig {
            protocol: if insecure.is_empty() {
                ClientProtocol::Https
            } else {
                ClientProtocol::HttpsExcept(insecure.clone())
            },
            // One streamed PUT per blob with its length: no per-chunk requests, and memory
            // stays bounded whatever the layer size (A-2).
            use_monolithic_push: true,
            connect_timeout: Some(Duration::from_secs(10)),
            read_timeout: Some(Duration::from_secs(60)),
            ..ClientConfig::default()
        });
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| ArtifactError::Network(format!("building the HTTP client: {e}")))?;
        Ok(OciRegistry {
            client,
            http,
            config,
            insecure,
            credentials: Mutex::new(BTreeMap::new()),
            tokens: Mutex::new(BTreeMap::new()),
            chunk_bytes: None,
        })
    }

    /// \[K-14\] Uploads every blob in requests of at most `bytes` bytes (at least one): a POST
    /// that opens the upload, a PATCH per chunk, and a PUT with the digest that closes it. For a
    /// registry that refuses a larger request, as hushrun's does (16 MiB).
    pub fn chunked(mut self, bytes: u64) -> OciRegistry {
        self.chunk_bytes = Some(usize::try_from(bytes.max(1)).unwrap_or(usize::MAX));
        self
    }

    /// \[B-70\] Uses `credential` for `host`, whatever the Docker configuration says of it.
    pub fn give(&self, host: &str, credential: Credential) {
        self.credentials
            .lock()
            .expect("lock")
            .insert(host.to_string(), credential);
    }

    fn credential(&self, host: &str) -> Result<Credential, ArtifactError> {
        if let Some(found) = self.credentials.lock().expect("lock").get(host) {
            return Ok(found.clone());
        }
        let found = self.config.credential(host)?;
        self.credentials
            .lock()
            .expect("lock")
            .insert(host.to_string(), found.clone());
        Ok(found)
    }

    fn auth(&self, host: &str) -> Result<RegistryAuth, ArtifactError> {
        Ok(match self.credential(host)? {
            Credential::Anonymous => RegistryAuth::Anonymous,
            Credential::Basic { username, password } => RegistryAuth::Basic(username, password),
        })
    }

    /// The reference `oci-client` uses, with the credential stored for its host.
    async fn reference(
        &self,
        repository: &Repository,
        reference: &str,
    ) -> Result<(Reference, RegistryAuth), ArtifactError> {
        let auth = self.auth(&repository.host)?;
        let image = if reference.starts_with("sha256:") {
            Reference::with_digest(
                repository.host.clone(),
                repository.path.clone(),
                reference.to_string(),
            )
        } else {
            Reference::with_tag(
                repository.host.clone(),
                repository.path.clone(),
                reference.to_string(),
            )
        };
        self.client
            .store_auth_if_needed(image.resolve_registry(), &auth)
            .await;
        Ok((image, auth))
    }

    fn base_url(&self, host: &str) -> String {
        let scheme = if self.insecure.iter().any(|h| h == host) {
            "http"
        } else {
            "https"
        };
        // Docker Hub's API lives on registry-1.docker.io.
        let api = if host == "docker.io" {
            "registry-1.docker.io"
        } else {
            host
        };
        format!("{scheme}://{api}")
    }

    /// Sends a request built by `build`, answering an authentication challenge once with a
    /// token for `scopes` (or basic credentials).
    async fn send(
        &self,
        host: &str,
        scopes: &[String],
        build: impl Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ArtifactError> {
        let key = (host.to_string(), scopes.join(" "));
        let cached = self.tokens.lock().expect("lock").get(&key).cloned();
        let mut request = build(&self.http);
        if let Some(token) = &cached {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.map_err(|e| network(host, &e))?;
        if response.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Ok(response);
        }
        let challenge = response
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .map(Challenge::parse)
            .ok_or_else(|| ArtifactError::Denied {
                host: host.to_string(),
                detail: "unauthorized, without an authentication challenge".into(),
            })?;
        let credential = self.credential(host)?;
        let request = match challenge {
            Challenge::Basic => match &credential {
                Credential::Basic { username, password } => {
                    build(&self.http).basic_auth(username, Some(password))
                }
                Credential::Anonymous => {
                    return Err(ArtifactError::Denied {
                        host: host.to_string(),
                        detail: "the registry wants credentials and none are configured".into(),
                    });
                }
            },
            Challenge::Bearer { realm, service } => {
                let token = self
                    .token(host, &realm, service.as_deref(), scopes, &credential)
                    .await?;
                self.tokens.lock().expect("lock").insert(key, token.clone());
                build(&self.http).bearer_auth(token)
            }
        };
        request.send().await.map_err(|e| network(host, &e))
    }

    /// \[K-14\] Uploads `stream` (the blob `digest`, `size` bytes) in requests of at most `chunk`
    /// bytes: POST `/blobs/uploads/`, a PATCH per chunk at the offset the upload has reached, then
    /// PUT `?digest=`. The bytes are hashed on the way: a blob that isn't its digest or its size
    /// is never closed, and the open upload expires on the registry.
    async fn push_chunked(
        &self,
        repository: &Repository,
        digest: &Digest,
        size: u64,
        mut stream: BlobStream,
        chunk: usize,
    ) -> Result<(), ArtifactError> {
        let host = repository.host.as_str();
        let what = repository.at_digest(digest);
        let base = self.base_url(host);
        let scopes = [format!("repository:{}:pull,push", repository.path)];
        let start = format!("{base}/v2/{}/blobs/uploads/", repository.path);
        let response = self
            .send(host, &scopes, |http| {
                http.post(&start).header("content-length", "0")
            })
            .await?;
        let mut location = match response.status().as_u16() {
            202 => upload_location(&base, &response, &what)?,
            status => {
                return Err(
                    upload_refused(host, &what, "opening its upload", status, response).await,
                );
            }
        };
        let mut hasher = Sha256::new();
        let mut offset = 0u64;
        let mut buffer = bytes::BytesMut::new();
        let mut ended = false;
        while !ended {
            match stream.next().await {
                Some(Ok(bytes)) => buffer.extend_from_slice(&bytes),
                Some(Err(e)) => {
                    return Err(ArtifactError::Invalid(format!("reading {what}: {e}")));
                }
                None => ended = true,
            }
            while buffer.len() >= chunk || (ended && !buffer.is_empty()) {
                let piece = buffer.split_to(chunk.min(buffer.len())).freeze();
                hasher.update(&piece);
                let end = offset + piece.len() as u64 - 1;
                let at = location.clone();
                let response = self
                    .send(host, &scopes, |http| {
                        http.patch(&at)
                            .header("content-type", "application/octet-stream")
                            .header("content-range", format!("{offset}-{end}"))
                            .body(piece.clone())
                    })
                    .await?;
                match response.status().as_u16() {
                    202 => location = upload_location(&base, &response, &what)?,
                    status => {
                        return Err(upload_refused(
                            host,
                            &what,
                            &format!("uploading bytes {offset}-{end}"),
                            status,
                            response,
                        )
                        .await);
                    }
                }
                offset = end + 1;
            }
        }
        let actual = Digest::from_bytes(hasher.finalize().into());
        if actual != *digest || offset != size {
            return Err(ArtifactError::DigestMismatch {
                what: format!("blob {digest} ({offset} of {size} bytes)"),
                expected: digest.to_string(),
                actual: actual.to_string(),
            });
        }
        let close = format!(
            "{location}{}digest={digest}",
            if location.contains('?') { '&' } else { '?' }
        );
        let response = self
            .send(host, &scopes, |http| {
                http.put(&close).header("content-length", "0")
            })
            .await?;
        match response.status().as_u16() {
            201 => Ok(()),
            status => {
                Err(upload_refused(host, &what, "closing its upload", status, response).await)
            }
        }
    }

    async fn token(
        &self,
        host: &str,
        realm: &str,
        service: Option<&str>,
        scopes: &[String],
        credential: &Credential,
    ) -> Result<String, ArtifactError> {
        let mut query: Vec<(&str, &str)> = scopes.iter().map(|s| ("scope", s.as_str())).collect();
        if let Some(service) = service {
            query.push(("service", service));
        }
        let mut request = self.http.get(realm).query(&query);
        if let Credential::Basic { username, password } = credential {
            request = request.basic_auth(username, Some(password));
        }
        let response = request.send().await.map_err(|e| network(host, &e))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ArtifactError::Denied {
                host: host.to_string(),
                detail: format!(
                    "the token service refused {} (HTTP {status})",
                    scopes.join(", ")
                ),
            });
        }
        #[derive(serde::Deserialize)]
        struct TokenResponse {
            #[serde(default)]
            token: Option<String>,
            #[serde(default)]
            access_token: Option<String>,
        }
        let body: TokenResponse = response.json().await.map_err(|e| {
            ArtifactError::Network(format!(
                "{host}: unexpected token response: {}",
                e.without_url()
            ))
        })?;
        body.token.or(body.access_token).ok_or_else(|| {
            ArtifactError::Network(format!("{host}: the token response has no token"))
        })
    }
}

/// A `WWW-Authenticate` challenge.
#[derive(Debug, PartialEq, Eq)]
enum Challenge {
    Basic,
    Bearer {
        realm: String,
        service: Option<String>,
    },
}

impl Challenge {
    fn parse(header: &str) -> Challenge {
        let (scheme, params) = header.split_once(' ').unwrap_or((header, ""));
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Challenge::Basic;
        }
        let mut realm = String::new();
        let mut service = None;
        // key="value" pairs separated by commas; values may contain commas inside quotes.
        let mut rest = params.trim();
        while let Some((key, after)) = rest.split_once('=') {
            let key = key.trim().trim_start_matches(',').trim();
            let after = after.trim_start();
            let (value, next) = if let Some(quoted) = after.strip_prefix('"') {
                match quoted.split_once('"') {
                    Some((value, next)) => (value, next),
                    None => (quoted, ""),
                }
            } else {
                after.split_once(',').unwrap_or((after, ""))
            };
            match key {
                "realm" => realm = value.to_string(),
                "service" => service = Some(value.to_string()),
                _ => {}
            }
            rest = next.trim_start().trim_start_matches(',');
        }
        Challenge::Bearer { realm, service }
    }
}

/// \[K-14\] Where an upload goes on: the answer's `Location`, made absolute against `base`.
fn upload_location(
    base: &str,
    response: &reqwest::Response,
    what: &str,
) -> Result<String, ArtifactError> {
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            ArtifactError::Network(format!("{what}: the registry named no upload location"))
        })?;
    Ok(
        if location.starts_with("http://") || location.starts_with("https://") {
            location.to_string()
        } else {
            format!("{base}/{}", location.trim_start_matches('/'))
        },
    )
}

/// \[K-14\] The error an upload's request that wasn't taken becomes: the registry's own words,
/// the first 300 characters of them.
async fn upload_refused(
    host: &str,
    what: &str,
    doing: &str,
    status: u16,
    response: reqwest::Response,
) -> ArtifactError {
    let text = response.text().await.unwrap_or_default();
    let said: String = text.chars().take(300).collect();
    match status {
        401 | 403 => ArtifactError::Denied {
            host: host.to_string(),
            detail: format!("{what}: {doing}: HTTP {status} {said}"),
        },
        _ => ArtifactError::Network(format!("{host}: {what}: {doing}: HTTP {status}: {said}")),
    }
}

fn network(host: &str, err: &reqwest::Error) -> ArtifactError {
    ArtifactError::Network(format!("{host}: {}", strip_url(err)))
}

fn strip_url(err: &reqwest::Error) -> String {
    let mut text = err.to_string();
    if let Some(url) = err.url() {
        text = text.replace(url.as_str(), "");
    }
    text
}

/// Maps an `oci-client` error; messages carry hosts and statuses, never credentials.
fn map(host: &str, what: &str, err: OciDistributionError) -> ArtifactError {
    use OciDistributionError as E;
    match err {
        E::AuthenticationFailure(detail) => ArtifactError::Denied {
            host: host.to_string(),
            detail,
        },
        E::UnauthorizedError { .. } => ArtifactError::Denied {
            host: host.to_string(),
            detail: format!("{what}: not authorized"),
        },
        E::ImageManifestNotFoundError(_) => ArtifactError::NotFound(what.to_string()),
        E::ServerError { code: 404, .. } => ArtifactError::NotFound(what.to_string()),
        E::ServerError {
            code: 401 | 403,
            message,
            ..
        } => ArtifactError::Denied {
            host: host.to_string(),
            detail: format!("{what}: {message}"),
        },
        E::RegistryError { envelope, .. }
            if envelope.errors.iter().any(|e| {
                matches!(
                    e.code,
                    oci_client::errors::OciErrorCode::Denied
                        | oci_client::errors::OciErrorCode::Unauthorized
                )
            }) =>
        {
            ArtifactError::Denied {
                host: host.to_string(),
                detail: format!("{what}: denied"),
            }
        }
        E::RegistryError { envelope, .. }
            if envelope.errors.iter().any(|e| {
                matches!(
                    e.code,
                    oci_client::errors::OciErrorCode::ManifestUnknown
                        | oci_client::errors::OciErrorCode::BlobUnknown
                        | oci_client::errors::OciErrorCode::NameUnknown
                )
            }) =>
        {
            ArtifactError::NotFound(what.to_string())
        }
        E::UnsupportedMediaTypeError(media_type) => ArtifactError::Unsupported(media_type),
        E::RequestError(e) => ArtifactError::Network(format!("{host}: {what}: {}", strip_url(&e))),
        other => ArtifactError::Network(format!("{host}: {what}: {other}")),
    }
}

#[async_trait]
impl Registry for OciRegistry {
    async fn manifest(
        &self,
        repository: &Repository,
        reference: &str,
    ) -> Result<Manifest, ArtifactError> {
        let (image, auth) = self.reference(repository, reference).await?;
        let what = format!("{repository}:{reference}");
        let (bytes, _) = self
            .client
            .pull_manifest_raw(&image, &auth, &MANIFEST_TYPES)
            .await
            .map_err(|e| map(&repository.host, &what, e))?;
        let media_type = media_type_of(&bytes)
            .ok_or_else(|| ArtifactError::Unsupported(format!("{what}: no mediaType")))?;
        Ok(Manifest {
            media_type,
            bytes: bytes.to_vec(),
        })
    }

    async fn head(
        &self,
        repository: &Repository,
        reference: &str,
    ) -> Result<Option<Digest>, ArtifactError> {
        let (image, auth) = self.reference(repository, reference).await?;
        let what = format!("{repository}:{reference}");
        match self.client.fetch_manifest_digest(&image, &auth).await {
            Ok(digest) => Digest::parse(&digest)
                .map(Some)
                .map_err(|_| ArtifactError::Unsupported(format!("{what}: digest {digest}"))),
            Err(e) => match map(&repository.host, &what, e) {
                ArtifactError::NotFound(_) => Ok(None),
                other => Err(other),
            },
        }
    }

    async fn blob_exists(
        &self,
        repository: &Repository,
        digest: &Digest,
    ) -> Result<bool, ArtifactError> {
        let (image, _) = self.reference(repository, "latest").await?;
        self.client
            .blob_exists(&image, &digest.to_string())
            .await
            .map_err(|e| map(&repository.host, &repository.at_digest(digest), e))
    }

    async fn mount(
        &self,
        from: &Repository,
        to: &Repository,
        digest: &Digest,
    ) -> Result<Mount, ArtifactError> {
        let url = format!(
            "{}/v2/{}/blobs/uploads/?mount={digest}&from={}",
            self.base_url(&to.host),
            to.path,
            from.path
        );
        // The token must cover pulling from the source as well (A-2).
        let scopes = [
            format!("repository:{}:pull,push", to.path),
            format!("repository:{}:pull", from.path),
        ];
        let response = self
            .send(&to.host, &scopes, |http| {
                http.post(&url).header("content-length", "0")
            })
            .await?;
        match response.status().as_u16() {
            201 => Ok(Mount::Mounted),
            // Declined: an upload session was opened instead; it expires unused.
            202 => Ok(Mount::Declined),
            401 | 403 => Err(ArtifactError::Denied {
                host: to.host.clone(),
                detail: format!("mounting {digest} from {from} into {to}"),
            }),
            // Some registries refuse mounts they don't support; uploading still works.
            404 | 405 | 501 => Ok(Mount::Declined),
            status => Err(ArtifactError::Network(format!(
                "{}: mounting {digest} into {to}: HTTP {status}",
                to.host
            ))),
        }
    }

    async fn copy_blob(
        &self,
        from: &Repository,
        to: &Repository,
        blob: &Descriptor,
    ) -> Result<(), ArtifactError> {
        let (source, _) = self.reference(from, "latest").await?;
        let (destination, _) = self.reference(to, "latest").await?;
        let descriptor = OciDescriptor {
            media_type: blob.media_type.clone(),
            digest: blob.digest.to_string(),
            size: i64::try_from(blob.size).unwrap_or(i64::MAX),
            urls: None,
            annotations: None,
            ..OciDescriptor::default()
        };
        let what = from.at_digest(&blob.digest);
        let pulled = self
            .client
            .pull_blob_stream(&source, &descriptor)
            .await
            .map_err(|e| map(&from.host, &what, e))?;
        let verdict = Arc::new(Mutex::new(None));
        let stream = verified(pulled.stream, blob.digest, blob.size, verdict.clone());
        let pushed = self
            .client
            .push_blob_stream(
                &destination,
                stream,
                &blob.digest.to_string(),
                Some(blob.size),
            )
            .await;
        if let Some(mismatch) = verdict.lock().expect("lock").take() {
            return Err(mismatch);
        }
        pushed
            .map(|_| ())
            .map_err(|e| map(&to.host, &to.at_digest(&blob.digest), e))
    }

    async fn push_blob(
        &self,
        repository: &Repository,
        digest: &Digest,
        size: u64,
        stream: BlobStream,
    ) -> Result<(), ArtifactError> {
        if let Some(chunk) = self.chunk_bytes {
            return self
                .push_chunked(repository, digest, size, stream, chunk)
                .await;
        }
        let (destination, _) = self.reference(repository, "latest").await?;
        let verdict = Arc::new(Mutex::new(None));
        let stream = verified(stream, *digest, size, verdict.clone());
        let pushed = self
            .client
            .push_blob_stream(&destination, stream, &digest.to_string(), Some(size))
            .await;
        if let Some(mismatch) = verdict.lock().expect("lock").take() {
            return Err(mismatch);
        }
        pushed
            .map(|_| ())
            .map_err(|e| map(&repository.host, &repository.at_digest(digest), e))
    }

    async fn pull_blob(
        &self,
        repository: &Repository,
        digest: &Digest,
        most: u64,
    ) -> Result<Vec<u8>, ArtifactError> {
        let mut bytes = Vec::new();
        self.pull_blob_to(repository, digest, most, &mut bytes)
            .await?;
        Ok(bytes)
    }

    async fn pull_blob_to(
        &self,
        repository: &Repository,
        digest: &Digest,
        most: u64,
        out: &mut (dyn std::io::Write + Send),
    ) -> Result<u64, ArtifactError> {
        let (source, _) = self.reference(repository, "latest").await?;
        let descriptor = OciDescriptor {
            media_type: "application/octet-stream".into(),
            digest: digest.to_string(),
            size: 0,
            urls: None,
            annotations: None,
            ..OciDescriptor::default()
        };
        let what = repository.at_digest(digest);
        let pulled = self
            .client
            .pull_blob_stream(&source, &descriptor)
            .await
            .map_err(|e| map(&repository.host, &what, e))?;
        let mut stream = pulled.stream;
        let mut hasher = Sha256::new();
        let mut written = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ArtifactError::Network(format!("{what}: {e}")))?;
            written += chunk.len() as u64;
            if written > most {
                return Err(ArtifactError::Invalid(format!(
                    "{what} is larger than {most} bytes"
                )));
            }
            hasher.update(&chunk);
            out.write_all(&chunk)
                .map_err(|e| ArtifactError::Invalid(format!("writing {what}: {e}")))?;
        }
        let actual = Digest::from_bytes(hasher.finalize().into());
        if actual != *digest {
            return Err(ArtifactError::DigestMismatch {
                what,
                expected: digest.to_string(),
                actual: actual.to_string(),
            });
        }
        Ok(written)
    }

    async fn referrers(
        &self,
        repository: &Repository,
        digest: &Digest,
    ) -> Result<Option<Vec<u8>>, ArtifactError> {
        let url = format!(
            "{}/v2/{}/referrers/{digest}",
            self.base_url(&repository.host),
            repository.path
        );
        let scopes = [format!("repository:{}:pull", repository.path)];
        let response = self
            .send(&repository.host, &scopes, |http| {
                http.get(&url).header("accept", crate::registry::OCI_INDEX)
            })
            .await?;
        let status = response.status().as_u16();
        let index = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.starts_with(crate::registry::OCI_INDEX));
        match status {
            200 if index => Ok(Some(
                response
                    .bytes()
                    .await
                    .map_err(|e| network(&repository.host, &e))?
                    .to_vec(),
            )),
            // No such API: the distribution spec's 404 (registry:2), or a registry that knows no
            // such route (405). Referrers live under the tag schema's tag.
            404 | 405 => Ok(None),
            // Anything else (a refusal, a server's error, a page that isn't an index) is an
            // error, never "no API": writing the tag schema's index then could hide a referrer.
            _ => Err(ArtifactError::Invalid(format!(
                "{}: the referrers of {digest} answered {status}{}",
                repository.host,
                if status == 200 {
                    " with no image index"
                } else {
                    ""
                }
            ))),
        }
    }

    async fn check_access(&self, repository: &Repository) -> Result<(), ArtifactError> {
        let url = format!(
            "{}/v2/{}/tags/list?n=1",
            self.base_url(&repository.host),
            repository.path
        );
        let scopes = [format!("repository:{}:pull", repository.path)];
        let response = self
            .send(&repository.host, &scopes, |http| http.get(&url))
            .await?;
        match response.status().as_u16() {
            200 | 404 => Ok(()),
            401 | 403 => Err(ArtifactError::Denied {
                host: repository.host.clone(),
                detail: format!("reading {repository}"),
            }),
            status => Err(ArtifactError::Network(format!(
                "{}: listing {repository}: HTTP {status}",
                repository.host
            ))),
        }
    }

    async fn push_manifest(
        &self,
        repository: &Repository,
        reference: &str,
        manifest: &Manifest,
    ) -> Result<Option<Digest>, ArtifactError> {
        let url = format!(
            "{}/v2/{}/manifests/{reference}",
            self.base_url(&repository.host),
            repository.path
        );
        let scopes = [format!("repository:{}:pull,push", repository.path)];
        let body = Bytes::from(manifest.bytes.clone());
        let response = self
            .send(&repository.host, &scopes, |http| {
                http.put(&url)
                    .header("content-type", &manifest.media_type)
                    .body(body.clone())
            })
            .await?;
        let status = response.status();
        let what = format!("{repository}:{reference}");
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ArtifactError::Denied {
                host: repository.host.clone(),
                detail: format!("pushing {what}"),
            });
        }
        // K-14: a registry that keeps a tag where it is (hushrun's: a fixed tag naming another
        // image, the moving one held by a newer run) says why in its error's message.
        if status == reqwest::StatusCode::CONFLICT {
            let text = response.text().await.unwrap_or_default();
            return Err(ArtifactError::Conflict {
                what,
                message: registry_message(&text),
            });
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ArtifactError::Network(format!(
                "{}: pushing {what}: HTTP {status}: {}",
                repository.host,
                text.chars().take(300).collect::<String>()
            )));
        }
        let reported = response
            .headers()
            .get("docker-content-digest")
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                Digest::parse(v).map_err(|_| ArtifactError::DigestMismatch {
                    what: format!("{what} (Docker-Content-Digest)"),
                    expected: manifest.digest().to_string(),
                    actual: v.to_string(),
                })
            })
            .transpose()?;
        Ok(reported)
    }
}

/// The first error's message of a registry's error body (`{"errors": [{"code", "message"}]}`),
/// else the body's first 300 characters.
fn registry_message(text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v["errors"][0]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| text.chars().take(300).collect())
}

/// Passes `stream` through, hashing it; at the end, a size or digest mismatch becomes an error
/// item (so the upload never completes) and is recorded in `verdict`.
fn verified(
    stream: futures_util::stream::BoxStream<'static, Result<Bytes, std::io::Error>>,
    expected: Digest,
    size: u64,
    verdict: Arc<Mutex<Option<ArtifactError>>>,
) -> impl Stream<Item = oci_client::errors::Result<Bytes>> + Send + 'static {
    let state = Arc::new(Mutex::new((Sha256::new(), 0u64)));
    let hashing = {
        let state = state.clone();
        stream.map(move |item| {
            let chunk = item.map_err(OciDistributionError::IoError)?;
            let mut guard = state.lock().expect("lock");
            guard.0.update(&chunk);
            guard.1 += chunk.len() as u64;
            Ok(chunk)
        })
    };
    let end = futures_util::stream::once(async move {
        let (hasher, length) = std::mem::take(&mut *state.lock().expect("lock"));
        let actual = Digest::from_bytes(hasher.finalize().into());
        if actual == expected && length == size {
            None
        } else {
            let mismatch = ArtifactError::DigestMismatch {
                what: format!("blob {expected} ({length} of {size} bytes)"),
                expected: expected.to_string(),
                actual: actual.to_string(),
            };
            *verdict.lock().expect("lock") = Some(mismatch.clone());
            Some(Err(OciDistributionError::IoError(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                mismatch.to_string(),
            ))))
        }
    })
    .filter_map(|item| async move { item });
    hashing.chain(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenges_parse() {
        assert_eq!(
            Challenge::parse(
                r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:a:pull,push""#
            ),
            Challenge::Bearer {
                realm: "https://auth.docker.io/token".into(),
                service: Some("registry.docker.io".into())
            }
        );
        assert_eq!(
            Challenge::parse(r#"Bearer realm="https://us-docker.pkg.dev/v2/token""#),
            Challenge::Bearer {
                realm: "https://us-docker.pkg.dev/v2/token".into(),
                service: None
            }
        );
        assert_eq!(
            Challenge::parse(r#"Basic realm="Registry""#),
            Challenge::Basic
        );
    }

    fn collect(
        digest: Digest,
        size: u64,
        chunks: Vec<&'static [u8]>,
    ) -> (Vec<Result<Bytes, String>>, Option<ArtifactError>) {
        let verdict = Arc::new(Mutex::new(None));
        let source =
            futures_util::stream::iter(chunks.into_iter().map(|c| Ok(Bytes::from_static(c))))
                .boxed();
        let items = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(
                verified(source, digest, size, verdict.clone())
                    .map(|r| r.map_err(|e| e.to_string()))
                    .collect::<Vec<_>>(),
            );
        let verdict = verdict.lock().unwrap().take();
        (items, verdict)
    }

    /// \[K-14\] A registry on the loopback that keeps blobs in memory, takes uploads as POST,
    /// PATCH and PUT, and answers 413 to any request whose body is larger than `most` bytes, as
    /// hushrun's does: every request's method and body size is logged.
    struct ChunkRegistry {
        server: hushrun_testkit::http::Server,
        log: Arc<Mutex<Vec<(String, usize)>>>,
        blobs: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    }

    impl ChunkRegistry {
        fn start(most: usize) -> ChunkRegistry {
            use hushrun_testkit::http::{Reply, Response, Server};
            let log = Arc::new(Mutex::new(Vec::new()));
            let blobs: Arc<Mutex<BTreeMap<String, Vec<u8>>>> = Arc::default();
            let uploads: Arc<Mutex<BTreeMap<String, Vec<u8>>>> = Arc::default();
            let (logged, held) = (log.clone(), blobs.clone());
            let server = Server::start(move |request| {
                logged
                    .lock()
                    .unwrap()
                    .push((request.method.clone(), request.body.len()));
                if request.body.len() > most {
                    return Reply::Send(Response::new(413));
                }
                let path = request.path.as_str();
                if path == "/v2/" {
                    return Reply::Send(Response::new(200));
                }
                if let Some((_, digest)) = path.split_once("/blobs/sha256:") {
                    let digest = format!("sha256:{digest}");
                    return Reply::Send(match held.lock().unwrap().get(&digest) {
                        Some(b) => Response::new(200)
                            .header("content-length", b.len().to_string())
                            .header("docker-content-digest", digest),
                        None => Response::new(404),
                    });
                }
                let Some((_, id)) = path.split_once("/blobs/uploads/") else {
                    return Reply::Send(Response::new(404));
                };
                let mut uploads = uploads.lock().unwrap();
                let at = |id: &str, len: usize| {
                    Response::new(202)
                        .header("location", format!("/v2/acme/shop/blobs/uploads/{id}"))
                        .header("range", format!("0-{}", len.saturating_sub(1)))
                };
                match request.method.as_str() {
                    "POST" => {
                        let id = format!("u{}", uploads.len());
                        if let Some(digest) = request.query("digest") {
                            held.lock()
                                .unwrap()
                                .insert(digest.into(), request.body.clone());
                            return Reply::Send(
                                Response::new(201)
                                    .header("location", format!("/v2/acme/shop/blobs/{digest}"))
                                    .header("docker-content-digest", digest),
                            );
                        }
                        uploads.insert(id.clone(), request.body.clone());
                        Reply::Send(at(&id, request.body.len()))
                    }
                    "PATCH" => {
                        let Some(bytes) = uploads.get_mut(id) else {
                            return Reply::Send(Response::new(404));
                        };
                        if let Some(range) = request.header("content-range") {
                            let start: usize = range.split('-').next().unwrap().parse().unwrap();
                            if start != bytes.len() {
                                return Reply::Send(Response::new(416));
                            }
                        }
                        bytes.extend_from_slice(&request.body);
                        let len = bytes.len();
                        Reply::Send(at(id, len))
                    }
                    "PUT" => {
                        let Some(mut bytes) = uploads.remove(id) else {
                            return Reply::Send(Response::new(404));
                        };
                        bytes.extend_from_slice(&request.body);
                        let digest = request.query("digest").unwrap().to_string();
                        if Digest::of(&bytes).to_string() != digest {
                            return Reply::Send(Response::new(400));
                        }
                        held.lock().unwrap().insert(digest.clone(), bytes);
                        Reply::Send(
                            Response::new(201)
                                .header("location", format!("/v2/acme/shop/blobs/{digest}"))
                                .header("docker-content-digest", digest),
                        )
                    }
                    _ => Reply::Send(Response::new(405)),
                }
            });
            ChunkRegistry { server, log, blobs }
        }

        fn registry(&self) -> OciRegistry {
            let host = self.server.addr().to_string();
            OciRegistry::new(DockerConfig::default(), vec![host]).unwrap()
        }

        fn repository(&self) -> Repository {
            Repository {
                host: self.server.addr().to_string(),
                path: "acme/shop".into(),
            }
        }
    }

    fn push(
        registry: &OciRegistry,
        repository: &Repository,
        bytes: Vec<u8>,
    ) -> Result<(), ArtifactError> {
        let digest = Digest::of(&bytes);
        let size = bytes.len() as u64;
        // A stream of pieces that don't line up with the chunks.
        let pieces: Vec<Result<Bytes, std::io::Error>> = bytes
            .chunks(3 << 20)
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(registry.push_blob(
                repository,
                &digest,
                size,
                futures_util::stream::iter(pieces).boxed(),
            ))
    }

    // covers: K-14
    #[test]
    fn a_chunked_push_never_sends_more_than_a_chunk_and_others_push_whole() {
        const MOST: usize = 16 << 20;
        let fake = ChunkRegistry::start(MOST);
        let repository = fake.repository();
        // 40 MiB and a bit: three chunks of at most 16 MiB, each at the offset reached.
        let big: Vec<u8> = (0..(40 << 20) + 5).map(|i| (i % 251) as u8).collect();
        let digest = Digest::of(&big).to_string();
        push(
            &fake.registry().chunked(MOST as u64),
            &repository,
            big.clone(),
        )
        .unwrap();
        assert_eq!(fake.blobs.lock().unwrap().get(&digest), Some(&big));
        let log = std::mem::take(&mut *fake.log.lock().unwrap());
        let patches: Vec<usize> = log
            .iter()
            .filter(|(m, _)| m == "PATCH")
            .map(|(_, n)| *n)
            .collect();
        assert_eq!(patches, [MOST, MOST, (8 << 20) + 5], "{log:?}");
        assert!(log.iter().all(|(_, n)| *n <= MOST), "{log:?}");
        // Bytes that aren't the digest are never closed: no PUT.
        let other = vec![7u8; 100];
        let refused = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fake.registry().chunked(64).push_blob(
                &repository,
                &Digest::of(b"something else"),
                100,
                futures_util::stream::iter([Ok(Bytes::from(other))]).boxed(),
            ));
        assert!(
            matches!(refused, Err(ArtifactError::DigestMismatch { .. })),
            "{refused:?}"
        );
        let log = std::mem::take(&mut *fake.log.lock().unwrap());
        assert!(!log.iter().any(|(m, _)| m == "PUT"), "{log:?}");
        // Any other registry: one request carries the whole blob, as before.
        let small = vec![1u8; 4096];
        push(&fake.registry(), &repository, small.clone()).unwrap();
        let log = std::mem::take(&mut *fake.log.lock().unwrap());
        assert!(!log.iter().any(|(m, _)| m == "PATCH"), "{log:?}");
        assert!(log.iter().any(|(_, n)| *n == small.len()), "{log:?}");
        assert!(
            fake.blobs
                .lock()
                .unwrap()
                .contains_key(&Digest::of(&small).to_string())
        );
    }

    // covers: A-2
    #[test]
    fn streamed_blobs_are_verified_while_copying() {
        let digest = Digest::of(b"hello world");
        let (items, verdict) = collect(digest, 11, vec![b"hello ", b"world"]);
        assert!(items.iter().all(Result::is_ok) && items.len() == 2);
        assert!(verdict.is_none());
        // Other bytes, or fewer of them, end the stream with an error.
        let (items, verdict) = collect(digest, 11, vec![b"hello ", b"there"]);
        assert!(items.last().unwrap().is_err());
        assert!(matches!(
            verdict,
            Some(ArtifactError::DigestMismatch { .. })
        ));
        let (items, _) = collect(digest, 11, vec![b"hello"]);
        assert!(items.last().unwrap().is_err());
    }
}

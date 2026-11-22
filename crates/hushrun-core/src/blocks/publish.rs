//! B-70: publishing a baked store's image into a backend's registry, `POST v0/build/images`.
//!
//! A backend may serve a read-only OCI registry built from the packs it already holds (its pull
//! API is the OCI distribution spec's, under `/v2/`). Nobody pushes blobs there: CI on the
//! repository's default branch plans, builds and signs the image as it would for any registry
//! (B-65 to B-67), then describes it in one request, [`PublishRequest`]: each layer's files in
//! order (a pack by its block, any other file by its bytes in `blobs`), the image manifest, config
//! and signature it made, the moving tag's digest it read (`previous`), and its commit. The
//! backend reads each new layer from the bytes it holds with B-65's function, refuses a manifest
//! that names other digests, accepts only packs its repository's trusted scope serves, and moves
//! the moving tag by compare-and-set on `previous`, only to a newer run. The fixed tag never
//! moves: a store published before keeps its image, and the moving tag goes to that one.
//!
//! | Request | Body | Answer |
//! |---|---|---|
//! | `POST v0/build/images` | [`PublishRequest`] | [`PublishResponse`], or [`PublishRefusal`] |
//!
//! A server with a registry (optional) answers it; one without answers 404 as a missing route.
//! A refusal's `error.code` and what the client does are listed at [`PublishRefusal`]. Retries are
//! safe: the same `image` bytes are the same publish.
//!
//! Like the rest of this crate it does no IO.

use serde::{Deserialize, Serialize};

use super::{Base64, Hex32};

/// The path, under an org's base URL.
pub const IMAGES_PATH: &str = "v0/build/images";
/// The most bytes a publish's body may have.
pub const MOST_PUBLISH_BYTES: usize = 64 << 20;
/// The most bytes one file that isn't a pack may have (a blob).
pub const MOST_BLOB_BYTES: usize = 8 << 20;
/// The most layers an image published so may have.
pub const MOST_LAYERS: usize = 8;
/// The most files a publish may name, in all its layers.
pub const MOST_FILES: usize = 200_000;

/// `POST v0/build/images`: CI publishes an image of its baked store into the backend's registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishRequest {
    /// The store's platform in a tag's words (`linux-x64`, B-66).
    pub platform: String,
    /// The store's manifest identity (B-63), `baked.json`'s `manifest`, which the fixed tag names.
    pub manifest: Hex32,
    /// The image manifest's bytes, exactly as the client made them: an OCI image manifest of
    /// `config` and one uncompressed tar layer per entry of `layers`, each annotated
    /// `dev.hushrun.layer` with its role, with no `subject`.
    pub image: Base64,
    /// The image config's bytes: the platform's OS and CPU, one diff id per layer, and the
    /// `dev.hushrun.baked-image` label naming `manifest` and `platform`.
    pub config: Base64,
    /// Each layer, bottom first, roles in the order deps, workspace, recent, each once; an empty
    /// layer is left out. The top one holds `hushrun/baked.json`.
    pub layers: Vec<PublishLayer>,
    /// The bytes of every file that isn't a pack. One the server holds already (same SHA-256,
    /// same org) may be left out; a refusal `missing_blobs` names those to send.
    #[serde(default)]
    pub blobs: Vec<Base64>,
    /// The image's signature (B-67), when it is signed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<PublishSignature>,
    /// The moving tag's image digest (`sha256:<hex>`) the client read before it planned, `None`
    /// when the tag named nothing. The compare-and-set's expected value.
    pub previous: Option<String>,
    /// The workspace's commit, which must be the one the job's ID token names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// One layer of a [`PublishRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishLayer {
    /// `deps`, `workspace` or `recent` (B-65).
    pub role: String,
    /// Exactly the layer's files, in any order (the layer's bytes sort them, B-65).
    pub files: Vec<PublishFile>,
}

/// A file of a [`PublishLayer`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishFile {
    /// Its path in the image, under `hushrun/`.
    pub path: String,
    /// Its size in bytes.
    pub size: u64,
    /// The SHA-256 of its bytes, 64 lowercase hex characters (no `sha256:` prefix).
    pub sha256: Hex32,
    /// For a pack, at `hushrun/packs/<address>.pack`: the block, as `baked.json`'s row names it.
    /// Any other file names none and isn't under `hushrun/packs/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<PublishPack>,
}

/// The block a pack of a [`PublishFile`] is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishPack {
    /// Its kind (`rustc/1`, `go/1`, `cc/1`).
    pub kind: String,
    /// Its address.
    pub address: Hex32,
}

/// An image's signature (B-67), as a registry keeps it: a referrer manifest whose `subject` is
/// the image, with the empty config and the bundle as its one layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishSignature {
    /// The referrer manifest's bytes.
    pub manifest: Base64,
    /// The Sigstore bundle's bytes.
    pub bundle: Base64,
}

/// What a publish did, for its 200 answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishState {
    /// The moving tag moved to `digest`, from `previous`.
    Published,
    /// The moving tag named `digest` already.
    Unchanged,
    /// A word this version doesn't know.
    #[serde(other)]
    Unknown,
}

/// The 200 answer to a [`PublishRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishResponse {
    pub state: PublishState,
    /// The image the moving tag names now: the one sent, or, when this store was published
    /// before, that image (the fixed tag never moves).
    pub digest: String,
    /// The image the moving tag named before, when one.
    #[serde(default)]
    pub previous: Option<String>,
    /// The image's name, `<registry host>/<org>/<name>`.
    pub image: String,
    pub tags: PublishTags,
    /// The layers of `digest`, bottom first, as the server read them.
    #[serde(default)]
    pub layers: Vec<PublishedLayer>,
}

/// The tags of a [`PublishResponse`] (B-66).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishTags {
    pub fixed: String,
    pub latest: String,
}

/// A layer of a [`PublishResponse`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishedLayer {
    /// `sha256:<hex>`.
    pub digest: String,
    pub size: u64,
}

/// A publish's refusal, `{"error": {"code", "message"}, "current"?}`:
///
/// | Status, code | Meaning | The client |
/// |---|---|---|
/// | 400 `invalid` | the body breaks a rule | fails, with the message |
/// | 400 `missing_blobs` | `current.missing`: blobs (`sha256:<hex>`) to send | sends them, once |
/// | 401 | no credential | fails |
/// | 403 `not_publisher` | `current.word`: `default_branch`, or the trust rule's word | fails, saying why |
/// | 404 | the server has no registry, or the org doesn't share | fails |
/// | 409 `stale` | the moving tag moved; `current.digest` names the one there | reads the previous image again, plans again, sends again |
/// | 409 `not_newer` | a newer run moved the moving tag | stops: a newer bake won |
/// | 409 `duplicate` | another image of this store was published meanwhile | sends the same request again |
/// | 409 `name_taken` | another repository of the org has this name | fails |
/// | 413 | over [`MOST_PUBLISH_BYTES`] | fails |
/// | 422 `missing_blocks` | `current.missing`: addresses not served in the trusted scope now | fails: a fresh bake fixes it |
/// | 422 `mismatch` | a layer read has another digest than the manifest names (`current.layers`), or a file's bytes differ | fails |
/// | 503 `busy`, `unavailable` | another request is publishing it, or a store didn't answer (`Retry-After`) | sends the same request again |
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishRefusal {
    pub error: RefusalWords,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<PublishCurrent>,
}

/// A refusal's code and message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefusalWords {
    pub code: String,
    #[serde(default)]
    pub message: String,
}

/// What a refusal says is so now. Each member is set by the codes that name it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishCurrent {
    /// `stale`: the image the moving tag names now (`None`: none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// `missing_blobs`: the blobs to send; `missing_blocks`: the addresses not served.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    /// `not_publisher`: why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word: Option<String>,
    /// `mismatch`: the layer digests the server read, bottom first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<String>,
}

/// B-70: the name a backend's registry gives the repository `repository` (`github.com/acme/shop`,
/// any case): its path's last part, lowercase, when that is one OCI path component
/// (`[a-z0-9]+([._-][a-z0-9]+)*`, at most 100 bytes). The image is `<registry host>/<org>/<name>`.
pub fn image_name(repository: &str) -> Option<String> {
    let last = repository
        .trim_end_matches('/')
        .rsplit('/')
        .next()?
        .to_ascii_lowercase();
    valid_name(&last).then_some(last)
}

/// Whether `name` is one OCI path component, as [`image_name`] requires.
pub fn valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 100 {
        return false;
    }
    let mut after_separator = true;
    for b in name.bytes() {
        match b {
            b'a'..=b'z' | b'0'..=b'9' => after_separator = false,
            b'.' | b'_' | b'-' if !after_separator => after_separator = true,
            _ => return false,
        }
    }
    !after_separator
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: B-70
    #[test]
    fn a_publish_reads_and_writes_as_the_protocol_says() {
        let request = PublishRequest {
            platform: "linux-x64".into(),
            manifest: Hex32([1; 32]),
            image: Base64(b"{}".to_vec()),
            config: Base64(b"{\"os\":\"linux\"}".to_vec()),
            layers: vec![PublishLayer {
                role: "recent".into(),
                files: vec![
                    PublishFile {
                        path: format!("hushrun/packs/{}.pack", Hex32([2; 32])),
                        size: 3,
                        sha256: Hex32([3; 32]),
                        pack: Some(PublishPack {
                            kind: "rustc/1".into(),
                            address: Hex32([2; 32]),
                        }),
                    },
                    PublishFile {
                        path: "hushrun/baked.json".into(),
                        size: 2,
                        sha256: Hex32([4; 32]),
                        pack: None,
                    },
                ],
            }],
            blobs: vec![Base64(b"{}".to_vec())],
            signature: None,
            previous: None,
            commit: Some("a".repeat(40)),
        };
        let json: serde_json::Value = serde_json::to_value(&request).unwrap();
        assert_eq!(json["previous"], serde_json::Value::Null);
        assert_eq!(json["image"], "e30=");
        assert_eq!(json["layers"][0]["files"][0]["pack"]["kind"], "rustc/1");
        assert_eq!(json["layers"][0]["files"][0]["sha256"], "03".repeat(32));
        assert!(json["layers"][0]["files"][1].get("pack").is_none());
        assert!(json.get("signature").is_none());
        let back: PublishRequest = serde_json::from_value(json).unwrap();
        assert_eq!(back, request);

        let answer: PublishResponse = serde_json::from_str(
            r#"{"state":"published","digest":"sha256:ab","previous":null,"image":"registry.example/acme/shop","tags":{"fixed":"hushrun-linux-x64-0123456789abcdef","latest":"hushrun-linux-x64-latest"},"layers":[{"digest":"sha256:cd","size":7}]}"#,
        )
        .unwrap();
        assert_eq!(answer.state, PublishState::Published);
        assert_eq!(answer.layers[0].size, 7);
        let later: PublishResponse = serde_json::from_str(
            r#"{"state":"moved-by-a-later-version","digest":"sha256:ab","image":"r/a/b","tags":{"fixed":"f","latest":"l"},"extra":1}"#,
        )
        .unwrap();
        assert_eq!(later.state, PublishState::Unknown);

        let refusal: PublishRefusal = serde_json::from_str(
            r#"{"error":{"code":"stale","message":"m"},"current":{"digest":null}}"#,
        )
        .unwrap();
        assert_eq!(refusal.error.code, "stale");
        assert_eq!(refusal.current.unwrap().digest, None);
    }

    // covers: B-70
    #[test]
    fn a_repository_s_registry_name_is_its_last_part_lowercase() {
        assert_eq!(image_name("github.com/Acme/Shop").as_deref(), Some("shop"));
        assert_eq!(
            image_name("gitlab.com/g/sub/my-app.rs").as_deref(),
            Some("my-app.rs")
        );
        for bad in [
            "github.com/acme/a_",
            "github.com/acme/-x",
            "github.com/a/b..c",
            "",
        ] {
            assert_eq!(image_name(bad), None, "{bad}");
        }
        assert!(!valid_name(&"a".repeat(101)));
    }
}

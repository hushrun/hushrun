//! B-70: publishing a baked store's image into the backend's registry with one request,
//! `POST v0/build/images`, instead of pushing blobs (the registry is read-only to the outside).
//!
//! [`super::Relay::publish_image`] is the one loop that sends it and decides each answer: the
//! same bytes again for an answer that may pass (503 `busy` or `unavailable`, 409 `duplicate`, no
//! answer at all), since the same image is the same publish on the server; a new plan from the new
//! previous image once for 409 `stale`; a newer bake's win for 409 `not_newer`; and, on success,
//! the layer digests the backend computed from the bytes it holds compared with the ones this bake
//! computed, a difference failing loudly.

use std::sync::Arc;
use std::time::{Duration, Instant};

use hushrun_core::blocks::publish::MOST_PUBLISH_BYTES;
use hushrun_core::blocks::{self, PublishRefusal, PublishRequest, PublishResponse};
use hushrun_core::digest::Digest;

/// How long a publish may take in all, its retries among them.
const PUBLISH_WITHIN: Duration = Duration::from_secs(600);
/// How long one request may take: the server reads every new layer before it answers.
const REQUEST_WITHIN: Duration = Duration::from_secs(300);
/// How many times an answer that may pass (503, 429) is waited out.
const MOST_WAITS: usize = 12;
/// The longest wait between two sends of the same request.
const LONGEST_WAIT: Duration = Duration::from_secs(30);
/// How many times a request that got no answer is sent again.
const MOST_UNANSWERED: usize = 3;
/// How many times a 409 `duplicate` is sent again.
const MOST_DUPLICATES: usize = 2;

/// A publish ready to send: the request, and what this bake computed of the image it describes.
pub(crate) struct Prepared {
    pub(crate) request: PublishRequest,
    /// The image manifest's digest.
    pub(crate) digest: Digest,
    /// Each layer's digest and size, bottom first.
    pub(crate) layers: Vec<(Digest, u64)>,
}

/// What a publish came to.
#[derive(Debug)]
pub(crate) enum Sent {
    /// The moving tag names `answer.digest` now. `before`: this store was published before, and
    /// the moving tag went to that image rather than the one sent.
    Published {
        answer: PublishResponse,
        before: bool,
        /// The bytes of the last request sent.
        bytes: u64,
        /// What every send took: how many requests, all their bytes, and whether a 409 `stale`
        /// made the bake plan again on the image the backend named.
        tries: Tries,
    },
    /// A newer run moved the moving tag: nothing of this bake was published.
    NotNewer(String),
}

/// What a publish's sends came to, whatever the answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Tries {
    pub(crate) requests: u32,
    pub(crate) bytes: u64,
    pub(crate) replanned: bool,
}

/// What a stale publish's new plan starts from: the image the backend says the moving tag names
/// now (`None`: none).
pub(crate) type Stale = Option<String>;

impl super::Relay {
    /// B-70: sends the publish `prepare` makes (`prepare(None)` first; `prepare(Some(now))`
    /// after a 409 `stale`, `now` the image the moving tag names there, once) and decides each
    /// answer. `Err` is the line to fail with.
    pub(crate) fn publish_image(
        self: &Arc<Self>,
        prepare: &mut dyn FnMut(Option<Stale>) -> Result<Prepared, String>,
    ) -> Result<Sent, String> {
        let started = Instant::now();
        let mut prepared = prepare(None)?;
        let mut body = encode(&prepared.request)?;
        let mut planned_again = false;
        let mut duplicates = 0;
        let mut waits = 0;
        let mut unanswered = 0;
        let mut tries = Tries::default();
        loop {
            let left = PUBLISH_WITHIN.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Err(format!(
                    "the backend didn't publish the image within {} minutes",
                    PUBLISH_WITHIN.as_secs() / 60
                ));
            }
            tries.requests += 1;
            tries.bytes += body.len() as u64;
            let reply = self.runtime.block_on(self.client.post_raw(
                blocks::IMAGES_PATH,
                body.clone(),
                left.min(REQUEST_WITHIN),
                None,
            ));
            let reply = match reply {
                Ok(reply) => reply,
                Err(_) if unanswered + 1 < MOST_UNANSWERED => {
                    unanswered += 1;
                    std::thread::sleep(Duration::from_secs(1).min(left));
                    continue;
                }
                Err(failure) => {
                    return Err(format!(
                        "the backend didn't answer the publish: {}",
                        failure.line()
                    ));
                }
            };
            if (200..300).contains(&reply.status) {
                let answer: PublishResponse = serde_json::from_slice(&reply.body)
                    .map_err(|e| format!("the backend's answer to the publish isn't one: {e}"))?;
                return checked(&prepared, answer, body.len() as u64, tries);
            }
            let refusal = serde_json::from_slice::<PublishRefusal>(&reply.body).ok();
            let code = refusal
                .as_ref()
                .map(|r| r.error.code.clone())
                .unwrap_or_default();
            let message = refusal
                .as_ref()
                .map(|r| r.error.message.clone())
                .unwrap_or_default();
            let current = refusal.and_then(|r| r.current).unwrap_or_default();
            match (reply.status, code.as_str()) {
                (409, "stale") if !planned_again => {
                    planned_again = true;
                    tries.replanned = true;
                    prepared = prepare(Some(current.digest))?;
                    body = encode(&prepared.request)?;
                }
                (409, "stale") => {
                    return Err(format!(
                        "the moving tag moved again while this bake planned on it (it names {} now): another bake is publishing; this one stops",
                        current.digest.as_deref().unwrap_or("no image")
                    ));
                }
                (409, "not_newer") => {
                    return Ok(Sent::NotNewer(if message.is_empty() {
                        "a newer run moved the moving tag".into()
                    } else {
                        message
                    }));
                }
                // The same request again: the answer then names the image published meanwhile.
                (409, "duplicate") if duplicates < MOST_DUPLICATES => duplicates += 1,
                (503 | 429, _) if waits < MOST_WAITS => {
                    waits += 1;
                    let wait = reply
                        .retry_after
                        .unwrap_or(Duration::from_secs(5))
                        .min(LONGEST_WAIT)
                        .min(PUBLISH_WITHIN.saturating_sub(started.elapsed()));
                    std::thread::sleep(wait);
                }
                (status, code) => return Err(refused(status, code, &message, &current)),
            }
        }
    }
}

/// The request's bytes, refused past what the backend takes.
fn encode(request: &PublishRequest) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    if bytes.len() > MOST_PUBLISH_BYTES {
        return Err(format!(
            "the image's description is {} bytes, more than the {} a backend takes in one publish (too many hints or files)",
            bytes.len(),
            MOST_PUBLISH_BYTES
        ));
    }
    Ok(bytes)
}

/// A 200's answer, once the layers the backend read are the ones this bake computed: the same
/// digests and sizes, in the same order, for the image it sent. An answer naming another image
/// (this store's, published before) names that image's layers, which this bake didn't compute.
fn checked(
    prepared: &Prepared,
    answer: PublishResponse,
    bytes: u64,
    tries: Tries,
) -> Result<Sent, String> {
    let before = answer.digest != prepared.digest.to_string();
    if !before {
        let here: Vec<(String, u64)> = prepared
            .layers
            .iter()
            .map(|(digest, size)| (digest.to_string(), *size))
            .collect();
        let there: Vec<(String, u64)> = answer
            .layers
            .iter()
            .map(|l| (l.digest.clone(), l.size))
            .collect();
        if here != there {
            return Err(mismatch(&here, &there));
        }
    }
    Ok(Sent::Published {
        answer,
        before,
        bytes,
        tries,
    })
}

fn mismatch(here: &[(String, u64)], there: &[(String, u64)]) -> String {
    let words = |layers: &[(String, u64)]| {
        layers
            .iter()
            .map(|(d, s)| format!("{d} ({s} bytes)"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "THE BACKEND COMPUTED OTHER LAYERS THAN THIS BAKE for the same files: here {}; the backend read {}. The layer bytes are one function of the files on both sides (B-65), so one side has other bytes for a file than its row names, or another version of that function: report it",
        if here.is_empty() {
            "none".to_string()
        } else {
            words(here)
        },
        if there.is_empty() {
            "none".to_string()
        } else {
            words(there)
        },
    )
}

/// The line a refusal fails with.
fn refused(status: u16, code: &str, message: &str, current: &blocks::PublishCurrent) -> String {
    let said = if message.is_empty() {
        String::new()
    } else {
        format!(" ({message})")
    };
    match (status, code) {
        (401, _) => format!(
            "the backend took no credential for the publish{said}: only CI publishes there, with its own ID token (`permissions: id-token: write` on GitHub, `id_tokens: HUSHRUN_ID_TOKEN` on GitLab)"
        ),
        (403, "not_publisher") => format!(
            "only the repository's CI, with its own ID token, on its default branch, publishes to the backend's registry; this run doesn't ({}){said}. Elsewhere, write the image into a folder with --oci-dir, or push it to a registry of your own with --oci REPOSITORY",
            current.word.as_deref().unwrap_or("no reason given")
        ),
        (404 | 405, _) => format!(
            "the backend has no registry, or the org doesn't share build blocks (answered {status}){said}: publish to a registry of your own with --oci REPOSITORY, or into a folder with --oci-dir"
        ),
        (409, "name_taken") => format!(
            "another repository of the org has this image's name in the backend's registry{said}: an owner settles it"
        ),
        (413, _) => format!("the image's description is too large for the backend (413){said}"),
        (422, "missing_blocks") => format!(
            "the backend no longer serves {} of the blocks in the trusted scope (swept, withdrawn or suspended: {}){said}; bake again",
            current.missing.len(),
            current
                .missing
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ),
        (422, "mismatch") => format!(
            "THE BACKEND READ OTHER LAYERS THAN THE IMAGE NAMES{said}: it read {}. A file's bytes differ from its row, or the two sides make layers differently: report it",
            if current.layers.is_empty() {
                "files whose bytes differ from what the request says".to_string()
            } else {
                current.layers.join(", ")
            }
        ),
        (400, "missing_blobs") => format!(
            "the backend lacks the bytes of {} files this bake kept from the previous image without holding them{said}: bake again with --compact",
            current.missing.len()
        ),
        (400, _) => format!("the backend refused the publish as invalid{said}: report it"),
        (status, "") => format!("the backend answered the publish {status}{said}"),
        (status, code) => format!("the backend answered the publish {status} {code}{said}"),
    }
}

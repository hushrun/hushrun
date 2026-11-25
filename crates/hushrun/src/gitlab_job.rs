//! B-48, B-49: a GitLab CI job's part in sharing build blocks. Its credential is its ID token,
//! read from `HUSHRUN_ID_TOKEN` alone, and the key the job proves it is the job with.
//!
//! **The token** ([`read`]) is one of three things, decided here and nowhere else:
//!
//! | `HUSHRUN_ID_TOKEN` | [`Token`] | What the run does |
//! |---|---|---|
//! | unset or empty | `Absent` | shares nothing; says how to declare it where the org shares |
//! | not gitlab.com's, another audience (compared exactly, as the backend does), no job, expired, or no `CI_JOB_TOKEN` beside it | `Unusable` | shares nothing, and says why, once |
//! | gitlab.com's, for this backend's URL, alive, with `CI_JOB_TOKEN` | `Usable` | its build requests carry it, signed with the job's key, and its results lookup carries it beside its bearer |
//!
//! It goes to no server but the one its audience names: a repository's files can name any
//! backend, and a token sent elsewhere could be used at hushrun's for as long as the job's
//! timeout. Nothing here verifies its signature, which is the backend's to do: what is read is
//! only what decides whether to send it.
//!
//! **The key** ([`JobKey::of`]) is derived, never stored: an Ed25519 seed by HKDF-SHA256 from the
//! job's `CI_JOB_TOKEN`, salted with a fixed label and bound to the ID token's `job_id` and `jti`.
//! The backend binds a job to the first key that opens it and withdraws the job, deleting what it
//! wrote, on a request with another key (B-49), so the key must be the same in every `hushrun
//! run` of the job, whatever its temporary folder, current folder or container: a value every
//! step of the job is handed is that, and a file is not. A copied ID token alone doesn't give the
//! key; GitLab masks `CI_JOB_TOKEN` in logs and it stops working when the job ends. Without it the
//! run shares nothing: every request of a GitLab job, reads included, must carry the job's key,
//! and any other key would withdraw the job.

use std::ffi::OsString;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use hushrun_attest::keys::SigningKey;
use serde::Deserialize;
use zeroize::Zeroizing;

/// The variable a job's `id_tokens:` names (B-48): the only place the token is read from.
pub(crate) const VARIABLE: &str = "HUSHRUN_ID_TOKEN";

/// The issuer of gitlab.com's CI ID tokens, exactly. A self-managed instance is not taken.
pub(crate) const ISSUER: &str = "https://gitlab.com";

/// The job's own token, which GitLab gives every step of a job: what the job's key is made from.
pub(crate) const JOB_TOKEN: &str = "CI_JOB_TOKEN";

/// The salt of the job key's HKDF: a fixed label, so the key is for this use alone.
const KEY_LABEL: &str = "hushrun.gitlab-job-key/v1";

/// B-48: whether this process is in a GitLab CI job, for build blocks: GitLab as the provider
/// (`GITLAB_CI`, unless `HUSHRUN_PROVIDER` names another), or GitLab's own job variables
/// `CI_JOB_ID` and `CI_SERVER_URL` beside the job's ID token or its job token, whatever
/// `HUSHRUN_PROVIDER` says: a container a step starts with them handed in, a job that names
/// another provider. There only the job's ID token counts: an agent token reads nothing of the
/// shared store. The one definition every part of the build path asks. A container handed an
/// agent token and none of these is a sandbox to hushrun, as to the backend.
pub(crate) fn is_job(env: &hushrun_ci::Env) -> bool {
    let set = |name: &str| env.get(name).is_some_and(|v| !v.trim().is_empty());
    hushrun_ci::detect(env) == hushrun_core::run::Provider::Gitlab
        || (set("CI_JOB_ID") && set("CI_SERVER_URL") && (set(VARIABLE) || set(JOB_TOKEN)))
}

/// Whether build blocks take this run for a CI job's: [`hushrun_ci::in_ci`], or a GitLab job
/// ([`is_job`]).
pub(crate) fn in_ci(env: &hushrun_ci::Env) -> bool {
    hushrun_ci::in_ci(env) || is_job(env)
}

/// What `HUSHRUN_ID_TOKEN` is, for the backend at `audience`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Token {
    /// It isn't set.
    Absent,
    /// It is set, and can't be sent: why, as a line ends with it.
    Unusable(String),
    /// It may be sent to the backend at its audience, with the job's key.
    Usable(Job),
}

/// A GitLab job that may share (B-48, B-49): its ID token and its key.
#[derive(Clone, Debug)]
pub struct Job {
    pub(crate) token: IdToken,
    pub(crate) key: Arc<JobKey>,
}

impl PartialEq for Job {
    fn eq(&self, other: &Job) -> bool {
        self.token == other.token && self.key.public == other.key.public
    }
}

impl Eq for Job {}

/// A gitlab.com ID token that may be sent to its audience, with what the run reads of it.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct IdToken {
    text: String,
    /// The `job_id` claim, what the job's signature names.
    pub(crate) job_id: i64,
    /// The `jti` claim: one token's own id, which names the job's key file with the job.
    jti: String,
    /// When it expires, from `exp`.
    pub(crate) expires: SystemTime,
}

impl std::fmt::Debug for IdToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IdToken(job {})", self.job_id)
    }
}

impl IdToken {
    /// The token itself, for a request's header.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Whether it has expired at `now`.
    pub(crate) fn expired(&self, now: SystemTime) -> bool {
        self.expires <= now
    }

    /// What tells this job apart, never the token: for markers and records (B-32).
    pub(crate) fn fingerprint(&self) -> String {
        hushrun_build::origin::fingerprint(&format!("gitlab-ci-job\n{}\n{}", self.job_id, self.jti))
    }
}

/// How a job declares its token for the backend at `audience`, as a line says it.
pub(crate) fn declare(audience: &str) -> String {
    format!("declare `id_tokens: {{{VARIABLE}: {{aud: {audience}}}}}` in the job")
}

/// B-48, B-49: what `HUSHRUN_ID_TOKEN` is, in a GitLab CI job whose backend is at `audience`
/// (the org's URL, as the run names it), at `now`, and the job's key made from `CI_JOB_TOKEN`.
pub(crate) fn read(
    env: &dyn Fn(&str) -> Option<OsString>,
    audience: &str,
    now: SystemTime,
) -> Token {
    let Some(text) = env(VARIABLE)
        .map(|value| value.to_string_lossy().trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Token::Absent;
    };
    let token = match parse(&text, audience, now) {
        Ok(token) => token,
        Err(why) => return Token::Unusable(why),
    };
    let Some(job_token) = env(JOB_TOKEN)
        .map(|value| Zeroizing::new(value.to_string_lossy().trim().to_string()))
        .filter(|value| !value.is_empty())
    else {
        return Token::Unusable(format!(
            "{VARIABLE} is set and {JOB_TOKEN} isn't: the job's key is made from it, and a request with another key would withdraw the job (a step in a container needs both)"
        ));
    };
    match JobKey::of(&job_token, &token) {
        Ok(key) => Token::Usable(Job {
            token,
            key: Arc::new(key),
        }),
        Err(why) => Token::Unusable(why),
    }
}

#[derive(Deserialize)]
struct Claims {
    iss: Option<String>,
    aud: Option<serde_json::Value>,
    exp: Option<u64>,
    jti: Option<String>,
    job_id: Option<serde_json::Value>,
}

fn parse(text: &str, audience: &str, now: SystemTime) -> Result<IdToken, String> {
    let not_one = || format!("{VARIABLE} isn't an ID token");
    let mut parts = text.split('.');
    let (Some(_), Some(payload), Some(_), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(not_one());
    };
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|_| not_one())?;
    let claims: Claims = serde_json::from_slice(&bytes).map_err(|_| not_one())?;
    if claims.iss.as_deref() != Some(ISSUER) {
        return Err(format!(
            "{VARIABLE} isn't a gitlab.com ID token (its issuer is {}): hushrun takes gitlab.com's only",
            claims.iss.as_deref().unwrap_or("not named")
        ));
    }
    // Exactly, as the backend compares it: a trailing slash is another audience.
    let wanted = audience;
    match &claims.aud {
        Some(serde_json::Value::String(aud)) if aud == wanted => {}
        other => {
            let found = match other {
                Some(serde_json::Value::String(aud)) => aud.clone(),
                Some(other) => other.to_string(),
                None => "not named".to_string(),
            };
            return Err(format!(
                "{VARIABLE}'s audience is {found}, not this backend's URL, one string: {}",
                declare(wanted)
            ));
        }
    }
    let job_id = match &claims.job_id {
        Some(serde_json::Value::String(id)) => id.parse::<i64>().ok(),
        Some(serde_json::Value::Number(id)) => id.as_i64(),
        _ => None,
    }
    .filter(|id| *id > 0)
    .ok_or_else(|| format!("{VARIABLE} names no job (`job_id`)"))?;
    let jti = claims
        .jti
        .filter(|jti| !jti.is_empty())
        .ok_or_else(|| format!("{VARIABLE} has no `jti`"))?;
    let expires = claims
        .exp
        .and_then(|exp| UNIX_EPOCH.checked_add(Duration::from_secs(exp)))
        .ok_or_else(|| format!("{VARIABLE} says nothing of when it expires (`exp`)"))?;
    if expires <= now {
        return Err(format!(
            "{VARIABLE} has expired: a job's token lives as long as the job's timeout"
        ));
    }
    Ok(IdToken {
        text: text.to_string(),
        job_id,
        jti,
        expires,
    })
}

/// B-49: the job's key, which every build request of the job is signed with.
pub(crate) struct JobKey {
    key: SigningKey,
    /// The public half as the header carries it: `ed25519:<base64>`.
    public: String,
    job_id: i64,
}

impl std::fmt::Debug for JobKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JobKey({})", self.public)
    }
}

/// HKDF's output length for an Ed25519 seed.
struct Seed;

impl ring::hkdf::KeyType for Seed {
    fn len(&self) -> usize {
        32
    }
}

impl JobKey {
    /// The key of `token`'s job, from the job's `CI_JOB_TOKEN`: the same in every run of the job,
    /// another for any other job or job token. HKDF-SHA256, salt [`KEY_LABEL`], input key
    /// material the job token, info `<job_id>\n<jti>`.
    pub(crate) fn of(job_token: &str, token: &IdToken) -> Result<JobKey, String> {
        let salt = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, KEY_LABEL.as_bytes());
        let info = format!("{}\n{}", token.job_id, token.jti);
        let info = [info.as_bytes()];
        let mut seed = Zeroizing::new([0u8; 32]);
        salt.extract(job_token.as_bytes())
            .expand(&info, Seed)
            .and_then(|okm| okm.fill(seed.as_mut()))
            .map_err(|_| "the job's key can't be made".to_string())?;
        let key = SigningKey::from_seed(&seed);
        let public = key.public_key().to_string();
        Ok(JobKey {
            key,
            public,
            job_id: token.job_id,
        })
    }

    /// `Hushrun-Job-Key`'s value.
    pub(crate) fn public(&self) -> &str {
        &self.public
    }

    /// `Hushrun-Job-Signature`'s value for a POST to `path` (`/o/acme/v0/build/open`) of org
    /// `org` with `body`.
    pub(crate) fn sign(&self, path: &str, org: &str, body: &[u8]) -> String {
        let message = hushrun_core::blocks::job_message("POST", path, org, self.job_id, body);
        base64::engine::general_purpose::STANDARD.encode(self.key.sign(message.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUDIENCE: &str = "https://api.example/o/acme";

    fn token(claims: serde_json::Value) -> String {
        let encode = |value: &serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
        };
        format!(
            "{}.{}.c2lnbmF0dXJl",
            encode(&serde_json::json!({"alg": "RS256", "kid": "k"})),
            encode(&claims)
        )
    }

    fn claims() -> serde_json::Value {
        serde_json::json!({
            "iss": ISSUER, "aud": AUDIENCE, "exp": 4_000_000_000u64, "iat": 1,
            "jti": "e1b8-4c2a", "job_id": "1212", "project_id": "42",
        })
    }

    fn read_as(value: Option<&str>, job_token: Option<&str>) -> Token {
        let value = value.map(OsString::from);
        let job_token = job_token.map(OsString::from);
        read(
            &move |name: &str| match name {
                VARIABLE => value.clone(),
                JOB_TOKEN => job_token.clone(),
                _ => None,
            },
            AUDIENCE,
            SystemTime::now(),
        )
    }

    fn read_with(value: Option<&str>) -> Token {
        read_as(value, Some("glcbt-64-the-job-s-own"))
    }

    fn usable() -> Job {
        match read_with(Some(&token(claims()))) {
            Token::Usable(job) => job,
            other => panic!("{other:?}"),
        }
    }

    // covers: B-48
    #[test]
    fn only_a_live_gitlab_com_token_for_this_backend_is_usable() {
        assert_eq!(read_with(None), Token::Absent);
        assert_eq!(read_with(Some("  ")), Token::Absent);
        let job = usable();
        assert_eq!(job.token.job_id, 1212);
        assert!(
            !format!("{job:?}").contains(job.token.text()),
            "never in Debug"
        );
        // No CI_JOB_TOKEN beside it: no key, so nothing is sent.
        match read_as(Some(&token(claims())), None) {
            Token::Unusable(why) => assert!(why.contains("CI_JOB_TOKEN isn't"), "{why}"),
            other => panic!("{other:?}"),
        }
        // The audience is compared exactly, as the backend compares it: a trailing slash is
        // another audience, and the line names the one to declare.
        let mut slash = claims();
        slash["aud"] = serde_json::json!(format!("{AUDIENCE}/"));
        match read_with(Some(&super::tests::token(slash))) {
            Token::Unusable(why) => assert!(
                why.contains("audience is https://api.example/o/acme/, not")
                    && why.contains("{aud: https://api.example/o/acme}}"),
                "{why}"
            ),
            other => panic!("{other:?}"),
        }
        let unusable = |change: &dyn Fn(&mut serde_json::Value), says: &str| {
            let mut claims = claims();
            change(&mut claims);
            match read_with(Some(&super::tests::token(claims))) {
                Token::Unusable(why) => assert!(why.contains(says), "{why}"),
                other => panic!("{says}: {other:?}"),
            }
        };
        // An array that holds the audience is another service's token too, as the backend says.
        unusable(
            &|c| c["aud"] = serde_json::json!([AUDIENCE, "sigstore"]),
            "id_tokens: {HUSHRUN_ID_TOKEN: {aud: https://api.example/o/acme}}",
        );
        unusable(
            &|c| c["aud"] = serde_json::json!("sigstore"),
            "audience is sigstore",
        );
        unusable(
            &|c| c["aud"] = serde_json::json!("https://api.example/o/other"),
            "audience is https://api.example/o/other",
        );
        unusable(
            &|c| c["iss"] = serde_json::json!("https://gitlab.com/"),
            "issuer",
        );
        unusable(
            &|c| c["iss"] = serde_json::json!("https://gitlab.acme.dev"),
            "gitlab.com's only",
        );
        unusable(&|c| c["exp"] = serde_json::json!(1), "has expired");
        unusable(&|c| c["job_id"] = serde_json::json!("x"), "names no job");
        unusable(
            &|c| {
                c.as_object_mut().unwrap().remove("job_id");
            },
            "names no job",
        );
        unusable(&|c| c["jti"] = serde_json::json!(""), "no `jti`");
        match read_with(Some("not.a-token")) {
            Token::Unusable(why) => assert!(why.contains("isn't an ID token"), "{why}"),
            other => panic!("{other:?}"),
        }
        // A number for `job_id` reads as the string does.
        let mut number = claims();
        number["job_id"] = serde_json::json!(1212);
        assert!(matches!(
            read_with(Some(&super::tests::token(number))),
            Token::Usable(job) if job.token.job_id == 1212
        ));
    }

    fn verify(key: &JobKey, path: &str, body: &[u8]) -> bool {
        let public = hushrun_core::signers::PublicKey::parse(key.public()).unwrap();
        let signature = base64::engine::general_purpose::STANDARD
            .decode(key.sign(path, "acme", body))
            .unwrap();
        let message = hushrun_core::blocks::job_message("POST", path, "acme", 1212, body);
        hushrun_attest::keys::verify_signature(&public, message.as_bytes(), &signature)
    }

    // covers: B-49
    #[test]
    fn every_run_of_a_job_makes_the_same_key_from_its_job_token_and_nothing_else_does() {
        let job = usable();
        assert!(
            job.key.public().starts_with("ed25519:"),
            "{}",
            job.key.public()
        );
        assert!(verify(&job.key, "/o/acme/v0/build/open", b"{}"));
        // Another run of the job, anywhere: nothing but the token and the job token goes in.
        assert_eq!(usable().key.public(), job.key.public());
        // Another job token (a thief with the ID token alone guesses one), another jti, another
        // job: another key.
        let other_job_token = match read_as(Some(&token(claims())), Some("glcbt-someone-else")) {
            Token::Usable(job) => job,
            other => panic!("{other:?}"),
        };
        assert_ne!(other_job_token.key.public(), job.key.public());
        for (name, value) in [("jti", "another-jti"), ("job_id", "1213")] {
            let mut c = claims();
            c[name] = serde_json::json!(value);
            let Token::Usable(other) = read_with(Some(&token(c))) else {
                panic!("{name}")
            };
            assert_ne!(other.key.public(), job.key.public(), "{name}");
        }
        // The derivation as written, by a fixed vector: a change to it would give a job that
        // runs two hushrun versions two keys.
        assert_eq!(
            job.key.public(),
            "ed25519:e+xow1Ci4fFcUH27NuLglHKWjFpFJLd+EyynsIdxJwM="
        );
    }
}

//! B-48, B-49: a GitLab CI job's part in sharing build blocks. Its credential is its ID token,
//! read from `HUSHRUN_ID_TOKEN` alone, and the key the job proves it is the job with.
//!
//! **The token** ([`read`]) is one of four things, decided here and nowhere else:
//!
//! | `HUSHRUN_ID_TOKEN` | [`Token`] | What the run does |
//! |---|---|---|
//! | unset or empty | `Absent` | shares nothing; says how to declare it where the org shares |
//! | not gitlab.com's, another audience, no job, expired | `Unusable` | shares nothing, and says why, once |
//! | gitlab.com's, for this backend's URL, alive | `Usable` | its build requests carry it, and its results lookup beside its bearer |
//!
//! It goes to no server but the one its audience names: a repository's files can name any
//! backend, and a token sent elsewhere could be replayed at hushrun's for as long as the job's
//! timeout. Nothing here verifies its signature, which is the backend's to do: what is read is
//! only what decides whether to send it.
//!
//! **The key** ([`JobKey::of`]) is made once per job and shared by every `hushrun run` of it,
//! since a second key withdraws the job (B-49). It lives in one file, named by the job's id and
//! the token's `jti`, under the system's temporary folder:
//!
//! | The file | Moved by | To |
//! |---|---|---|
//! | absent | the job's first run: a key written whole to a temporary file (0600), linked into place, which fails if another run linked first | present |
//! | present, this user's, 0600, of this job | every later run: read, never replaced | used |
//! | present otherwise (another user's, readable by others, a link, unreadable, another job's) | nothing: never read as the key, never replaced | the run shares nothing, with one line |
//! | present, its token expired over an hour ago | any job's run, at the start ([`sweep`]) | absent |

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use hushrun_attest::keys::SigningKey;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// The variable a job's `id_tokens:` names (B-48): the only place the token is read from.
pub(crate) const VARIABLE: &str = "HUSHRUN_ID_TOKEN";

/// The issuer of gitlab.com's CI ID tokens, exactly. A self-managed instance is not taken.
pub(crate) const ISSUER: &str = "https://gitlab.com";

/// What a key file's JSON says it is.
const KEY_SCHEMA: &str = "hushrun.gitlab-job-key/v1";

/// What every key file's name starts with.
const KEY_FILE: &str = "hushrun-job-";

/// A key file whose token expired this long ago is another job's leftover.
const KEPT_AFTER_EXPIRY: Duration = Duration::from_secs(3600);

/// What `HUSHRUN_ID_TOKEN` is, for the backend at `audience`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Token {
    /// It isn't set.
    Absent,
    /// It is set, and can't be sent: why, as a line ends with it.
    Unusable(String),
    /// It may be sent to the backend at its audience.
    Usable(IdToken),
}

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

/// B-48: what `HUSHRUN_ID_TOKEN` is, in a GitLab CI job whose backend is at `audience` (the
/// org's URL), at `now`.
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
    match parse(&text, audience, now) {
        Ok(token) => Token::Usable(token),
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
    let wanted = audience.trim_end_matches('/');
    match &claims.aud {
        Some(serde_json::Value::String(aud)) if aud.trim_end_matches('/') == wanted => {}
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

#[derive(Serialize, Deserialize)]
struct KeyFile {
    schema: String,
    job_id: i64,
    /// The token's `exp`, in seconds since the epoch: when the sweep may remove the file.
    expires: u64,
    key: String,
}

impl JobKey {
    /// The key of `token`'s job, in `dir`: made by the job's first run, read by every later one.
    /// `Err` is the line to say: the run shares nothing.
    pub(crate) fn of(dir: &Path, token: &IdToken) -> Result<JobKey, String> {
        sweep(dir, SystemTime::now());
        let path = key_path(dir, token);
        let read = || read_key(&path, token.job_id);
        if fs::symlink_metadata(&path).is_ok() {
            return read();
        }
        let key = SigningKey::generate().map_err(|err| format!("the job's key: {err}"))?;
        let expires = token
            .expires
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let file = KeyFile {
            schema: KEY_SCHEMA.to_string(),
            job_id: token.job_id,
            expires,
            key: key.to_pem().to_string(),
        };
        let bytes = Zeroizing::new(serde_json::to_vec(&file).map_err(|e| e.to_string())?);
        match place(&path, &bytes) {
            Ok(()) => Ok(JobKey::new(key, token.job_id)),
            // Another run of the job linked its key first: that one is the job's.
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => read(),
            Err(err) => Err(format!("the job's key at {}: {err}", path.display())),
        }
    }

    fn new(key: SigningKey, job_id: i64) -> JobKey {
        let public = key.public_key().to_string();
        JobKey {
            key,
            public,
            job_id,
        }
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

/// Where `token`'s job keeps its key in `dir`: `hushrun-job-<job id>-<digest of the jti>.key`.
fn key_path(dir: &Path, token: &IdToken) -> PathBuf {
    let jti = hushrun_build::origin::fingerprint(&token.jti);
    dir.join(format!(
        "{KEY_FILE}{}-{}.key",
        token.job_id,
        &jti[..16.min(jti.len())]
    ))
}

/// Writes `bytes` whole to a temporary file of this user's alone beside `path`, then links it
/// into place, which fails with `AlreadyExists` when another run linked first. The temporary
/// file goes either way.
fn place(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|err| io::Error::other(err.to_string()))?;
    let temp = dir.join(format!(".{KEY_FILE}{}.tmp", hex::encode(random)));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let written = (|| {
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::hard_link(&temp, path)
    })();
    let _ = fs::remove_file(&temp);
    written
}

/// The key in `path`, when the file is this user's alone, a plain file, and of job `job_id`.
fn read_key(path: &Path, job_id: i64) -> Result<JobKey, String> {
    let refused = |why: &str| {
        format!(
            "the job's key at {} {why}: it is never replaced, so this run shares nothing",
            path.display()
        )
    };
    let meta =
        fs::symlink_metadata(path).map_err(|err| refused(&format!("can't be read ({err})")))?;
    if !meta.file_type().is_file() {
        return Err(refused("isn't a file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != rustix::process::geteuid().as_raw() {
            return Err(refused("is another user's"));
        }
        if meta.mode() & 0o077 != 0 {
            return Err(refused("can be read by others"));
        }
    }
    let bytes =
        Zeroizing::new(fs::read(path).map_err(|err| refused(&format!("can't be read ({err})")))?);
    let file: KeyFile = serde_json::from_slice(&bytes).map_err(|_| refused("isn't a job's key"))?;
    if file.schema != KEY_SCHEMA || file.job_id != job_id {
        return Err(refused("isn't this job's key"));
    }
    let key = SigningKey::from_pem(&file.key).map_err(|_| refused("isn't a job's key"))?;
    Ok(JobKey::new(key, job_id))
}

/// Removes this user's key files in `dir` whose token expired over an hour before `now`, and
/// temporary files a run left there over an hour ago: no request can be signed with them any
/// more. Another user's, and anything else, are left alone.
pub(crate) fn sweep(dir: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.file_type().is_file() || !own(&meta) {
            continue;
        }
        let gone = if name.starts_with(KEY_FILE) && name.ends_with(".key") {
            fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<KeyFile>(&bytes).ok())
                .filter(|file| file.schema == KEY_SCHEMA)
                .and_then(|file| UNIX_EPOCH.checked_add(Duration::from_secs(file.expires)))
                .is_some_and(|expires| expires + KEPT_AFTER_EXPIRY < now)
        } else if name.starts_with(&format!(".{KEY_FILE}")) && name.ends_with(".tmp") {
            meta.modified().is_ok_and(|at| at + KEPT_AFTER_EXPIRY < now)
        } else {
            false
        };
        if gone {
            let _ = fs::remove_file(&path);
        }
    }
}

#[cfg(unix)]
fn own(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    meta.uid() == rustix::process::geteuid().as_raw()
}

#[cfg(not(unix))]
fn own(_: &fs::Metadata) -> bool {
    true
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

    fn read_with(value: Option<&str>) -> Token {
        let value = value.map(OsString::from);
        read(
            &move |name: &str| (name == VARIABLE).then(|| value.clone()).flatten(),
            AUDIENCE,
            SystemTime::now(),
        )
    }

    fn usable() -> IdToken {
        match read_with(Some(&token(claims()))) {
            Token::Usable(token) => token,
            other => panic!("{other:?}"),
        }
    }

    // covers: B-48
    #[test]
    fn only_a_live_gitlab_com_token_for_this_backend_is_usable() {
        assert_eq!(read_with(None), Token::Absent);
        assert_eq!(read_with(Some("  ")), Token::Absent);
        let token = usable();
        assert_eq!(token.job_id, 1212);
        assert!(
            !format!("{token:?}").contains(token.text()),
            "never in Debug"
        );
        // A trailing slash on either side is the same URL.
        let mut slash = claims();
        slash["aud"] = serde_json::json!(format!("{AUDIENCE}/"));
        assert!(matches!(
            read_with(Some(&super::tests::token(slash))),
            Token::Usable(_)
        ));
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
            Token::Usable(t) if t.job_id == 1212
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
    fn every_run_of_a_job_signs_with_the_one_key_its_first_run_made() {
        let dir = tempfile::tempdir().unwrap();
        let token = usable();
        let first = JobKey::of(dir.path(), &token).unwrap();
        assert!(first.public().starts_with("ed25519:"), "{}", first.public());
        assert!(verify(&first, "/o/acme/v0/build/open", b"{}"));
        // The next run of the job reads the same key, also when runs start at once.
        let again = JobKey::of(dir.path(), &token).unwrap();
        assert_eq!(again.public(), first.public());
        let at_once: Vec<String> = std::thread::scope(|scope| {
            let other = tempfile::tempdir().unwrap();
            let runs: Vec<_> = (0..8)
                .map(|_| {
                    let dir = other.path().to_path_buf();
                    let token = token.clone();
                    scope.spawn(move || JobKey::of(&dir, &token).unwrap().public().to_string())
                })
                .collect();
            runs.into_iter().map(|r| r.join().unwrap()).collect()
        });
        assert!(at_once.iter().all(|k| *k == at_once[0]), "{at_once:?}");
        // One key file, of this user's alone, and no temporary file left.
        let files: Vec<_> = fs::read_dir(dir.path()).unwrap().flatten().collect();
        assert_eq!(files.len(), 1, "{files:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = files[0].metadata().unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Another token, of another job, has its own key.
        let mut other = claims();
        other["job_id"] = serde_json::json!("1213");
        other["jti"] = serde_json::json!("other");
        let Token::Usable(other) = read_with(Some(&super::tests::token(other))) else {
            panic!()
        };
        assert_ne!(
            JobKey::of(dir.path(), &other).unwrap().public(),
            first.public()
        );
    }

    // covers: B-49
    #[cfg(unix)]
    #[test]
    fn a_key_file_the_job_cant_trust_is_never_read_nor_replaced() {
        use std::os::unix::fs::PermissionsExt as _;
        let token = usable();
        type Plant<'a> = &'a dyn Fn(&Path);
        let cases: [(&str, Plant); 4] = [
            ("can be read by others", &|path| {
                fs::write(path, "{}").unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
            }),
            ("isn't a file", &|path| {
                std::os::unix::fs::symlink("/dev/null", path).unwrap();
            }),
            ("isn't a job's key", &|path| {
                fs::write(path, "not json").unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
            }),
            ("isn't this job's key", &|path| {
                let file = KeyFile {
                    schema: KEY_SCHEMA.into(),
                    job_id: 7,
                    expires: 4_000_000_000,
                    key: SigningKey::generate().unwrap().to_pem().to_string(),
                };
                fs::write(path, serde_json::to_vec(&file).unwrap()).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
            }),
        ];
        for (says, plant) in cases {
            let dir = tempfile::tempdir().unwrap();
            let path = key_path(dir.path(), &token);
            plant(&path);
            let before = fs::symlink_metadata(&path).unwrap();
            let err = JobKey::of(dir.path(), &token).err().expect(says);
            assert!(err.contains(says), "{err}");
            let after = fs::symlink_metadata(&path).unwrap();
            assert_eq!(
                before.modified().unwrap(),
                after.modified().unwrap(),
                "{says}"
            );
            assert_eq!(before.file_type(), after.file_type(), "{says}");
        }
    }

    // covers: B-49
    #[test]
    fn the_sweep_removes_keys_whose_tokens_expired_over_an_hour_ago_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let token = usable();
        let live = key_path(dir.path(), &token);
        JobKey::of(dir.path(), &token).unwrap();
        let mut old = claims();
        old["job_id"] = serde_json::json!("9");
        old["jti"] = serde_json::json!("nine");
        old["exp"] = serde_json::json!(2_000_000_000u64);
        let Token::Usable(old) = read_with(Some(&super::tests::token(old))) else {
            panic!()
        };
        JobKey::of(dir.path(), &old).unwrap();
        let stale = key_path(dir.path(), &old);
        fs::write(dir.path().join("hushrun-job-notes.txt"), "mine").unwrap();
        let at = |secs: u64| UNIX_EPOCH + Duration::from_secs(secs);
        sweep(dir.path(), at(2_000_000_000 + 3599));
        assert!(stale.exists(), "within the hour after its expiry");
        sweep(dir.path(), at(2_000_000_000 + 3601));
        assert!(!stale.exists());
        assert!(live.exists());
        assert!(dir.path().join("hushrun-job-notes.txt").exists());
    }
}

//! Where signatures come from (T-12): a key file, or keyless signing through Sigstore; the timestamp authorities that timestamp each signature (E-10); and the Sigstore
//! trust root that verifies certificates and timestamps. All of it comes from the environment,
//! never from the content under evaluation (T-4).

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as _, anyhow};

use hushrun_attest::remote::Secret;
use hushrun_attest::remote::fulcio::{self, Fulcio, Keyless};
use hushrun_attest::remote::oidc::{
    GithubActions, GitlabIdToken, Given, Interactive, SIGSTORE_CLIENT, SIGSTORE_ISSUER, TokenSource,
};
use hushrun_attest::remote::tsa::{self, Authorities};
use hushrun_attest::tuf::{self, SIGSTORE_MIRROR, SIGSTORE_ROOT, SIGSTORE_TARGETS};
use hushrun_attest::{
    NoTimestamps, SignError, Signer, SigningKey, Timestamper, TrustedRoot, sign_event_bundle,
};
use hushrun_core::digest::Digest;
use hushrun_core::event::Event;
use hushrun_core::run::Provider;
use hushrun_core::signers::CertIdentity;
use hushrun_core::time::{Dur, Time};

use crate::app::App;
use crate::exit::{Exit, Failure};

/// `HUSHRUN_TSA=none`: no timestamps.
pub const NO_TSA: &str = "none";
/// Where an ID token for Fulcio waits outside GitHub Actions: on GitLab CI, an `id_tokens`
/// entry with `aud: sigstore` (T-12).
const SIGSTORE_ID_TOKEN: &str = "SIGSTORE_ID_TOKEN";

/// Signing failures: an unreachable or refusing signer exits 9; a configuration error, 2.
pub fn sign_failure(err: SignError) -> Failure {
    match err {
        SignError::Unavailable(_) => Failure::new(Exit::Signer, err),
        SignError::Config(_) => Failure::usage(err),
    }
}

/// How this command signs.
pub struct Signing {
    signer: Box<dyn Signer>,
    timestamps: Box<dyn Timestamper>,
    /// `HUSHRUN_TSA=none`: a missing timestamp is what was asked for, not news.
    timestamps_off: bool,
    warned: AtomicBool,
    clock: Box<dyn Fn() -> Time + Send + Sync>,
    root: TrustedRoot,
}

impl Signing {
    pub fn signer(&self) -> &dyn Signer {
        self.signer.as_ref()
    }

    /// The Sigstore trust root this command reads with too.
    pub fn root(&self) -> &TrustedRoot {
        &self.root
    }

    /// `doctor`: the authority that answered with a timestamp that verifies, `None` with
    /// `HUSHRUN_TSA=none`, or why none did.
    pub fn probe_timestamp(&self) -> Result<Option<String>, String> {
        if self.timestamps_off {
            return Ok(None);
        }
        let response = self
            .timestamps
            .timestamp(b"hushrun doctor")
            .map_err(|e| e.to_string())?;
        hushrun_attest::timestamp::verify(&response, b"hushrun doctor", &self.root, None)
            .map(|stamp| Some(stamp.authority))
    }

    /// `event`, with `time` set to when it is signed (E-4), signed into a Sigstore bundle with a
    /// timestamp (E-1, E-10). Without a timestamp, a key's event still goes out, and a warning
    /// says so once per command; a keyless event can't.
    pub fn sign(&self, event: &Event, digest: &Digest) -> Result<(Event, Vec<u8>), Failure> {
        let mut event = event.clone();
        let now = (self.clock)();
        if now > event.time {
            // A lease is as long as it was asked to be, from when the event is signed.
            let later = Dur::from_millis(u64::try_from(now.millis_since(event.time)).unwrap_or(0));
            if let Some(started) = event.started.as_mut() {
                started.lease_until = started.lease_until.saturating_add(later);
            }
            event.time = now;
        }
        let signed = sign_event_bundle(
            &event,
            digest,
            self.signer.as_ref(),
            self.timestamps.as_ref(),
        )
        .map_err(sign_failure)?;
        if let Some(why) = signed.untimestamped
            && !self.timestamps_off
            && !self.warned.swap(true, Ordering::SeqCst)
        {
            tracing::warn!(
                "{why}. Events go out without a trusted timestamp, and count only while {}'s machine counts",
                self.signer.describe()
            );
        }
        Ok((event, signed.bytes))
    }
}

impl Signing {
    /// \[ADR 0026\] A GC statement for the deletion of `key` from `ledger` (L-9), signed into a
    /// bundle as events are, so that a keyless identity can sign it too.
    pub fn sign_gc(&self, ledger: &str, key: &str) -> Result<Vec<u8>, Failure> {
        hushrun_attest::gc::sign_gc_bundle(
            ledger,
            key,
            (self.clock)(),
            self.signer.as_ref(),
            self.timestamps.as_ref(),
        )
        .map_err(sign_failure)
    }
}

/// What `HUSHRUN_SIGNING` and the signing variables choose.
enum Source {
    Key,
    Sigstore,
    /// \[ADR 0028\] The key this machine registered at sign-in.
    Machine,
}

/// A TUF repository of a Sigstore trust root, and the root updates start from.
pub struct Instance {
    pub mirror: String,
    pub initial: Vec<u8>,
    /// Public-good Sigstore, whose trust root is also vendored.
    pub public_good: bool,
}

impl App<'_> {
    fn var(&self, name: &str) -> Option<String> {
        self.env
            .get(name)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    }

    /// The time now: `--now` when given, else the clock.
    pub fn clock(&self) -> Box<dyn Fn() -> Time + Send + Sync> {
        match self.global.now {
            Some(_) => {
                let now = self.now;
                Box::new(move || now)
            }
            None => Box::new(|| Time::from_timestamp(jiff::Timestamp::now())),
        }
    }

    fn signing_source(&self) -> Result<Source, Failure> {
        let key = self.var("HUSHRUN_SIGNING_KEY").is_some()
            || self.var("HUSHRUN_SIGNING_KEY_FILE").is_some();
        let choice = self.var("HUSHRUN_SIGNING").map(|v| v.to_ascii_lowercase());
        match (choice.as_deref(), key) {
            (Some("key"), true) | (None, true) => Ok(Source::Key),
            (Some("key"), false) => Err(Failure::usage(anyhow!(
                "HUSHRUN_SIGNING=key: set HUSHRUN_SIGNING_KEY (PEM) or HUSHRUN_SIGNING_KEY_FILE"
            ))),
            (Some("sigstore"), _) => Ok(Source::Sigstore),
            (Some("machine"), _) => Ok(Source::Machine),
            (Some(other), _) => Err(Failure::usage(anyhow!(
                "HUSHRUN_SIGNING is key, sigstore or machine, not {other:?}"
            ))),
            // ADR 0026 item 7: a GitHub Actions job with an ID token signs keylessly by default,
            // as the repository's CI (T-15).
            (None, false) if self.run.in_ci && self.has_github_id_token() => Ok(Source::Sigstore),
            // ADR 0028 item 5: elsewhere a machine key signs: a person's, registered at sign-in,
            // or an agent's, such as a GitLab pipeline's (HUSHRUN_MACHINE_KEY or
            // HUSHRUN_MACHINE_KEY_FILE), since only GitHub Actions is a repository's keyless CI.
            // In CI, only a key the job names: never the one a person's sign-in left on a runner,
            // which would sign the pipeline's events as that person.
            (None, false)
                if (!self.run.in_ci || self.names_machine_key())
                    && crate::machine::load().map_err(Failure::usage)?.is_some() =>
            {
                Ok(Source::Machine)
            }
            // A CI job with another ID token, and no machine key, signs keylessly.
            (None, false) if self.run.in_ci && self.var(SIGSTORE_ID_TOKEN).is_some() => {
                Ok(Source::Sigstore)
            }
            (None, false) if self.run.in_ci => Err(Failure::usage(anyhow!(
                "this command signs events: in GitHub Actions, give the job `permissions: id-token: write`; elsewhere, give it an agent's machine key in HUSHRUN_MACHINE_KEY or HUSHRUN_MACHINE_KEY_FILE (`hushrun token create --agent NAME` makes one with the token), or set HUSHRUN_SIGNING_KEY (PEM) or HUSHRUN_SIGNING_KEY_FILE"
            ))),
            (None, false) => Err(Failure::usage(anyhow!(
                "this command signs events: sign this machine in with `hushrun login`, which registers its key, or set HUSHRUN_SIGNING=sigstore for keyless signing, or HUSHRUN_SIGNING_KEY (PEM) or HUSHRUN_SIGNING_KEY_FILE"
            ))),
        }
    }

    /// Why this command couldn't sign an event here, when it couldn't, without reading a trust
    /// root or a key's server (C-22).
    pub(crate) fn cannot_sign(&self) -> Option<Failure> {
        self.signing_source().err()
    }

    /// Whether this CI job can get an ID token for keyless signing (T-12): GitHub Actions', or
    /// `SIGSTORE_ID_TOKEN`.
    pub(crate) fn has_id_token(&self) -> bool {
        self.has_github_id_token() || self.var(SIGSTORE_ID_TOKEN).is_some()
    }

    /// Whether the environment names a machine key: an agent's, given to a CI job.
    fn names_machine_key(&self) -> bool {
        self.var("HUSHRUN_MACHINE_KEY").is_some() || self.var("HUSHRUN_MACHINE_KEY_FILE").is_some()
    }

    /// Whether this is a GitHub Actions job with `permissions: id-token: write`, whose ID token
    /// makes it the repository's keyless CI (T-15).
    fn has_github_id_token(&self) -> bool {
        self.var("ACTIONS_ID_TOKEN_REQUEST_URL").is_some()
            && self.var("ACTIONS_ID_TOKEN_REQUEST_TOKEN").is_some()
    }

    /// \[ADR 0028\] This machine's key, registered when it signed in, or an agent's, registered
    /// with its token (C-18).
    fn machine_key(&self) -> Result<SigningKey, Failure> {
        crate::machine::load()
            .map_err(Failure::usage)?
            .map(|machine| machine.key)
            .ok_or_else(|| {
                Failure::usage(anyhow!(
                    "HUSHRUN_SIGNING=machine: this machine has no key yet: `hushrun login` makes and registers one"
                ))
            })
    }

    /// How this command signs events (T-12).
    pub fn signing(&self) -> Result<Signing, Failure> {
        let source = self.signing_source()?;
        self.signing_from(source, self.sigstore_root()?)
    }

    /// How this command signs events, with the trust root it already read.
    pub fn signing_with(&self, root: &TrustedRoot) -> Result<Signing, Failure> {
        self.signing_from(self.signing_source()?, root.clone())
    }

    /// Signing with `key`, a key file a command names, and the timestamp authorities (E-10).
    pub fn signing_by_key(&self, key: SigningKey, root: TrustedRoot) -> Result<Signing, Failure> {
        let (timestamps, timestamps_off) = self.timestamper(&root)?;
        Ok(Signing {
            signer: Box::new(key),
            timestamps,
            timestamps_off,
            warned: AtomicBool::new(false),
            clock: self.clock(),
            root,
        })
    }

    fn signing_from(&self, source: Source, root: TrustedRoot) -> Result<Signing, Failure> {
        let (timestamps, timestamps_off) = self.timestamper(&root)?;
        let signer: Box<dyn Signer> = match source {
            Source::Key => Box::new(self.signing_key()?),
            Source::Machine => Box::new(self.machine_key()?),
            Source::Sigstore => {
                if timestamps_off {
                    return Err(Failure::usage(anyhow!(
                        "keyless signing needs a timestamp authority, and HUSHRUN_TSA is none: its certificates last minutes"
                    )));
                }
                let url = self
                    .var("HUSHRUN_FULCIO_URL")
                    .unwrap_or_else(|| fulcio::PUBLIC_GOOD.to_string());
                let fulcio = Fulcio::new(&url).map_err(sign_failure)?;
                Box::new(
                    Keyless::new(fulcio, self.id_tokens()?, root.clone(), self.clock())
                        .map_err(sign_failure)?,
                )
            }
        };
        Ok(Signing {
            signer,
            timestamps,
            timestamps_off,
            warned: AtomicBool::new(false),
            clock: self.clock(),
            root,
        })
    }

    /// A signer with a key: a key file `key_file` names, else the command's key or machine key,
    /// for what `hushrun keys public` shows.
    pub fn envelope_signer(&self, key_file: Option<&Path>) -> Result<Box<dyn Signer>, Failure> {
        if let Some(path) = key_file {
            let pem = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))
                .map_err(Failure::usage)?;
            return Ok(Box::new(
                SigningKey::from_pem(&pem).map_err(Failure::usage)?,
            ));
        }
        match self.signing_source()? {
            Source::Key => Ok(Box::new(self.signing_key()?)),
            Source::Machine => Ok(Box::new(self.machine_key()?)),
            Source::Sigstore => Err(Failure::usage(anyhow!(
                "HUSHRUN_SIGNING=sigstore signs without a key: pass --key, or set HUSHRUN_SIGNING_KEY"
            ))),
        }
    }

    /// `doctor`: the identity Fulcio would certify for this job, from the claims of its ID
    /// token, read without verifying them; `None` outside GitHub Actions and GitLab CI.
    pub fn keyless_identity(&self) -> Result<Option<CertIdentity>, Failure> {
        if self.run.provider == Provider::Gitlab
            && self.var("ACTIONS_ID_TOKEN_REQUEST_URL").is_none()
        {
            if self.var(SIGSTORE_ID_TOKEN).is_none() {
                return Ok(None);
            }
            let http = hushrun_attest::remote::Http::new().map_err(sign_failure)?;
            let token = self
                .id_tokens()?
                .id_token(&http, fulcio::AUDIENCE)
                .map_err(sign_failure)?;
            let claims =
                hushrun_attest::remote::oidc::claims(token.expose()).map_err(sign_failure)?;
            return hushrun_attest::remote::oidc::gitlab_identity(&claims)
                .map(Some)
                .map_err(sign_failure);
        }
        if self.var("ACTIONS_ID_TOKEN_REQUEST_URL").is_none() {
            return Ok(None);
        }
        let http = hushrun_attest::remote::Http::new().map_err(sign_failure)?;
        let token = self
            .id_tokens()?
            .id_token(&http, fulcio::AUDIENCE)
            .map_err(sign_failure)?;
        let claims = hushrun_attest::remote::oidc::claims(token.expose()).map_err(sign_failure)?;
        let claim = |name: &str| {
            claims
                .get(name)
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let server = self
            .var("GITHUB_SERVER_URL")
            .unwrap_or_else(|| "https://github.com".into());
        let signer = claim("job_workflow_ref").map(|r| format!("{server}/{r}"));
        Ok(Some(CertIdentity {
            issuer: claim("iss").unwrap_or_default(),
            subject: signer.clone().unwrap_or_default(),
            repository_id: claim("repository_id"),
            repository: claim("repository").map(|r| format!("{server}/{r}")),
            reference: claim("ref"),
            workflow: claim("workflow_ref").map(|r| format!("{server}/{r}")),
            signer_workflow: signer,
            trigger: claim("event_name"),
        }))
    }

    /// ID tokens for keyless signing: GitHub Actions' endpoint, else `SIGSTORE_ID_TOKEN` (on
    /// GitLab CI an `id_tokens` entry, checked for its audience and expiry, T-12 \[M9\]), else
    /// \[M8\], for a person at their terminal, their own: they sign in with their browser, at
    /// `HUSHRUN_OIDC_ISSUER` (default: public-good Sigstore's) as client
    /// `HUSHRUN_OIDC_CLIENT_ID` (default `sigstore`). Without a terminal, only an issuer named
    /// in `HUSHRUN_OIDC_ISSUER` signs in: a script never opens a browser it wasn't told to.
    fn id_tokens(&self) -> Result<Box<dyn TokenSource>, Failure> {
        if let (Some(url), Some(token)) = (
            self.var("ACTIONS_ID_TOKEN_REQUEST_URL"),
            self.var("ACTIONS_ID_TOKEN_REQUEST_TOKEN"),
        ) {
            return Ok(Box::new(GithubActions::new(url, Secret::new(token))));
        }
        if let Some(token) = self.var(SIGSTORE_ID_TOKEN) {
            if self.run.provider == Provider::Gitlab {
                return Ok(Box::new(GitlabIdToken::new(
                    SIGSTORE_ID_TOKEN,
                    Secret::new(token),
                    self.clock(),
                )));
            }
            return Ok(Box::new(Given(Secret::new(token))));
        }
        let person = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
        if !self.run.in_ci && (person || self.var("HUSHRUN_OIDC_ISSUER").is_some()) {
            let browser = self.var("BROWSER");
            return Ok(Box::new(Interactive {
                issuer: self
                    .var("HUSHRUN_OIDC_ISSUER")
                    .unwrap_or_else(|| SIGSTORE_ISSUER.to_string()),
                client_id: self
                    .var("HUSHRUN_OIDC_CLIENT_ID")
                    .unwrap_or_else(|| SIGSTORE_CLIENT.to_string()),
                show: Box::new(move |url| {
                    eprintln!("Sign in to sign as yourself: {url}");
                    open_browser(browser.as_deref(), url);
                }),
                wait: Duration::from_secs(300),
            }));
        }
        Err(Failure::usage(anyhow!(
            "{}",
            if self.run.provider == Provider::Gitlab {
                "keyless signing needs an ID token: declare it in the job's `id_tokens` as `SIGSTORE_ID_TOKEN: {aud: sigstore}`"
            } else {
                "keyless signing needs an ID token: in GitHub Actions, give the job `permissions: id-token: write`; in GitLab CI, declare `SIGSTORE_ID_TOKEN: {aud: sigstore}` under `id_tokens`; elsewhere, set SIGSTORE_ID_TOKEN, or run hushrun at a terminal to sign in with your browser"
            }
        )))
    }

    /// The timestamp authorities (E-10): `HUSHRUN_TSA`, URLs tried in order, or `none`; by
    /// default Sigstore's. The second value says whether timestamps are off.
    fn timestamper(&self, root: &TrustedRoot) -> Result<(Box<dyn Timestamper>, bool), Failure> {
        let urls: Vec<String> = match self.var("HUSHRUN_TSA") {
            Some(value) if value.eq_ignore_ascii_case(NO_TSA) => {
                return Ok((Box::new(NoTimestamps), true));
            }
            Some(value) => value
                .split(',')
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .map(str::to_string)
                .collect(),
            None => vec![tsa::PUBLIC_GOOD.to_string()],
        };
        let authorities = Authorities::new(&urls, root.clone()).map_err(sign_failure)?;
        Ok((Box::new(authorities), false))
    }

    /// The TUF repository of the Sigstore trust root (T-12): `HUSHRUN_SIGSTORE_TUF_URL`, with
    /// the root in `HUSHRUN_SIGSTORE_TUF_ROOT`; by default public-good Sigstore's.
    pub fn sigstore_instance(&self) -> Result<Instance, Failure> {
        let mirror = self.var("HUSHRUN_SIGSTORE_TUF_URL");
        let root = self.var("HUSHRUN_SIGSTORE_TUF_ROOT");
        match (mirror, root) {
            (None, None) => Ok(Instance {
                mirror: SIGSTORE_MIRROR.to_string(),
                initial: SIGSTORE_ROOT.to_vec(),
                public_good: true,
            }),
            (Some(mirror), Some(path)) => Ok(Instance {
                public_good: false,
                initial: std::fs::read(&path)
                    .with_context(|| format!("reading HUSHRUN_SIGSTORE_TUF_ROOT {path}"))
                    .map_err(Failure::usage)?,
                mirror,
            }),
            _ => Err(Failure::usage(anyhow!(
                "HUSHRUN_SIGSTORE_TUF_URL and HUSHRUN_SIGSTORE_TUF_ROOT name another TUF repository and the root to start from: set both, or neither"
            ))),
        }
    }

    /// The Sigstore trust root (T-12): the file `HUSHRUN_SIGSTORE_TRUSTED_ROOT` names; else the
    /// one `hushrun sigstore refresh` cached, when it verifies offline against the TUF root and
    /// is no older than the snapshot in the binary; else that snapshot.
    pub fn sigstore_root(&self) -> Result<TrustedRoot, Failure> {
        if let Some(path) = self.var("HUSHRUN_SIGSTORE_TRUSTED_ROOT") {
            let bytes = std::fs::read(&path)
                .with_context(|| format!("reading HUSHRUN_SIGSTORE_TRUSTED_ROOT {path}"))
                .map_err(Failure::usage)?;
            return TrustedRoot::parse(&bytes, &format!("HUSHRUN_SIGSTORE_TRUSTED_ROOT {path}"))
                .map_err(|e| Failure::usage(anyhow!(e)));
        }
        let instance = self.sigstore_instance()?;
        match SigstoreCache::for_instance(&instance).map(|cache| cache.load(&instance)) {
            Some(Ok(Some(root))) => return Ok(root),
            Some(Err(why)) => tracing::warn!("ignoring the cached Sigstore trust root: {why}"),
            _ => {}
        }
        if instance.public_good {
            Ok(TrustedRoot::public_good().clone())
        } else {
            Err(Failure::usage(anyhow!(
                "no trust root of {} is cached: run `hushrun sigstore refresh`",
                instance.mirror
            )))
        }
    }
}

/// The cached trust root of one TUF repository: `<cache>/sigstore/<host>/`, with
/// `root/<n>.json` for the root versions after the initial one, `targets.json` and
/// `trusted_root.json`. Readers verify it all again before using it. \[ADR 0061\] Since they
/// verify together, they are written together: in a directory of their own, `set-…`, which
/// `current` names once it is whole. A cache from before, with no `current`, holds them at the
/// top.
pub struct SigstoreCache {
    pub dir: PathBuf,
}

/// \[ADR 0061\] The file that names the set in use, beside the sets.
const CURRENT_SET: &str = "current";

/// What a valid cache holds.
pub struct Cached {
    pub roots: Vec<Vec<u8>>,
    pub trusted_root: Vec<u8>,
    pub verified: tuf::Verified,
}

impl SigstoreCache {
    /// The cache of `instance`, or `None` when caching is off (`HUSHRUN_CACHE_DIR=""`).
    pub fn for_instance(instance: &Instance) -> Option<SigstoreCache> {
        let host: String = reqwest_host(&instance.mirror)
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        crate::context::cache_dir().map(|dir| SigstoreCache {
            dir: dir.join("sigstore").join(host),
        })
    }

    /// The directory of the set in use: the one `current` names, else the cache's own.
    fn set(&self) -> PathBuf {
        std::fs::read_to_string(self.dir.join(CURRENT_SET))
            .ok()
            .map(|name| name.trim().to_string())
            .filter(|name| name.starts_with("set-") && !name.contains(['/', '\\', '.']))
            .map_or_else(|| self.dir.clone(), |name| self.dir.join(name))
    }

    /// Reads and verifies what is cached; `None` when nothing is.
    pub fn read(&self, instance: &Instance) -> Result<Option<Cached>, String> {
        let set = self.set();
        let read = |name: &str| std::fs::read(set.join(name)).ok();
        let (Some(targets), Some(trusted_root)) = (read("targets.json"), read("trusted_root.json"))
        else {
            return Ok(None);
        };
        let first = tuf::Root::parse(&instance.initial)
            .map_err(|e| format!("the initial TUF root: {e}"))?
            .version;
        let mut roots = Vec::new();
        while let Some(bytes) = read(&format!("root/{}.json", first + 1 + roots.len() as u64)) {
            roots.push(bytes);
        }
        let verified = tuf::verify_cached(&instance.initial, &roots, &targets, &trusted_root)
            .map_err(|e| format!("{}: {e}", set.display()))?;
        Ok(Some(Cached {
            roots,
            trusted_root,
            verified,
        }))
    }

    /// The cached trust root, when it verifies and, for public-good Sigstore, is no older than
    /// the one in the binary.
    pub fn load(&self, instance: &Instance) -> Result<Option<TrustedRoot>, String> {
        let Some(cached) = self.read(instance)? else {
            return Ok(None);
        };
        if instance.public_good {
            let vendored = tuf::Root::parse(SIGSTORE_ROOT)
                .and_then(|root| root.verify_role("targets", SIGSTORE_TARGETS))
                .map(|t| t.version)
                .unwrap_or(0);
            if cached.verified.targets_version < vendored {
                return Ok(None);
            }
        }
        let source = format!(
            "the Sigstore trust root cached from {} (targets version {})",
            instance.mirror, cached.verified.targets_version
        );
        TrustedRoot::parse(&cached.trusted_root, &source).map(Some)
    }

    /// Writes a refreshed trust root. \[ADR 0061\] All of it, or none: the files go in a new set
    /// of their own, and `current` names it once they are there, with a rename, so a reader
    /// finds the old set or the new one, and never a new `targets.json` beside an old
    /// `trusted_root.json`. Refreshes at the same moment each write their own set, and the last
    /// to finish wins. Sets before the one this replaces are removed.
    pub fn write(
        &self,
        roots: &[Vec<u8>],
        first: u64,
        targets: &[u8],
        trusted_root: &[u8],
    ) -> Result<(), Failure> {
        let dir = &self.dir;
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let set = tempfile::Builder::new()
            .prefix("set-")
            .tempdir_in(dir)
            .with_context(|| format!("writing in {}", dir.display()))?;
        let write = |name: &str, bytes: &[u8]| -> Result<(), Failure> {
            let path = set.path().join(name);
            let parent = path.parent().expect("a file in the set");
            std::fs::create_dir_all(parent)
                .and_then(|()| std::fs::write(&path, bytes))
                .with_context(|| format!("writing {}", path.display()))?;
            Ok(())
        };
        for (i, root) in roots.iter().enumerate() {
            write(&format!("root/{}.json", first + 1 + i as u64), root)?;
        }
        write("targets.json", targets)?;
        write("trusted_root.json", trusted_root)?;
        let name = set
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .expect("a set's name")
            .to_string();
        let replaced = std::fs::read_to_string(dir.join(CURRENT_SET)).ok();
        let current = dir.join(CURRENT_SET);
        tempfile::NamedTempFile::new_in(dir)
            .and_then(|mut pointer| {
                use std::io::Write as _;
                pointer.write_all(format!("{name}\n").as_bytes())?;
                pointer.persist(&current).map_err(|e| e.error)?;
                Ok(())
            })
            .with_context(|| format!("writing {}", current.display()))?;
        let _ = set.keep();
        // A reader may still be in the set this one replaced; the ones before it, nobody is.
        let keep = [Some(name), replaced.map(|n| n.trim().to_string())];
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let old = entry.file_name().to_string_lossy().into_owned();
            if old.starts_with("set-") && !keep.iter().flatten().any(|n| *n == old) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    fn cache(dir: &Path) -> SigstoreCache {
        SigstoreCache {
            dir: dir.join("sigstore/host"),
        }
    }

    fn public_good() -> Instance {
        Instance {
            mirror: SIGSTORE_MIRROR.to_string(),
            initial: SIGSTORE_ROOT.to_vec(),
            public_good: true,
        }
    }

    const TRUSTED_ROOT: &[u8] = hushrun_attest::trust_root::PUBLIC_GOOD;

    // covers: T-12
    #[test]
    fn a_refresh_that_stops_partway_keeps_the_set_it_had() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());
        // A cache from before sets: the files at the top.
        std::fs::create_dir_all(&cache.dir).unwrap();
        std::fs::write(cache.dir.join("targets.json"), SIGSTORE_TARGETS).unwrap();
        std::fs::write(cache.dir.join("trusted_root.json"), TRUSTED_ROOT).unwrap();
        assert!(cache.read(&public_good()).unwrap().is_some());

        // A refresh that can't finish: what an older hushrun wrote beside the files, and a
        // `current` that can't be replaced.
        std::fs::create_dir_all(cache.dir.join(".trusted_root.json.tmp/x")).unwrap();
        std::fs::create_dir_all(cache.dir.join("current/x")).unwrap();
        assert!(cache.write(&[], 1, b"{}", b"{}").is_err());
        let still = cache.read(&public_good()).unwrap().expect("the set it had");
        assert_eq!(still.trusted_root, TRUSTED_ROOT);
        std::fs::remove_dir_all(cache.dir.join("current")).unwrap();

        // A whole one is read, and replaces the set before the last.
        for _ in 0..3 {
            cache.write(&[], 1, SIGSTORE_TARGETS, TRUSTED_ROOT).unwrap();
            assert!(cache.read(&public_good()).unwrap().is_some());
        }
        let sets = std::fs::read_dir(&cache.dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("set-"))
            .count();
        assert_eq!(sets, 2);
        let named = std::fs::read_to_string(cache.dir.join("current")).unwrap();
        assert!(cache.dir.join(named.trim()).join("targets.json").exists());
    }
}

/// \[M8\] Opens `url` in the person's browser: `$BROWSER`, else the system's opener. Failing to
/// is fine: the URL is printed too.
fn open_browser(browser: Option<&str>, url: &str) {
    let Some(argv) = browser_argv(browser, url, std::env::consts::OS) else {
        return;
    };
    let _ = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// The command that opens `url` on `os`: `$BROWSER` when set, else the system's opener. Each
/// takes the URL as one argument that no shell reads: on Windows, `cmd /C start` would end it at
/// its first `&`, and run what follows.
fn browser_argv(browser: Option<&str>, url: &str, os: &str) -> Option<Vec<String>> {
    let mut argv: Vec<String> = match browser {
        Some(browser) => browser.split_whitespace().map(str::to_string).collect(),
        None if os == "macos" => vec!["open".into()],
        None if os == "windows" => vec!["rundll32".into(), "url.dll,FileProtocolHandler".into()],
        None => vec!["xdg-open".into()],
    };
    if argv.is_empty() {
        return None;
    }
    argv.push(url.to_string());
    Some(argv)
}

fn reqwest_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hushrun_core::event::{EventKind, ListingRecord, Producer, RunInfo, SourceInfo, Started};
    use hushrun_core::names::ScopeName;
    use hushrun_core::run::{Custody, Provider, Trigger};

    fn started_at(time: Time, lease_until: Time) -> Event {
        Event {
            kind: EventKind::Started,
            repository: "github.com/acme/app".into(),
            scope: ScopeName::new("default").unwrap(),
            stage: None,
            canonicalization: hushrun_core::CANONICALIZATION.into(),
            source: SourceInfo::default(),
            run: RunInfo {
                provider: Provider::Local,
                id: "1".into(),
                attempt: 1,
                url: None,
                trigger: Trigger::Manual,
                job: None,
                job_id: None,
            },
            producer: Producer {
                tool: "hushrun".into(),
                version: "0".into(),
                custody: Custody::Ci,
                invoked_by: None,
            },
            time,
            policy_digest: Digest::of(b"policy"),
            forced: None,
            started: Some(Started {
                lease_until,
                listings: Some(vec![ListingRecord {
                    statement: "a signed listing".into(),
                    keys: None,
                }]),
            }),
            check: None,
            artifact: None,
            deployed: None,
            passed: None,
            failed: None,
            superseded: None,
            approval: None,
            checkpoint: Some("a checkpoint".into()),
        }
    }

    // covers: E-4, E-10
    #[test]
    fn events_are_stamped_when_signed_and_keep_their_lease() {
        let at = |text: &str| Time::parse(text).unwrap();
        let signed_at = at("2026-10-15T12:30:00Z");
        let signing = Signing {
            signer: Box::new(SigningKey::from_seed(&[1; 32])),
            timestamps: Box::new(NoTimestamps),
            timestamps_off: true,
            warned: AtomicBool::new(false),
            clock: Box::new(move || signed_at),
            root: TrustedRoot::public_good().clone(),
        };
        // Planned at noon with a one-minute lease, signed half an hour later.
        let event = started_at(at("2026-10-15T12:00:00Z"), at("2026-10-15T12:01:00Z"));
        let (stamped, bytes) = signing.sign(&event, &Digest::of(b"manifest")).unwrap();
        assert_eq!(stamped.time, signed_at);
        let started = stamped.started.as_ref().unwrap();
        assert_eq!(started.lease_until, at("2026-10-15T12:31:00Z"));
        // What `plan --start` recorded, and the checkpoint, are signed unchanged (E-4, E-5).
        assert_eq!(started.listings, event.started.as_ref().unwrap().listings);
        assert_eq!(stamped.checkpoint, event.checkpoint);
        assert!(bytes.starts_with(b"{\"mediaType\":"));
        // A time already later than the clock stays.
        let later = started_at(at("2026-10-15T13:00:00Z"), at("2026-10-15T14:00:00Z"));
        let (kept, _) = signing.sign(&later, &Digest::of(b"manifest")).unwrap();
        assert_eq!(kept, later);
    }

    #[test]
    fn the_sign_in_url_reaches_the_browser_whole() {
        let url = "https://issuer.example/auth?response_type=code&client_id=sigstore&state=s&code_challenge=c";
        assert_eq!(
            browser_argv(None, url, "windows").unwrap(),
            ["rundll32", "url.dll,FileProtocolHandler", url]
        );
        assert_eq!(browser_argv(None, url, "macos").unwrap(), ["open", url]);
        assert_eq!(browser_argv(None, url, "linux").unwrap(), ["xdg-open", url]);
        assert_eq!(
            browser_argv(Some("firefox --new-window"), url, "linux").unwrap(),
            ["firefox", "--new-window", url]
        );
        assert_eq!(browser_argv(Some("  "), url, "linux"), None);
    }
}

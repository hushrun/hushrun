//! What `hushrun login` keeps: for each host, the personal token it was given and the email the
//! token acts as, and the backend `hushrun run` uses when `HUSHRUN_BACKEND` isn't set. The file
//! is `<config dir>/hushrun/credentials.json`, or `HUSHRUN_CREDENTIALS`, and only its owner can
//! read it. Tokens never go anywhere but their own host.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow};
use serde::{Deserialize, Serialize};

use hushrun_core::ledger_url::LedgerUrl;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Credentials {
    /// The backend `hushrun run` uses by default: an org's URL, such as `https://host/o/acme`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// By origin (`https://host`): a personal token works for every org of its user there.
    #[serde(default)]
    pub hosts: BTreeMap<String, Login>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Login {
    pub token: String,
    pub email: String,
    /// \[ADR 0028\] The key id of this machine's key, which the sign-in registered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// \[ADR 0041\] The org the sign-in registered it in. A machine is registered per org, so any
    /// other org registers it on its first write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_org: Option<String>,
}

impl Credentials {
    /// `HUSHRUN_CREDENTIALS`, else `<config dir>/hushrun/credentials.json`.
    pub fn path() -> Option<PathBuf> {
        match hushrun_ci::var_os("HUSHRUN_CREDENTIALS") {
            Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
            _ => dirs::config_dir()
                .map(|dir| dir.join(hushrun_core::PRODUCT).join("credentials.json")),
        }
    }

    /// \[B-21\] The build blocks' settings, `build.json`, beside the credentials file: so
    /// `HUSHRUN_CREDENTIALS=/x/credentials.json` keeps them in `/x/build.json`. `hushrun run` and
    /// `hushrun build` both read it from here.
    pub fn build_settings_path() -> Option<PathBuf> {
        Credentials::path().map(|path| path.with_file_name("build.json"))
    }

    /// What was saved; nothing when there is no file.
    pub fn load() -> anyhow::Result<Credentials> {
        match Credentials::path() {
            Some(path) => Credentials::load_from(&path),
            None => Ok(Credentials::default()),
        }
    }

    /// \[ADR 0061\] Changes what was saved, with `change`, and saves it, while no other hushrun
    /// does: each sign-in and sign-out reads the file, changes one part and writes it all, so two
    /// at once would otherwise lose the first one's change. What `change` returns, and where the
    /// file is.
    pub fn update<R>(change: impl FnOnce(&mut Credentials) -> R) -> anyhow::Result<(R, PathBuf)> {
        let path = Credentials::path().ok_or_else(|| {
            anyhow!("there is no configuration directory to keep credentials in: set HUSHRUN_CREDENTIALS")
        })?;
        let answer = Credentials::update_at(&path, change)?;
        Ok((answer, path))
    }

    fn update_at<R>(path: &Path, change: impl FnOnce(&mut Credentials) -> R) -> anyhow::Result<R> {
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(".lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(name))
            .with_context(|| format!("locking {}", path.display()))?;
        // A file system that can't lock is no reason not to sign in: the change goes in as before.
        if let Err(err) = lock.lock() {
            tracing::debug!("can't lock {}: {err}", path.display());
        }
        let mut saved = Credentials::load_from(path)?;
        let answer = change(&mut saved);
        saved.save_to(path)?;
        Ok(answer)
    }

    fn load_from(path: &Path) -> anyhow::Result<Credentials> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("reading {}", path.display())),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Credentials::default()),
            Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Writes the file whole, readable by its owner only.
    fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let temp = dir.join(format!(".credentials-{}.tmp", std::process::id()));
        let written = (|| -> io::Result<()> {
            let mut file = create_private(&temp)?;
            file.write_all(&serde_json::to_vec_pretty(self)?)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            std::fs::rename(&temp, path)
        })();
        if let Err(err) = written {
            let _ = std::fs::remove_file(&temp);
            return Err(err).with_context(|| format!("writing {}", path.display()));
        }
        Ok(())
    }

    /// The login for `url`'s host.
    pub fn login_for(&self, url: &LedgerUrl) -> Option<&Login> {
        self.login_at(&origin(url))
    }

    /// \[B-25\] Whether a person is signed in on this machine for `origin`, whatever credential
    /// the command itself carries: what tells a person's machine, whose build settings an
    /// agent's run follows, from a sandbox. It gives no token: a login is read through
    /// [`Credentials::login_at`].
    pub fn person_at(&self, origin: &str) -> bool {
        self.hosts.contains_key(origin)
    }

    /// The login for `origin`. \[ADR 0046\] With an agent set there is none, as if signed out,
    /// whatever the file holds: no person's login speaks for an agent, and every reader of a
    /// login comes through here.
    pub fn login_at(&self, origin: &str) -> Option<&Login> {
        if hushrun_ci::Env::from_process().agent_set() {
            return None;
        }
        self.hosts.get(origin)
    }
}

/// `scheme://host[:port]` of `url`: where a personal token is good.
pub fn origin(url: &LedgerUrl) -> String {
    let text = url.to_string();
    let after = text.find("://").map_or(0, |i| i + 3);
    match text[after..].find('/') {
        Some(slash) => text[..after + slash].to_string(),
        None => text,
    }
}

#[cfg(unix)]
pub(crate) fn create_private(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
pub(crate) fn create_private(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(test)]
mod tests {
    use super::{Credentials, Login, origin};
    use hushrun_core::ledger_url::LedgerUrl;

    // covers: C-13
    #[test]
    fn sign_ins_at_the_same_moment_keep_each_others_logins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config/credentials.json");
        let threads: Vec<_> = (0..16)
            .map(|n| {
                let path = path.clone();
                std::thread::spawn(move || {
                    Credentials::update_at(&path, |saved| {
                        // As long as a sign-in takes between reading and writing.
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        saved.hosts.insert(
                            format!("https://host-{n}.example"),
                            Login {
                                token: format!("hru_{n}"),
                                email: "ana@acme.dev".into(),
                                machine: None,
                                machine_org: None,
                            },
                        );
                    })
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap().unwrap();
        }
        assert_eq!(Credentials::load_from(&path).unwrap().hosts.len(), 16);
    }

    #[test]
    fn a_token_is_good_for_its_origin() {
        let url = |text: &str| LedgerUrl::parse(text).unwrap();
        assert_eq!(
            origin(&url("https://Hushrun.Example/o/acme")),
            "https://hushrun.example"
        );
        assert_eq!(
            origin(&url("http://127.0.0.1:8787/o/acme")),
            "http://127.0.0.1:8787"
        );
        assert_eq!(
            origin(&url("https://hushrun.example")),
            "https://hushrun.example"
        );
    }
}

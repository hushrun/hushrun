//! \[C-18\] This machine's key (ADR 0028): an ECDSA P-256 key hushrun makes the first time the
//! machine signs in, kept beside the credentials file and never in a repository. Every sign-in
//! registers it with proof that the machine holds it; results and ledger events from this
//! machine carry its signature, so the server takes a person's or an agent's writes only from a
//! machine it registered. What counts for a promotion stays the repository's trust: a machine's
//! events count only where hushrun.yaml's `trust:` section on the default branch lists its key.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow};
use base64::Engine as _;

use hushrun_attest::SigningKey;
use hushrun_core::digest::Digest;
use hushrun_core::signers::PublicKey;

use crate::credentials::Credentials;

/// The key's file beside the credentials file.
pub const FILE: &str = "machine-key.pem";

/// This machine's key, and where it came from.
pub struct Machine {
    pub key: SigningKey,
    /// `HUSHRUN_AGENT`, `HUSHRUN_MACHINE_KEY`, `HUSHRUN_MACHINE_KEY_FILE`, or the file hushrun
    /// keeps.
    pub source: String,
}

impl Machine {
    pub fn public_key(&self) -> PublicKey {
        self.key.public_key()
    }

    pub fn keyid(&self) -> String {
        self.public_key().keyid()
    }

    /// The proof a device sign-in sends: the key's signature over
    /// `hushrun-machine/v1\n<device_code>\n<keyid>\n`.
    pub fn device_proof(&self, device_code: &str) -> String {
        self.prove(device_code)
    }

    /// The proof `POST {base}/v0/machines` sends for `token`: over
    /// `hushrun-machine/v1\n<hex of SHA-256 of the token>\n<keyid>\n`.
    pub fn token_proof(&self, token: &str) -> String {
        self.prove(&Digest::of(token.as_bytes()).hex())
    }

    /// \[ADR 0028, amended 2026-10-28\] The proof an agent token's creation sends for this key,
    /// the agent's: the key's signature over
    /// `hushrun-agent-machine/v1\n<org>\n<agent>\n<keyid>\n`.
    pub fn agent_proof(&self, org: &str, agent: &str) -> String {
        let message = format!(
            "hushrun-agent-machine/v1\n{org}\n{agent}\n{}\n",
            self.keyid()
        );
        base64::engine::general_purpose::STANDARD.encode(self.key.sign(message.as_bytes()))
    }

    fn prove(&self, what: &str) -> String {
        let message = format!("hushrun-machine/v1\n{what}\n{}\n", self.keyid());
        base64::engine::general_purpose::STANDARD.encode(self.key.sign(message.as_bytes()))
    }

    /// `Hushrun-Signature` for a result recorded under `key` (64 hex) with `body`:
    /// `<keyid> <base64 signature over hushrun-result/v1\n<key>\n<hex of SHA-256 of body>\n>`.
    pub fn result_signature(&self, key: &str, body: &[u8]) -> String {
        let message = format!("hushrun-result/v1\n{key}\n{}\n", Digest::of(body).hex());
        format!(
            "{} {}",
            self.keyid(),
            base64::engine::general_purpose::STANDARD.encode(self.key.sign(message.as_bytes()))
        )
    }
}

/// The machine's name, as the dashboard lists it: its host name.
pub fn name() -> String {
    std::process::Command::new("hostname")
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|h| h.trim().chars().take(100).collect::<String>())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "this machine".into())
}

/// Where hushrun keeps the key: `HUSHRUN_MACHINE_KEY_FILE`, else beside the credentials file.
/// \[ADR 0046\] That one is a person's, so an agent no string can be read for doesn't get it.
fn path() -> Option<PathBuf> {
    match hushrun_ci::var_os("HUSHRUN_MACHINE_KEY_FILE") {
        Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ if hushrun_ci::Env::from_process().agent_unreadable() => None,
        _ => Credentials::path().map(|p| p.with_file_name(FILE)),
    }
}

/// This machine's key, when it has one: \[ADR 0046\] the agent's in `HUSHRUN_AGENT`, else
/// `HUSHRUN_MACHINE_KEY` (the PEM itself, for sandboxes that keep no files), else the key file.
/// Nothing when there is none yet.
pub fn load() -> anyhow::Result<Option<Machine>> {
    if let Some(agent) = hushrun_ci::Env::from_process().agent() {
        // Fixed words, as for the rest of the string: nothing of the key in a message.
        let key = SigningKey::from_pem(agent.key())
            .map_err(|_| anyhow!(crate::results::STRING_NEEDS_REPLACING))?;
        return Ok(Some(Machine {
            key,
            source: hushrun_core::agent::VARIABLE.into(),
        }));
    }
    if let Ok(pem) = hushrun_ci::var("HUSHRUN_MACHINE_KEY")
        && !pem.trim().is_empty()
    {
        let key = SigningKey::from_pem(&pem).map_err(|e| anyhow!("HUSHRUN_MACHINE_KEY: {e}"))?;
        return Ok(Some(Machine {
            key,
            source: "HUSHRUN_MACHINE_KEY".into(),
        }));
    }
    let Some(path) = path() else {
        return Ok(None);
    };
    match std::fs::read_to_string(&path) {
        Ok(pem) => {
            refuse_inside_a_repository(&path)?;
            let key = SigningKey::from_pem(&pem).map_err(|e| anyhow!("{}: {e}", path.display()))?;
            Ok(Some(Machine {
                key,
                source: path.display().to_string(),
            }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The key the environment names: \[ADR 0046\] the one in `HUSHRUN_AGENT`, `HUSHRUN_MACHINE_KEY`,
/// or the file `HUSHRUN_MACHINE_KEY_FILE` names. An agent signs only with one (ADR 0028, amended
/// 2026-10-28), never with a key file a person's sign-in left on the machine. Nothing when none
/// is set.
pub fn named() -> anyhow::Result<Option<Machine>> {
    let set = |name: &str| hushrun_ci::var_os(name).is_some_and(|value| !value.is_empty());
    if !set("HUSHRUN_MACHINE_KEY") && !set("HUSHRUN_MACHINE_KEY_FILE") {
        return Ok(None);
    }
    match load()? {
        Some(machine) => Ok(Some(machine)),
        None => Err(anyhow!(
            "HUSHRUN_MACHINE_KEY_FILE names {}, which isn't there: {}",
            path().map(|p| p.display().to_string()).unwrap_or_default(),
            crate::results::AGENT_NEEDS_ITS_KEY
        )),
    }
}

/// This machine's key, made the first time: a P-256 key in a file only its owner can read,
/// written through a temporary file and a rename.
pub fn load_or_create() -> anyhow::Result<Machine> {
    if let Some(machine) = load()? {
        return Ok(machine);
    }
    let path = path().ok_or_else(|| {
        anyhow!("there is no configuration directory to keep this machine's key in: set HUSHRUN_MACHINE_KEY_FILE")
    })?;
    refuse_inside_a_repository(&path)?;
    let key = SigningKey::generate_p256().map_err(|e| anyhow!("making this machine's key: {e}"))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let temp = dir.join(format!(".machine-key-{}.tmp", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut file = crate::credentials::create_private(&temp)?;
        file.write_all(key.to_pem().as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)
    })();
    if let Err(err) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(err).with_context(|| format!("writing {}", path.display()));
    }
    tracing::info!("made this machine's key: {}", path.display());
    Ok(Machine {
        key,
        source: path.display().to_string(),
    })
}

/// A machine key inside the repository hushrun runs in could be committed, or reach a check an
/// isolated run exports: hushrun refuses one (ADR 0028 item 1, ADR 0018 item 4).
fn refuse_inside_a_repository(path: &Path) -> anyhow::Result<()> {
    let Ok(cwd) = std::env::current_dir() else {
        return Ok(());
    };
    let Ok(hushrun_vcs::Detected::Git { root }) = hushrun_vcs::detect(&cwd) else {
        return Ok(());
    };
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let canonical = |p: &Path| {
        p.ancestors()
            .find_map(|a| {
                a.canonicalize()
                    .ok()
                    .map(|c| c.join(p.strip_prefix(a).unwrap_or(Path::new(""))))
            })
            .unwrap_or_else(|| p.to_path_buf())
    };
    if canonical(&absolute).starts_with(canonical(&root)) {
        return Err(anyhow!(
            "{} is inside the git repository at {}: a machine's key must never be committed. Keep it in hushrun's configuration directory (the default), or set HUSHRUN_MACHINE_KEY_FILE elsewhere",
            path.display(),
            root.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proofs_and_signatures_verify_with_the_public_key() {
        let machine = Machine {
            key: SigningKey::generate_p256().unwrap(),
            source: "test".into(),
        };
        let b64 = |text: &str| {
            base64::engine::general_purpose::STANDARD
                .decode(text)
                .unwrap()
        };
        let keyid = machine.keyid();
        let proof = b64(&machine.device_proof("device-1"));
        let message = format!("hushrun-machine/v1\ndevice-1\n{keyid}\n");
        assert!(hushrun_attest::keys::verify_signature(
            &machine.public_key(),
            message.as_bytes(),
            &proof
        ));
        let token = "hru_x";
        let proof = b64(&machine.token_proof(token));
        let message = format!(
            "hushrun-machine/v1\n{}\n{keyid}\n",
            Digest::of(token.as_bytes()).hex()
        );
        assert!(hushrun_attest::keys::verify_signature(
            &machine.public_key(),
            message.as_bytes(),
            &proof
        ));
        let header = machine.result_signature("ab", b"{}");
        let (id, sig) = header.split_once(' ').unwrap();
        assert_eq!(id, keyid);
        let message = format!("hushrun-result/v1\nab\n{}\n", Digest::of(b"{}").hex());
        assert!(hushrun_attest::keys::verify_signature(
            &machine.public_key(),
            message.as_bytes(),
            &b64(sig)
        ));
    }
}

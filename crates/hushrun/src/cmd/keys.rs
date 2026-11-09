//! `hushrun keys generate` and `hushrun keys public`: Ed25519 and ECDSA P-256 key files, for
//! trust.yaml (E-2).

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context as _, anyhow};
use serde_json::json;

use hushrun_attest::SigningKey;
use hushrun_core::signers::PublicKey;

use crate::app::App;
use crate::cli::{KeyAlgorithm, KeysCommand};
use crate::exit::{CmdResult, Exit, Failure};
use crate::output::{self, Format, Outputs};

pub fn run(app: &App<'_>, command: &KeysCommand) -> CmdResult {
    match command {
        KeysCommand::Generate { out, algorithm } => generate(app, out.as_deref(), *algorithm),
        KeysCommand::Public { key } => public(app, key.as_deref()),
    }
}

fn generate(app: &App<'_>, out: Option<&Path>, algorithm: KeyAlgorithm) -> CmdResult {
    let key = match algorithm {
        KeyAlgorithm::Ed25519 => SigningKey::generate(),
        KeyAlgorithm::EcdsaP256 => SigningKey::generate_p256(),
    }
    .map_err(|e| Failure::new(Exit::Unexpected, e))?;
    let pem = key.to_pem();
    match out {
        Some(path) => write_private(path, pem.as_bytes())?,
        None => {
            if app.global.format != Format::Text {
                return Err(Failure::usage(anyhow!(
                    "--format {:?} prints no private key: pass --out FILE",
                    app.global.format
                )));
            }
            // The key goes to stdout for redirection; everything else to stderr.
            output::print(&pem)?;
            eprintln!("public key: {}\nkey id: {}", key.public_key(), key.keyid());
            return Ok(Exit::Ok);
        }
    }
    describe(app, &key.public_key(), out)
}

fn public(app: &App<'_>, file: Option<&Path>) -> CmdResult {
    let public = app
        .envelope_signer(file)?
        .public_key()
        .ok_or_else(|| Failure::usage(anyhow!("this signer has no key")))?;
    describe(app, &public, None)
}

fn describe(app: &App<'_>, key: &PublicKey, file: Option<&Path>) -> CmdResult {
    let mut outputs = Outputs::new();
    outputs.set("public_key", key.to_string());
    outputs.set("keyid", key.keyid());
    match app.global.format {
        Format::Text => {
            let mut text = format!("public key: {key}\nkey id: {}\n", key.keyid());
            if let Some(file) = file {
                text.push_str(&format!("private key: {} (mode 0600)\n", file.display()));
            }
            output::print(&text)?;
        }
        Format::Json => output::print_json(&json!({
            "schema": "hushrun.keys/v1",
            "publicKey": key.to_string(),
            "keyid": key.keyid(),
            "file": file.map(|f| f.display().to_string()),
        }))?,
        Format::Env => output::print(&outputs.to_env())?,
        Format::Dotenv => output::print(&outputs.to_dotenv()?)?,
    }
    Ok(Exit::Ok)
}

/// Creates `path` with mode 0600; never overwrites a key.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            Failure::usage(anyhow!(
                "{} exists; hushrun never overwrites a key",
                path.display()
            ))
        } else {
            Failure::new(
                Exit::Unexpected,
                anyhow!("creating {}: {err}", path.display()),
            )
        }
    })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

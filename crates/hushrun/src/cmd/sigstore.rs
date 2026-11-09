//! `hushrun sigstore refresh` (T-12): updates the Sigstore trust root from its TUF repository,
//! verified, into the cache, where commands that verify read it offline.

use anyhow::anyhow;
use serde_json::json;

use hushrun_attest::TrustedRoot;
use hushrun_attest::remote::tuf::refresh as tuf_refresh;
use hushrun_attest::tuf::{Root, SIGSTORE_ROOT, SIGSTORE_TARGETS};

use crate::app::App;
use crate::cli::SigstoreCommand;
use crate::exit::{CmdResult, Exit, Failure};
use crate::output::{self, Format};
use crate::signing::{SigstoreCache, sign_failure};

pub fn run(app: &App<'_>, command: &SigstoreCommand) -> CmdResult {
    match command {
        SigstoreCommand::Refresh => refresh(app),
    }
}

fn refresh(app: &App<'_>) -> CmdResult {
    let instance = app.sigstore_instance()?;
    let cache = SigstoreCache::for_instance(&instance).ok_or_else(|| {
        Failure::usage(anyhow!(
            "HUSHRUN_CACHE_DIR is empty, which turns caching off: there is nowhere to keep the trust root"
        ))
    })?;
    let cached = match cache.read(&instance) {
        Ok(cached) => cached,
        Err(why) => {
            tracing::warn!("replacing the cached trust root, which doesn't verify: {why}");
            None
        }
    };
    // Never older than what is cached, or than the snapshot in the binary.
    let mut floor = cached.as_ref().map(|c| c.verified.targets_version);
    if instance.public_good {
        let vendored = Root::parse(SIGSTORE_ROOT)
            .and_then(|root| root.verify_role("targets", SIGSTORE_TARGETS))
            .map(|targets| targets.version)
            .ok();
        floor = floor.max(vendored);
    }
    let roots = cached.map(|c| c.roots).unwrap_or_default();
    let refreshed = tuf_refresh(&instance.mirror, &instance.initial, &roots, floor, app.now)
        .map_err(sign_failure)?;
    let first = Root::parse(&instance.initial)
        .map_err(|e| Failure::usage(anyhow!("the initial TUF root: {e}")))?
        .version;
    cache.write(
        &refreshed.roots,
        first,
        &refreshed.targets,
        &refreshed.trusted_root,
    )?;
    let root = TrustedRoot::parse(&refreshed.trusted_root, "the refreshed trust root")
        .map_err(|e| Failure::usage(anyhow!(e)))?;
    match app.global.format {
        Format::Json => output::print_json(&json!({
            "schema": "hushrun.sigstore.refresh/v1",
            "repository": instance.mirror,
            "rootVersion": refreshed.root_version,
            "targetsVersion": refreshed.targets_version,
            "trustedRoot": root.digest,
            "certificateAuthorities": root.certificate_authorities.len(),
            "timestampAuthorities": root.timestamp_authorities.len(),
            "cache": cache.dir.display().to_string(),
        }))?,
        _ => output::print(&format!(
            "cached the trust root of {} (TUF root version {}, targets version {}, {}): {} certificate authorities, {} timestamp authorities, in {}\n",
            instance.mirror,
            refreshed.root_version,
            refreshed.targets_version,
            root.digest,
            root.certificate_authorities.len(),
            root.timestamp_authorities.len(),
            cache.dir.display()
        ))?,
    }
    Ok(Exit::Ok)
}

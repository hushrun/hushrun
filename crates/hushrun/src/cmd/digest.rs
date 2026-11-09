//! `hushrun digest` (§10.4): prints the source digest of each scope.

use std::fmt::Write as _;
use std::time::Instant;

use anyhow::Context as _;
use serde_json::json;

use hushrun_digest::CANONICALIZATION;
use hushrun_vcs::{ExplainEntry, Snapshot, digest_scopes};

use crate::cli::{DigestArgs, GlobalOpts};
use crate::context::{Content, Source};
use crate::exit::{CmdResult, Exit, Failure};
use crate::output::{self, Format, Outputs};

pub fn run(global: &GlobalOpts, args: &DigestArgs) -> CmdResult {
    let started = Instant::now();
    let source = match (&args.dir, args.worktree, &args.rev) {
        (Some(dir), _, _) => Source::Dir(dir),
        (None, true, _) => Source::Repo(Snapshot::Worktree),
        (None, false, rev) => {
            Source::Repo(Snapshot::Rev(rev.clone().unwrap_or_else(|| "HEAD".into())))
        }
    };
    let content = Content::open(global, source)?;
    let selected = content.select(&args.scopes, true)?;
    if args.manifest_out.is_some() && selected.len() != 1 {
        return Err(Failure::usage(anyhow::anyhow!(
            "--manifest-out writes one manifest: select exactly one scope with --scope"
        )));
    }
    let run = digest_scopes(
        content.vcs.as_ref(),
        &content.listing,
        &content.scopes,
        &selected,
        args.explain,
    )?;
    let elapsed = started.elapsed();

    if let Some(path) = &args.manifest_out {
        std::fs::write(path, &run.scopes[0].manifest)
            .with_context(|| format!("writing {}", path.display()))?;
    }

    let revision = &content.listing.revision;
    let mut outputs = Outputs::new();
    if let [only] = run.scopes.as_slice() {
        outputs.set("digest", only.digest.to_string());
    } else {
        for scope in &run.scopes {
            outputs.set(
                format!("digest_{}", scope.scope.env_key()),
                scope.digest.to_string(),
            );
        }
    }
    if let Some(rev) = &revision.revision {
        outputs.set("revision", rev);
    }
    if let Some(path) = &global.dotenv {
        outputs.write_dotenv(path)?;
    }
    let mut summary = String::from("### hushrun digest\n\n| Scope | Digest |\n|---|---|\n");
    for scope in &run.scopes {
        summary.push_str(&format!("| {} | `{}` |\n", scope.scope, scope.digest));
    }
    crate::ci::Ci::from_process(global).report(&outputs, &summary)?;

    match global.format {
        Format::Text => {
            let mut text = String::new();
            if let Some(entries) = &run.explain {
                text.push_str(&explain_text(entries));
            }
            for scope in &run.scopes {
                let _ = writeln!(text, "{}  {}", scope.digest, scope.scope);
            }
            output::print(&text)?;
        }
        Format::Json => {
            let mut value = json!({
                "schema": "hushrun.digest/v1",
                "canonicalization": CANONICALIZATION,
                "source": revision,
                "scopes": run.scopes.iter().map(|s| json!({
                    "scope": s.scope,
                    "digest": s.digest,
                    "files": s.files,
                    "bytes": s.bytes,
                })).collect::<Vec<_>>(),
                "stats": {
                    "entries": content.listing.entries.len(),
                    "cacheHits": run.stats.cache_hits,
                    "hashed": run.stats.hashed,
                    "hashedBytes": run.stats.hashed_bytes,
                    "elapsedMs": elapsed.as_millis() as u64,
                },
            });
            if let Some(entries) = &run.explain {
                value["entries"] = serde_json::to_value(entries).map_err(anyhow::Error::from)?;
            }
            output::print_json(&value)?;
        }
        Format::Env => output::print(&outputs.to_env())?,
        Format::Dotenv => output::print(&outputs.to_dotenv()?)?,
    }
    tracing::info!(
        "{} entries, {} from cache, {} hashed ({} bytes) in {} ms",
        content.listing.entries.len(),
        run.stats.cache_hits,
        run.stats.hashed,
        run.stats.hashed_bytes,
        elapsed.as_millis()
    );
    Ok(Exit::Ok)
}

fn explain_text(entries: &[ExplainEntry]) -> String {
    let mut text = String::new();
    for entry in entries {
        let hash = entry.hash.as_deref().unwrap_or("-");
        let _ = write!(text, "{} {hash:<64} {}", entry.kind, entry.path);
        for scope in &entry.scopes {
            let why = match (&scope.included_by, &scope.excluded_by) {
                (Some(_), Some(ex)) => format!("out, excluded by {ex}"),
                (Some(inc), None) => format!("in, by {inc}"),
                (None, _) => "out, no include matches".to_string(),
            };
            let _ = write!(text, "  [{}: {why}]", scope.scope);
        }
        text.push('\n');
    }
    text
}

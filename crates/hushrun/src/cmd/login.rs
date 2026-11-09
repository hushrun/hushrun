//! `hushrun login` and `hushrun logout`: signing the CLI in to a backend, so that `hushrun run`
//! shares results with an org. The CLI asks the host for a code, a person signed in to the
//! dashboard approves it, and the CLI keeps the personal token it then collects (see
//! `credentials.rs`).

use std::path::PathBuf;

use anyhow::anyhow;

use hushrun_core::ledger_url::LedgerUrl;
use hushrun_vcs::Detected;

use crate::cli::{GlobalOpts, LoginArgs, LogoutArgs};
use crate::context;
use crate::credentials::{self, Credentials};
use crate::exit::{CmdResult, Exit, Failure};
use crate::hosted::{self, open};
use crate::results::{NamedBy, error_message, plain};

pub fn login(global: &GlobalOpts, args: &LoginArgs) -> CmdResult {
    let (url, named) = target(global, args.url.as_deref())?;
    let origin = credentials::origin(&url);
    if let Some(named) = named {
        eprintln!("Signing in to {url}, {named}.");
    }
    // An org's URL names the org the sign-in is for, and stays the backend whatever the
    // server answers; otherwise the server names the org the person approved it for.
    let org = hosted::org_of(&url.to_string());
    let before = Credentials::load()?.backend;
    let signed = hosted::sign_in(&origin, org.as_deref(), args.no_browser)?;
    let backend = match org {
        Some(_) => Some(url.to_string()),
        None => signed.backend.clone(),
    };
    // Whether the token reaches the org. \[ADR 0061\] An org that says the person isn't one of
    // its members doesn't become where results go: the backend stays what it was.
    let access = backend.as_deref().map(hosted::org_info);
    let outsider = matches!(
        &access,
        Some(Err(err)) if err.downcast_ref::<hosted::NotAMember>().is_some()
    );
    let kept = match outsider {
        true => before.clone(),
        false => backend.clone(),
    };
    Credentials::update(|saved| saved.backend = kept)?;

    let (Some(backend), Some(access)) = (backend, access) else {
        println!("Signed in to {origin} as {}.", signed.email);
        // A server that names its org at another origin is no help: the token is for this one.
        match signed.org.as_deref() {
            // Approved for no org: the person may have none yet.
            None => eprintln!(
                "warning: the sign-in wasn't approved for an organization, so results stay on this machine: create or join one {}, then run hushrun login {origin}/o/<org> to share them",
                signed.dashboard.as_deref().map_or_else(
                    || format!("in {origin}'s dashboard"),
                    |dashboard| format!("at {dashboard}/new")
                )
            ),
            Some(org) => eprintln!(
                "warning: {origin} named no URL here for {org}, so results stay on this machine: run hushrun login {origin}/o/{org} to share them"
            ),
        }
        return Ok(Exit::Ok);
    };
    match access {
        Ok(info) => println!(
            "Signed in to {} as {}: hushrun run now shares results there.",
            info.org, signed.email
        ),
        Err(err) if outsider => {
            let still = before.map_or_else(
                || "results stay on this machine".to_string(),
                |before| format!("results still go to {before}"),
            );
            eprintln!(
                "warning: signed in as {}, but {backend} refused it, so {still}: {err:#}",
                signed.email
            );
        }
        Err(err) => eprintln!(
            "warning: signed in as {}, but {backend} refused it: {err:#}",
            signed.email
        ),
    }
    Ok(Exit::Ok)
}

pub fn logout(global: &GlobalOpts, args: &LogoutArgs) -> CmdResult {
    let saved = Credentials::load()?;
    if args.url.is_none() && saved.hosts.is_empty() {
        println!("You aren't signed in anywhere.");
        return Ok(Exit::Ok);
    }
    let (url, _) = target(global, args.url.as_deref())?;
    let origin = credentials::origin(&url);
    let Some(login) = saved.hosts.get(&origin).cloned() else {
        println!("You aren't signed in to {origin}.");
        return Ok(Exit::Ok);
    };
    // Revoked, a copy of the file is worth nothing either.
    let revoked = open(&origin, Some(&login.token)).and_then(|host| {
        host.json_request("POST", "cli/logout", None)
            .map_err(|e| Failure::new(Exit::Ledger, plain(e)))
    });
    match revoked {
        Ok((200..=299, _)) => {}
        Ok((status, body)) => eprintln!(
            "warning: {origin} didn't revoke the token ({}); it's forgotten here",
            error_message(&body).unwrap_or_else(|| status.to_string())
        ),
        Err(failure) => eprintln!(
            "warning: {origin} didn't revoke the token ({:#}); it's forgotten here",
            failure.error
        ),
    }
    // \[ADR 0061\] What the file holds now, which another sign-in may have changed meanwhile.
    Credentials::update(|saved| {
        // A sign-in there since this one began keeps its own token, and the backend.
        if saved
            .hosts
            .get(&origin)
            .is_none_or(|l| l.token != login.token)
        {
            return;
        }
        saved.hosts.remove(&origin);
        if saved
            .backend
            .as_deref()
            .and_then(|b| LedgerUrl::parse(b).ok())
            .is_some_and(|b| credentials::origin(&b) == origin)
        {
            saved.backend = None;
        }
    })?;
    println!("Signed out of {origin} ({}).", login.email);
    Ok(Exit::Ok)
}

/// The backend to sign in to (C-13), and what named it when the command line didn't: URL;
/// else `HUSHRUN_BACKEND`; else the backend the repository's package.json names; else the last
/// one; else hushrun's hosted backend. The order is `hushrun run`'s (C-11), so that a login
/// reaches the backend a run then uses.
fn target(
    global: &GlobalOpts,
    given: Option<&str>,
) -> Result<(LedgerUrl, Option<&'static str>), Failure> {
    if let Some(text) = given {
        return Ok((parse(text)?, None));
    }
    if let Some(text) = hushrun_ci::var("HUSHRUN_BACKEND")
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        return Ok((parse(&text)?, Some("which HUSHRUN_BACKEND names")));
    }
    // A repository's file chooses where a person signs in: say which file did.
    if let Some((text, named_by)) =
        repository_root(global).and_then(|root| hosted::named_backend(&root))
    {
        let source = match named_by {
            NamedBy::Policy => "which this repository's hushrun.yaml names",
            _ => "which this repository's package.json names",
        };
        return Ok((parse(&text)?, Some(source)));
    }
    if let Some(text) = Credentials::load()?.backend {
        return Ok((parse(&text)?, Some("where you signed in last")));
    }
    Ok((parse(&hosted::origin())?, Some("hushrun's hosted backend")))
}

/// The top directory of the git repository hushrun runs in, if it runs in one.
pub(crate) fn repository_root(global: &GlobalOpts) -> Option<PathBuf> {
    let start = context::start_dir(global).ok()?;
    match hushrun_vcs::detect(&start) {
        Ok(Detected::Git { root }) => Some(root),
        _ => None,
    }
}

fn parse(text: &str) -> Result<LedgerUrl, Failure> {
    LedgerUrl::parse(text.trim()).map_err(|e| {
        Failure::usage(anyhow!(
            "hushrun login signs in to an https:// backend, such as https://hushrun.example/o/acme: {e}"
        ))
    })
}

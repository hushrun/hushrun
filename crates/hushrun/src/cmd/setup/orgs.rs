//! Which of a person's orgs a repository goes to (C-15, ADR 0041). A repository's own files, or
//! `--org`, name it when they can. Otherwise it is the person's to say: the org they signed in to
//! last is a suggestion, never the answer, because that is how every repository of someone with
//! several orgs used to land in one.

use std::io::{BufRead, IsTerminal as _, Write};

use anyhow::anyhow;

use crate::cli::InitArgs;
use crate::exit::{Exit, Failure};
use crate::hosted::{self, MyOrg, MyOrgs, OrgList};

/// Where the org's URL goes in a plan when several orgs could be it.
const UNCHOSEN_BACKEND: &str = "<your org's URL, once you choose it>";

/// The URL of the org a repository that names none goes to, for someone with a login at `origin`:
/// their only one; of several, the one they choose at a terminal; without one to ask on, the one
/// the repository's owner is named like, or a refusal that lists them. `None` when a sign-in has
/// to settle it: there is no login, it stopped working, or they belong to no org.
pub fn choose(
    repository: Option<&str>,
    origin: &str,
    args: &InitArgs,
) -> Result<Option<String>, Failure> {
    let mine = match hosted::my_orgs(origin, None) {
        OrgList::Orgs(mine) => mine,
        OrgList::SignedOut => return Ok(None),
        OrgList::Older => return Ok(hosted::saved_backend(origin)),
        OrgList::Failed(why) => {
            return Err(Failure::new(
                Exit::Ledger,
                anyhow!(
                    "{why}: init can't tell which of your organizations this repository is for, and nothing changed. Try again, or add --org <slug>"
                ),
            ));
        }
    };
    let orgs = &mine.orgs;
    let url = |org: &MyOrg| Some(hosted::org_url(origin, &org.slug));
    match orgs.as_slice() {
        [] => return Ok(None),
        [only] => return Ok(url(only)),
        _ => {}
    }
    let owners = owners_org(orgs, repository);
    if !args.yes && std::io::stdin().is_terminal() {
        let last = last_org(orgs, origin);
        println!("\n{}", question(repository, orgs.len()));
        for line in rows(&mine) {
            println!("{line}");
        }
        let index = ask(
            &mut std::io::stdin().lock(),
            &mut std::io::stdout(),
            orgs,
            owners.or(last),
        )
        .map_err(|err| Failure::new(Exit::Unexpected, err))?
        .ok_or_else(|| {
            Failure::usage(anyhow!(
                "no organization chosen, so nothing changed: run npx hushrun init again, or add --org <slug>"
            ))
        })?;
        return Ok(url(&orgs[index]));
    }
    let Some(index) = owners else {
        return Err(Failure::usage(anyhow!(
            "{} is for which of your organizations? init can't ask here: add --org <slug>, with one of {}",
            repository.unwrap_or("this repository"),
            slugs(orgs)
        )));
    };
    println!(
        "\nUsing {}, the organization named like the repository's owner: --org <slug> picks another of {}.",
        orgs[index].slug,
        slugs(orgs)
    );
    Ok(url(&orgs[index]))
}

/// What a plan shows for the org when nothing is decided or written: as [`choose`], but it asks
/// nothing; and, when init would ask, what it says of that.
pub fn preview(repository: Option<&str>, origin: &str) -> (Option<String>, Option<String>) {
    let mine = match hosted::my_orgs(origin, Some(hosted::PLAN_BUDGET)) {
        OrgList::Orgs(mine) => mine,
        OrgList::SignedOut => return (None, None),
        OrgList::Older => return (hosted::saved_backend(origin), None),
        OrgList::Failed(why) => {
            tracing::debug!("{origin}: reading the login's organizations: {why}");
            return (Some(UNCHOSEN_BACKEND.to_string()), None);
        }
    };
    let orgs = &mine.orgs;
    match orgs.as_slice() {
        [] => (None, None),
        [only] => (Some(hosted::org_url(origin, &only.slug)), None),
        _ => {
            let said = format!(
                "You belong to {} organizations: init asks which one this repository records to, or takes it from --org <slug>, one of {}.",
                orgs.len(),
                slugs(orgs)
            );
            let backend = owners_org(orgs, repository).map_or_else(
                || UNCHOSEN_BACKEND.to_string(),
                |index| hosted::org_url(origin, &orgs[index].slug),
            );
            (Some(backend), Some(said))
        }
    }
}

/// `init --org`'s refusal for an org the person isn't in: their orgs, when the backend lists them.
pub fn not_yours(org: &str, email: &str, origin: &str) -> Failure {
    let yours = match hosted::my_orgs(origin, None) {
        OrgList::Orgs(mine) if !mine.orgs.is_empty() => {
            format!("yours are {}", slugs(&mine.orgs))
        }
        _ => "check the slug, or ask an admin of it to invite you".to_string(),
    };
    Failure::usage(anyhow!("{email} isn't a member of {org}: {yours}"))
}

fn slugs(orgs: &[MyOrg]) -> String {
    let slugs: Vec<&str> = orgs.iter().map(|org| org.slug.as_str()).collect();
    slugs.join(", ")
}

/// The org named like the repository's owner, `acme` for `github.com/acme/shop`. A person's own
/// org by the owner's name: as sure as init gets without asking.
fn owners_org(orgs: &[MyOrg], repository: Option<&str>) -> Option<usize> {
    let owner = repository?.split('/').nth(1)?.to_ascii_lowercase();
    orgs.iter().position(|org| org.slug == owner)
}

/// The org `hushrun login` saved, if it is still one of theirs.
fn last_org(orgs: &[MyOrg], origin: &str) -> Option<usize> {
    let last = hosted::org_of(&hosted::saved_backend(origin)?)?;
    orgs.iter().position(|org| org.slug == last)
}

fn question(repository: Option<&str>, count: usize) -> String {
    format!(
        "You belong to {count} organizations. Which one is {} for?",
        repository.unwrap_or("this repository")
    )
}

/// The picker's lines: a numbered org each, then where another comes from.
fn rows(mine: &MyOrgs) -> Vec<String> {
    let mut lines: Vec<String> = mine
        .orgs
        .iter()
        .enumerate()
        .map(|(index, org)| {
            let number = index + 1;
            if org.name == org.slug {
                format!("  {number}. {}: {}", org.slug, org.role)
            } else {
                format!("  {number}. {} ({}): {}", org.name, org.slug, org.role)
            }
        })
        .collect();
    if let Some(dashboard) = &mine.dashboard {
        lines.push(format!(
            "For a new one, create it at {}/new, then run npx hushrun init again.",
            dashboard.trim_end_matches('/')
        ));
    }
    lines
}

/// Asks until the answer is one of `orgs`, by its number or slug, and Enter takes `default`. `None`
/// when the input ends: stopping is safer than a guess.
fn ask(
    input: &mut impl BufRead,
    output: &mut impl Write,
    orgs: &[MyOrg],
    default: Option<usize>,
) -> anyhow::Result<Option<usize>> {
    loop {
        match default {
            Some(index) => write!(output, "Organization [{}]: ", index + 1)?,
            None => write!(output, "Organization: ")?,
        }
        output.flush()?;
        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0 {
            return Ok(None);
        }
        let answer = answer.trim();
        let chosen = if answer.is_empty() {
            default
        } else {
            answer
                .parse::<usize>()
                .ok()
                .and_then(|number| number.checked_sub(1))
                .filter(|index| *index < orgs.len())
                .or_else(|| orgs.iter().position(|org| org.slug == answer))
        };
        if chosen.is_some() {
            return Ok(chosen);
        }
        writeln!(
            output,
            "Answer a number from 1 to {}, or an organization's slug.",
            orgs.len()
        )?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org(slug: &str, name: &str, role: &str) -> MyOrg {
        MyOrg {
            slug: slug.into(),
            name: name.into(),
            role: role.into(),
        }
    }

    fn orgs() -> Vec<MyOrg> {
        vec![
            org("acme", "Acme Inc", "owner"),
            org("initech", "initech", "owner"),
            org("umbrella", "Umbrella", "member"),
        ]
    }

    /// What `ask` makes of `typed`, and what it wrote.
    fn answers(typed: &str, default: Option<usize>) -> (Option<usize>, String) {
        let mut output = Vec::new();
        let chosen = ask(&mut typed.as_bytes(), &mut output, &orgs(), default).unwrap();
        (chosen, String::from_utf8(output).unwrap())
    }

    // covers: C-15
    #[test]
    fn a_number_or_a_slug_chooses_and_enter_takes_the_default() {
        assert_eq!(answers("2\n", None).0, Some(1));
        assert_eq!(answers("initech\n", Some(0)).0, Some(1));
        assert_eq!(answers(" 3 \n", Some(0)).0, Some(2));
        let (chosen, wrote) = answers("\n", Some(2));
        assert_eq!(chosen, Some(2));
        assert_eq!(wrote, "Organization [3]: ");
    }

    // covers: C-15
    #[test]
    fn nothing_is_guessed() {
        // Without a default, Enter chooses nothing, and it asks again.
        let (chosen, wrote) = answers("\n\n1\n", None);
        assert_eq!(chosen, Some(0));
        assert_eq!(
            wrote.matches("Answer a number from 1 to 3").count(),
            2,
            "{wrote}"
        );
        // Neither does an answer that is none of them.
        for bad in ["0\n", "4\n", "acm\n", "-1\n", "ACME\n"] {
            let (chosen, wrote) = answers(&format!("{bad}2\n"), Some(0));
            assert_eq!(chosen, Some(1), "{bad:?}: {wrote}");
        }
        // The end of the input is no answer, whatever the default.
        assert_eq!(answers("", Some(0)).0, None);
        assert_eq!(answers("nope\n", Some(0)).0, None);
    }

    // covers: C-15
    #[test]
    fn the_owners_org_is_the_one_named_like_it() {
        let orgs = orgs();
        assert_eq!(owners_org(&orgs, Some("github.com/initech/shop")), Some(1));
        assert_eq!(owners_org(&orgs, Some("github.com/InItEcH/shop")), Some(1));
        assert_eq!(owners_org(&orgs, Some("gitlab.com/acme/shop")), Some(0));
        assert_eq!(owners_org(&orgs, Some("github.com/stark/shop")), None);
        assert_eq!(owners_org(&orgs, Some("github.com")), None);
        assert_eq!(owners_org(&orgs, None), None);
    }

    // covers: C-15
    #[test]
    fn the_picker_shows_each_org_and_where_a_new_one_comes_from() {
        let mine = MyOrgs {
            orgs: orgs(),
            dashboard: Some("https://app.hushrun.com/".into()),
        };
        assert_eq!(
            rows(&mine),
            [
                "  1. Acme Inc (acme): owner",
                "  2. initech: owner",
                "  3. Umbrella (umbrella): member",
                "For a new one, create it at https://app.hushrun.com/new, then run npx hushrun init again.",
            ]
        );
    }
}

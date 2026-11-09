# 0064. The CLI's source lives in this repository

- **Status:** accepted
- **Date:** 2026-11-09
- **Rules:** none changed
- **Amends:** [0063](0063-an-open-source-cli.md), whose source was copied here from the maintainers'
  own repository; [0038](0038-one-repository.md); [0039](0039-npm-trusted-publishing.md);
  [0042](0042-everything-can-go-out-from-the-mac.md)

## Context

When the CLI went open source (0063), its source still lived in the maintainers' private
repository beside the hosted service, and an allowlisted copy of it was committed here. Every
change went through that copy: a pull request here had to be applied there and copied back, the
copy was one more place where something private could slip out, and the releases were built on a
maintainer's machine because the private repository's CI minutes had run out. Public repositories'
CI minutes cost nothing, macOS and Windows runners included, and npm records provenance only for
packages published from a public repository's workflow.

## Decision

1. **This repository is where the CLI is developed.** Commits and pull requests land here, on
   `main`, and nothing is copied in from elsewhere.
2. **The hosted service builds with the protocol crates from here,** `hushrun-core`,
   `hushrun-attest`, `hushrun-ledger` and `hushrun-tlog`, as a git dependency pinned to a commit
   of `main`. Those four crates keep a fixed version, so a release of the CLI changes nothing for
   it. The service accepts older CLIs, and a change that needs the service first lands there, is
   deployed, and only then released here.
3. **A release is a tag a maintainer pushes.** `scripts/release.sh prepare <version>` makes the
   version commit and its tag; the tag starts `release.yml` (dist), which builds every target on
   GitHub's runners and publishes the release, then `publish.yml` (the container image and
   `install-hushrun.sh`) and `npm.yml`. Before tagging, the maintainers check that the service
   runs the release's protocol crates.
4. **No secrets in this repository.** Releases and the image use GitHub's token for the run; npm
   trusts `npm.yml` in the environment `release`, which a maintainer approves, with provenance.
   Tags that start a release can only be pushed by maintainers.
5. **Everything pushed here is public at once,** so CI scans every push for secrets and dead links
   (`scripts/check-tree.py`, `scripts/verify.sh`), and the maintainers' pre-push hook
   (`scripts/hooks/pre-push`) adds a private list of terms that must never appear here.
   Documents about the maintainers' own infrastructure stay out; where code cites such an ADR's
   number, a short stub here keeps the number.

## Consequences

- A pull request is reviewed and merged here, with its author's commits and sign-off.
- A change to the protocol crates that the service needs takes two steps: the change here, then
  the service's pin moving to it.
- Releases no longer depend on one machine, and CI runs on Linux and macOS for every change.
- The release settings that kept the private source out of releases (no source archive, no
  artifact attestations) can now be turned on.

## Alternatives considered

- **Keep copying from the private repository.** It keeps one commit able to change the CLI and the
  service together, at the cost of a sync on every change and the risks above.
- **Publish the four crates to crates.io for the service.** It would need a publish before every
  deploy of the service, and a semver promise on internal APIs.

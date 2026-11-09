# 0035. `hushrun update`, a newer-release notice, and CI on the newest release

- **Status:** accepted
- **Date:** 2026-10-29
- **Rules:** C-20 and C-21 (new); §10.1, §10.3, §12.6, §13, §14
- **Supersedes:** the [plan](../plan.md)'s "Recipes always pin an exact version, never a moving
  tag such as `:0`" (§14), and its setup action and install script, which installed a pinned
  version only; [0009](0009-m6-github-actions.md) item 8's "The setup action installs an exact
  version"; [0016](0016-onboarding.md) item 2's install "by exact version";
  [0019](0019-gitlab.md) item 17's required `HUSHRUN_VERSION` and its refusal of an unpinned
  version, item 18's "Tagged with the exact version only, never `latest` or `0`", and its
  rejected alternative "A `:latest` or `:0` image tag"; [RFC 0001](../rfc/0001-product-architecture.md)'s
  "recipes pin an exact version (§14)"

## Context

v0.1.0 is out (ADR 0034). Whoever installed it with the shell or PowerShell installer can update
it only by running the installer again, and nothing tells them that a newer release is out.

CI stayed on the release it was set up with. The setup action required `version` ("Pin it;
there is no latest"), and so did the reusable workflow; `install-hushrun.sh` refused to run
without `HUSHRUN_VERSION`, and refused a release whose checksum it didn't pin; the image was
tagged with its version only (ADR 0019 item 18); and `hushrun adopt` wrote the version of the
binary that ran it. Plan §14 made it the rule. A fix to how hushrun plans, signs or records
reached a pipeline only when someone edited the pipeline, which few do.

On 2026-10-29 the maintainers decided on three things: a `hushrun update` command for local installs;
a notice, at most once a day, that a newer release is out; and GitHub and GitLab installs that
use the newest release by default, with pinning as an opt-in.

## Decision

1. **The newest release** is the one `https://github.com/hushrun/hushrun/releases/latest`
   redirects to, `…/releases/tag/v<version>`: GitHub's latest release, never a pre-release.
   Everything reads that redirect rather than GitHub's REST API, which allows 60 requests an
   hour per address without a token, and then takes the archive and its checksum from that
   tag, so that both come from one release. `hushrun update`, the notice, the setup action, the
   reusable workflow, `install-hushrun.sh` and the image's `latest` tag all agree on it.

2. **`hushrun update`** (C-20) replaces the running hushrun with the newest release, the way
   the installer would, when hushrun's installer installed it:
   - **Which installs.** The installers leave a receipt, `hushrun-receipt.json`, in
     `$XDG_CONFIG_HOME/hushrun`, else `~/.config/hushrun`; on Windows in
     `%XDG_CONFIG_HOME%\hushrun`, else `%LOCALAPPDATA%\hushrun`. It names the install's prefix
     and layout. update replaces a binary only when a receipt of hushrun's names it as the
     running one.
   - **How.** It downloads the release's archive for this platform and the archive's `.sha256`,
     and checks one against the other; unpacks the archive with the system's `tar`, as the
     shell installer does (Windows' own `tar.exe` reads `.zip`); runs the new binary, which
     must say `hushrun <version>`; and renames it over the old one, in the same directory. The
     running hushrun keeps the file it started from. Windows doesn't let a running program's
     file be replaced, so there the running `hushrun.exe` is first renamed `hushrun.exe.old`,
     which the next update removes. Then the receipt says the new version. A failure before
     the rename leaves hushrun as it was.
   - **Other installs** are left alone, with exit 2 and how to update them: npm's (the binary
     is in `node_modules`), CI's, a binary built from source or copied by hand, and a copy
     other than the one a receipt names. Already the newest release: it says so, and exits 0.
     It can't tell which release is the newest, or the download fails its check: exit 1.
   - **Mirrors and tests.** `HUSHRUN_INSTALLER_GITHUB_BASE_URL`, the installers' own variable,
     points it at another GitHub, a mirror, or a test's server: `https://`, or `http://` on
     this machine. Downloads follow GitHub's redirects to its storage, `https://` only, and
     never print their signed URLs.
   - **No new crate**, and no updater program: reqwest, rustls with ring, tokio, sha2, tempfile
     and semver were already built into the binary through other crates.

3. **The notice** (C-21): at most once a day, one line on stderr after the command,
   ``hushrun 0.2.0 is available: run `hushrun update` ``.
   - **Only for people:** with stderr on a terminal, outside CI (`CI`, `GITHUB_ACTIONS`,
     `GITLAB_CI`, `BUILDKITE`, `CIRCLECI`, `JENKINS_URL`, `TF_BUILD` and the other markers), with
     `--format text` and without `-q`. Never on stdout, and never from `hushrun update`.
   - **Never in the way.** It asks GitHub on a thread of its own while the command runs, with a
     3 s timeout, and waits at most 200 ms after the command for the answer. It asks once a
     day; a question that got no answer in time is asked again an hour later at the earliest.
     Every failure is silent: the command's output and exit code are what they would have
     been.
   - **Its memory** is `update-check.json` in hushrun's cache directory (§12.6): when it last
     asked, when GitHub last answered and with which release, and when it last said so. It
     records a question before asking, so commands running at the same time ask once; without
     a cache directory (`HUSHRUN_CACHE_DIR` set to nothing), it doesn't ask.
   - **`HUSHRUN_NO_UPDATE_CHECK`** turns it off. Unlike §10.1's other booleans, any value but
     nothing, `false`, `0`, `no` or `off` turns it off, without a warning: a value hushrun
     can't read can only have been meant to.
   - **What GitHub learns** from it: the machine's address, and hushrun's version in the
     `User-Agent`, at most once a day for each machine and cache directory, as from any
     download. It isn't telemetry (plan §2.2): it says nothing about what hushrun does, and
     nothing goes to hushrun's own servers.

4. **CI installs the newest release by default**, and one line pins a release:
   - **The setup action:** `version` is optional, `latest` by default; it resolves the newest
     release once, and installs it from its tag. `version: 0.2.0` pins one. `sha256` goes only
     with a pinned version, since the newest release's archive changes with each release. A new
     output, `version`, says which it installed.
   - **The reusable workflow:** `version` is optional too. The plan job installs the caller's
     version, else the newest release, and outputs it; every later job of the run installs that
     one, so one run never mixes two releases, even when a release lands during it.
   - **`install-hushrun.sh`:** `HUSHRUN_VERSION` is optional, `latest` by default. A release the
     copy lists is checked against the checksum it pins, as before; any other, such as a newer
     one, against the `.sha256` published with it, fetched again on every run, so that nothing
     a cache kept decides what is trusted, and only over TLS: an `http://` mirror needs a pin or
     `HUSHRUN_SHA256`. `HUSHRUN_SHA256` and `HUSHRUN_DOWNLOAD_URL` need `HUSHRUN_VERSION`. It
     takes `HUSHRUN_INSTALLER_GITHUB_BASE_URL` too. Any copy works; it stays POSIX `sh`.
   - **The GitLab recipe:** the image is `ghcr.io/hushrun/hushrun:latest`. The plan job adds the
     release it ran to its dotenv report, `HUSHRUN_VERSION`, and later jobs install that release
     when their image has another or none. Pinning sets `HUSHRUN_VERSION` and the image's tag
     and digest.
   - **The image:** `publish.yml` tags each release's image with its version, and with `latest`
     only when that version is GitHub's latest release then. A run by hand for an older release
     never moves `latest` back, and a pre-release never gets it.
   - **`hushrun adopt`** writes no `version` by default, and a comment with its own version that
     shows how to pin; `--hushrun-version` pins one. In `hushrun.adopt/v1`, `version` is null
     unless pinned.
   - **Recipes and docs** pin no version, and say how to pin one.

## Consequences

- **A release reaches every unpinned pipeline at once**, at its next run: fixes ship without
  anyone editing a pipeline. That cuts both ways. A bad release, or a compromised one, reaches
  them as fast, with no review in between. Teams that want a pipeline to change only through a
  reviewed commit pin the release: `version` with `sha256` in the setup action, `version` in
  the reusable workflow, `HUSHRUN_VERSION` with a vendored script that pins its checksum, and
  the image by digest. A team can pin production and not staging.
- **A checksum published with the release proves that the download is that release, not that
  the release is good.** It is what the setup action checked before when `sha256` wasn't given.
  A pin taken from the release when it was reviewed proves more, and still works.
- **Pinning the reusable workflow by commit pins everything it runs but hushrun's release**
  (ADR 0016 item 2), unless the caller passes `version`.
- **Runs of one pipeline may run different releases**, such as staging on Monday's release and
  production on Tuesday's. The plan's and events' formats are versioned (`hushrun.plan/v1`,
  `event/v1`), and a newer hushrun reads what an older one wrote.
- **People's machines contact GitHub**, at most once a day, while hushrun runs at a terminal.
  The public docs say so, and how to turn it off.
- **Contracts:** `hushrun update` and its exit codes (0, 1, 2); `HUSHRUN_NO_UPDATE_CHECK` and
  `HUSHRUN_INSTALLER_GITHUB_BASE_URL`; the setup action's optional `version` and its `version`
  output; the reusable workflow's optional `version` and its plan job's `version` output;
  `install-hushrun.sh`'s optional `HUSHRUN_VERSION`; the image's `latest` tag; and
  `hushrun.adopt/v1`'s `version`, now null unless pinned.
- **0.1.0 has no `hushrun update`**: its users run the installer again, as the public README
  says, and update from then on.

## Alternatives considered

- **axoupdater**, from dist's makers, which reads the receipt and runs the new release's
  installer. Its GitHub support takes reqwest's default TLS, which is rustls with aws-lc-rs:
  C code that plan §0.2 and §12.3 rule out. It brings crates the binary doesn't have, the
  `image` crate among them. And the installers it runs check less than update does: the
  PowerShell installer checks no checksum, and the shell installer skips its check where
  `sha256sum` is missing.
- **dist's updater** (`install-updater = true`): the installers would download a second
  program, `hushrun-update`, onto every machine.
- **Unpacking in Rust:** `.tar.xz` takes an xz decoder and `.zip` a zip reader, new crates for
  what the system's `tar` does, which the shell installer needs anyway.
- **A process of its own for the notice's question**, so that no command waits at all: a
  process that outlives the command, which people and endpoint security notice, for a notice
  that can wait an hour.
- **A moving major tag, such as `:0`,** for the image: while hushrun is 0.x it is the newest
  release anyway, and the newest release was what was wanted.
- **Exact versions, bumped by a bot** such as Renovate or Dependabot: every repository has to
  set one up and review its pull requests, and a vendored script needs a rule of its own. The
  owner chose the newest release by default; a team that pins can still use a bot.

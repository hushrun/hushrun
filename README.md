# hushrun

hushrun skips a check that this exact code already passed: on a laptop, in CI and in a coding
agent's sandbox. In a deploy pipeline, CI builds and validates each change once, and every later
environment deploys the same artifact by digest.

This repository holds hushrun's releases and its GitHub Actions. hushrun's source code isn't
public.

[Docs](https://hushrun.com/docs) · [Quickstart](https://hushrun.com/docs/quickstart) ·
[Guides](https://hushrun.com/docs/guides) ·
[Compared with Nx, Turborepo and Bazel](https://hushrun.com/docs/compare) ·
[Pricing](https://hushrun.com/pricing)

## Install

On macOS and Linux:

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/hushrun/hushrun/releases/latest/download/hushrun-installer.sh | sh
```

On Windows:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/hushrun/hushrun/releases/latest/download/hushrun-installer.ps1 | iex"
```

Then, at a repository's root:

```bash
hushrun init
```

Each release has an archive for macOS, Linux and Windows on x86_64 and arm64, each with its
SHA-256 checksum.

## Update

```bash
hushrun update
```

replaces the hushrun that the installers above installed with the newest release, after
checking its SHA-256. hushrun 0.1.0 has no `update`: run the install command again. Installed
with npm, update the `hushrun` package with your package manager instead.

At a terminal, hushrun asks GitHub at most once a day whether a newer release is out, and says
so in one line. It never asks in CI. Set `HUSHRUN_NO_UPDATE_CHECK=1` to turn it off.

## In CI

CI installs the newest release unless you pin one, so a new release reaches every pipeline that
pins none at its next run: fixes without an edit, and a bad release as fast. To install one
release until you change it, pin it, with its checksum.

- **GitHub Actions:** `hushrun/hushrun/action/setup` installs the newest release and checks its
  SHA-256. `.github/workflows/hushrun-deploy.yml` is the reusable deploy workflow that
  `hushrun adopt` sets up; every job of a run installs the same release. Pin both to a commit of
  this repository.

  ```yaml
  - uses: hushrun/hushrun/action/setup@main   # pin to a release commit
    # with: { version: 0.2.0, sha256: … }     # to install one release until you change it
  ```

- **GitLab CI:** jobs that only run hushrun use the image `ghcr.io/hushrun/hushrun:latest`, or a
  version's tag. Other jobs install the same release with `install-hushrun.sh`, attached to each
  release, which checks the archive's SHA-256 against the checksums pinned in it, or for a
  release it doesn't list, the one published with the release.

## License

hushrun is free to use under the terms in [LICENSE.md](LICENSE.md), and isn't open source.
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) lists the open-source crates hushrun is built
with, and their licenses.

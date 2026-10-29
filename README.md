# hushrun

hushrun skips a check that this exact code already passed: on a laptop, in CI and in a coding
agent's sandbox. In a deploy pipeline, CI builds and validates each change once, and every later
environment deploys the same artifact by digest.

This repository holds hushrun's releases and its GitHub Actions. hushrun's source code isn't
public.

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

## In CI

- **GitHub Actions:** `hushrun/hushrun/action/setup` installs an exact release and checks its
  SHA-256. `.github/workflows/hushrun-deploy.yml` is the reusable deploy workflow that
  `hushrun adopt` sets up. Pin both to a commit of this repository.

  ```yaml
  - uses: hushrun/hushrun/action/setup@main   # pin to a release commit
    with: { version: 0.1.0 }
  ```

- **GitLab CI:** jobs that only run hushrun use the image `ghcr.io/hushrun/hushrun:<version>`.
  Other jobs install the same release with `install-hushrun.sh`, attached to each release, which
  checks the archive's SHA-256 against the checksums pinned in it.

## License

hushrun is free to use under the terms in [LICENSE.md](LICENSE.md), and isn't open source.
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) lists the open-source crates hushrun is built
with, and their licenses.

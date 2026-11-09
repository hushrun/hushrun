# hushrun

hushrun skips a check that this exact code already passed: on a laptop, in CI and in a coding
agent's sandbox. In a deploy pipeline, CI builds and validates each change once. A digest that
is deployed already isn't deployed again, and a re-run or a revert deploys the image that passed,
by digest.

This repository holds hushrun's source code, its releases and its GitHub Actions. The CLI is
open source under the MIT license.

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
  SHA-256. `.github/workflows/hushrun-deploy.yml` is the reusable deploy workflow that the
  workflow `hushrun adopt` writes, `.github/workflows/hushrun.yml`, calls; every job of a run
  installs the same release. Pin both to a commit of this repository.

  ```yaml
  - uses: hushrun/hushrun/action/setup@main   # pin to a release commit
    # with: { version: 0.2.0, sha256: … }     # to install one release until you change it
  ```

- **GitLab CI:** jobs that only run hushrun use the image `ghcr.io/hushrun/hushrun:latest`, or a
  version's tag. Other jobs install the same release with `install-hushrun.sh`, attached to each
  release, which checks the archive's SHA-256 against the checksums pinned in it, or for a
  release it doesn't list, the one published with the release.

## Building from source

```bash
cargo build --release
./target/release/hushrun --version
```

The toolchain is pinned in `rust-toolchain.toml`. The release archives above are the supported
way to install; building from source is for auditing and contributing.

| Path | What it holds |
|---|---|
| `crates/hushrun` | The CLI |
| `crates/hushrun-*` | Its libraries: configuration and the planner, source digests, signing and verification, the event log and its HTTP client, the transparency log, CI providers, registries, reports |
| `docs/spec` | The normative rules, by ID (`D-3`, `L-10`, …) |
| `docs/adr` | The decisions behind them |
| `docs/schemas` | JSON schemas of what hushrun writes |
| `recipes`, `action` | CI recipes and the GitHub Actions |
| `testdata` | Fixtures and the scenario catalog |

hushrun's hosted service, which shares results across an org's machines and CI, isn't part of
this repository; the CLI talks to it over a documented HTTP API.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Report security problems privately, as
[SECURITY.md](SECURITY.md) says.

## License

hushrun is open source under the [MIT license](LICENSE.md).
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) lists the open-source crates its binaries are
built with, and their licenses. The name and logo aren't covered by the license:
[TRADEMARKS.md](TRADEMARKS.md).

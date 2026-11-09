# The hushrun container image

`ghcr.io/hushrun/hushrun:<version>` holds one release of hushrun: Alpine, `git`, CA certificates
and the release's static musl binary, for `linux/amd64` and `linux/arm64` (plan §14, ADR 0019).
`ghcr.io/hushrun/hushrun:latest` is the newest release's (ADR 0035).
GitLab jobs that only run hushrun use it as their `image:`; jobs on a toolchain image install the
binary with [`install-hushrun.sh`](../../recipes/gitlab/install-hushrun.sh) instead.

- **A version's tag, and `latest`.** Each release is tagged with its version, such as `0.9.0`.
  The release workflow also tags it `latest` when it is the
  newest release, GitHub's latest: never a pre-release, and never an older release published
  again by hand. There is no major-version tag such as `0`. The recipes use `latest`; a pipeline
  that should stay on one release pins its version, by the digest its release notes give:
  `ghcr.io/hushrun/hushrun:0.9.0@sha256:…`.
- **The release's bytes.** The release workflow downloads the
  release's `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` archives, checks them
  against their checksum files, and copies their binaries into the image. The image carries
  BuildKit provenance and an SBOM.
- **No entrypoint.** GitLab runs a job's script with the image's shell; `docker run IMAGE hushrun
  plan …` runs a command.
- **`safe.directory=*`.** The runner may check the repository out as another user than the job's,
  and git refuses such a repository otherwise.
- **The license and the notices.** `/usr/share/licenses/hushrun/` holds `LICENSE.md` and
  `THIRD-PARTY-NOTICES.md`, as the release archives do.

The image is public, like the releases in `hushrun/hushrun` (ADR 0034): its visibility is set in
the package's settings on GitHub. To have runners pull it from your own registry instead, mirror
it there pinned by digest, for example with hushrun itself:

```bash
hushrun artifact copy ghcr.io/hushrun/hushrun:0.9.0@sha256:… registry.gitlab.com/acme/tools/hushrun:0.9.0
```

## Building it locally

```bash
cargo build --release --locked --package hushrun --target x86_64-unknown-linux-musl
mkdir -p target/image/dist/amd64
cp target/x86_64-unknown-linux-musl/release/hushrun target/image/dist/amd64/
cp LICENSE.md THIRD-PARTY-NOTICES.md target/image/
docker build --platform linux/amd64 --build-arg VERSION=dev \
  -f deploy/image/Dockerfile -t hushrun:dev target/image
docker run --rm hushrun:dev hushrun --version
```

For `linux/arm64`, build `aarch64-unknown-linux-musl` (with `cargo zigbuild` on other hosts) into
`dist/arm64/`.

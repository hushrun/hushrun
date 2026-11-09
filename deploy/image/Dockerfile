# syntax=docker/dockerfile:1
#
# The hushrun container image (plan §14, ADR 0019): Alpine, git, CA certificates and one
# release's static musl binary, for CI jobs such as GitLab's that run in a container.
#
# The build context holds the binaries as dist/<arch>/hushrun (amd64, arm64), and LICENSE.md and
# THIRD-PARTY-NOTICES.md. The release workflow takes the binaries from the release's
# archives after checking their checksums, so the image runs the bytes the archives hold.
# deploy/image/README.md shows a local build.
FROM alpine:3.24@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6

ARG TARGETARCH
ARG VERSION=dev

LABEL org.opencontainers.image.title="hushrun" \
      org.opencontainers.image.description="Build and validate each change once, then promote the exact artifact by digest." \
      org.opencontainers.image.source="https://github.com/hushrun/hushrun" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.licenses="MIT"

# git reads the checkout; the CA certificates verify ledgers, registries and Sigstore. The
# runner may check the repository out as another user than the job's, and git refuses such a
# repository unless it is marked safe.
RUN apk add --no-cache git ca-certificates \
 && git config --system --add safe.directory '*'

COPY --chmod=0755 dist/${TARGETARCH}/hushrun /usr/local/bin/hushrun
# hushrun's own license (SPDX calls such a license LicenseRef-<name>), and the open-source crates
# the binary is built with, with their licenses.
COPY --chmod=0644 LICENSE.md THIRD-PARTY-NOTICES.md /usr/share/licenses/hushrun/

# No ENTRYPOINT: GitLab runs a job's script with the image's shell.
CMD ["hushrun", "--help"]

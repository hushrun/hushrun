# syntax=docker/dockerfile:1
# .cursor/Dockerfile for Cursor's cloud agents: hushrun, and the repository's newest baked store
# in /hushrun, where runs read it. HUSHRUN_REGISTRY_TOKEN is a build secret (a pull token from the
# dashboard's Shared store page): it is used by this one step and kept in no layer.
FROM mcr.microsoft.com/devcontainers/base:ubuntu

RUN curl --proto '=https' --tlsv1.2 -LsSf \
      https://github.com/hushrun/hushrun/releases/latest/download/hushrun-installer.sh \
    | env HUSHRUN_INSTALL_DIR=/usr/local/bin INSTALLER_NO_MODIFY_PATH=1 sh

RUN --mount=type=secret,id=HUSHRUN_REGISTRY_TOKEN,env=HUSHRUN_REGISTRY_TOKEN \
    hushrun build pull --image registry.hushrun.com/acme/shop --to /hushrun || true

# syntax=docker/dockerfile:1
# .cursor/Dockerfile for Cursor's cloud agents: the hushrun binary. The baked store is not a layer
# of this image: a layer would keep the store from the day the Dockerfile last changed.
# .cursor/hushrun-install.sh, which `hushrun init --agent-clouds cursor` writes, pulls it into
# /hushrun while Cursor builds the environment.
FROM mcr.microsoft.com/devcontainers/base:ubuntu

RUN curl --proto '=https' --tlsv1.2 -LsSf \
      https://github.com/hushrun/hushrun/releases/latest/download/hushrun-installer.sh \
    | env HUSHRUN_UNMANAGED_INSTALL=/usr/local/bin INSTALLER_NO_MODIFY_PATH=1 sh

#!/bin/sh
# Codex cloud's setup script. Setup has the network and the secrets; the agent may have neither.
# HUSHRUN_BEARER is a secret: a bearer agent token (hushrun token create --bearer).
set -e
npm ci
# A session of this repository for the agent, which later runs use.
HUSHRUN_TOKEN="$HUSHRUN_BEARER" npx --no-install hushrun login --session --hours 12 || true
# The newest results and compiled code, for runs with no network.
npx --no-install hushrun fetch || true

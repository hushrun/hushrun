#!/bin/sh
# Devin's machine setup, run before the machine is saved. HUSHRUN_AGENT, as a secret in Devin's
# settings, shares results.
set -e
npm ci
npx --no-install hushrun build pull --to "$HOME/.hushrun-baked" || true
echo 'export HUSHRUN_BUILD_LOWER="$HOME/.hushrun-baked"' >> "$HOME/.bashrc"

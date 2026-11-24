#!/bin/sh
# Jules' setup script. Jules sets no CI, so runs are local; HUSHRUN_AGENT, where Jules takes
# environment variables, shares them.
set -e
npm ci
npx --no-install hushrun fetch || true

#!/bin/bash
# `devcontainer features test`'s check of the feature: hushrun is installed and runs.
set -e
source dev-container-features-test-lib
check "hushrun runs" hushrun --version
check "on-create script" test -x /usr/local/hushrun/on-create.sh
reportResults

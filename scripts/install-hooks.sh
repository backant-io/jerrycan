#!/usr/bin/env bash
# Point git at the repo's committed hooks. Run once after cloning.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
git config core.hooksPath scripts/hooks
echo "Installed: git core.hooksPath -> scripts/hooks"

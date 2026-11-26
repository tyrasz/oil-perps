#!/usr/bin/env bash
# Build everything a live local run needs (docs/localnet.md):
#   - the localnet SBF binaries (perps-core with --features localnet, the venues,
#     vamm and the TEST ONLY mock-pyth) into target/deploy-localnet, never target/deploy
#   - IDLs and TS types (scripts/gen-idl.sh) into target/idl, target/types
#   - the keeper binary (backend/keeper, debug profile: it shares `cargo test`'s deps)
# SKIP_SBF=1 reuses existing target/deploy-localnet binaries.
set -euo pipefail
source "$(dirname "$0")/common.sh"

if [ "${SKIP_SBF:-0}" != 1 ] || [ ! -f "$DEPLOY/perps_core.so" ] || [ ! -f "$DEPLOY/mock_pyth.so" ]; then
  "$ROOT/scripts/build-sbf.sh" --localnet
fi

"$ROOT/scripts/gen-idl.sh"

cargo build --manifest-path "$ROOT/backend/keeper/Cargo.toml"
echo "localnet: build done ($DEPLOY, target/idl, $KEEPER_BIN)"

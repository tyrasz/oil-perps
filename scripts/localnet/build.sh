#!/usr/bin/env bash
# Build everything a live local run needs (docs/localnet.md):
#   - the localnet SBF binaries (perps-core with --features localnet, the venues,
#     vamm and the TEST ONLY mock-pyth) into target/deploy-localnet, never target/deploy
#   - IDLs and TS types (scripts/idlgen) into target/idl, target/types
#   - the keeper binary (backend/keeper, debug profile: it shares `cargo test`'s deps)
# SKIP_SBF=1 reuses existing target/deploy-localnet binaries.
set -euo pipefail
source "$(dirname "$0")/common.sh"

if [ "${SKIP_SBF:-0}" != 1 ] || [ ! -f "$DEPLOY/perps_core.so" ] || [ ! -f "$DEPLOY/mock_pyth.so" ]; then
  "$ROOT/scripts/build-sbf.sh" --localnet
fi

cargo build --release --manifest-path "$ROOT/scripts/idlgen/Cargo.toml"
IDLGEN="$ROOT/scripts/idlgen/target/release/idlgen"
mkdir -p "$ROOT/target/idl" "$ROOT/target/types"
for prog in perps-core orderbook vamm prop-amm mm-registry; do
  name=${prog//-/_}
  "$IDLGEN" "$ROOT/programs/$prog/src/lib.rs" "$ROOT/target/idl/$name.json" "$ROOT/target/types/$name.ts"
done

cargo build --manifest-path "$ROOT/backend/keeper/Cargo.toml"
echo "localnet: build done ($DEPLOY, target/idl, $KEEPER_BIN)"

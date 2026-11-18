#!/usr/bin/env bash
# Run the Anchor TypeScript suite (tests/oil-perps.ts) against a fresh local
# validator, the way `anchor test` does with `[test] upgradeable = true`:
# perps-core is deployed upgradeable with the provider wallet as upgrade
# authority, so the suite can run initialize_config.
#
# Prerequisites: scripts/build-sbf.sh (target/deploy/perps_core.so), the
# Solana CLI tools on PATH, a wallet at ~/.config/solana/id.json, and
# `npm ci` in the repository root.
#
# anchor-cli 0.28 no longer builds with current Rust, so the IDLs and TS
# types are generated with scripts/idlgen (the same parser `anchor build` uses).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WALLET=${ANCHOR_WALLET:-$HOME/.config/solana/id.json}
LEDGER=${LEDGER_DIR:-$ROOT/target/test-ledger}
PROGRAM_ID=Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS

cargo build --release --manifest-path "$ROOT/scripts/idlgen/Cargo.toml"
IDLGEN="$ROOT/scripts/idlgen/target/release/idlgen"
mkdir -p "$ROOT/target/idl" "$ROOT/target/types"
# anchor.workspace loads every program in Anchor.toml
for prog in perps-core orderbook vamm prop-amm mm-registry; do
  name=${prog//-/_}
  "$IDLGEN" "$ROOT/programs/$prog/src/lib.rs" "$ROOT/target/idl/$name.json" "$ROOT/target/types/$name.ts"
done

rm -rf "$LEDGER"
solana-test-validator --reset --quiet --ledger "$LEDGER" --limit-ledger-size 50000000 \
  --upgradeable-program "$PROGRAM_ID" "$ROOT/target/deploy/perps_core.so" "$WALLET" \
  > "$ROOT/target/test-validator.log" 2>&1 &
VALIDATOR=$!
trap 'kill $VALIDATOR 2>/dev/null || true' EXIT

for _ in $(seq 1 60); do
  solana -u http://127.0.0.1:8899 cluster-version >/dev/null 2>&1 && break
  sleep 1
done

cd "$ROOT"
ANCHOR_PROVIDER_URL=http://127.0.0.1:8899 ANCHOR_WALLET="$WALLET" \
  npx ts-mocha -p ./tsconfig.json -t 1000000 tests/oil-perps.ts

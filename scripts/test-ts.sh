#!/usr/bin/env bash
# Run the Anchor TypeScript suite (tests/oil-perps.ts) against a fresh local
# validator, the way `anchor test` does with `[test] upgradeable = true`:
# perps-core is deployed upgradeable with the provider wallet as upgrade
# authority, so the suite can run initialize_config.
#
# Prerequisites: scripts/build-sbf.sh (target/deploy/perps_core.so), the
# Agave 4.4.1 CLI tools on PATH (solana-test-validator; docs/lp-pool.md D48),
# a wallet at ~/.config/solana/id.json, and `npm ci` in the repository root.
#
# The IDLs and TS types come from scripts/gen-idl.sh (the IDL builder
# `anchor build` 1.2.2 uses, without anchor-cli; D49).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WALLET=${ANCHOR_WALLET:-$HOME/.config/solana/id.json}
LEDGER=${LEDGER_DIR:-$ROOT/target/test-ledger}
PROGRAM_ID=Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS

# target/idl/*.json (anchor.workspace) and target/types/*.ts (the suite's types)
"$ROOT/scripts/gen-idl.sh"

# Price feeds for every flow that reads a price (lp_deposit,
# lp_request_withdrawal and lp_withdraw need a fresh oracle; read_price allows
# |publish_time − now| ≤ 60 s): Pyth Receiver PriceUpdateV2 accounts in
# genesis, owned by the system program, which the suite configures as the
# oracle program (D47). Nothing on the validator can write them, so
# scripts/test-oracles.js writes one OIL feed every 50 s for the next three
# hours ($75.00 ± $0.05) plus a stale one, a GOLD and a SILVER one, a
# Partially verified one, one with the wrong owner and a legacy v2 account.
# The suite picks a feed that is fresh when it needs one.
ORACLES="$ROOT/target/test-oracles"
cd "$ROOT" # node resolves @solana/web3.js from here
rm -rf "$ORACLES"
node "$ROOT/scripts/test-oracles.js" "$ORACLES"

rm -rf "$LEDGER"
solana-test-validator --reset --quiet --ledger "$LEDGER" --limit-ledger-size 50000000 \
  --upgradeable-program "$PROGRAM_ID" "$ROOT/target/deploy/perps_core.so" "$WALLET" \
  --account-dir "$ORACLES/accounts" \
  > "$ROOT/target/test-validator.log" 2>&1 &
VALIDATOR=$!
trap 'kill $VALIDATOR 2>/dev/null || true' EXIT

for _ in $(seq 1 60); do
  solana -u http://127.0.0.1:8899 cluster-version >/dev/null 2>&1 && break
  sleep 1
done

cd "$ROOT"
ANCHOR_PROVIDER_URL=http://127.0.0.1:8899 ANCHOR_WALLET="$WALLET" TEST_ORACLES="$ORACLES/manifest.json" \
  npx ts-mocha -p ./tsconfig.json -t 1000000 tests/oil-perps.ts

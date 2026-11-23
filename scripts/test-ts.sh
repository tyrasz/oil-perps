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

# Pyth price accounts for the LP flows (lp_deposit, lp_request_withdrawal and
# lp_withdraw need a fresh oracle; read_price allows |publish_time − now| ≤ 60 s).
# Nothing on the validator can write a Pyth account, so genesis gets one feed
# every 50 s for the next three hours ($75.00 ± $0.05, owned by the system
# program, which the suite configures as the oracle program) plus one stale
# feed. The suite picks a feed that is fresh when it creates each market.
ORACLES="$ROOT/target/test-oracles"
cd "$ROOT" # node resolves @solana/web3.js from here
rm -rf "$ORACLES"
mkdir -p "$ORACLES/accounts"
node - "$ORACLES" <<'NODE'
const fs = require("fs");
const path = require("path");
const crypto = require("crypto");
const { PublicKey } = require("@solana/web3.js");
const dir = process.argv[2];
// pyth-sdk-solana 0.8 PriceAccount (repr C, 3312 bytes)
function priceAccount(price, conf, expo, ts) {
  const d = Buffer.alloc(3312);
  d.writeUInt32LE(0xa1b2c3d4, 0); // magic
  d.writeUInt32LE(2, 4); // VERSION_2
  d.writeUInt32LE(3, 8); // AccountType::Price
  d.writeUInt32LE(3312, 12); // size
  d.writeUInt32LE(1, 16); // PriceType::Price
  d.writeInt32LE(expo, 20);
  d.writeBigInt64LE(BigInt(ts), 96); // timestamp
  d.writeBigInt64LE(BigInt(price), 184); // prev_price
  d.writeBigUInt64LE(BigInt(conf), 192); // prev_conf
  d.writeBigInt64LE(BigInt(ts), 200); // prev_timestamp
  d.writeBigInt64LE(BigInt(price), 208); // agg.price
  d.writeBigUInt64LE(BigInt(conf), 216); // agg.conf
  d.writeUInt32LE(1, 224); // agg.status = Trading
  return d;
}
const now = Math.floor(Date.now() / 1000);
const feeds = [];
const write = (i, ts) => {
  const key = new PublicKey(crypto.createHash("sha256").update(`oil-perps-test-oracle-${i}`).digest());
  const data = priceAccount(7_500_000_000, 5_000_000, -8, ts);
  const account = {
    pubkey: key.toBase58(),
    account: {
      lamports: 1_000_000_000,
      data: [data.toString("base64"), "base64"],
      owner: "11111111111111111111111111111111",
      executable: false,
      rentEpoch: 0,
      space: data.length,
    },
  };
  fs.writeFileSync(path.join(dir, "accounts", `feed-${i}.json`), JSON.stringify(account));
  return { pubkey: key.toBase58(), publishTime: ts };
};
const stale = write("stale", 1_000_000_000);
for (let i = 0, ts = now - 60; ts <= now + 3 * 3600; i++, ts += 50) feeds.push(write(i, ts));
fs.writeFileSync(path.join(dir, "manifest.json"), JSON.stringify({ price: 75_000_000, stale, feeds }));
NODE

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

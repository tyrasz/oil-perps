#!/usr/bin/env bash
# Boot a live local simulation on localhost (docs/localnet.md):
#   1. a fresh solana-test-validator (ledger under target/localnet-ledger,
#      --limit-ledger-size) with perps-core, mm-registry, orderbook, prop-amm
#      and vamm deployed upgradeable (the local wallet as upgrade authority,
#      as scripts/test-ts.sh does) and the TEST ONLY mock-pyth
#   2. setup.ts: mint, mock-pyth feed, protocol config, the OIL market on
#      SCHEDULE_TEST_HOURLY with its seeded LP pool, venues, funded bot wallets
#   3. the components in LOCALNET_COMPONENTS (default: price keeper bots checker)
# Run scripts/localnet/build.sh first. Stop with scripts/localnet/down.sh.
set -euo pipefail
source "$(dirname "$0")/common.sh"
need solana-test-validator
need solana

for so in perps_core mm_registry orderbook prop_amm vamm mock_pyth; do
  [ -f "$DEPLOY/$so.so" ] || { echo "localnet: $DEPLOY/$so.so is missing: run scripts/localnet/build.sh" >&2; exit 1; }
done
[ -f "$ROOT/target/idl/perps_core.json" ] || { echo "localnet: IDLs missing: run scripts/localnet/build.sh" >&2; exit 1; }
if running validator || solana -u "$RPC_URL" cluster-version >/dev/null 2>&1; then
  echo "localnet: something already serves $RPC_URL; run scripts/localnet/down.sh first" >&2
  exit 1
fi

rm -rf "$LOCALNET_DIR" "$LEDGER"
mkdir -p "$PIDS" "$LOGS"

# Disk: the test validator writes ~0.5 MB of RocksDB a second, mostly the
# code_shred column family (coding shreds, which --limit-ledger-size does not
# count: it counts data shreds, ~5 a slot here), and logs at info (~330 MB an
# hour) into the ledger directory. With the old 50M (or 300k) limit the ledger
# never purged. With 20k it starts purging after ~25 minutes and the SST files
# stay small, but RocksDB keeps its write-ahead logs (*.log, ~350 MB each ~10
# minutes) until every column family has flushed: measured 2026-11-25, the
# ledger grew to ~4.4 GB over the first ~1 h 55 min, then the old logs were
# released (down to ~3.2 GB) and it cycles from there. Keep ~5 GB free for a
# multi-hour run; warn-level logs keep the log file small. Raise LEDGER_LIMIT / VALIDATOR_LOG
# only with disk to spare.
RUST_LOG=${VALIDATOR_LOG:-solana=warn} \
start validator solana-test-validator --reset --quiet \
  --ledger "$LEDGER" --limit-ledger-size "${LEDGER_LIMIT:-20000}" \
  --bind-address 127.0.0.1 --rpc-port "$RPC_PORT" --faucet-port "$((RPC_PORT + 1001))" \
  --upgradeable-program Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS "$DEPLOY/perps_core.so" "$WALLET" \
  --upgradeable-program MMReg11111111111111111111111111111111111111 "$DEPLOY/mm_registry.so" "$WALLET" \
  --upgradeable-program 2XhUqeGhdkhKdZXkjHmMmFMXBJfYVVCbFxKjKqkCdMzt "$DEPLOY/orderbook.so" "$WALLET" \
  --upgradeable-program PropAMM111111111111111111111111111111111111 "$DEPLOY/prop_amm.so" "$WALLET" \
  --upgradeable-program 3Vft6oJxPvHXELuqrPgCdPqJf1LxGpqDxBdqNhJxLZbm "$DEPLOY/vamm.so" "$WALLET" \
  --bpf-program MockPyth11111111111111111111111111111111111 "$DEPLOY/mock_pyth.so"

for i in $(seq 1 90); do
  solana -u "$RPC_URL" cluster-version >/dev/null 2>&1 && break
  running validator || { echo "localnet: the validator exited; see $LOGS/validator.log" >&2; exit 1; }
  sleep 1
  [ "$i" = 90 ] && { echo "localnet: validator did not come up" >&2; exit 1; }
done
# Let the first slots land before deploy-time accounts are read
sleep 2

ts scripts/localnet/src/setup.ts 2>&1 | tee "$LOGS/setup.log"

COMPONENTS=${LOCALNET_COMPONENTS-price keeper bots checker}
state() { node -e "console.log(require('$LOCALNET_DIR/state.json')['$1'])"; }
for c in $COMPONENTS; do
  case $c in
    price) start price bash -c "cd '$ROOT' && exec node '$ROOT/node_modules/ts-node/dist/bin.js' -P scripts/localnet/tsconfig.json --transpile-only scripts/localnet/src/price.ts" ;;
    bots) start bots bash -c "cd '$ROOT' && exec node '$ROOT/node_modules/ts-node/dist/bin.js' -P scripts/localnet/tsconfig.json --transpile-only scripts/localnet/src/bots.ts" ;;
    checker) start checker bash -c "cd '$ROOT' && exec node '$ROOT/node_modules/ts-node/dist/bin.js' -P scripts/localnet/tsconfig.json --transpile-only scripts/localnet/src/checker.ts" ;;
    keeper)
      [ -x "$KEEPER_BIN" ] || { echo "localnet: $KEEPER_BIN is missing: run scripts/localnet/build.sh" >&2; exit 1; }
      # The keeper reads a .env from its working directory: run it in LOCALNET_DIR
      start keeper env -C "$LOCALNET_DIR" \
        SOLANA_RPC_URL="$RPC_URL" KEEPER_KEYPAIR="$LOCALNET_DIR/keys/keeper.json" \
        PERPS_PROGRAM_ID=Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS USDC_MINT="$(state mint)" MARKETS=OIL \
        KEEPER_FUNDING_CHECK_SECS="${KEEPER_FUNDING_CHECK_SECS:-5}" \
        KEEPER_LIQUIDATION_CHECK_SECS="${KEEPER_LIQUIDATION_CHECK_SECS:-3}" \
        KEEPER_ADL_CHECK_SECS="${KEEPER_ADL_CHECK_SECS:-5}" \
        KEEPER_POOL_CHECK_SECS="${KEEPER_POOL_CHECK_SECS:-10}" \
        KEEPER_ERROR_BACKOFF_SECS="${KEEPER_ERROR_BACKOFF_SECS:-10}" \
        KEEPER_SETTLE_PERIOD_SECS="${KEEPER_SETTLE_PERIOD_SECS:-120}" \
        RUST_LOG="${KEEPER_LOG:-oil_perps_keeper=info}" "$KEEPER_BIN"
      ;;
    *) echo "localnet: unknown component $c" >&2; exit 1 ;;
  esac
done
echo "localnet: up at $RPC_URL; state $LOCALNET_DIR/state.json; logs $LOGS"

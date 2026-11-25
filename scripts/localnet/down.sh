#!/usr/bin/env bash
# Stop a live local simulation: bots and checker first (the checker writes its
# final report on SIGTERM), then the price driver, the keeper and the
# validator; delete the ledger unless KEEP_LEDGER=1.
set -euo pipefail
source "$(dirname "$0")/common.sh"
mkdir -p "$LOCALNET_DIR"
touch "$LOCALNET_DIR/stop"
stop bots TERM 20
stop checker TERM 120
stop price TERM 10
stop keeper TERM 10
stop validator TERM 20
# Anything left from an earlier run on this ledger
pkill -f "solana-test-validator.*$LEDGER" 2>/dev/null || true
if [ "${KEEP_LEDGER:-0}" != 1 ]; then
  rm -rf "$LEDGER"
fi
rm -f "$LOCALNET_DIR/stop"
echo "localnet: down"

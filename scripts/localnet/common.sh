# Shared settings for scripts/localnet/*.sh (sourced). LOCALHOST ONLY.
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
LOCALNET_DIR=${LOCALNET_DIR:-$ROOT/target/localnet}
LEDGER=${LOCALNET_LEDGER:-$ROOT/target/localnet-ledger}
RPC_PORT=${RPC_PORT:-8899}
RPC_URL=http://127.0.0.1:$RPC_PORT
WALLET=${ANCHOR_WALLET:-$HOME/.config/solana/id.json}
DEPLOY=$ROOT/target/deploy-localnet
KEEPER_BIN=${KEEPER_BIN:-$ROOT/backend/keeper/target/debug/oil-perps-keeper}
PIDS=$LOCALNET_DIR/pids
LOGS=$LOCALNET_DIR/logs
export LOCALNET_DIR RPC_URL ANCHOR_WALLET=$WALLET

ts() {
  # ts-node from the root node_modules, transpile-only (tsc --noEmit checks types separately)
  (cd "$ROOT" && node "$ROOT/node_modules/ts-node/dist/bin.js" -P "$ROOT/scripts/localnet/tsconfig.json" --transpile-only "$@")
}

need() {
  command -v "$1" >/dev/null 2>&1 || { echo "localnet: $1 is not on PATH (see docs/localnet.md)" >&2; exit 1; }
}

# start NAME CMD...: run in the background, pid in $PIDS/NAME.pid, output in $LOGS/NAME.log
start() {
  local name=$1; shift
  mkdir -p "$PIDS" "$LOGS"
  nohup "$@" >"$LOGS/$name.log" 2>&1 &
  echo $! >"$PIDS/$name.pid"
  echo "localnet: started $name (pid $!, log $LOGS/$name.log)"
}

running() {
  [ -f "$PIDS/$1.pid" ] && kill -0 "$(cat "$PIDS/$1.pid")" 2>/dev/null
}

# stop NAME [SIGNAL] [WAIT_SECS]
stop() {
  local name=$1 sig=${2:-TERM} wait=${3:-10}
  [ -f "$PIDS/$name.pid" ] || return 0
  local pid; pid=$(cat "$PIDS/$name.pid")
  if kill -0 "$pid" 2>/dev/null; then
    kill -"$sig" "$pid" 2>/dev/null || true
    for _ in $(seq 1 $((wait * 5))); do kill -0 "$pid" 2>/dev/null || break; sleep 0.2; done
    kill -0 "$pid" 2>/dev/null && kill -KILL "$pid" 2>/dev/null || true
  fi
  rm -f "$PIDS/$name.pid"
}

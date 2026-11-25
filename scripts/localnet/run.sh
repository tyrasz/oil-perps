#!/usr/bin/env bash
# A timed live run (the CI smoke step and docs/localnet.md use it):
#   build (unless SKIP_BUILD=1), up.sh, trade for DURATION_SECS and until
#   MIN_WEEKLY_REOPENS weekly reopens have been crossed plus REOPEN_TAIL_SECS,
#   then stop the bots, let the checker write its final report into REPORT_DIR
#   (with the run's logs) and tear everything down. Exits with the checker's
#   status: non-zero on any invariant violation, unexpected error code or a
#   component that died. MAX_SECS bounds the whole run.
set -euo pipefail
source "$(dirname "$0")/common.sh"
DURATION=${DURATION_SECS:-1500}
MIN_REOPENS=${MIN_WEEKLY_REOPENS:-0}
TAIL=${REOPEN_TAIL_SECS:-240}
MAX=${MAX_SECS:-$((DURATION + 4200))}
export REPORT_DIR=${REPORT_DIR:-$ROOT/target/localnet-report}
HERE=$(cd "$(dirname "$0")" && pwd)

[ "${SKIP_BUILD:-0}" = 1 ] || "$HERE/build.sh"
trap '"$HERE/down.sh" >/dev/null 2>&1 || true' EXIT
"$HERE/up.sh"

start=$(date +%s)
reached=""
died=""
while :; do
  sleep 10
  now=$(date +%s)
  for c in validator price keeper bots checker; do
    running "$c" || { died=$c; break 2; }
  done
  reopens=$(grep -c "weekly reopen at" "$LOGS/checker.log" 2>/dev/null || true)
  if [ -z "$reached" ] && [ "${reopens:-0}" -ge "$MIN_REOPENS" ]; then reached=$now; fi
  el=$((now - start))
  if [ "$el" -ge "$DURATION" ] && [ -n "$reached" ] && { [ "$MIN_REOPENS" = 0 ] || [ $((now - reached)) -ge "$TAIL" ]; }; then break; fi
  if [ "$el" -ge "$MAX" ]; then echo "localnet: MAX_SECS reached with $reopens weekly reopens" >&2; break; fi
  [ $((el % 300)) -lt 10 ] && echo "localnet: ${el}s, weekly reopens $reopens, $(grep -c VIOLATION "$LOGS/checker.log" || true) violations so far"
done

touch "$LOCALNET_DIR/stop"
stop bots TERM 30
stop price TERM 10
# The checker's final pass, then its report (SIGTERM)
stop checker TERM 180
status=$(cat "$LOCALNET_DIR/checker.status" 2>/dev/null || echo 1)
if [ -n "$died" ]; then
  echo "localnet: component $died died during the run; see $LOGS/$died.log" >&2
  status=1
fi
mkdir -p "$REPORT_DIR/logs"
cp "$LOGS"/*.log "$LOGS"/*.jsonl "$REPORT_DIR/logs/" 2>/dev/null || true
cp "$LOCALNET_DIR/state.json" "$REPORT_DIR/" 2>/dev/null || true
echo "localnet: report in $REPORT_DIR/report.md (status $status)"
exit "$status"

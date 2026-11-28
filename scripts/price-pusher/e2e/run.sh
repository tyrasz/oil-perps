#!/usr/bin/env bash
# The pusher end to end on a localnet with Pyth's real pro-compatible
# programs (docs/lp-pool.md D54), without a Hermes key:
# solana-test-validator clones the Receiver, the push oracle, Wormhole, the
# guardian set, the Receiver config and the treasury from mainnet (read-only
# RPC), and fake-hermes.js serves a real signed XAU/USD update rebuilt from a
# mainnet transaction (hermes-xau-update.json, rebuild-update.py). Checks:
#   1. the first run creates the GOLD market's shard-0 account (csXV1aFF…) with
#      the update's price, owned by the Receiver, Full;
#   2. a second run sends nothing (Hermes has nothing newer);
#   3. a wrong API key gets no price (401) and sends nothing.
# Needs the Agave 4.4.1 CLI on PATH and `npm ci` in scripts/price-pusher.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
PUSHER=$(cd "$HERE/.." && pwd)
WORK=${WORK_DIR:-$(mktemp -d)}
mkdir -p "$WORK"
RPC=http://127.0.0.1:8899
MAINNET=${MAINNET_RPC:-https://api.mainnet-beta.solana.com}
ACCOUNT=csXV1aFF7uAfTHYWx6dDj718hnf7dUDU8v54dubib17
read -r GS RC TR < <(python3 -c "import json,sys;c=json.load(open(sys.argv[1]));print(c['guardian_set'],c['receiver_config'],c['treasury'])" "$HERE/clone-accounts.json")

cleanup() { kill "${VALIDATOR:-}" "${HERMES:-}" 2>/dev/null || true; }
trap cleanup EXIT

solana-test-validator --reset --quiet --ledger "$WORK/ledger" --url "$MAINNET" \
  --clone-upgradeable-program rec2HHDDnjLfj4kE7VyEtFA1HPGQLK33259532cRyHp \
  --clone-upgradeable-program pyt2F414BA6dPttK6RddPZUdHfapoBN24GL5wbrPCou \
  --clone-upgradeable-program HDw2E7P8X1SkCyjvoGsfBGAVUutKcj874bXjHrpVYrVL \
  --clone "$GS" --clone "$RC" --maybe-clone "$TR" > "$WORK/validator.log" 2>&1 &
VALIDATOR=$!
node "$HERE/fake-hermes.js" "$HERE/hermes-xau-update.json" > "$WORK/hermes.log" 2>&1 &
HERMES=$!
for _ in $(seq 1 90); do solana -u "$RPC" cluster-version > /dev/null 2>&1 && break; sleep 1; done

solana-keygen new --no-bip39-passphrase --silent --force -o "$WORK/payer.json" > /dev/null
solana -u "$RPC" airdrop 10 "$(solana-keygen pubkey "$WORK/payer.json")" > /dev/null

export PYTH_PROGRAMS=pro PYTH_API_KEY=test-key HERMES_URL=http://127.0.0.1:8788
export RPC_URLS="$RPC,$RPC" PUSHER_KEYPAIR="$WORK/payer.json"
cd "$PUSHER"
npx tsx src/main.ts --once GOLD > "$WORK/run1.log"
npx tsx src/main.ts --once GOLD > "$WORK/run2.log"
PYTH_API_KEY=wrong npx tsx src/main.ts --once GOLD > "$WORK/run3.log"

solana -u "$RPC" account "$ACCOUNT" --output json -o "$WORK/account.json" > /dev/null
python3 - "$WORK" "$HERE/hermes-xau-update.json" <<'PY'
import base64, json, struct, sys
w = sys.argv[1]
events = lambda f: [json.loads(l)["event"] for l in open(f"{w}/{f}")]
assert "pushed" in events("run1.log"), events("run1.log")
assert not {"due", "pushed"} & set(events("run2.log")), events("run2.log")
assert "error" in events("run3.log") and "pushed" not in events("run3.log"), events("run3.log")
a = json.load(open(f"{w}/account.json"))["account"]
d = base64.b64decode(a["data"][0])
want = json.load(open(sys.argv[2]))["parsed"][0]["price"]
assert a["owner"] == "rec2HHDDnjLfj4kE7VyEtFA1HPGQLK33259532cRyHp" and len(d) == 134 and d[40] == 1
price, conf, expo, publish = struct.unpack_from("<qQiq", d, 73)
assert (str(price), str(conf), expo, publish) == (want["price"], want["conf"], want["expo"], want["publish_time"]), (price, conf, expo, publish)
print(f"e2e: ok (account {len(d)} bytes, price {price}e{expo}, published {publish})")
PY

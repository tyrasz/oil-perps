# Price pusher

`scripts/price-pusher` keeps every market's Pyth feed account fresh. Each market pins the push oracle's shard-0 `PriceUpdateV2` account for its feed id ([lp-pool.md](lp-pool.md) D47, D51). Pyth sponsors only XAU/USD among the market feeds, and only every 30–60 s (D53), so the protocol pushes all of them itself. This is the launch requirement in §18 #19 and #21. Decisions are in D54.

## Running it

```sh
cd scripts/price-pusher
npm ci --ignore-scripts
PYTH_PROGRAMS=pro PYTH_API_KEY=... RPC_URLS=https://rpc-a,https://rpc-b \
  PUSHER_KEYPAIR=~/pusher.json npm start            # every commodity with a feed id
npm start -- --once GOLD OIL                        # one pass, two markets
npm start -- --dry-run                              # read and decide, never sign or send
```

| Variable | Default | |
|---|---|---|
| `PYTH_PROGRAMS` | (required) | `pro` or `legacy`: the program pair the markets pin (§18 #18) |
| `PYTH_API_KEY` | (required) | Hermes refuses requests without one since 2026-08-26; sent as `Authorization: Bearer` |
| `HERMES_URL` | `https://hermes.pyth.network` | `https://pyth.dourolabs.app/hermes` serves the same API |
| `RPC_URLS` | mainnet-beta | comma-separated. The first one reads and confirms; every one gets each transaction |
| `PUSHER_KEYPAIR` | `ANCHOR_WALLET`, then `~/.config/solana/id.json` | fee payer only: it holds no perps-core role |
| `DEVIATION_BPS` | 10 | push when Hermes has moved this far from the stored price; at most 10 |
| `HEARTBEAT_SECS` | 10 | push when the stored update is this old; at most 10 |
| `LAG_ALERT_BPS` | 10 | log a `lag` event past this |
| `POLL_MS` | 1000 | loop period |
| `PRIORITY_FEE` | 50000 | compute-unit price, micro-lamports |
| `MIN_BALANCE_SOL` | 0.5 | log `lowBalance` below this |

Each pass does the following:

1. Reads the latest Hermes prices and the accounts.
2. Decides per feed (`src/policy.ts`):
   - **missing** account: push. The first push creates it, and `initialize_market` needs it to exist.
   - Hermes **not newer** than the account: skip. The market is closed, or Hermes is behind. The push oracle would refuse the update anyway.
   - Otherwise, **deviation** ≥ `DEVIATION_BPS` or **heartbeat** age ≥ `HEARTBEAT_SECS`: push.
3. Fetches the signed update for the feeds that are due and posts it with Pyth's SDK (`@pythnetwork/pyth-solana-receiver`, `addUpdatePriceFeed` on shard 0). The SDK closes the encoded-VAA account to reclaim its rent.
4. Sends the transactions in order to every RPC, rebroadcasting until the first RPC confirms.

Before pushing, it checks that its derived address matches the SDK's. It never pushes over an account that perps-core would refuse (wrong owner, not a `PriceUpdateV2`, another feed id); it logs `badAccount` instead.

Output is one JSON object per line. The events are `start`, `due`, `pushed`, `lag`, `noHermesPrice`, `badAccount`, `lowBalance`, `error` and `fatal`.

## Operating it

- **Pause on `lag`.** A `lag` event means the stored price is at least `LAG_ALERT_BPS` from Hermes while Hermes is newer. Alert on it. §18 #21 asks for the market to be paused (`pause_market`, admin or guardian) while the lag lasts. The pusher holds no role and does not pause anything.
- **Run more than one.** Run at least two pushers, on different hosts with different RPC sets. A push of an update that has already landed is refused by the push oracle, so duplicates cost only fees.
- **Fees.** Each push posts a VAA and verifies it (about 87k CU on mainnet), then updates each due feed (about 27k CU each). With a 10 s heartbeat, five markets are pushed at least 6 times a minute while they are open.

## Tests

- `npm run typecheck && npm run selftest` (CI job `price-pusher`): the decisions and the configuration.
- `e2e/run.sh` (by hand, needs the Agave 4.4.1 CLI) checks the push path end to end without a Hermes key:
  - It starts `solana-test-validator` with the pro-compatible Receiver, push oracle and Wormhole, the guardian set, the Receiver config and the treasury, all cloned from mainnet over read-only RPC.
  - `e2e/fake-hermes.js` serves a real signed XAU/USD update. `e2e/rebuild-update.py` rebuilt it from a mainnet push-oracle transaction: the Wormhole VAA and the Merkle proof of the price message.
  - It checks that:
    - the first run creates the GOLD market's account (`csXV1aFF…`) with the update's price;
    - a second run sends nothing;
    - a wrong key gets a 401 and sends nothing.
- The account the real push oracle wrote in that run is the perps-core fixture `tests/fixtures/localnet_pusher_xau_usd_pro.bin`. A GOLD market opens and closes on it (`tests/oracle/mainnet.rs`).

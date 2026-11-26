# Live local simulation (localnet)

A live run puts the real SBF programs on `solana-test-validator` on
localhost, with the real keeper binary, live bots, a live invariant checker
and the frontend pointed at it. It complements the in-process agent sim
(`sim/`, solana-program-test, simulated clock): here time is the wall clock,
so a test-only session schedule compresses a trading week into one hour and a
run of a few hours crosses several weekly closes and reopens.

**Nothing in this document may reach a public cluster.** The test schedule
and the mock oracle exist only for localhost runs; see [Safety](#safety).

Contents: [running a live simulation](#running-a-live-simulation) (the
harness in `scripts/localnet/`), [what runs](#what-runs) (setup, price
driver, keeper, bots, checker, frontend), [what a live run covers and cannot
cover](#what-a-live-run-covers-and-cannot-cover), [CI](#ci), and the on-chain
pieces: [test schedule](#test-schedule-schedule_test_hourly--2-d46),
[feature gate](#feature-gate-perps-core-localnet),
[mock oracle](#mock-oracle-programsmock-pyth),
[localnet build](#localnet-build), [safety](#safety).

## Running a live simulation

Prerequisites: the Agave 4.4.1 CLI tools on `PATH` (`solana`,
`solana-test-validator`, `cargo-build-sbf`; [lp-pool.md](lp-pool.md) D48), a wallet at
`~/.config/solana/id.json` (it becomes the upgrade authority, protocol admin
and mint authority on the local validator), `npm ci` in the repository root
and in `frontend/` (screenshots only), and about 5 GB of free disk for the
ledger: it grows ~2 GB an hour until RocksDB releases its write-ahead logs
(after ~2 h in the 2026-11-25 run, at ~4.4 GB) and then cycles below that
(`--limit-ledger-size` purges old slots but not the logs; see the comment in
`up.sh`).

```
scripts/localnet/build.sh   # localnet SBF build, IDLs/types, keeper binary
scripts/localnet/up.sh      # validator + setup + price driver, keeper, bots, checker
# ... watch target/localnet/logs/*.log, open the frontend (below) ...
scripts/localnet/down.sh    # checker writes its report, everything stops, ledger deleted
```

Or one timed run, which is what the smoke run and CI use:

```
DURATION_SECS=1500 MIN_WEEKLY_REOPENS=1 REOPEN_TAIL_SECS=300 \
  REPORT_DIR=/some/dir scripts/localnet/run.sh
```

`run.sh` builds (skip with `SKIP_BUILD=1`), runs `up.sh`, trades for at least
`DURATION_SECS` and until `MIN_WEEKLY_REOPENS` weekly reopens have been
crossed plus `REOPEN_TAIL_SECS`, stops the bots, lets the checker do a final
pass and write `report.json` / `report.md` into `REPORT_DIR` (with a copy of
the logs and `state.json`), tears down, and exits non-zero on any checker
violation or a component that died. `MAX_SECS` bounds the run. The test week
is aligned to the hour (UTC), so the weekly close is at minute 50 and the
reopen at minute 0: a run started at minute 25 with `DURATION_SECS=1500`
crosses one close and one reopen in about 40 minutes.

| file | what |
|---|---|
| `scripts/localnet/common.sh` | paths, ports (`RPC_PORT`, default 8899), component start/stop helpers |
| `scripts/localnet/build.sh` | `scripts/build-sbf.sh --localnet`, `scripts/gen-idl.sh` IDLs and types, `cargo build` of `backend/keeper` |
| `scripts/localnet/up.sh` / `down.sh` / `run.sh` | boot / stop / timed run (above) |
| `scripts/localnet/src/setup.ts` | mint, feed, protocol config, market, pool, venues, wallets → `target/localnet/state.json` |
| `scripts/localnet/src/price.ts` | mock-pyth price driver |
| `scripts/localnet/src/bots.ts` | trading bots |
| `scripts/localnet/src/checker.ts` | live invariant checker and report |
| `scripts/localnet/src/{lib,view,params}.ts` | shared: localhost guard, PDAs, sending and error classification, consistent reads, parameters |
| `scripts/localnet/src/logic.ts`, `selftest.ts` | the harness's pure decisions (reopen print, checker coverage, gap and bad-debt measurement, gapper sizing) and their validator-free tests (CI) |
| `scripts/localnet/screenshots.js` | the frontend against the running validator, Playwright screenshots |

Everything lives under `target/` (gitignored): the ledger in
`target/localnet-ledger` (`--limit-ledger-size` 20 000 data shreds, `LEDGER_LIMIT`;
warn-level validator log, `VALIDATOR_LOG`; deleted by `down.sh` unless
`KEEP_LEDGER=1`), keys, state, pids and logs in `target/localnet/`
(`logs/tx.jsonl` every bot transaction, `logs/price.jsonl` every publish,
`logs/checks.jsonl` every violation or warning, one `.log` per component).
`up.sh` refuses to start while anything serves the RPC port, and wipes
`target/localnet/` and the ledger first: every run starts from genesis.
`LOCALNET_COMPONENTS` picks what `up.sh` starts after setup (default
`price keeper bots checker`; an empty value stops after setup).

## What runs

### Validator and setup

`solana-test-validator --reset --bind-address 127.0.0.1` with perps-core
(localnet feature), mm-registry, orderbook, prop-amm and vamm deployed
upgradeable with the local wallet as upgrade authority (as
`scripts/test-ts.sh` does; venue registration pins each venue's ProgramData),
and mock-pyth as a plain BPF program. `setup.ts` then:

1. funds the oracle, keeper, matcher and 25 bot keys with SOL and a 6-decimal
   USDC-like mint;
2. creates the OIL feed (mock-pyth, $75: a Full `PriceUpdateV2` carrying the
   production OIL feed id, `Commodities.USOILSPOT` `925ca92f…b3e6`) and
   `initialize_config` with `oracle_program` = mock-pyth;
3. in one transaction: `initialize_market` (initialize-market.ts's OIL values,
   funding interval 60 s, `pyth_feed_id` = that id; setup then checks that the
   market pinned the feed, the id and mock-pyth as its oracle program), `update_market_config` (test schedule, pre-close
   window 300 s, weekend IM/MM 20%/10%, 1% confidence, $7.50 floor) and
   `initialize_pool` with the scaled params of the table below. The schedule
   is switched before the pool exists because `validate` checks the scaled
   params against the market's schedule: they are invalid on an always-open
   or CME market (the claim window minimum there is 87_300 s);
4. seeds the pool with the protocol's $1M (`init_lp_position` + `lp_deposit`)
   as soon as the LP deposit window is open (open and outside Q; it waits up
   to 12 minutes if setup runs in the weekend; after a wait the seed
   transaction also cranks `update_funding`, since the keeper is not running
   yet and `lp_deposit` refuses a stale accumulator, `FundingStale`;
   [lp-pool.md](lp-pool.md) D52). Departure from D11, localnet
   only: the seed is a second transaction (on mainnet the bootstrap is one
   transaction so nobody trades against an unseeded pool; nobody else is on
   a local validator);
5. re-checks `PoolParams::validate` on the test schedule with the TS mirror
   (`frontend/src/utils/pool.ts`) and on chain (a simulated
   `propose_pool_params`);
6. `initialize_registry` + `register_venue` for mm-registry (50 bps band, 15 s
   oracle, $50k per fill, the taker signs) and `initialize_orderbook` +
   `register_venue` + `set_matcher` for the orderbook (100 bps, 30 s, $50k).

Pool params (`params.ts`): `PoolParams::production()` except the time fields
and the insurance target:

| field | production | localnet |
|---|---|---|
| `insurance_target` | $250,000 | $20,000 (`INSURANCE_TARGET_USDC`). Not a time field: production's target fills from fees within the first test weeks and then pays every deficit a live run produces (the 2026-11-25 runs: $250k by the third weekly reopen, largest deficit $33k), so the LP-absorb step of the waterfall was never reached. Fees above the target go to LP cash |
| `weekend_cap_window_secs` (W) | 86_400 | 300 |
| `lp_reopen_quiet_secs` (Q) | 1_800 | 120 |
| `withdrawal_delay_secs` | 345_600 | 600 (`LP_WITHDRAWAL_DELAY_SECS`) |
| `withdrawal_claim_window_secs` | 259_200 | 1_500 (minimum 300 + 4·120 + 600 = 1_380) |
| `profit_close_quiet_secs` | 300 | 60 |

The ADL keeper key (`initialize_pool`'s `adl_keeper`) is the keeper binary's key.

### Price driver (`price.ts`)

Publishes the mock-pyth feed every `PRICE_TICK_SECS` (3) while the test
schedule is open: a geometric Brownian motion (σ ≈ 5% per test week,
`PRICE_SIGMA`) with Poisson jumps (about one per 10 open minutes, σ 1%). While
closed the price is frozen but still republished every 5 s, so reads stay
fresh (`PRICE_CLOSED_REPUBLISH=0` lets the feed go stale instead, as a real
Pyth feed of a closed market would). Every reopen is gapped: after a daily
break N(0, 0.4%); after the weekend half the time 8–15% either way, 10% of the
time 18–25%, otherwise N(0, 1.5%). `PRICE_WEEKEND_GAPS="-0.12,0.2"` forces the
first gaps, `PRICE_SEED` the path.

The gap print is sent in the last `PRICE_REOPEN_LEAD_SECS` (3) closed
seconds, read from the chain clock, while nothing can trade, so the first
open slot already carries the new price. (Until 2026-11-25 it was sent once
the chain clock showed the reopen second, so it landed a slot or more after
the reopen, and for that slot the market was open on a fresh pre-gap price:
an open or a fill landing there executed at it.) One daily break a week is
the exception, on purpose: during the break at `c = 1_500`
(`PRICE_STALE_BREAK`, 1–3, 0 = none) nothing is republished and the reopen
print is held back `PRICE_STALE_REOPEN_DELAY` (8) seconds after the reopen,
so the market reopens on a feed older than 60 s and every price read is
refused `StaleOraclePrice` until the print lands (the quiet-time prober
sends an open into that window; the checker's I8 allows 15 s after a
reopen). `price.jsonl` logs every print with its chain time and event
(`weekend_gap`, `break_gap`, `jump`, `close`).

### Keeper (the real `backend/keeper` binary)

Run with `SOLANA_RPC_URL`, `KEEPER_KEYPAIR`, `PERPS_PROGRAM_ID`, `USDC_MINT`
(the run's mint) and `MARKETS=OIL`, from `target/localnet/` (it reads a
`.env` from its working directory). The only keeper change for localnet: its
poll intervals and back-offs, tuned for CME hours, can be set from the
environment (defaults unchanged): `KEEPER_FUNDING_CHECK_SECS` (60; localnet
5), `KEEPER_LIQUIDATION_CHECK_SECS` (10; 3), `KEEPER_ADL_CHECK_SECS` (10; 5),
`KEEPER_POOL_CHECK_SECS` (30; 10), `KEEPER_ERROR_BACKOFF_SECS` (60/300; 10),
`KEEPER_SETTLE_PERIOD_SECS` (900; 120). Its session decisions come from the
shared `session.rs`, so it knows the test schedule with no change. Its key
receives liquidation rewards (setup creates its USDC account).

Two keeper changes came out of the first full live run (both in every
build, both with unit tests):

* **Chain time.** Every session, margin and funding decision uses
  `Clock::unix_timestamp` read from the Clock sysvar at the client's
  commitment (`liquidator::chain_now`), not the host's wall clock, which ran
  1–1.3 s ahead of the local chain: all 80 `settle_position_pnl`
  `MarketClosed` refusals of that run were within 2 s of a session edge, and
  some `update_funding` calls were `FundingNotDue`. Settles and funding
  cranks also skip the last `SESSION_EDGE_GUARD_SECS` (2) before a close
  (`MarketData::is_in_session_for`): decided at the chain's clock, they land
  a slot later. Liquidations are not held back.
* **Liquidation passes.** The pass sorts the liquidatable positions worst
  first (most negative equity, then the larger notional), signs one
  transaction each with one blockhash, sends them all and confirms them
  together (`ix::send_batch`, at most 64 a pass), instead of one
  send-and-confirm (~0.5 s) at a time in pubkey order. After the −25% gap of
  that run it took 18 s to work through 67 candidates and lost 33 of them to
  owners who closed first (`PositionAlreadyClosed`). A candidate closed or
  recovered before its transaction lands (`PositionAlreadyClosed`,
  `NotLiquidatable`, at preflight or on chain) is logged as skipped, not as
  an ERROR.

### Bots (`bots.ts`)

TypeScript, one process, one async loop per agent, built on
`@anchor-lang/core` 1.2.2 (Anchor 1.2's TS client, the successor of
`@coral-xyz/anchor`) with the IDLs from `scripts/gen-idl.sh` (`target/idl`,
the programs typed with `target/types`; [lp-pool.md](lp-pool.md) D49) and the
frontend's `pool.ts` / `poolMath.ts` / `session.ts` mirrors for quotes, caps
and gates (the same code the UI uses, so no formula is re-derived). TS was
chosen over a Rust crate because Anchor's TS client builds every perps-core,
mm-registry and orderbook instruction from the IDLs, the frontend mirrors are
TS, and the SDK and TS suite already exercise the same flows; the keeper is
the Rust client in the run. Agents (wallets in `params.ts`):

* **Market makers** `mm-tight` (8 bps), `mm-wide` (22 bps): register on
  mm-registry, a maker approval on its venue, one two-sided quote refreshed
  every ~3 s around the oracle and leaned against inventory, 4x leverage
  (valid under the weekend IM too); hedge inventory past a limit against the
  house and recycle fill positions.
* **Limit traders** `limit-1`, `limit-2` rest bids and asks on the orderbook
  near the oracle (some crossing); the **matcher** key matches crossing
  orders of different owners with `match_orders`, split at the $50k fill cap.
* **Retail** (momentum, mean-reversion, two degens at max leverage, a house-
  only trader, a swing trader who adds margin): open against the house
  (`open_position` with an acceptable price from the house quote) or take MM
  quotes (`fill_quote` with a one-shot taker approval), close on a hold timer,
  flatten before the weekend (degens carry), deposit and withdraw collateral.
* **LPs**: `lp-steady` (deposits, requests a tenth, claims, redeposits),
  `lp-gap` (in right after Q, requests before the weekend), `lp-buffer`
  (requests a quarter when traders sit near the net cap and claims at the
  weekend withdraw line), `lp-lapse` (never claims in time; its claim after
  the window is refused `WithdrawalExpired`, then it cancels), and `lp-big`
  (about a quarter of `B`: requests nine tenths of its shares once the net
  is above 0.3·`B` and claims at the line until it fits; a claim above the
  exit room left under the weekend withdraw line is refused
  `PoolCapWouldBreach` (D45), one that fits is paid and it deposits again
  when the net is low). `lp-buffer`'s quarter (~$30–40k) always fits the
  room (hundreds of thousands at production `G`), so it is `lp-big` that
  reaches the line. They crank `update_funding` first when the accumulator
  is stale, as the frontend does.
* **Adversaries** (from `sim/src/adversary.rs`, the ones that make sense
  live): four **whales** trade one direction into the net cap from four
  accounts (so the per-account cap does not bind first) until refused
  (`AccountExposureLimit`, `PoolExposureLimit`, `PoolWeekendExposureLimit`)
  and hold it: one direction per test week, flipped at the weekly reopen,
  a 3% chance a step of closing out; a **self-cross pair** crosses itself on
  the orderbook and closes one leg against the house to push the net (D25:
  capped like an open); two **gappers** open long and short just before the
  pre-close window at a leverage whose equity, after up to `max_impact`,
  already covers the weekend maintenance margin plus 1.5% (8x with
  production margins; `logic.ts` `gapperLeverage`), and top back up to that
  from 30 s before W to the close, so they carry a thin but valid margin
  into the weekend and a reopen gap of more than ~1.5% against one
  liquidates it and a large one leaves bad debt (until 2026-11-25 they
  opened at 9.8x, the pre-close window's jump to the 10% weekend
  maintenance margin made them liquidatable on the spot, and the keeper cut
  4 of 6 gapper-weeks before their first top-up); a **quiet-time prober**
  carries a position over the weekend, closes right after the reopen inside
  the profit-close quiet time (`ProfitCloseQuiet` when profitable), probes
  opens while closed (`MarketClosed`) and into the price driver's stale
  reopen (`StaleOraclePrice`).
* **Admin: the G raise.** `G_RAISE_AFTER_SECS` (1_200) after the bots
  start, mid-week and at least 300 s before W, the admin raises the market's
  weekend IM from 20% to `G_RAISE_TO` (3_000 bps) with
  `update_market_config`: "a later raise of `G`", which §3.1 lists as an
  exception to the weekend bound. The weekend line `α/G` drops from 0.5× to
  0.33×, under the 0.4× net cap (`validate` checked `k_net·G ≤ α·10_000`
  at the old `G` and `update_market_config` does not see the pool), so from
  then on the always-on weekend cap binds on net-raising opens before the
  net cap (`PoolWeekendExposureLimit`), the withdraw line leaves no exit
  room at the cap (`PoolCapWouldBreach`), and net put on before the raise
  sits above the line until it closes or the keeper's `wTrig` exposure ADL
  cuts it inside W. With production params the weekend cap can never bind
  first (it allows 0.5·`capNAV` against the net cap's 0.4·`capNAV`, and the
  premium shortfall that separates them is bounded by `max_impact` on one
  trade); this raise is the one way a live run reaches it.
  `G_RAISE_AFTER_SECS=0` turns it off.

House opens are serialized in the process (the Position PDA is seeded by
`Market.total_positions`), as are order placements (seeded by the book's
sequence number). A refused close is not retried for 30 s; profitable closes
wait out the profit-close quiet time except the prober's.

### Checker (`checker.ts`)

Every `CHECK_SECS` (5) and on every session edge it reads the market, pool,
feed, vault token account and clock in one `getMultipleAccounts` call, then
every open Position, UserAccount and LpPosition, then the first set again; a
snapshot is used only if the market, pool and vault are byte-identical across
the reads (every position, user or LP change moves one of them). Checks:

| | invariant |
|---|---|
| I1 | vault tokens = free + position collateral + insurance + cash + treasury (fewer is a violation, more a warning) |
| I2 | `free_collateral` = Σ `UserAccount.collateral_balance` (one market per validator) |
| I3 | `position_collateral` = Σ open `Position.collateral` |
| I4 | `open_positions`, long/short OI, entry sums and funding sums = the sums over open positions |
| I5 | `lp_shares` = Σ `LpPosition.shares` + `LP_DEAD_SHARES`; `pending_withdraw_shares` = Σ pending |
| I6 | `NAV_redeem ≤ NAV_mint` (`pool_view` via `poolMath.ts`) |
| I7 | no open position below maintenance (the margins in force) for more than `LIQ_GRACE_SECS` (60) while open, the oracle fresh and the keeper alive |
| I8 | the oracle is fresh while open (15 s after a reopen allowed) |
| I9 | no house open lands while the market is closed |
| I10 | at each weekly close `G·net_notional ≤ α·B`, unless what happened since the bound last held is a documented §3.1 exception: a price rise with the net, a cap-exempt liquidation, a deficit or payout lowering `B`, a raise of `G` (a coarse attribution, not the sim's by-size one) |
| I11 | no unexpected error code in any transaction: every failure is classified (`program:ErrorName`) and must be an expected refusal (`EXPECTED_REFUSALS` in `lib.ts`: the program's own session, cap, quiet-time, slippage, approval and race outcomes) or one the bot provokes on purpose; `VaultInvariantViolated`, `MathOverflow`, `BucketUnderflow`, `VenueMarginInvariant` and `PoolInsolvent` are always violations |

| C0 | the checker itself checked enough: at least one consistent snapshot; at most `MAX_UNCHECKED_FRAC` (20%) of the check cycles threw or found the market busy; at most `MAX_CHECK_GAP_SECS` (60) of chain time between checked snapshots; a checked snapshot on both sides of every session edge. Until 2026-11-25 a checker whose every read failed (IDL drift, RPC down) logged "check failed", counted nothing and reported PASS |

It also follows the keeper's transactions (`getSignaturesForAddress` of the
keeper key, events decoded from the logs) and writes `report.json` and
`report.md`: transactions and CU by kind, the error histogram (expected or
not), keeper actions by instruction, liquidations (stage, size, penalty,
reward, deficit), ADL events, bad debt (every `BadDebt` event once, from
keeper liquidations and from bot closes, with what insurance paid, what LPs
absorbed and what went pending; `PositionLiquidated` repeats the deficit and
is not counted again), every weekly close (net, `B`, `G·nn/(α·B)` and its
week maximum), every reopen gap (the price driver's `weekend_gap` print
against its print before it, from `price.jsonl`, not the first print after
the checker's first snapshot), price events and the LP NAV path (NAV/share,
`NAV_redeem`, `B`, net, open positions, pending bad debt). A failed send
logs the signature whenever one exists and the thrown value itself
(`util.inspect` for a non-Error), and a failure with neither a signature
nor simulation logs is `infra:SendFailed` (with a signature but no landed
transaction, `infra:Unconfirmed`), not `other:[object Object]`.

### Frontend

```
node scripts/localnet/screenshots.js [label]     # SCREENSHOT_DIR, FRONTEND_PORT (3100)
```

starts `vite` with `VITE_NETWORK=localnet`, `VITE_RPC_URL`, `VITE_USDC_MINT`
and `VITE_PYTH_FEED_OIL` from `state.json` and saves the trading page and the
LP vault (deviceScaleFactor 2). To use it by hand, start the dev server with
the same variables. `VITE_PYTH_FEED_OIL` is only the configured default since
lp-pool.md D51: the frontend sends, and reads prices from, the account the
market pins (`Market.pyth_price_feed`). Localnet-only frontend behaviour: prices come from the
market's oracle account over RPC (`VITE_ORACLE_SOURCE=chain`, the localnet
default; `utils/oracleAccount.ts`, which shows a price only for a Full update
of the market's feed id owned by the market's oracle program, as
`read_price` requires) instead of `backend/api`, which reads
Hermes, a public service; the WebSocket and backend order book are off; an
in-browser burner wallet is offered so wallet-only panels load (it holds no
SOL: `solana airdrop` to it on localhost to trade). The price chart's candles
are still the component's synthetic history (there is no candle source
without `backend/api`); the header price, session notice, order entry,
pool headroom and LP vault are live.

## What a live run covers and cannot cover

Covered with the real binaries and the wall clock, and hit in the
reference run below: house opens and closes with skew impact and acceptable
prices, venue fills on both venues (taker and maker approvals, bands, the
per-fill cap), the net cap and the per-account cap, closes that raise the
net, the weekend cap on opens and on net-raising closes once the G raise
put it under the net cap (it cannot bind first with production `G`), the
pre-close window (weekend margins, LP exits shut), the weekly close and the daily breaks, a reopen gap printed before the reopen, a
reopen on a stale feed (`StaleOraclePrice`), liquidations by the keeper
inside the pre-close window and right after a gap (stage 1 and stage 2),
a deficit paid by insurance, `settle_position_pnl` by the keeper, funding
every 60 open seconds (keeper and LP cranks), LP deposits, requests, claims
at the weekend withdraw line that fit the exit room, lapsed requests, the
profit-close quiet time after every session open, and every bucket and
aggregate after every change. What a run hit is in its report: the error
histogram counts every refusal by code.

Reachable with these settings but not guaranteed in one run (none of them
happened in the reference run):

* **The weekend withdraw line refusing a claim** (`PoolCapWouldBreach`,
  D45). `capNAV` already keeps traders off capital whose withdrawal was
  requested before the net filled, so a claim requested earlier fits the
  room at a steady price by construction; the line refuses a claim
  requested after the net filled, while the net stays put through the
  600 s delay, or after a price move or another LP's claim took the room
  (§3.1). `lp-big` requests about a quarter of `B` once the net is above
  0.3·`B`; in the reference run its three claims (and `lp-buffer`'s
  three) all fit. The in-process tests (`production_exit_buffer_at_the_net_cap`,
  `pool.test.ts`) and the sim's `lp_withdraw_weekend_bound` probe cover the
  refusal.
* **LPs absorbing a deficit** (`BadDebt.lp_absorbed > 0`): needs one
  deficit, or several within the day, above the $20k insurance target. The
  reference run's only deficit was $427 (a gapper after a +13.6% gap), paid
  by insurance. A gap of 20%+ against the whales' net, which sits near the
  cap all week, would do it.
* **Exposure-mode (mode-1) ADL**: needs `wTrig` (the net above `α·B/G`
  inside W, which the G raise makes possible for net put on before it),
  `nTrig` (above the 0.6× hard line) or `dTrig` (a 15% drawdown). In the
  reference run the weekend cap held the net at 0.90 of the line at the
  close (0.99 at most during the week), so no trigger fired.

Not covered, and why:

* **Deficit-mode (mode-0) ADL and the pending bad-debt path.** Pending debt
  needs a deficit beyond insurance and the LP loss cap (5% of NAV a day, the
  `LOSS_WINDOW_SECS` window, ≈ $75–100k here), and mode-0 ADL then only takes
  a profitable position whose score is above the decaying floor, which
  starts at 1_000_000 bps (100x) and falls over 21_600 open s (≈ 7.7 h wall
  on the test schedule): no profitable position scores that high in the
  first hours. The previous version of this page said an 18–25% gap against
  the gappers produces it; it does not (their $60–110k cannot exceed
  insurance once fees have filled it, and the floor blocks the ADL anyway).
* The time constants in [What a live run cannot reach](#what-a-live-run-cannot-reach):
  bad-debt write-off, the ADL floor decay beyond its first hours, the loss
  and drawdown windows rolling over, `apply_pool_params`, the funding
  catch-up cap.
* `settle_market`, `pause_market`, venue disable/pause, referrals, admin
  hand-over, `withdraw_treasury`: admin flows no bot drives.
* prop-amm and vamm are deployed but are not perps-core venues; no bot
  trades them.
* The CME schedule itself (DST, holidays): the sim and the session vectors
  cover it; a live run uses the test schedule only.
* Contention at mainnet scale: one validator, 25 bot wallets, a few
  transactions a second.

## CI

* Every push: the SBF job type-checks the harness
  (`npx tsc -p scripts/localnet/tsconfig.json`, seconds, after
  `scripts/gen-idl.sh` has written the IDL types it uses) and runs its
  validator-free self-test (`scripts/localnet/src/selftest.ts`: the reopen
  print timing, the checker's C0 coverage verdict, the weekend-gap and
  bad-debt measurement, the gapper's sizing, error text of non-Error
  throws; about a second), and the programs job runs the feature-gated
  schedule tests and mock-pyth's tests.
* The live smoke run is a separate job, `localnet-smoke`, that runs only on a
  manual `workflow_dispatch` (`DURATION_SECS=300 scripts/localnet/run.sh`,
  report uploaded as an artifact). It is not part of the push pipeline
  because it is not cheap and its coverage depends on the clock: it adds
  the localnet SBF build (about 2 min on a warm cache), a keeper build in
  the job (several minutes cold), up to 12 min of setup waiting for the LP
  deposit window when it starts in a test weekend, and the trading time, so
  15–30 min; and five minutes of trading may fall entirely inside a
  pre-close window or the weekend, so whether it sees a close, a reopen gap
  or a liquidation depends on when it starts. It has not been run on GitHub's
  runners yet (only locally, see below).
* Reference run (2026-11-25, after the review fixes; `SKIP_BUILD=1
  DURATION_SECS=2700 MIN_WEEKLY_REOPENS=1 REOPEN_TAIL_SECS=600
  scripts/localnet/run.sh` on the localnet build of the same tree): 3_508 s
  of chain time from minute 11 to minute 10 of the next hour: six daily-break
  edges, one weekly close, one reopen. 7_790 transactions (6_284 landed,
  1_506 expected refusals, 0 unexpected), 669 of 669 check cycles checked a
  consistent snapshot, at most 7 s apart, 0 violations, PASS. The G raise
  (20% → 30% weekend IM) landed 20 minutes in with the net at $519k against
  a new line of $527k; from then on the weekend cap refused 375 net-raising
  trades (whales 328, retail opens 31, gapper opens 5, closes 11), and at the weekly close
  `G·nn/(α·B)` was 0.904 (0.991 at most during the week). The stale break
  reopened on a 62 s old feed, stale for 9 s (the prober's open was refused
  `StaleOraclePrice`). The weekend gap print (+13.62%) went out 3 s before
  the reopen. The keeper liquidated one position inside the pre-close window
  (stage 1, 9.5% against the 10% weekend margin), the short gapper 3 s after
  the reopen (stage 2, a $427.48 deficit, paid by insurance: the gapper had
  carried its topped-up margin through the window), and one position 8.5
  minutes later; it settled 235 losers, cranked funding 9 times, logged 6
  races at INFO and 9 ERROR lines, all "Oracle price is stale" during the
  stale reopen. The long gapper opened after five weekend-cap refusals,
  carried a profit over the gap and closed the next week. Report, logs and
  screenshots: `localnet-run/rerun/` in the session scratchpad (not in the
  repository). The report's bad-debt table shows insurance $0.00 for that
  deficit: the decoded event fields are camelCase (`insurancePaid`), which
  the table read as snake_case; fixed after the run (`badDebtEvents`), and
  `report.json` has the right value.
* Earlier local evidence (2026-11-25, before the review fixes, `DURATION_SECS=1500 MIN_WEEKLY_REOPENS=1
  REOPEN_TAIL_SECS=300 scripts/localnet/run.sh`, build included): 3_199 s of
  chain time from minute 11 to minute 5 of the next hour, crossing three
  daily breaks, one weekly close (`G·nn/(α·B)` 0.657 at the close, 0.777 at
  most during the week) and one reopen with a −14.6% gap. 7_079 bot
  transactions (6_328 landed, 751 expected refusals, 0 unexpected), 613
  consistent snapshots, 0 violations. The keeper cranked funding, settled
  169 losers, liquidated four positions (stage 1) when the pre-close window
  raised the maintenance margin and one (stage 2) on the gap, leaving a
  $2,543.78 deficit that insurance paid in full (no ADL needed). An earlier run
  (+11.6% gap, twelve liquidations, a $940.60 deficit) found one unexpected
  code, an MM's `update_quote` on a quote a taker had just filled and closed,
  which is now an expected refusal for that bot.
* The keeper's log used to show ERROR lines for refusals it raced into:
  `settle_position_pnl` sent just after a session edge (`MarketClosed`, from
  deciding by the wall clock), `PositionAlreadyClosed` from liquidations
  and settles an owner's close beat, `FundingNotDue` when an LP's crank
  landed first. The keeper now decides on the chain clock, holds settles
  and cranks back in the last 2 s before a close, and logs the races
  (`PositionAlreadyClosed`, `NotLiquidatable`, `NothingToSettle`,
  `SettleWouldBankrupt`, `FundingNotDue`) at INFO. What is still logged as
  ERROR on localnet is the price driver's deliberate stale reopen: for the
  ~9 s the feed is older than 60 s every check that needs a price fails
  "Oracle price is stale", as it would on a real stale feed.

## Test schedule: `SCHEDULE_TEST_HOURLY = 2` (D46)

`programs/perps-core/src/session.rs`. A CME-like week compressed into one
hour of Unix time: week `ts.div_euclid(3_600)`, offset `c = ts.rem_euclid(3_600)`.

| `c` (s) | state |
|---|---|
| `[0, 750)` | open ("Sunday"/"Monday", the week's first open) |
| `[750, 810)` | daily break |
| `[810, 1_500)` | open |
| `[1_500, 1_560)` | daily break |
| `[1_560, 2_250)` | open |
| `[2_250, 2_310)` | daily break |
| `[2_310, 3_000)` | open (last "day") |
| `[3_000, 3_600)` | weekend close (600 s); reopens at the next hour |

* 2_820 open seconds a week (`session::TEST_WEEK_OPEN`), four session opens
  (week start and the end of each break), one weekly close at `c = 3_000`.
* Every schedule-aware function knows it: `is_open`,
  `seconds_until_weekly_close` (to `c = 3_000`; breaks do not count),
  `in_pre_close_window`, `open_seconds_between` (O(1), additive),
  `seconds_since_open` (since the week start or the last break end),
  `funding_current`. The keeper and the sim `#[path]`-include the same file;
  the frontend mirror is `frontend/src/utils/session.ts` (also
  `timeAfterOpenSeconds`, `nextFundingTime`, `marketSession`, which reports
  `reopens: "in 9m 12s (test weekend)"` while closed).
* Reference vectors: `scripts/gen-session-vectors.py` writes
  `TEST_SESSION_VECTORS` (`(ts, is_open, until_close | -1, since_open | -1,
  open seconds since Unix 0)`) after the unchanged CME vectors in
  `session_vectors.rs`, and `testSessions` in `sessionVectors.json`. They come
  from an explicit segment table, independent of the program's arithmetic.

### Schedule-aware LP gate rule (D38 → D46)

`PoolParams::validate` keeps every week's ungated LP exit time on the
market's schedule (`pool_math`):

| | `week_open_secs` | `lp_ungated_secs` | `session_opens_per_week` |
|---|---|---|---|
| `SCHEDULE_ALWAYS_OPEN`, `SCHEDULE_CME_GLOBEX` (and any other id) | 414_000 | 86_400 | 5 |
| `SCHEDULE_TEST_HOURLY` | 2_820 | 600 (the CME ratio 86_400/414_000 of 2_820 = 589, rounded) | 4 |

* `lp_gates_leave_exit_time(s, W, Q)`: `W + opens·Q + ungated ≤ week_open`.
* `min_claim_window_secs(s, W, Q) = W + opens·Q + ungated`;
  `effective_claim_window_secs(s, stored, W, Q)` holds a stored request to it.
* For the two production schedules these are exactly the old fixed formulas
  (`W + 5·Q + 86_400 ≤ 414_000`); `lp_gate_rule_is_unchanged_on_production_schedules`
  checks 20_000 random and edge (W, Q, claim) triples, and `validate` answers
  the same for an always-open and a CME market. Production `PoolParams` do
  **not** validate on a test-schedule market (W = 1 day alone is longer than a
  test week), so a localnet market needs scaled params.
* Suggested localnet pool params (time fields only; everything else
  production): `weekend_cap_window_secs` (W) 300, `lp_reopen_quiet_secs` (Q)
  120, `withdrawal_claim_window_secs` ≥ 1_380 (= 300 + 4·120 + 600; e.g.
  1_500), `withdrawal_delay_secs` 600–900, `profit_close_quiet_secs` 60, and
  on the market `pre_close_window_secs` 300, `funding_interval` 60 (the
  minimum `initialize_market` allows). These pass `validate`
  (`test_schedule_scales_the_lp_gate_rule`) and the per-second D38 scan
  (`valid_test_schedule_gates_always_leave_exit_time`).
* Order of setup: `initialize_market`, then `update_market_config` with
  `session_schedule = 2`, then the pool's params. `update_market_config` does
  not see the pool, so switching an existing pool's market between schedules
  can leave params that no longer fit the new week (localnet only).

### What a live run cannot reach

These constants are not scaled (they are wall-clock or open-time spans that
the program does not tie to the schedule), so in a run of a few hours:

| constant | value | on the test schedule |
|---|---|---|
| `ADL_WRITE_OFF_SECS` | 86_400 open s | ≈ 30.6 test weeks ≈ 30.6 h wall: pending bad debt is never written off |
| `ADL_FLOOR_DECAY_SECS` | 21_600 open s | ≈ 7.7 h wall: the deficit-ADL score floor is still 87% of its start after 1 h of pending, so deficit ADL only reaches very high-score positions early in a run |
| `LOSS_WINDOW_SECS` | 86_400 wall s | the LP loss-cap window never rolls over in a short run |
| `DD_WINDOW_SECS` | 7 days wall | the drawdown peak never rolls off |
| `MIN_POOL_TIMELOCK_SECS` / `PARAMS_TIMELOCK_MARGIN_SECS` | 1 day / 5 days | `apply_pool_params` cannot land (propose and cancel can) |
| `MAX_FUNDING_CATCHUP_PERIODS` | 168 periods | reachable only if cranks stop for 168 open minutes (≈ 3.6 test weeks at a 60 s interval) |
| `MAX_PRICE_AGE_SECS` | 60 s | unchanged and real: the price driver must publish at least every 60 s while anything needs a price |

Validation upper bounds (W ≤ 5 days, Q ≤ 1 day, profit quiet ≤ 1 h,
pre-close window ≤ 2 days) are not tightened for the test schedule; the D38
rule above is what bounds W and Q there.

## Feature gate: perps-core `localnet`

* `session::is_valid_schedule` accepts `SCHEDULE_TEST_HOURLY` only when
  perps-core is built with `--features localnet`
  (`cfg!(feature = "localnet")`); it is used by `update_market_config`
  (`MarketConfigParams::validate`), so a default build rejects the schedule
  with `PerpsError::InvalidMarketConfig`,
  like any unknown id. Tests: `test_schedule_is_only_valid_in_localnet_builds`,
  `test_schedule_needs_the_localnet_feature` (unit) and
  `test_schedule_needs_a_localnet_build` (integration; native, and on SBF
  against `target/deploy`, which is a default build).
* The feature is off by default and nothing in the workspace enables it:
  `cargo test` in CI, `scripts/build-sbf.sh` and any production deploy are
  default builds. The pure session functions compile in every build, so the
  keeper, sim and frontend mirrors need no feature (the keeper and sim declare
  the cfg in `[lints.rust]` only to silence `unexpected_cfgs`).

## Mock oracle: `programs/mock-pyth`

**TEST ONLY. Never deploy to devnet/mainnet or set a production
`ProtocolConfig.oracle_program` to it.** Whoever holds a feed's authority sets
its price at will.

* Program id `MockPyth11111111111111111111111111111111111` (no keypair: load
  it with `solana-test-validator --bpf-program <id> target/deploy-localnet/mock_pyth.so`;
  the random `mock_pyth-keypair.json` cargo-build-sbf writes next to it is
  unused). Plain `solana-program` 3 (the solana-v3 cohort Anchor 1.2 uses),
  no Anchor, no IDL.
* Feeds are Pyth Solana Receiver `PriceUpdateV2` accounts in
  pyth-solana-receiver-sdk 2.0.0's layout (134 bytes; docs/lp-pool.md D47),
  written with perps-core's `pyth_receiver.rs` and owned by the program, so
  perps-core's `oracle::read_price` reads them as it reads the Receiver's
  once `ProtocolConfig.oracle_program` = the mock-pyth id (`initialize_config`
  / protocol roles) and the market was created with the feed's id. Every
  price write stamps `publish_time` and `posted_slot` from the on-chain
  `Clock`, so `|publish_time − now| ≤ 60 s` is real.
* Instructions (first byte tag, little-endian):

  | tag | name | data after the tag | accounts |
  |---|---|---|---|
  | 0 | `Initialize` | `feed_id: [u8; 32], price: i64, conf: u64, expo: i32` (52 bytes) | feed `[writable, signer]`, authority `[signer]` |
  | 1 | `SetPrice` | `price: i64, conf: u64, expo: i32, status: u32` (24 bytes) | feed `[writable]`, authority `[signer]` |
  | 2 | `SetAuthority` | `new_authority: [u8; 32]` | feed `[writable]`, authority `[signer]` |
  | 3 | `SetVerification` | `level: u8` (0 Partial, 1 Full), `num_signatures: u8` | feed `[writable]`, authority `[signer]` |
  | 4 | `SetFeedId` | `feed_id: [u8; 32]` | feed `[writable]`, authority `[signer]` |

  Create the feed first with `SystemProgram.createAccount` (space 134,
  owner = mock-pyth, rent-exempt), in the same transaction as `Initialize` or
  before. `Initialize` writes a Full update (publish and previous publish
  time = now, EMA = price); it fails on an initialized account
  (`AccountAlreadyInitialized`) and without the feed's signature; a non-
  authority signer gets `IllegalOwner`; a wrong size `InvalidAccountData`;
  data of the wrong length (the pre-D47 20-byte `Initialize` included)
  `InvalidInstructionData`.
* `status`: 1 Trading publishes (`prev_publish_time` takes the old publish
  time); 0 Unknown, 2 Halted and 3 Auction write nothing, so the update
  freezes and goes stale after 60 s, as a Pyth feed does while its market is
  closed (4+ rejected).
* The authority is the update's `write_authority` (bytes 8..40).
  `SetVerification` (a Partial update has the Receiver's Partial layout,
  every later field one byte on) and `SetFeedId` exist for negative tests:
  perps-core refuses them with `OracleNotFullyVerified` and
  `OracleFeedIdMismatch`. `mock_pyth::read_update` reads either layout.
* Tests: `cargo test -p mock-pyth` (native) and
  `SBF_OUT_DIR=$PWD/target/deploy-localnet cargo test -p mock-pyth` (the SBF
  binary), reading the feed back with perps-core's `pyth_receiver.rs` and
  `price::validate_price`.
* The harness (lp-pool.md D51): `lib.ts` creates the feed with space 134
  (`PRICE_ACCOUNT_SIZE`) and the 53-byte `Initialize` carrying the OIL feed id
  (`mockPythInitializeIx`; `SetPrice` is unchanged), `setup.ts` passes the
  same id to `initialize_market`, and `decodeFeed` reads the update with the
  frontend's `parsePriceUpdateV2` (6-dp price, truncated as perps-core does)
  and throws on anything perps-core would not parse. `readView` also refuses
  a feed whose owner or feed id is not the market's, so the checker never
  reasons on a price no instruction can read. The self-test covers the
  encoders and the decoder.

## Localnet build

```
export PATH=<agave 4.4.1 release>/bin:$PATH
scripts/build-sbf.sh --localnet
```

Builds into `target/deploy-localnet/` (never `target/deploy/`; `SBF_OUT_DIR`
is ignored in this mode): `perps_core.so` with `--features localnet`,
`prop_amm.so`, `mm_registry.so`, `orderbook.so` (identical to the default
build), `vamm.so` and `mock_pyth.so`. The same work directory
(`target/sbf-build`) and stack check as the default build; building one mode
after the other rebuilds only perps-core. The SBF tests of the localnet
binary pair it with the feature in the test crate:

```
SBF_OUT_DIR=$PWD/target/deploy-localnet cargo test -p perps-core --features localnet --test integration
SBF_OUT_DIR=$PWD/target/deploy-localnet cargo test -p mock-pyth
```

## Safety

* Production builds cannot select the test schedule: the only path to
  `Market.session_schedule` is `update_market_config`, whose validation
  rejects id 2 unless perps-core was compiled with `localnet`, and the
  default build (CI, `scripts/build-sbf.sh`, `anchor build`) never is.
* mock-pyth is not in the default SBF build, is not referenced by any
  production config or script, and states its test-only status in its crate
  docs and here. A market whose `ProtocolConfig.oracle_program` is mock-pyth
  trusts whoever holds the feed authority, which is fine on localhost only.
* Keep both binaries in `target/deploy-localnet` and only ever load them into
  `solana-test-validator` on localhost.
* The harness talks to localhost only: every TypeScript component connects
  through `lib.ts` `connect()`, which throws on any RPC URL whose host is not
  `127.0.0.1`/`localhost`; `screenshots.js` checks the same; the validator
  binds `127.0.0.1`. Nothing in `scripts/localnet/` takes a cluster name.
* The frontend's localnet behaviour (chain price source, burner wallet,
  localnet RPC) is selected only by `VITE_NETWORK=localnet` /
  `VITE_ORACLE_SOURCE=chain`; a build without them is unchanged.

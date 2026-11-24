# Trading simulation

An agent-based simulation that drives the real programs in-process:
perps-core, mm-registry and orderbook. Set `SBF_OUT_DIR` to run their SBF
binaries. perps-core's LP pool is the house counterparty
(docs/lp-pool.md): it is initialised with the production parameters
(`PoolParams::production()`, with a one-day withdrawal delay so the unwind
fits) and the protocol's $1M seed in the market's own transaction at t0.
The agents:

- 2 market makers quoting on mm-registry;
- retail traders: momentum, mean-reversion, high-leverage, limit-order and
  house (`open_position` against the pool, at the skew price);
- a keeper that matches, cranks funding when an interval of open time is
  due, liquidates (stage-1 slices, stage-2 backstop), runs deficit-mode ADL
  while bad debt is pending and exposure-mode ADL while a trigger holds,
  settles losers' PnL into pool cash, writes off stale debt, sweeps
  insurance above its target, closes settled positions and expired orders;
- the protocol's seed LP, and public LPs (`agents.rs`): a steady one that
  deposits, requests part of its shares, withdraws or cancels; one that
  times its exit around the weekend gap (requests so the delay ends inside
  the pre-close window `W`, then tries to withdraw in `W` and at the reopen
  inside `Q`, which the gates refuse); and one that never claims its
  request, which must lapse after the claim window. Every LP redeems
  every share at the end.

They trade one OIL market on the CME session schedule, Wednesday through the
weekend reopen, against a random-walk oracle with jumps and weekend gaps.

```sh
cargo run -p sim --release -- --scenario normal --seed 1          # native
scripts/build-sbf.sh                                              # then, on SBF:
SBF_OUT_DIR=$PWD/target/deploy cargo run -p sim --release -- --scenario stress --seed 1 --report out.json
```

Options: `--scenario normal|stress|adversarial`, `--seed N`, `--days D` (default 5),
`--tick-secs S` (default 300), `--report path.json`, `--lenient-solvency`
(`--strict-solvency`, the default, is accepted and does nothing).

- **stress** adds 10–20% gaps at every reopen, 10x positions held into the
  gaps, and orders larger than the venues' per-fill cap, so bad debt and
  fill splitting happen.
- **adversarial** adds colluding pairs crossing at the band edge at maximum
  leverage, wash trades, a prober that tries every approval and venue
  rejection, a guardian pause, and `settle_market` at the end; and the LP
  pool's probes (`adversary.rs`), which script their own state: an LP
  sandwich around a donation, a flash move that makes the prober's 10x long
  stage-1 liquidatable (self-liquidation must lose against a close; an LP
  sandwich around the keeper's liquidations gains nothing), a self-crossed
  venue pair whose legs close until the net cap (and the weekend cap,
  inside `W` and outside it, with the weekend IM raised first so the
  always-on weekend cap is the tighter, D43) refuses one (a house open past
  it too; a free LP request must not make exposure ADL eligible; in longer
  runs an LP claim with the net at the weekend cap), and a 30% gap at one daily reopen
  under a pair sized to leave bad debt pending: the winner's close and its
  own mode-0 ADL inside the profit-close quiet time, a mode-0 ADL under the
  fresh floor, LP flows inside `Q`, in `W` and while debt is pending are
  refused; an opposite-side winner's close is haircut; a donation leaves one
  unit pending and a mode-0 ADL of a whole winner closes only the slice it
  needs. Also: funding probes when a period is due (LP flows refused, a
  trade cranks first), LP params that would gate exits for whole weeks
  rejected (D38), a non-keeper mode-1 ADL, and after settlement a winner's
  close refused in the quiet time and an LP sandwich around the sweep.

Every transaction's outcome is predicted by an independent model of the
engine (`model.rs`, `predict.rs`, `schedule.rs`). The model runs each
instruction on copies of the accounts it touches, as read just before; the
result is either the error the engine must fail with, or the exact Market,
LpPool, Position, UserAccount and LpPosition it must leave, its token flows
and its events (FundingUpdated, BadDebt, Adl). It shares perps-core's
dependency-free `pool_math.rs` and `session.rs` (`#[path]`), as the keeper
does; the instruction logic on top of them is its own. It checks, as
transactions land, each tick and at the end:

- open interest;
- every landed transaction's effects, field by field, and its events;
- **2a** the vault holds exactly the buckets (free, position collateral,
  insurance, cash, treasury), each equal to the model's, with free
  collateral every user's and position collateral every open position's;
- **2b** every token flow in or out of the vault;
- **2c** the pool's aggregates are the exact sums over open positions, each
  user's house notional its open house positions', the O(1) valuation
  brackets what closing everything would realise
  (`NAV_mint ≥ cash − U_eng ≥ NAV_redeem`), and nothing moves the pool or
  the market without a modelled transaction;
- **2** solvency, fatal by default: the vault covers free collateral plus
  every open position's claim at the solvency haircut, or bad debt is
  pending for ADL (`--lenient-solvency` reports it as a finding instead);
- liquidation predicates on mark notional (the engine's vs the model's),
  and nothing liquidatable survives the keeper;
- exact allowance tracking;
- that every settlement balances (payouts and fees against collateral,
  realised PnL, haircuts and deficits) and matched pairs settle as a
  zero-sum pair;
- the session schedule;
- **7** at the end: tokens conserved, nothing open, no debt pending, all
  free collateral withdrawn, and every LP share but the dead ones redeemed
  (the unwind warps past the withdrawal delay into an open session outside
  the pool's W and Q gates).

- **13** every probe's on-chain outcome is the one the spec requires (a
  named rejection, or a landing with the stated effect), checked against
  the chain apart from the model; a probe whose state a run did not reach
  is listed as not reached, with why;
- **14** every LP flow that lands passed the §9 gates, recomputed from the
  sim's own schedule (session, funding current, `Q`, `W`, pending debt,
  delay, claim window, and once settled the settled quiet time, D41);
- **15** the §17.4 weekend criterion at every reopen after a weekly close
  whose gap is at most G (weekly closes the unwind's warps cross are
  recorded too, at a zero gap); CI runs `stress` seed 1 for 5 days so one
  weekend is evaluated;
- **16** coverage: a run of at least two days reaches every required path of
  its scenario (`scenario.rs`), and no `label:rejection` pair is kept from
  the chain by a predicted rejection more than 25 times a simulated day
  (at least 50).

Admin transitions (pause, unpause through `update_market_config`,
`settle_market` with its funding crank (D42), and the pool-params
proposal and cancel) are predicted field by field like any other
instruction, not copied from the chain.

Any violation, or any error the model did not predict, makes it exit 1.

The report's `pool` section has the NAV path (hourly), the drawdown,
capital use (`capNAV`, `B`, net against the caps), haircuts by kind,
PoolCashInsufficient rejections, deficits and how the waterfall took them,
ADL by mode and trigger, funding `periods_dropped` (must be 0), and per
weekly close the net, `G·net/B`, the carried-in excess over `α·B/G` and
the documented exceptions that explain it (`weekend_bound_breaks` totals
every step that broke `G·net ≤ α·B`; the exceptions cover a step by size,
and an excess with any step they do not cover fails invariant 15, D43,
D44), the
reopen gap and the weekend drawdown against its bound (docs/lp-pool.md
§17.4), with the week's liquidations, ADL by mode, haircuts, deficits, LP
flows and peak drawdown. The `probes` section lists each probe's checks and
why any was not reached; `lps` lists each LP's own flows and outcomes.

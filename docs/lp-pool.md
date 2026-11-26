# LP pool counterparty for perps-core: implementation spec

Turns `docs/solvency-research.md` §3.3 (design C) into a buildable change. Base: `f9b8354`. Status: implemented in eba23dc and later; §0 lists departures.
Binding inputs are the owner decisions: hybrid design, a protocol-seeded pool that the public can also
deposit into, insurance excess to LPs. Where this spec departs from research §3.3, §0 says so and why.

Conventions:
- Prices, USDC and contract sizes are 6 dp, so 1 contract = `1_000_000`. Ratios are bps (`10_000` = 100%). Leverage is 3 dp.
- `floor` / `ceil` are on non-negative integers. `ceil(a, b) = (a + b - 1) / b` uses the existing `ceil_div`.
- Signed division is `div_euclid`, which floors. `ceil_s(a, b) = -((-a).div_euclid(b))` for `b > 0`.
- All on-chain code must build with Rust 1.95 (platform-tools v1.57), sBPF v3 (D48):
  - no let-chains (stable only on edition 2024; the crates are edition 2021). `div_ceil` is fine since D48;
  - every typed account is `Box<Account<..>>`;
  - every helper marked **[NI]** is `#[inline(never)]`.
- "Against the withdrawer" means the party taking value out of the pool gets the rounded-down amount.

---

## 0. Decisions and departures

D1–D21 depart from research §3.3. D22–D42 are where the code departs from this spec as first written (D22 and D23 are owner decisions after the review of eba23dc; D35–D40 come from the second review round, D41–D42 from the third); D43 is the owner's decision on the weekend bound (§18 #2, #4), D44 its review, D45 the owner's decision on LP exit liveness at the net cap (§18 #15), D46 the localnet-only test schedule and mock oracle for live runs ([localnet.md](localnet.md)), D48 the toolchain migration and D49 its TypeScript stage, D47 the Pyth Solana Receiver `PriceUpdateV2` oracle that follows them (on-chain stage), D50 its keeper stage and D51 its TypeScript, config and docs stage, D52 the review of the oracle and toolchain migration (freshness, the pinned account's address, prop-amm's owner snapshot, the sim port). The sections below describe the code.

| # | Decision | Why |
|---|---|---|
| D1 | `insurance` stays in `Market.insurance_fund`, and LpPool does not copy it | Every writer already holds `Market` mut, and the keeper, frontend and sim already read that field. A second copy would be a second source of truth |
| D2 | `pending_bad_debt` is an **ADL recovery target**, not a liability. `NAV_redeem` does not subtract it again | Pool cash never received the deficit (§2.6), so NAV already carries the loss. Subtracting it again would double count |
| D3 | New permissionless `settle_position_pnl` moves a **losing** position's unrealized loss into `cash` (Drift-style `settle_pnl`) | `cash ≥ 0` is required, but a matched winner that closes first is paid from cash while the loser's −X is still unrealized. Without this crank, an unfunded or thin pool blocks profitable closes until the losers close |
| D4 | Weekend cap = `α·NAV/G` only. No insurance term and no separate `k_net_weekend` | Insurance pays trader bad debt, not the pool's directional gap loss (owner's waterfall), so counting it overstates capacity. With G derived, `α/G` *is* the weekend multiple |
| D5 | ADL closes at **mark**. Deficit mode then haircuts the winner's profit by `min(pending, profit)` | Same economic effect as the bankruptcy price, with no per-position bankruptcy-price state |
| D6 | `Market.max_open_interest` stays a per-side **contracts** cap and becomes editable. The capital-linked caps live in LpPool | Venue fills, the sim model and the frontend all count it in contracts. Fix the comment in `scripts/initialize-market.ts` ("10M USDC" → "10M contracts") |
| D7 | Leftover equity from a stage-1 liquidation goes to the owner's **free collateral**, not to their wallet | It needs no owner token account, so the keeper's account list keeps its shape |
| D8 | The stage-2 keeper reward stays `liquidation_fee × MMR × notional`, but uses the MMR **in force** and `opened_notional` (D21) | Research §3.3 asks for the in-force MMR. `opened_notional` equals today's entry notional until `settle_position_pnl` runs, so the existing reward numbers hold |
| D9 | The four `msg!` lines the indexer parses stay byte-identical: "Position opened:", "Position closed:", "Position liquidated:", "Funding updated:". New `emit!` events are added alongside them | `backend/indexer/src/parser.rs` matches on these strings |
| D10 | Every new pricing, fee and liquidation parameter has a neutral value (§15.2) | The existing tests keep their numbers when the harness uses neutral params and seeds the pool |
| D11 | No trading (house or venue) until the pool has LPs. The first deposit locks `LP_DEAD_SHARES` permanently, and `initialize_pool` and the protocol's seed `lp_deposit` are sent in one transaction | Trading before the seed can make `NAV_mint < 0` (bricking the first deposit) or build value that a dust first depositor captures. Dead shares mean `lp_shares` never returns to 0 |
| D12 | Research's U/cash lines (GMX 60/55/50/45) are replaced. The PnL haircut applies only when `U > cash`, which means the pool is insolvent at mark. The house-open freeze, exposure-ADL and LP-withdraw lines use the **drawdown** of per-share mark NAV against a rolling peak | Matched losers who settle or close first raise `cash` and `U` by the same amount, so `U/cash` climbs while LP NAV and net are flat. The U/cash lines then haircut winners whose loser has already paid, and block LPs, with no LP risk present |
| D13 | Every pool-funded profit payout (close, stage-1 and stage-2 liquidation, settled close, ADL) first pays the opposite side's `pending_bad_debt` (`min(pending_opp, profit)`). Research blocked such closes instead | Without it, opposite-side winners close ahead of the ADL keeper and the deficit falls on LPs whatever the LP cap says. Taxing beats blocking: closes stay live, and closing costs the same as being ADL'd |
| D14 | A close that raises `\|net\|` is checked against the net cap and, inside the window, the weekend cap (research §3.3; the first draft dropped it). Since D43 the weekend cap applies at any time while `W > 0` | Otherwise a self-crossed venue pair followed by closing one leg puts unlimited net on the pool (red-team F1) |
| D15 | The insurance and LP loss caps are per rolling 24 h **loss window**, measured against NAV at the window start, not per `liquidate` call | Each bankrupt position is its own call, so a per-call cap is no cap at a gapped reopen |
| D16 | Deficit-mode ADL is permissionless and uses an on-chain score floor that decays with time since the debt was booked. Exposure-mode ADL stays keeper-signed and uses a timelocked `adl_exposure_score_floor_bps`. There is no `set_adl_score_floor` | A floor set by the key that also picks the targets constrains nothing |
| D17 | LP flows have extra gates: `lp_withdraw` is closed in the pre-close window, deposits and withdrawals are closed for `lp_reopen_quiet_secs` after each session open, a matured request lapses after a claim window, and every LP flow needs funding to be current | These close the standing weekend put, hidden-deficit pricing at the reopen, and funding-crank jumps |
| D18 | Value swept from insurance or donated to the pool vests into `NAV_redeem` over `withdrawal_delay_secs`. `NAV_mint` counts it at once | A predictable jump in NAV is otherwise captured by a deposit just before it and a request just after |
| D19 | A settled market gets a permissionless `close_settled_position`, and a settled market with no open positions sweeps its whole insurance fund to LP cash | Otherwise bankrupt and abandoned positions keep `U` and `NAV_redeem` wrong for ever, and the fund is stuck |
| D20 | Inside the pre-close window, exposure ADL is eligible while `net_notional·G > α·B` (the weekend force-reduce) | The weekend cap alone does not bound net carried in from before the window (red-team F2) |
| D21 | The per-account house notional, the close fee and the stage-2 reward use `opened_notional = floor(size·opened_price, 1e6)`, which never changes | `settle_position_pnl` rewrites `entry_price`; anything keyed on it drifts or can be moved by any caller |
| D22 | Owner decision. Every deficit-mode (mode 0) ADL slice pays the close fee `ceil(opened_notional(x)·taker_fee, 10_000)` through `distribute_fee`. As in `close_position`, the haircuts apply first and the fee is then taken from `C_x + paid`, never more. No skew impact (ADL stays at the oracle). `adl` carries no referral accounts, so there is no referral split. Exposure-mode ADL stays fee-free. `Adl` gains `fee` (§7.2) | Mode 0 is permissionless, so without a fee it is a free close for any winner opposite pending debt (review of eba23dc) |
| D23 | Owner decision. New `PoolParams.profit_close_quiet_secs` (production 300, neutral 0, `≤ 3_600`). `close_position` with `P > 0` fails `ProfitCloseQuiet` while `seconds_since_open(schedule, now) < profit_close_quiet_secs` (§4.5 step 2), and so does every deficit-mode (mode 0) ADL, which always pays a profit (§7.2 step 7, second review). On a settled market the quiet time runs from `settled_at` instead (D35). Losing closes (`P ≤ 0`), liquidations, exposure-mode ADL (keeper-signed) and venue hedges are not gated | A winner could close in the seconds after an open, before its bankrupt matched counterparty is liquidated, and escape the D13 haircut (review of eba23dc). Mode-0 ADL was the same escape whenever any older opposite pending existed (its floor reaches 0 after 6 open hours), since the owner may sign it. Accepted residual: a winner that is itself liquidatable after the gap (e.g. its collateral was cut by `settle_position_pnl`) can still be stage-1 liquidated, by itself, in the quiet time; it pays the stage-1 penalty net of the keeper share, and gating liquidations would leave risk open exactly when it matters |
| D24 | A partial ADL or stage-1 slice settles only its own funding, `fnd(q) − fnd(q − x)`; the remainder keeps its funding basis (`last_funding_payment` unchanged, `pool_replace` keeps `f_old`). A partial whose remainder equity would be `≤ 0` becomes a full close (ADL: `remainder_equity`; stage 1: needs `C' > 0` **and** remainder equity `> 0`). Both remainder-equity guards are belt-and-braces: the remainder's PnL and funding are pro rata with the slice's (same sign up to a unit), and stage 1 keeps `pen < E`, so no input found reaches them; `remainder_equity` is unit-tested directly | Settling all funding on the slice let a small slice pay out the whole position's funding and leave a bankrupt remainder (§6.3 step 3, §7.2 steps 3, 4, 9) |
| D25 | Mode-0 ADL that raises `\|net\|` must fit the close-path caps (`check_close_caps`, D14) unless settled. Like closes it gets no `PoolAdlPending` or drawdown freeze | A self-crossed venue pair plus a self-ADL otherwise puts unbounded net on the pool while opens are frozen (§3.1) |
| D26 | A vesting merge keeps `unvested(now)` and ends at the amount-weighted, floored time `now + max(1, floor((rem·(vest_end − now) + y·delay)/(rem + y)))` instead of restarting at `now + delay` | A 1-unit donation restarted the whole schedule, holding value out of `NAV_redeem` for ever (§10.3) |
| D27 | `withdraw_treasury` rejects `destination == vault_token_account` (`VaultInvariantViolated`) | A self-transfer took treasury off the books and stranded the tokens (§10.2) |
| D28 | The drawdown freeze, `dTrig` and the `lp_withdraw` drawdown block read `drawdown_now`, the drawdown against the peak **after** `touch_peak`, not `v.dd_bps` | Otherwise the 7-day rolling reset never takes effect inside the instruction that triggers it, since the revert undoes it (§2.2) |
| D29 | Interfaces: `absorb_deficit` takes the position key (for `BadDebt`); `session::funding_current(schedule, settled, last_funding_time, carry_secs, interval, now)` takes plain values and `Market::funding_current(now)` wraps it; `open_seconds_between` returns 0 when `b ≤ a`; `PositionLiquidated` leads with `position`; the ADL accounts struct is `AdlPosition` (the event owns `Adl`); `settle_close` lives in `instructions/settle_close.rs`; constants are defined once in `pool_math.rs` and re-exported; `PoolParams::production()` exists and `neutral()` is built from it | `session.rs` and `pool_math.rs` stay free of program types for the keeper and the sim; events need the position to be indexed |
| D30 | Saturation instead of an error where the spec gives no error path: `nps`, `opened_notional`, `notional_floor` and `loss_window_nav` saturate at `u64::MAX`; `adl_score` saturates at `u128::MAX` and is 0 when `E ≤ 0` | Unreachable at realistic prices inside `MAX_OPEN_INTEREST`; failing there would brick every pool instruction |
| D31 | Error choices the spec left open: mode 0 with nothing pending gives `AdlNotRequired` only after the profit and score checks; `E ≤ 0` gives `AdlCandidateNotProfitable`; mode 1 on a settled market gives `MarketSettled`; `write_off_bad_debt` with nothing pending gives `AdlNotRequired`; the checked OI decrement in `settle_close` gives `MathOverflow`; a wrong-mint treasury destination gives Anchor's `ConstraintRaw`. `adl` computes `pool_view` and `touch_peak` before its checks (nothing is applied before all pass) | Same outcome; the codes are pinned by tests |
| D32 | `propose_pool_params` with `lp_shares == 0` emits `PoolParamsApplied` as well as `PoolParamsProposed`; `apply_pool_params` and `cancel_pool_params` reset `pending_params` to default | Indexers see an immediate change take effect; no stale proposal stays in the account |
| D33 | Logs (D9 format unchanged): "Position closed:" prints `Fee=` as the fee collected (`fee_c`) and `Settlement=` as the payout; `close_settled_position` prints the same line; the non-indexer "Referral:" line drops its "(base X)" part | The line reports what moved; a settled close is a close |
| D35 | New `Market.settled_at: i64` (287..295, padding 11 → 3; §1.1), set by `settle_market`. Once settled, pending age is `open_seconds_between(schedule, since, max(since, settled_at)) + (now − max(since, settled_at))`: open time up to settlement, wall time after it (§7.2 step 5, §7.4). The profit-close quiet time (D23) of a settled market is `now − settled_at < profit_close_quiet_secs`, for `close_settled_position`, `close_position` and mode-0 ADL alike. `settled_at == 0` (settled before the field existed) keeps wall-clock age and no settled quiet time | Settlement made age retroactively wall-clock, so debt booked two open hours before a weekend became writable at once, and the mode-0 floor dropped to 0. And one settlement price can bankrupt many losers at once: without a quiet time, winners closed (permissionlessly) before the losers' deficits were booked and skipped the D13 haircut. Whether the 300 s suffices for the keeper to close every bankrupt loser of a large book is an operational question; the alternative (a counter of bankrupt positions at settlement) was not adopted |
| D36 | Pending bad debt has one clock per side: `pending_since` becomes `pending_since_long` (offset 216) and `pending_since_short` is appended after `bump` (545..553, LpPool padding 56 → 48). `absorb_deficit` weights only the bankrupt side's own clock; `apply_payout` and `recover_pending` reset a side's clock when that side reaches 0. The mode-0 floor uses the clock of the side the candidate repays. `write_off_bad_debt` writes off each side whose own age is `≥ ADL_WRITE_OFF_SECS`, and fails `BadDebtNotStale` if neither is. `BadDebt` carries both clocks | One weighted clock let fresh debt on one side inherit the other side's age: its floor started near 0, and it could be written off with the old side after a few hours of pursuit instead of 24 |
| D37 | A mode-0 ADL closes at most the smallest slice whose bad-debt haircut covers the opposite pending: `x = min(requested, deficit_slice)`, where `deficit_slice` is `ceil((need + 4)·q / P(q))` checked against the slice's actual profit (else `q`), `need = opp`, or `ceil(opp·U/cash)` when `U > cash` (the solvency haircut scales the profit first). The minimum-remainder and remainder-equity full-close rules then apply as before (§7.2 step 3), and the D22 fee is charged on `x` only | Mode 0 is permissionless: with dust pending, anyone could force-close a whole winner and, since D22, charge it the full close fee |
| D38 | `validate` also requires `W + 5·Q + 86_400 ≤ 414_000` (one CME week of open time; `W = weekend_cap_window_secs`, `Q = lp_reopen_quiet_secs`), and the claim-window floor becomes `W + 5·Q + 86_400` (was `W + Q + 86_400`). `lp_withdraw` holds a request to `max(claim_window_secs, W + 5·Q + 86_400)` with the gates in force now (`effective_claim_window_secs`). New pool_math: `WEEK_OPEN_SECS`, `LP_UNGATED_SECS`, `lp_gated_secs_per_week`, `min_claim_window_secs`, `effective_claim_window_secs` | Each bound alone let `W` and `Q` together shut `lp_withdraw` on a CME market for whole weeks (e.g. `W = 4 d, Q = 1 d`, or `Q ≥ 82_800`); `Q` gates each of the five weekly opens, not one. A later apply that widened `W` or `Q` could also make a stored window expire inside gated time |
| D39 | `NAV_redeem` and `nav_mark` (so `B`, `capNAV`, `nps` and the drawdown) count the refundable skew premium `X = min(ceil(mark·net²/(2·skew_scale·1e6)), ceil(net_notional·max_impact_bps/1e4))` as owed to traders: they use `U + X` in place of `U` (§2.2). `NAV_mint` keeps `U` (it errs against the depositor). `X = 0` with `skew_scale == 0` or once settled. `pool_view_raw` takes `skew_scale` and `max_impact_bps` | House opens pay the SIP-279 premium into `U` and closing back at the same oracle returns it, so counting it as LP value let withdrawers take value that goes back to traders (a trader-LP could raise it in one transaction), loosened every cap, and recorded a peak the unwinding then showed as drawdown |
| D40 | Funding is cranked inline (`crank_funding`: charge the due periods, emit `FundingUpdated` and the "Funding updated:" line) by `open_position`, `venue_fill`, `close_position` (unless settled), `liquidate` and `adl` (unless settled), before they set or settle a position's funding basis; `update_funding` is the same code (`apply_funding`) and still fails `FundingNotDue` when nothing is due. `settle_position_pnl` does not crank (its market is read-only, and resetting the basis to a stale `F` leaves the missed periods to be charged to the position by the next crank, which is right) | With up to 168 periods charged per crank, an open just before the first crank after an outage was credited (or charged) for hours it never held, and a close just before it escaped them; LP flows were gated (F12), trades were not |
| D41 | Third review. A settled market's `lp_withdraw` refuses with `LpFlowWindowClosed` while `settled_at > 0 && now − settled_at < max(profit_close_quiet_secs, lp_reopen_quiet_secs)` (wall time; `pool_math::settled_withdraw_quiet`). The other settled exemptions of §9.4 (W, Q, claim window, drawdown block, hard-net post-check) stand | D35's quiet time protected the D13 haircut from winners but not LP exits: until the keeper closes a loser the settlement price bankrupted, `adl_required()` is false and `NAV_redeem` still counts the unbooked loss (through `h·loss` for a house loser, through matched cancellation for a venue pair), so an LP with a matured request who raced the keeper took value from the LPs who stayed (e.g. $562.5k paid against a fair $537.5k in the reviewer's $1M-pool case). Not adopted: holding settled withdrawals until every position with `E < 0` at `settlement_price` is closed (needs a counter of such positions) |
| D42 | Third review. `settle_market` first charges the funding due since the last crank (`apply_funding` at the settlement price, logged and emitted like any crank), then sets `settlement_price`, `settled_at` and `is_paused`. It takes no oracle account: the settlement price prices the step | Nothing can crank a settled market (`crank_funding` returns early and `update_funding` fails `MarketSettled`), so without it the periods due before settlement, often many when settling during an oracle outage, were dropped and every position closed on the stale `cumulative_funding`: the one close path D40 left open |
| D43 | Owner decision: "go with 0.5× net cap for the hard weekend bound". (1) Production `k_net_bps` 10_000 → 5_000 (`α/G` with α 10%, G 20%), `hard_net_bps` 15_000 → 7_500 and `adl_net_target_bps` 5_000 → 2_500 (their ratios to `k_net` kept); `per_account_cap_bps` stays 2_500 of the net cap; neutral unchanged (§15). (2) With the weekend bound on (`weekend_cap_window_secs > 0`, `pool_math::weekend_bound_on`), `validate` also requires `k_net_bps·G ≤ weekend_lp_loss_bps·10_000` at the market's `G` (`k_net_within_weekend_bound`). (3) The weekend cap `net_notional_after·G ≤ α·X` binds every net-raising house open (`X = capNAV`), net-raising close and net-raising mode-0 ADL (`X = B`) at all times while the bound is on, not only inside `W`; `W` keeps its other roles (LP exits shut, `wTrig`). (4) `lp_withdraw` (not settled, bound on) also requires `G·net_notional ≤ α·B'`, failing with the hard-net post-check's `PoolCapWouldBreach` (message widened to "hard line or weekend bound"). `check_open_caps` and `check_close_caps` lose their `now` argument. What this guarantees, and what it does not, is §3.1 | The weekend bound becomes hard by construction for net put on under the caps: `G·net_at_close ≤ α·B` no longer depends on the Friday force-reduce, which can only remove winners. `update_market_config` does not see the pool, so a later raise of the IM or weekend IM raises a derived `G` past what `validate` saw; the always-on cap tightens with it at once instead of only inside `W`. The withdraw check stops LP capital that backs the net from leaving below the bound outside `W`. Cost: half the weekday net capacity (red-team F2's "not adopted" alternative, now the owner's choice) |
| D44 | D43 review. (1) The weekend cap's capital is net of the trade's own skew-premium shortfall: `X − premium_shortfall(net, net_after, oracle, exec, size)` (`pool_math`), the rise in the unwind premium `NAV_redeem` owes (D39) beyond the premium the trader paid at `exec`; `check_open_caps` and `check_close_caps` take `exec` (a mode-0 ADL slice passes the mark, so all of its `Δx` counts). (2) `apply_pool_params` still checks `validate` only: a params change that tightens the bound over net already open (W turned on, α lowered, `weekend_gap_bps` raised) is a stated exception (§3.1 item 4), not refused. (3) With the net at the 0.5× cap, an LP who requests after the net was put on cannot claim until the net shrinks: exposure ADL stops at the same line. Stated as a cost of D43 (§3.1, §9.4, R8); the exit-liveness rule is an open owner decision (§18 #15) | (1) A net-raising trade that crosses into the `max_impact` clamp pays `s·o·max_impact` while `x` rises by more, so `B` fell in the same transaction by up to `o·skew_scale·max_impact²/2` (reviewer: $9,375 on a $12.75M pool at $75, ending at `≈ α/(1 − 0.0007)`); a mode-0 ADL at the mark lowered `B` by its whole `Δx`. With it subtracted, a capped trade keeps `G·net_notional ≤ α·B` after it, up to rounding. (2) Refusing it needs the oracle in `apply_pool_params` (a new account); the admin sees the net when proposing and the timelock is 10 d. (3) Before D43 the hard-net post-check at 1.5× left a third of `B` free with the net at the 1× cap |
| D45 | Owner decision on §18 #15: "option 1", a buffer between the trading net cap and the weekend withdraw line, instead of a keeper-run exit ADL. (1) Production `k_net_bps` 5_000 → 4_000 (0.4×), `hard_net_bps` 7_500 → 6_000 and `adl_net_target_bps` 2_500 → 2_000 (the same 1.5× and 0.5× of `k_net`); `per_account_cap_bps` stays 2_500 of the net cap (0.1 × `capNAV` per account). The weekend line stays `α/G` = 0.5× at G 20%, so the **exit buffer** `1 − k_net·G/α` is 20%. Neutral params are unchanged: `PoolParams::neutral()` pins `adl_net_target_bps` at 2_500 instead of taking the new production value (§15.2). (2) No on-chain logic changes: `validate` keeps `k_net·G ≤ α·10_000`, so `k_net = α/G` stays valid but leaves no exit room at the cap. (3) `scripts/initialize-market.ts` keeps the same 20% buffer for every commodity: `k_net = min(production, floor(0.8·α·10_000/G))`, `hard_net` and the drawdown-ADL target at 1.5× and 0.5× of it (OIL and GOLD get exactly `PoolParams::production()`; SILVER and COPPER at G 24% get 3_333 / 4_999 / 1_666, NATGAS at G 30% 2_666 / 3_999 / 1_333). (4) What the room is, exactly, is §3.1 (exit-liveness cost); the in-process test `production_exit_buffer_at_the_net_cap` measures $400,000.000092 of exit room on `B` = $1,999,999.999992 with traders at the cap (20.00%), and one share more is refused. The room is pool-wide and first come, first served among matured claims (`capNAV` keeps traders, not other LPs, off pending capital), and a price rise, a fall in `B`, a cap-exempt liquidation of the minority side or a later raise of `G` can eat it (§3.1) | With the net cap at the weekend line (D43, D44) an LP who requested after traders filled the cap could not claim any amount until traders closed. With the buffer, LPs keep about 20% of `B` of exit room at the cap at a steady price, at the cost of a fifth of D43's weekday capacity: the net cap is now 0.4× (0.5× under D43, 1× before it). The rejected alternative, a keeper-run exit ADL (§18 #15 option (a)), closes profitable traders on LP timing (bad trader UX), can be gamed by an LP who also trades the opposite side, and cannot touch a losing net. A utilization or holding fee on the net remains a possible later addition |
| D46 | Owner request: a live localnet simulation with a test schedule. (1) New `session::SCHEDULE_TEST_HOURLY = 2`: a CME-like week compressed into each hour of Unix time (open `[0, 3_000)` minus 60 s breaks at 750, 1_500 and 2_250 s, weekend `[3_000, 3_600)`; 2_820 open s, four opens a week), known to every session function and mirror. (2) `is_valid_schedule` accepts it only in a perps-core built with the `localnet` cargo feature (off by default; `scripts/build-sbf.sh --localnet` into `target/deploy-localnet`), so a default/production build rejects it with `InvalidMarketConfig`. (3) D38 becomes schedule-aware: `lp_gated_secs_per_week`, `min_claim_window_secs` and `effective_claim_window_secs` take the schedule, and `validate` uses `lp_gates_leave_exit_time(m.session_schedule, W, Q)`: `W + opens·Q + ungated ≤ week_open_secs` with (414_000, 86_400, 5) for every schedule but the test one and (2_820, 600, 4) for it, so nothing changes for `SCHEDULE_ALWAYS_OPEN` or `SCHEDULE_CME_GLOBEX` (pinned by `lp_gate_rule_is_unchanged_on_production_schedules` and the unchanged CME session vectors). (4) A test-only `programs/mock-pyth` oracle (writable Pyth v2 feeds stamped with the on-chain clock), built only by `--localnet`; perps-core is unchanged for it. Details and what a live run cannot reach: [localnet.md](localnet.md) | A run of a few hours on `solana-test-validator` crosses several weekly closes and reopens with the real binaries, keeper and bots; the gate keeps the schedule out of production builds |
| D47 | Oracle migration, stage 1 (on-chain programs and their Rust tests): the Pyth Solana Receiver's `PriceUpdateV2` replaces the legacy Pyth v2 `PriceAccount` (the OIL key `GVXRS…` in the TS configs is Pyth's legacy BTC/USD account, not oil). (1) `Market` appends `pyth_feed_id: [u8; 32]` (295..327), `oracle_program` (327..359; `ProtocolConfig.oracle_program` when the feed was set), `pending_price_feed` (359..391), `pending_feed_id` (391..423) and `pending_oracle_at: i64` (423..431, 0 = none); LEN 298 → 434, still 3 padding bytes (pre-launch: no market to migrate). `pyth_price_feed` stays at 104 as the pinned account (on mainnet the push oracle's shard-0 feed account, PDA `[0u16 LE, feed_id]`; the mock on a localnet), and the open interest stays at 168/176. `InitializeMarketParams` appends `pyth_feed_id` (zero → `InvalidMarketConfig`); `initialize_market` keeps its owner constraint and requires a Full update for that id (no age check: a market may be created while closed). (2) `read_price` / `read_price_max_age` (every price read: open, close, liquidate, adl, settle_position_pnl, update_funding, venue_fill with the venue's tighter age, lp_deposit, lp_request_withdrawal, lp_withdraw; their account lists are unchanged) check in order: the key (each Accounts constraint still refuses another key with `ConstraintRaw` first), owner == `Market.oracle_program`, at least 133 bytes, the discriminator, `VerificationLevel::Full`, the feed id, `|publish_time − now| ≤ max age` both ways (as before; since D52 the future side is a fixed 60 s whatever the age limit), then the unchanged `validate_price` (non-positive or not representable in 6 dp → `InvalidOraclePrice`, so `settle_market` stays the path for a negative WTI; confidence; floor). A wrong owner, size, discriminator or verification tag is `InvalidOracleAccount` (6031; an unparsable account used to be `InvalidOraclePrice`); new `OracleNotFullyVerified` (6093) and `OracleFeedIdMismatch` (6094). EMA, `prev_publish_time`, `posted_slot` and `write_authority` are not used. (3) Feed rotation (`instructions/oracle_admin.rs`): `propose_oracle_feed(feed_id)` (admin; accounts `admin`, `config`, `market` mut, `lp_pool`, `new_price_feed` owned by `config.oracle_program`) validates as `initialize_market` does and is refused once settled. While `lp_shares == 0` it applies at once (`OracleFeedProposed` + `OracleFeedApplied`, as D32's params); otherwise it is pending until `now + params_timelock_secs(d, d, W, W)` of the pool's current params (`d = withdrawal_delay_secs`, `W = weekend_cap_window_secs`: 4 d + 1 d + 5 d = 10 d at production, the 5 d margin at neutral, never under 1 d), and a new proposal replaces it and restarts the clock. `apply_oracle_feed()` (anyone; `market` mut, `config`, `new_price_feed` == the pending key, else `InvalidOracleAccount`) is `MarketSettled` on a settled market (D52), `OracleChangeTimelocked` (6096) before that time or with nothing pending, re-validates the feed against `config.oracle_program` as it is then and copies that program into the market. `cancel_oracle_feed()` (admin; `config`, `market` mut) is `NoPendingOracleChange` (6095) without one. Events `OracleFeedProposed` / `OracleFeedApplied` / `OracleFeedCancelled { market, price_feed, feed_id, effective_at }`. A later `update_roles` change of `oracle_program` reaches existing markets only through this rotation. (4) The reader is the dependency-free `pyth_receiver.rs` (a perps-core module; prop-amm `include!`s it, mock-pyth `#[path]`s it, the keeper will), pinned to pyth-solana-receiver-sdk 2.0.0 (a dev-dependency only) by `tests/pyth_receiver_parity.rs`: 5,000 random Full updates each way, Partial updates, the shared rejections, and the SDK-made fixtures `tests/fixtures/price_update_v2_{full,partial}.bin` for mirrors that cannot link the SDK. The SDK does build for sBPF v3 (a probe program reading `Account<PriceUpdateV2>`: 81,888 bytes, no stack report), but it is not linked: it adds 89 crates to the production build (among them solana-program 6.0 next to Anchor's v3 component crates, pythnet-sdk, pyth-sdk 0.5, borsh 0.9, k256/ecdsa, serde_json, schemars) for a fixed 133-byte layout, `Account<PriceUpdateV2>` fixes the owner to the Receiver's id (no localnet mock), `get_price_no_older_than` checks age one way only, and the keeper and mock-pyth need the reader without Anchor anyway. (5) prop-amm: `LpVault.pyth_feed_id` is taken from the padding (LEN unchanged), `InitializeVaultParams` appends `pyth_feed_id`, and open/close read with the same checks and order (owner == `config.oracle_program`, read each time until D52 snapshotted it into `LpVault.oracle_program`; 60 s two-sided age; default 1% confidence); new `PropAmmError::OracleNotFullyVerified` (6017) and `OracleFeedIdMismatch` (6018). orderbook and mm-registry only forward the feed account; vamm reads none. (6) mock-pyth (test only) owns 134-byte `PriceUpdateV2` accounts whose `write_authority` is the feed authority: `Initialize(feed_id, price, conf, expo)` (53 bytes with the tag), `SetPrice` with status 1 publishes (`prev_publish_time` = the old publish time) while 0/2/3 write nothing (the update goes stale after 60 s, as a closed Pyth market's does), `SetAuthority`, and new `SetVerification(level, num_signatures)` and `SetFeedId` for negative tests ([localnet.md](localnet.md)). (7) Tests: `tests/oracle/mod.rs` runs every refusal and the accepted edge cases (±60 s, exactly 1% confidence, exponents 0, +1 and −12, the unused 134th byte) at open_position, five refusals at each of the nine other price readers, the market limits, the pinned key, the settled path and the whole rotation; venue_fill, initialize_market and prop-amm have their own cases; all run natively and on SBF. CU on SBF: a price read costs about 626 CU more (update_funding 19_155 → 19_783, adl 55_354 → 55_980, the instructions whose test CU is deterministic). The other measurements move in 1,500-CU steps between runs of the same binary (PDA bump searches over the tests' random keys); the lowest of six runs: open 55_047, close 41_273, liquidate 59_674, worst cases 60_507 / 55_813, venue_fill 53_714, lp_deposit / lp_withdraw 36_375 / 46_041, fill_quote 79_985, match_orders 80_843. propose_oracle_feed 18_580, apply_oracle_feed 14_945, cancel_oracle_feed about 8_000. No limit changed; the oracle helpers are [NI]; perps_core.so 1_027_928 → 1_062_408 bytes. The CI sim runs on SBF (six 2-day runs, stress seed 1 over 5 days) give reports identical to D48's except compute units, wall time and the `StaleOraclePrice` error examples (now `require!`, which names `src/oracle.rs`). (8) Not migrated in this stage: the keeper (it still `#[path]`s `pyth_legacy.rs`, which no program compiles any more, and decodes the Market prefix unchanged), and the TypeScript side (`scripts/test-ts.sh` genesis feeds, `initialize-market.ts`'s feed table, the localnet harness's mock-pyth encoders and `initialize_market` call, the frontend's `oracleAccount.ts` and feed config), which still write or read legacy feeds and so fail against these programs until their stage (the keeper is migrated in D50, the TypeScript side in D51). The committed IDL copies are refreshed (`gen-idl.sh --sync`) and the frontend's error table lists the four new errors (its test reads `errors.rs`). Owner decisions this leaves open are §18 #17–#20 | Pinning one account per market, which the push oracle only moves forward, limits a third-party pusher to "newer than stored"; accepting any Receiver-owned update with the right feed id would let a caller pick any verified update inside the age window (Pyth's "adversarial selection"). Snapshotting the owner into `Market` keeps every price-reading instruction's account list unchanged. The timelock is the pool-params rule, so LPs have their withdrawal delay and claim window to try to leave before a feed they distrust takes effect; it is not a guaranteed exit (a claim can still be refused by `PoolCapWouldBreach`, `PoolWithdrawalsBlocked` or `LpFlowWindowClosed`), and it does not bound the admin, who can `settle_market` at any positive price at once, so the admin stays a trusted party for prices (D52, §18 #22). Pinning also does not stop a caller from choosing between the stored price and a newer one it pushes itself (D52, §18 #21). Hand parsing keeps the production binary free of the SDK's dependency tree and is held to the SDK's own bytes by the parity test |
| D48 | Toolchain migration, stage 1 (on-chain programs and their Rust tests). (1) Agave 4.4.1 CLI (cargo-build-sbf 4.5.0, platform-tools v1.57 = Rust 1.95); every binary is sBPF v3 (`--arch v3 --tools-version v1.57`; `scripts/build-sbf.sh` checks ELF `e_flags == 3`). The stack gate now matches platform-tools v1.57's "overflows the maximum allowed frame space" as well as "Stack offset" (cargo-build-sbf still exits 0 on it); the AbiEnumVisitor exemption, the blake3/ahash pins and the lockfile v3 rewrite are gone. (2) Anchor 1.2.2 on the solana-v3 cohort (anchor-lang's default; anchor-spl with `token`, `token_2022`, `solana-v3`: `init` with `token::mint` needs `token_2022`); mock-pyth on solana-program 3. API changes only: `ctx.bumps.<name>`, `CpiContext::new(program_id: Pubkey, ..)`, `UncheckedAccount` for raw accounts, `borsh::to_vec`, `T::DISCRIMINATOR`, the instructions-sysvar loaders from `solana-instructions-sysvar` 3, features `idl-build` / `anchor-debug` / `custom-heap` / `custom-panic`, `Xargo.toml` deleted. (3) Anchor 1.x refuses the same account in two mutable `Account` fields (`ConstraintDuplicateMutableAccount`, 2040) before any field constraint. Three fields are `dup` so their refusals keep their codes: `venue_fill.maker_user` (maker == taker stays `SelfTrade`), `close_position.referrer_token_account` (the trader's own account stays `InvalidReferralParams`), `withdraw_treasury.destination` (the vault stays `VaultInvariantViolated`); each is still refused, so no aliased fill, close or withdrawal commits. Any other aliased mutable pair now fails 2040, where it used to reach the handler. (4) pyth-sdk-solana 0.8 does not build on these crates: the dependency-free `pyth_legacy.rs` (shared with prop-amm by `include!` and with the mock-pyth tests by `#[path]`) reads the same legacy v2 `PriceAccount` and returns what `load_price_feed_from_account_info(..)?.get_price_no_older_than(now, age)` did (header checks, aggregate while Trading else `prev_*`, account `expo`, two-sided age); 200,000 random accounts gave identical results against pyth-sdk-solana 0.8 itself (one-off check, not in the repo), and its unit tests pinned byte fixtures. Only difference: an age difference that overflows i64 is stale instead of a panic. This was the stopgap until the oracle stage: D47 replaced it on chain and D50 in the keeper, after which nothing compiled the file or its tests, and D52 deleted it. (5) Tests: solana-program-test 4.4.1 (`agave-unstable-api`) on host Rust 1.98.1. They reach program-test through `test-sdk/` (crate `oil-test-sdk`, depended on as `solana-sdk`, so test paths are unchanged), because solana-sdk 5.0 switches on solana-account's `bincode` feature and solana-runtime 4.4.1 then fails to compile. Natively, the v3-cohort programs call solana-sysvar 3's syscall stubs while program-test sets solana-sysvar 5's, so `test-sdk/src/native_stubs.rs` forwards them (clock, CPI, stack height). Anchor 1.2's native `emit!` drops events, so perps-core's events go through `emit_event!` (lib.rs): on SBF exactly `anchor_lang::emit!`, natively it also logs "Program data: …" through those stubs. Natively there are still no `msg!` lines and no return data, so the one return-data assert (mm-registry `fill_quote`) runs on SBF only. CU on SBF with the same runtime (old = the Solana 1.18 / Anchor 0.28 / sBPF v0 binaries of 26b4124): open 63_790 → 57_421, close 52_859 → 43_649, liquidate 77_254 → 62_052, update_funding 23_059 → 19_155, adl 63_462 → 55_354, liquidate worst cases 73_862 / 68_429 → 61_364 / 55_191, venue_fill 66_807 → 54_590, lp_deposit / lp_withdraw 42_150 / 53_788 → 38_960 / 48_620, fill_quote 103_816 → 77_895, match_orders 104_084 → 80_211; no limit changed. Binaries are 33–45% smaller (perps_core 1_544_704 → 1_027_928 bytes). The tests and asserts are unchanged except the gated return data; the only tests gone are the `test_id` tests that Anchor 0.28's `declare_id!` generated. Not migrated in this stage: `sim/` (out of the workspace then; ported later, D52 (7)), backend, SDK, frontend, TS tests, `scripts/idlgen`, `scripts/test-ts.sh`, the localnet harness and CI (D49 and D51 the TypeScript side, D50 the keeper) | Pre-launch, every deploy is new, and once SIMD-0500 is active a cluster refuses v0–v2 deploys and upgrades. pyth-solana-receiver-sdk (next stage) depends on anchor-lang with default features, which fixes the solana-v3 cohort; it shares types with the 4.x test stack. The legacy reader keeps oracle behaviour identical, so the toolchain change is tested on its own before the oracle changes. `dup` on exactly the fields whose refusal codes tests pin keeps behaviour, and Anchor's new check covers every other pair |
| D49 | Toolchain migration, stage 2 (TypeScript consumers of D48). (1) `@coral-xyz/anchor` 0.28 (root: TS suite, `scripts/`, localnet harness, mm-registry SDK) and `@project-serum/anchor` 0.26 (frontend) → `@anchor-lang/core` 1.2.2, Anchor 1.2's TS client (the 0.30+ API: `new Program(idl, provider)` takes the program id from the IDL's `address`; `initialize-market.ts` and the harness refuse an IDL whose address is not the expected id, the frontend overrides it with `PERPS_CORE_PROGRAM_ID`). (2) IDLs are spec 0.1.0, from `scripts/gen-idl.sh`: `scripts/idlgen` now wraps anchor-lang-idl 0.1.4's `IdlBuilder` (the builder anchor-cli 1.2.2 calls; it compiles each program's lib tests with `idl-build` and reads what `__anchor_private_print_idl` prints) and anchor-cli 1.2.2's `idl_ts` for `target/types`, without anchor-cli. Like `build-sbf.sh` it builds dev-dependency-free copies (in `target/idl-build`, with the workspace lockfile's versions): lib tests link the dev-dependencies, and solana-program-test 4.4 would add several GB and need Rust 1.98, so any host Rust that builds anchor-lang 1.2.2 works. Resolution stays off (`Anchor.toml`). anchor-lang-idl 0.1.4 passes a literal `+{toolchain}` argument to cargo when `RUSTUP_TOOLCHAIN` is set (a missing `format!`; rustup sets it under `cargo +<t> run`), so idlgen moves that toolchain's `bin` to the front of `PATH` and unsets the variable. Instruction, account and event discriminators and every account's Borsh layout are unchanged (each IDL account size is its `LEN` minus its padding), so the hand-written codecs (`perpsPool.ts`, the SDK's perps-core instructions, the frontend's `oracleAccount.ts`) are unchanged; the TS suite's layout tests check them against the IDL coder and the deployed program. (3) A `Program` camelCases every IDL name, account, type and event names included: `coder.accounts.decode("market", ..)`, `coder.types.encode("poolParams", ..)`, `program.rawIdl` for the Rust names (the TS suite's error-table test). The harness's `decodeEvents` decodes with the camelCased IDL and restores the Rust event name ("PositionLiquidated"), so `tx.jsonl`, the checker and its report keep the names and camelCase fields Anchor 0.28 gave. (4) 0.26/0.28 filled omitted well-known accounts (`systemProgram`, `tokenProgram`, sysvars) by name; 1.2 resolves only an IDL `address`, PDA or relation, which resolution off leaves out (signers still default to the provider wallet). Every consumer is therefore typed with the generated IDL type (`Program<PerpsCore>`, `Program<MmRegistry>`, `Program<Orderbook>`), so `tsc` rejects an instruction with an account missing; a bare `Program` has no account or method names in 1.2, which the SDK's untyped `Program` relied on. The typing found three frontend calls without `systemProgram` that 1.2 would fail at runtime: `create_referral_code`, `apply_referral_code` (`useOnChainReferral.ts`) and `init_lp_position` (`useVault.ts`); they now pass it. (5) The frontend and the SDK build without `target/`, so they keep committed copies: `frontend/src/idl/perps_core.{json,ts}` and `sdk/mm-registry/src/idl/mm_registry.{json,ts}` (the SDK exports `MM_REGISTRY_IDL` and `MmRegistryIdl`). `scripts/gen-idl.sh --sync` refreshes them and `--check` (CI, programs job) fails on drift. (6) CI: Rust 1.98.1 in every Rust job (the 1.97 pin was for solana-client 1.18), the Agave 4.4.1 release and platform-tools v1.57 cached under `~/agave-v4.4.1` and `~/.cache/solana/v1.57` (sbf and localnet-smoke), `gen-idl.sh` before the harness's `tsc`, and the frontend job also runs `npm run build`. `yarn.lock` regenerated with the new dependency. Verified: `scripts/test-ts.sh` 75 passing on solana-test-validator 4.4.1 (as before), the harness's `tsc` and self-test, the frontend's 180 vitest tests and build, and a 10-minute live localnet run with the keeper binary built in stage 1 (setup, price driver, keeper, bots, checker; PASS, one weekly close and no reopen crossed, no unexpected error code) | The on-chain stage changed the IDL format and the client package; keeping the TS layer on 0.28 would have meant hand-converting IDLs. Typing every program with its IDL makes a missing or renamed account a compile error instead of a runtime one, which matters most for the frontend and the harness, which no test suite runs end to end. Committed IDL copies keep the frontend and SDK buildable on their own, and the CI check keeps them honest |
| D50 | Oracle migration, stage 2a (the keeper, `backend/keeper`, after D47). (1) It `#[path]`s perps-core's `pyth_receiver.rs` in place of `pyth_legacy.rs` (which nothing compiled after this; deleted in D52), and the new `oracle.rs` mirrors `oracle::read_price` in the program's order: the pinned key (`Market.pyth_price_feed`), owner == `Market.oracle_program` (the RPC account's owner), at least 133 bytes, the discriminator, `Full`, the feed id (`Market.pyth_feed_id`), `|publish_time − now| ≤ MAX_PRICE_AGE_SECS` both ways, then the shared `validate_price`. Each refusal is an `OracleRejection` that names the error perps-core returns (ConstraintRaw 2003, `InvalidOracleAccount` 6031, `OracleNotFullyVerified` 6093, `OracleFeedIdMismatch` 6094, `StaleOraclePrice` 6009, `InvalidOraclePrice` 6008, `OracleConfidenceTooWide` 6029, `OraclePriceBelowFloor` 6030), and its log message ends with that code (`…; program error 6093`); a test pins these numbers, and those in `ix.rs`, against the variant order of `errors.rs`. (2) `MarketData` decodes the D47 fields (`pyth_feed_id`, `oracle_program`, `pending_price_feed`, `pending_feed_id`, `pending_oracle_at`; LEN 434, checked against the `Market::LEN` sum in `state/market.rs`); a market in the 298-byte layout no longer decodes, and a program-owned account at a market address that does not decode is now logged as a warning (it used to be skipped silently, as an uninitialized market still is). (3) Liquidation, ADL and the settle loop read the price through `oracle::fetch_oracle_price` (account lists unchanged: the market's current `pyth_price_feed`, re-read every pass, so an applied rotation is followed on the next pass). The funding loop now reads the price before it sends `update_funding` (which reads it before `FundingNotDue`), so a feed the program would refuse is logged with its reason and no crank is sent; it used to fail at preflight. (4) Tests (139 pass, `cargo test` on Rust 1.98.1): the SDK-made fixtures `tests/fixtures/price_update_v2_{full,partial}.bin` (valid at `WEDNESDAY` = 75_123_456; the shared writer gives the same bytes), stale at ±61 s and fresh at ±60 s, wrong feed id (XAU's on the OIL fixture, one flipped bit, an unset id; checked before age), Partial (the fixture, the writer's, before feed id and age), an unknown tag, wrong owner, wrong key, short, wrong discriminator, a legacy v2 account, and the price checks (exactly 1% confidence, the floor, negative, exponents 0, +1, −12, the market's own limit). (5) Not done here: the keeper does not crank `apply_oracle_feed` (anyone may) or report a pending rotation; it was not run against a validator in this stage (the localnet harness still writes legacy feeds until its stage) | The keeper decides with the program's own parser and check order, so it sends nothing the program would refuse on the price, and a refusal's log names the error code a failed transaction would carry. Pinning the codes and `Market::LEN` against the program's sources catches drift that a hand-written mirror would not |
| D51 | Oracle migration, stage 2b (TypeScript, config and docs; what D47 (8) left out besides the keeper, which is D50). (1) **One feed table**, `frontend/src/config/pythFeeds.ts` (dependency-free; the frontend, the deploy scripts through `scripts/pyth-feeds.ts` and the localnet harness import it): the two Pyth program pairs (`pro`: Receiver `rec2HHDD…`, push oracle `pyt2F414…`; `legacy`: `rec5EKMG…` / `pythWSns…`), shard 0, the Pyth Core feed ids (OIL `Commodities.USOILSPOT` `925ca92f…b3e6`, GOLD XAU/USD `765d2ba9…4bb2`, SILVER XAG/USD `f2fb02c3…871e`, NATGAS and COPPER `null`) and the push-feed seeds `[shard_id as u16 LE, feed_id]`. Each market's account is that PDA under the push oracle. A test pins the derivation to Pyth's documented SOL/USD shard-0 account (`7UVimffx…` under `pythWSns…`) and to the SDK-computed commodity accounts (OIL `65sQHMgQ…` pro / `4LPPjSGx…` legacy, GOLD `csXV1aFF…` / `2uPQGpm8…`, SILVER `6Ls6yntv…` / `H9JxsWwt…`; whether they exist on any cluster was not checked). Every legacy key (`GVXRS…`, which is Pyth's legacy BTC/USD account, and the unverified `sXgHcPCN…`, `77JipqJa…`, `DBE3N8uN…`, `4wxQsP2B…`) is gone from the frontend config, `initialize-market.ts` and the `backend/api/src/routes.rs` constants (shard-0 pro addresses, empty for NATGAS/COPPER; only the constant changed, backend/api was not compiled). (2) `frontend/src/utils/oracleAccount.ts` mirrors `pyth_receiver.rs`: `parsePriceUpdateV2` with the same rejection order (`tooShort` < 133 bytes, `wrongDiscriminator`, `notFullyVerified`, `badVerificationLevel`), a Full writer, the two-sided `isFresh`, `decodeOraclePrice` (null unless Full; `trading` now means fresh by publish time, since PriceUpdateV2 has no status) and `marketOraclePrice`, which the chain price source uses: owner == `Market.oracle_program` (offset 327) and feed id == `Market.pyth_feed_id` (295), else no price. The tests read the SDK fixtures (`tests/fixtures/price_update_v2_{full,partial}.bin`): the parser reads the SDK's bytes, the writer reproduces them exactly, and the Partial fixture is refused. (3) Frontend transactions pass the market's pinned `pyth_price_feed` (open in `useTrading`, every LP flow via `useLpPool().priceFeed`) instead of the config table, so a rotation needs no frontend release. `pythPriceFeed(id)` (`VITE_PYTH_FEED_<ID>`, else the shard-0 PDA under `VITE_PYTH_PROGRAMS`, default `pro`) is only the configured default now and is derived on first use (web3.js cannot derive addresses at import under jsdom). `CommodityConfig.pythPriceFeed` → `pythFeedId`. (4) SDK: `MARKET_LEN` 434, `MARKET_ORACLE_OFFSETS`, `decodeMarketOracleFields`; `PerpsMarketInfo` gains `pythFeedId` and `oracleProgram` (the fill's accounts are unchanged). (5) Deploy scripts (shared helpers moved to `scripts/common.ts`). `initialize-market.ts` requires `PYTH_PROGRAMS=pro|legacy` (no default, as `PYTH_PROGRAM_ID` had none: the pair is §18 #18). The config's oracle program is that pair's Receiver, and an existing config with another one is refused. The feed is the table's shard-0 account; NATGAS/COPPER are refused before anything is sent. `assertPythFeed` checks existence ("the pusher must push first"), owner, discriminator, Full and feed id, with no age check (as on chain). `pythFeedId` goes into `initialize_market`. An existing market must pin the table's account and id (otherwise rotate it) and is checked against its own snapshot owner. New `scripts/oracle-feed.ts status\|propose\|apply\|cancel <COMMODITY> [--feed-id hex] [--feed key]` checks everything the program checks before it sends. (6) TS suite: genesis feeds come from `scripts/test-oracles.js`, a writer independent of the TS decoder whose output the suite compares byte for byte with the SDK fixtures: OIL every 50 s for 3 h, stale, XAU, XAG, Partial (13 signatures), a wrong owner (the undeployed mock-pyth id: solana-test-validator 4.4.1 panics at genesis on a token-program-owned account of these bytes) and a legacy v2 account. The GOLD market gets its own XAU feed. New tests (10): the writer's bytes; Market 434 bytes with the oracle fields where the SDK and the IDL read them; `initialize_market` refusals (zero id `InvalidMarketConfig`, Partial `OracleNotFullyVerified`, XAG or XAU id `OracleFeedIdMismatch`, wrong owner / legacy / missing account `InvalidOracleAccount`, a stale feed accepted at creation); a publish time 75–300 s ahead is `StaleOraclePrice`; a key other than the pinned one is `ConstraintRaw`; rotation before the seed (admin only, the same refusals, applied at once with `OracleFeedProposed` + `OracleFeedApplied`, to the XAU id and back; with nothing pending, `apply` is `OracleChangeTimelocked` with the zero key and `InvalidOracleAccount` with any other key, and `cancel` is `NoPendingOracleChange`; the seed must then use the new feed; a settled market is `MarketSettled`); and the seeded rotation at neutral (432_000 s) and production (864_000 s) params (pending fields, the event's `effective_at`, `apply` before then, a wrong key, a replacing proposal that restarts the clock, an admin-only cancel with its event). The error table has 97 entries. A successful `apply_oracle_feed` after the timelock cannot happen on a live validator (5 or 10 days); only the Rust tests cover it. (7) Localnet harness: `PRICE_ACCOUNT_SIZE` 134, `mockPythInitializeIx` (53 bytes, with the feed id; `SetPrice` unchanged), `decodeFeed` parses PriceUpdateV2 and throws on anything else, `readView` refuses a feed whose owner or id is not the market's, `setup.ts` passes `pythFeedId` (the production OIL id) and checks what the market pinned, and `state.json` gains `feedId`; the self-test covers the encoders and the decoder. `EXPECTED_REFUSALS` is unchanged (no bot provokes the new errors). Not migrated: backend/api's Hermes client and backend/indexer (out of scope). The keeper was migrated in parallel (D50) and was not run with this harness in this stage. (8) Verified: `scripts/test-ts.sh` 85 passing on solana-test-validator 4.4.1 (75 before, plus the 10 above); the frontend's vitest (198) and `npm run build`; the harness's `tsc` and self-test (7); `gen-idl.sh` output identical to the committed copies; a 5-minute live localnet run without the keeper (setup, price driver, bots, checker; PASS, 60 of 60 checks consistent, two daily-break edges including the stale reopen, whose probe was refused `StaleOraclePrice` as expected, 0 unexpected codes); and a localhost dry run of `initialize-market.ts` and `oracle-feed.ts` (scratch copies with a local mint in place of the testnet USDC mint, PriceUpdateV2 genesis accounts at the pro shard-0 addresses owned by `rec2HHDD…`): the refusals without `PYTH_PROGRAMS` and for NATGAS, config and market creation (the stale-seed fallback, then a seeded GOLD market), an existing config of the other pair refused, a wrong feed id refused before sending, an immediate rotation and back on the unseeded market, and a 864_000 s pending rotation on the seeded one, with `apply` refused and `cancel` clearing it. `backend/api` was not compiled | One table shared by app, scripts and harness, derived rather than copied, so a feed id change cannot leave a second copy behind among them (two copies remain outside it: `backend/api/src/routes.rs`'s addresses, pinned to this table's derivation by a frontend test since D52, and `scripts/test-oracles.js`, which the TS suite compares with the SDK fixtures), and its derivation is tested against Pyth's own published address. Transactions take the pinned key from the market because the program accepts no other, which makes rotations transparent to clients. The scripts refuse an unknown feed id or an unstated program pair rather than guess, since the wrong guess fails closed only on chain and after a deploy. Mirroring the Rust rejection order lets UIs and scripts report the same reason the program would |
| D52 | Review of the oracle and toolchain migration (D47–D51). (1) **Freshness.** `pyth_receiver::is_fresh(publish, now, max_age)` is now `now − publish ≤ max_age` and `publish − now ≤ MAX_FUTURE_PUBLISH_SECS` (60), the future side a fixed bound separate from the age limit (pyth-solana-receiver-sdk's `get_price_no_older_than` has no future bound at all). `publish_time` is Pythnet/publisher wall time and `Clock::unix_timestamp` the cluster's stake-weighted estimate, which can lag it, so a just-pushed price can be ahead of the clock; with a venue's 1–15 s `max_oracle_age_secs` the two-sided check refused it as stale, so every venue fill failed while the clock lagged (fail closed, liveness). Reads at the 60 s limit (open, close, liquidate, adl, settle_position_pnl, update_funding, the LP flows, prop-amm) accept exactly what they did (±60 s); only reads with a shorter age now accept a publish time up to 60 s ahead. Mirrors: the keeper and prop-amm share the file; the frontend's `isFresh` (`MAX_FUTURE_PUBLISH_SECS`), the harness's `oracleFresh` (which now calls it) and the sim's predictor (`is_fresh` itself) follow. Tests: the venue case that pinned "16 s ahead is stale for a 15 s venue" now pins 61 s ahead refused and 60 s ahead filled; unit tests for ages 0, 1, 15, 60, 3_600 and the extremes. (2) **The pinned account's address.** New `pyth_push.rs` (a perps-core module; prop-amm `include!`s it): Pyth's two Receiver ids and their push oracles (`rec2HHDD…` / `pyt2F414…`, `rec5EKMG…` / `pythWSns…`, pyth-solana-receiver-sdk 2.0.0 `lib.rs`) and `push_feed_address` = PDA `[0u16 LE, feed_id]` under the push oracle (the push oracle's own seeds). `initialize_market`, `propose_oracle_feed` and `apply_oracle_feed` (`oracle::load_pinnable_update`) and prop-amm's `initialize_vault` refuse, with `InvalidOracleAccount` after the existing checks, an account that is not that shard-0 PDA whenever the oracle program is one of the two Receivers. Reason: the Receiver's `post_update` lets anyone create a Receiver-owned `PriceUpdateV2` with themselves as write authority, overwrite it with any verified update (older ones too) and close it; only the push oracle's accounts move forward only, which D47's pinning argument assumes. For any other owner (the localnet mock, the tests' fake program) nothing is derived and the account is not constrained, so localnet and the suites are unchanged; the admin who sets `oracle_program` stays trusted, as before. A unit test pins the derivation to Pyth's SOL/USD account and the D51 commodity accounts; integration tests refuse a Receiver-owned keypair account, the other pair's PDA and another feed id's PDA at `initialize_market`, at a rotation and at `initialize_vault`, and accept the PDA. Shard 0 only, as the scripts. (3) **Settled markets.** `apply_oracle_feed` is `MarketSettled` on a settled market (a settled market's feed is final; `propose_oracle_feed` already refused); a rotation left pending by `settle_market` stays visible and the admin can still cancel it. (4) **prop-amm owner snapshot.** `LpVault.oracle_program` (new, after `pyth_feed_id`; LEN 350 → 382 with 32 padding bytes kept; pre-launch) is `config.oracle_program` at `initialize_vault`, and open/close check the feed's owner against it instead of the live config, so `set_oracle_program` reaches new vaults only, as perps-core's per-market snapshot. A vault has no feed rotation: it stays bound to its account and owner for good (a move to the other Receiver pair is a new vault). `state/vault.rs` gains the account-size test (LEN = 8 + Borsh fields + 32, the field offsets 286 and 318). (5) **Not changed on chain, with reasons.** (a) *Adversarial selection across a push*: anyone can push a newer signed update into the pinned account, so a trader can open, close, deposit or withdraw at the stored price while knowing a newer one, and a liquidator can choose the update in (stored, now] that hurts most; the edge is the move between the stored price and Hermes, at most 60 s of it. The program cannot see Hermes, a shorter age only shrinks the window while making every read depend on someone pushing that often (else the market fails closed, as a closed session does), and it would change the price driver, the sim's cadence and many tests; the bound that matters is how closely the protocol's pusher tracks Hermes, now a stated launch requirement (§18 #21), with an on-chain bound (a shorter trade age or delayed orders) left to the owner. (b) *Timelock vs `settle_market`*: D47's rationale is corrected (admin trust remains; §18 #22); `settle_market` is unchanged. (6) `programs/perps-core/src/pyth_legacy.rs` (D48 (4)'s stopgap legacy reader) is deleted: after D47 and D50 nothing compiled it or its tests. Comments corrected: `send_events` and `install_native_stubs` (`tests/support/market_env.rs`) and `test-sdk/src/native_stubs.rs` now say perps-core's events do reach the log natively (`emit_event!`) and only `msg!` lines, return data and Anchor `emit!` events (mm-registry, orderbook) do not; `pool_math.rs` and §2.7/§16/F40 no longer call the keeper anchor 0.29; `venue-settlement.md` is marked historical (pre-D48). (7) **The sim port** (done with the toolchain stage, recorded here): `sim/` is back in the workspace (`Cargo.toml` members) on the D48 test stack (Anchor 1.2.2, program-test 4.4.1, `test-sdk` as `solana-sdk`, host Rust 1.98.1); test-sdk's `native_stubs::install` runs before the market is set up; a failed transaction's error name comes from the failing program's `#[error_code]` enum (`chain.rs`, by program and code, read from each program's source) instead of the log, because natively Anchor 1.2 logs no `msg!` lines; `quiet_events.rs` (which wrapped Solana 1.16's `sol_log_data` stub so native events reached the log) is deleted, since `emit_event!` does that for perps-core now and the sim reads no other program's events. Scenarios, invariants, model and predictions are unchanged; the six 2-day SBF runs and the 5-day stress run gave reports identical to the pre-migration baselines (26b4124 binaries) except compute units, wall time and error-example line numbers; native runs match SBF except mode, compute units and error examples ([sim/README.md](../sim/README.md)). (8) **Localnet setup** (pre-existing, it failed the smoke run): when setup waits for the LP deposit window (a start between about minute 50 and minute 2 of the hour, or in a daily break), the seed transaction cranks `update_funding` (permissionless) between the mock-pyth republish and `lp_deposit`, because the keeper starts after setup and `lp_deposit` refuses an accumulator more than the 60 s funding interval behind (`FundingStale`); decided at the chain clock with `session.ts`'s `fundingCurrent`, and a send the clock overtakes (`FundingStale` / `FundingNotDue`) is retried (at most three tries). The first smoke run that waited showed a second harness bug the wait exposes: the checker followed the keeper with a `getSignaturesForAddress(until: <last seen>)` cursor, and the cursor (setup's funding transfer, the keeper key's only signature before its first transaction) was purged by `--limit-ledger-size` before the keeper sent anything, after which every poll failed "Transaction … not found" and the report lost the keeper's transactions; it now reads the newest 1_000 and skips those already seen. (9) Other copies: a frontend test pins `backend/api/src/routes.rs`'s feed addresses to `pythFeeds.ts`'s pro shard-0 derivation (D51's one-table claim is narrowed accordingly). Feed ids: XAU/USD `765d2ba9…4bb2`, XAG/USD `f2fb02c3…871e` and OIL `925ca92f…b3e6` (`Commodities.USOILSPOT`) are each used with that meaning by several independent public integrations (found by a GitHub code search), but Pyth's own list (docs.pyth.network, Hermes) was not reachable from this session, so §18 #17's check before launch stays. Still open, not built by CI: `backend/indexer` (solana-sdk 1.17, anchor-client 0.29; it imports `solana_client` without declaring it, so it did not build before the migration either, and it does not know the `Oracle*` events) and `backend/api` (not compiled here). (10) Verified (every `ci.yml` step, locally, host Rust 1.98.1, Agave 4.4.1): the programs job (`cargo test` of the six programs natively, the `localnet` feature schedule tests, `cargo test -p sim`, `gen-idl.sh --check`); `build-sbf.sh` (no stack report, every `.so` sBPF v3; perps_core 1_064_856 bytes, +2_448 for the address check) and the SBF integration tests; the six 2-day SBF sim runs and the 5-day stress run, each report identical field for field to the D47 stage's except compute units, wall time and error examples, 0 violations; the keeper's 139 tests; `npm ci`, `gen-idl.sh`, the harness's `tsc` and self-test (7); `scripts/test-ts.sh` (85 passing on solana-test-validator 4.4.1); the frontend's `npm ci`, vitest (199) and `npm run build`; the session vectors. Live localnet run (`SKIP_BUILD=1 DURATION_SECS=1500 MIN_WEEKLY_REOPENS=1 REOPEN_TAIL_SECS=300 scripts/localnet/run.sh` on the localnet build of this tree, with the keeper binary): setup waited ~100 s through a daily break and its seed cranked funding; 2_966 s of chain time from minute 15 to minute 5 of the next hour, four daily-break edges (the stale reopen among them), one weekly close (`G·nn/(α·B)` 0.936) and one reopen (−14.5% gap printed 3 s before it); 6_380 transactions (5_016 landed, 1_364 expected refusals, 0 unexpected); 588 of 588 check cycles consistent, at most 6 s apart; 0 violations, PASS. The keeper, reading `PriceUpdateV2` with D50's reader, settled 179 losers, cranked funding 9 times (2 more lost the race to an LP's crank, logged INFO), liquidated two positions at stage 1 in the pre-close window and one at stage 2 one second after the gapped reopen (a $1,834.46 deficit, paid by insurance); its 11 ERROR lines are all the deliberate stale reopen ("Oracle price is stale …; program error 6009"). An earlier attempt (setup waited 11 minutes through the weekend) found the checker cursor bug in (8) and was stopped. Not verified: GitHub's runners, a successful `apply_oracle_feed` on a validator (timelock), any account or id on a public cluster | A two-sided check modelled on Solana-side timestamps refuses fresh Pythnet prices whenever the cluster clock lags, and the lag is not the price's fault; a fixed future bound keeps a sanity limit without tying it to a venue's age. Deriving the push-feed address on chain makes D47's "moves forward only" true by construction instead of by the admin's care, at no cost to localnet. Snapshotting prop-amm's owner gives it perps-core's rule that a config change does not move existing price sources. Adversarial selection cannot be closed by the program without either an off-chain price it cannot see or a liveness cost the owner should choose, so it is a launch requirement on the pusher and an owner item |
| D34 | Tests as built (§17): the bucket watcher runs after every transaction (not between instructions) and requires `vault − Σ buckets` unchanged, stricter than §11's `≥`, and also checks the §1.2 sums for accounts it has seen; `assert_conservation` checks the exact identity but not `U == 0` (venue tests assert `v.u == 0`); §17.2 #9 inside `W`, #13's thirty positions and #14's clock restart are unit tests; #11 and #13 are one integration test; §17.3's liquidate worst case is two cases; some tests set pool state with `patch`; the harness oracle starts at `WEDNESDAY`, `weekend_config()` / `cme_config()` gained `max_open_interest: 0`, and the mm-registry sweep test dropped its `SetComputeUnitLimit` to fit 1,232 bytes; the sim starts `total_minted` at `LP_SEED` | program-test cannot observe state inside a transaction; the rest keeps tests on real instructions where possible |

---

## 1. State

### 1.1 Changed accounts (append only; every LEN unchanged)

| Account | Appended field (offset) | Padding | LEN |
|---|---|---|---|
| `Market` | `funding_carry_secs: u32` after `funding_rate` (283..287): open seconds accrued but not yet charged (§8); then `settled_at: i64` (287..295): set by `settle_market` (D35) | 15 → 3 | 298 |
| `Position` | `opened_price: u64` after `bump` (136..144): set by `init_open`, never changed. `entry_price` is the PnL basis and `settle_position_pnl` resets it | 32 → 24 | 168 |
| `UserAccount` | `house_entry_notional: u64` after `bump` (69..77): Σ `opened_notional` of this user's open `ExecutionSource::OrderBook` positions (per-account cap; D21) | 16 → 8 | 85 |
| `ProtocolConfig`, `Venue`, `VenueApproval`, `Vault` | unchanged | | 201 / 267 / 191 / 145 |

- `open_position` must call `Position::init_open` (it sets `opened_price = entry_price`).
- New `Position::opened_notional(size) -> u64 = floor(size·opened_price, 1e6)`. Every path that adds or removes per-account house notional uses it, removing `opened_notional(q) − opened_notional(q − x)` for a slice `x` of `q` so the sum telescopes exactly. Decrements are `checked_sub` → `BucketUnderflow`, never saturating.
- `Market.max_open_interest` becomes editable (§4.12).
- The SDK offsets 104, 144, 152 and 247, and the Venue offsets 160..166, do not move.
- D47 later appended the oracle fields to `Market` (295..431: `pyth_feed_id`, `oracle_program`, `pending_price_feed`, `pending_feed_id`, `pending_oracle_at`), LEN 298 → 434, and took `pyth_feed_id` from prop-amm's `LpVault` padding; the offsets above still do not move.

### 1.2 `LpPool`: new, PDA `[b"lp_pool", market]`, `LEN = 601`

```rust
#[account] #[derive(Default)]
pub struct LpPool {                          // offset
    pub market: Pubkey,                      //   8
    pub adl_keeper: Pubkey,                  //  40  signs exposure-mode adl (§7)
    // Buckets (USDC). insurance = Market.insurance_fund (D1)
    pub free_collateral: u64,                //  72  Σ UserAccount.collateral_balance
    pub position_collateral: u64,            //  80  Σ open Position.collateral
    pub cash: u64,                           //  88  LP-owned
    pub treasury: u64,                       //  96  protocol revenue
    // Aggregates over OPEN positions (exact integer sums, never rounded)
    pub long_entry_sum: u128,                // 104  Σ size·entry_price      (scale 1e12)
    pub short_entry_sum: u128,               // 120
    pub long_funding_sum: i128,              // 136  Σ size·last_funding_payment
    pub short_funding_sum: i128,             // 152
    pub open_positions: u64,                 // 168  real count (≠ Market.total_positions nonce)
    // LP shares (one class)
    pub lp_shares: u64,                      // 176  includes LP_DEAD_SHARES once seeded
    pub pending_withdraw_shares: u64,        // 184
    pub max_pool_cash: u64,                  // 192  LP-capital cap (compared with NAV_mint, §9.1); 0 = unlimited; admin, immediate
    // Bad debt / ADL
    pub pending_bad_debt_long: u64,          // 200  deficit of bankrupt LONGS (recovered from profitable shorts)
    pub pending_bad_debt_short: u64,         // 208
    pub pending_since_long: i64,             // 216  debt-weighted booking time of pending_bad_debt_long (§2.6); 0 when it is 0 (D36)
    pub bad_debt_cum: u64,                   // 224  Σ deficits D, saturating
    pub bad_debt_recovered_cum: u64,         // 232  haircuts + sweeps + donations applied to pending
    pub bad_debt_written_off_cum: u64,       // 240
    // Loss window (D15)
    pub loss_window_start: i64,              // 248  0 = no window yet
    pub loss_window_nav: u64,                // 256  max(NAV_redeem,0) when the window opened
    pub lp_absorbed_window: u64,             // 264
    pub ins_paid_window: u64,                // 272
    // Drawdown (D12)
    pub nps_peak: u64,                       // 280  peak per-share mark NAV (scale 1e6)
    pub nps_peak_at: i64,                    // 288
    // Vesting of swept / donated value (D18)
    pub vest_amount: u64,                    // 296
    pub vest_start: i64,                     // 304
    pub vest_end: i64,                       // 312  0 = nothing vesting
    pub donated_cum: u64,                    // 320  stat
    pub pending_params_at: i64,              // 328  0 = no proposal
    pub params: PoolParams,                  // 336  104 bytes
    pub pending_params: PoolParams,          // 440  104 bytes
    pub bump: u8,                            // 544
    pub pending_since_short: i64,            // 545  the same for pending_bad_debt_short (D36)
}                                            // 553..601 padding (48): reserved for a junior/first-loss tranche (§9.6)
impl LpPool {
    pub const SEED: &'static [u8] = b"lp_pool";
    pub const PADDING: usize = 56;           // was 64; profit_close_quiet_secs took 4 bytes in each PoolParams copy (D23)
    pub const LEN: usize = 8 + 32*2 + 8*4 + 16*4 + 8*21 + PoolParams::LEN*2 + 1 + PADDING; // 601, unchanged
    pub fn adl_required(&self) -> bool { self.pending_bad_debt_long > 0 || self.pending_bad_debt_short > 0 }
}
```

LpPool is a new account, so its layout is free; the append-only rule (§1.1) applies to existing accounts.

`PoolParams` (Borsh, `Clone, Copy, Debug, Default, PartialEq`, `LEN = 104`). Offsets are relative to the start of the struct:

```rust
pub struct PoolParams {
    pub skew_scale: u64,                     //  0  contracts (6 dp); 0 = no price impact
    pub insurance_target: u64,               //  8  USDC
    pub insurance_claim_cap_per_window: u64, // 16  USDC per loss window; 0 = unlimited
    pub liq_min_remaining_notional: u64,     // 24  USDC; smaller remainders are closed in full
    pub k_net_bps: u32,                      // 32  net cap, × capNAV
    pub hard_net_bps: u32,                   // 36  exposure-ADL line, × B; 0 = off
    pub per_account_cap_bps: u32,            // 40  × net cap; 0 = off
    pub weekend_lp_loss_bps: u32,            // 44  α
    pub weekend_gap_bps: u32,                // 48  G override; 0 = derive (§3.1)
    pub weekend_cap_window_secs: u32,        // 52  W; 0 = weekend bound (always-on cap, withdraw check, D43), force-reduce and LP pre-close gate off
    pub withdrawal_delay_secs: u32,          // 56
    pub withdrawal_claim_window_secs: u32,   // 60  open seconds after unlock; 0 = never lapses
    pub lp_reopen_quiet_secs: u32,           // 64  no LP deposit/withdraw this long after each session open
    pub adl_net_target_bps: u32,             // 68  drawdown-mode ADL reduces net to this × B
    pub adl_exposure_score_floor_bps: u32,   // 72  minimum score for exposure-mode ADL
    pub dd_freeze_bps: u16,                  // 76  drawdown that freezes net-raising house opens; 0 = off
    pub dd_adl_bps: u16,                     // 78  drawdown that enables exposure ADL; 0 = off
    pub dd_withdraw_block_bps: u16,          // 80  drawdown that blocks lp_withdraw; 0 = off
    pub redeem_loss_haircut_bps: u16,        // 82  h: share of net trader losses NAV_redeem counts
    pub open_fee_bps: u16,                   // 84
    pub max_impact_bps: u16,                 // 86  clamp on |skew premium|
    pub liq_penalty_bps: u16,                // 88  of closed mark notional (stage 1)
    pub liq_keeper_share_bps: u16,           // 90  of the penalty
    pub backstop_threshold_bps: u16,         // 92  backstop line = MMR_in_force × this / 10_000
    pub min_liq_slice_bps: u16,              // 94  minimum stage-1 slice, of size
    pub treasury_fee_bps: u16,               // 96  of net fees
    pub lp_loss_cap_window_bps: u16,         // 98  × loss_window_nav; the LP's share of one loss window
    pub profit_close_quiet_secs: u32,        // 100 no profitable close_position this long after each session open (D23)
}
```

`PoolParams::validate(&self, m: &Market) -> bool`. Every condition must hold, otherwise `InvalidPoolParams`:
- `k_net_bps <= 100_000`.
- `hard_net_bps == 0 || hard_net_bps >= k_net_bps`. `adl_net_target_bps <= k_net_bps`.
- `per_account_cap_bps <= 10_000`. Each of `weekend_lp_loss_bps`, `weekend_gap_bps` and `redeem_loss_haircut_bps` is `<= 10_000`.
- With the weekend bound on (`weekend_cap_window_secs > 0`): `k_net_bps·G ≤ weekend_lp_loss_bps·10_000`, with `G = g_bps(m)` (§3.1) at validation time (D43). Production gives `4_000·2_000 = 0.8 × 1_000·10_000`: 20% under the edge, the LP exit buffer at the cap (D45); `k_net = α/G` itself still validates. `apply_pool_params` re-validates against the market as it is then (§5), but does not look at the net already open: a change that tightens the bound (W turned on, α lowered, `weekend_gap_bps` raised) over existing net leaves it above `α·B/G` until it closes (§3.1 item 4, D44). A later `update_market_config` that raises `G` is covered for new net by the always-on weekend cap (§3).
- `weekend_cap_window_secs <= 5*86_400`, `withdrawal_delay_secs <= 30*86_400`, `lp_reopen_quiet_secs <= 86_400` and `profit_close_quiet_secs <= 3_600`.
- `W + 5·Q + 86_400 <= 414_000` (`W = weekend_cap_window_secs`, `Q = lp_reopen_quiet_secs`; 414_000 is a CME week's open seconds, and `Q` gates each of its five session opens), so every week has at least one open day in which `lp_withdraw` is not gated (D38).
- `withdrawal_claim_window_secs == 0 || withdrawal_claim_window_secs >= W + 5·Q + 86_400`, so a matured request always has at least one open day in which `lp_withdraw` is not gated (§9.4). `lp_withdraw` holds a stored window to at least this bound under the gates in force when it runs (D38).
- Drawdown lines: each `<= 10_000`; if `dd_freeze_bps` and `dd_adl_bps` are both non-zero, `dd_freeze_bps <= dd_adl_bps`.
- `open_fee_bps <= 1_000`, `max_impact_bps <= 1_000` and `treasury_fee_bps <= 10_000`.
- `1 <= backstop_threshold_bps <= 10_000`, `1 <= min_liq_slice_bps <= 10_000`, `liq_keeper_share_bps <= 10_000` and `lp_loss_cap_window_bps <= 10_000`.
- `liq_penalty_bps as u64 * 10_000 < m.maintenance_margin_ratio as u64 * backstop_threshold_bps as u64`. With penalty 0 this always holds. This keeps the stage-1 penalty below the equity of any stage-1 position (§6.3).
- If stage 1 is enabled (`backstop_threshold_bps < 10_000`): `liq_penalty_bps as u64 * (10_000 − liq_keeper_share_bps) as u64 >= (max_impact_bps as u64 + m.taker_fee as u64) * 10_000`. A self-liquidating owner recovers the keeper share of the penalty, so the net penalty must cover the impact and close fee it would dodge (§2.4).

Constants (defined in `pool_math.rs`, §2.7, and re-exported from `state/pool.rs`; D29):
- `MAX_OPEN_INTEREST: u64 = 1_000_000_000_000_000` (1e9 contracts). It bounds every i128 product in §2.
- `MAX_FUNDING_CATCHUP_PERIODS: u64 = 168`.
- `ADL_WRITE_OFF_SECS: i64 = 86_400`, in **open** seconds (§7.4).
- `ADL_FLOOR_START_BPS: u64 = 1_000_000` and `ADL_FLOOR_DECAY_SECS: i64 = 21_600` (open seconds; §7.2).
- `LOSS_WINDOW_SECS: i64 = 86_400` (wall clock; §2.6).
- `DD_WINDOW_SECS: i64 = 7*86_400` (wall clock; §2.2).
- `MIN_POOL_TIMELOCK_SECS: i64 = 86_400` and `PARAMS_TIMELOCK_MARGIN_SECS: i64 = 5*86_400` (§5).
- `LP_DEAD_SHARES: u64 = 1_000_000` (§9.1).
- `LP_CLASS_COMMON: u8 = 0`.

### 1.3 `LpPosition`: new, PDA `[b"lp_position", lp_pool, owner, &[class]]`, `LEN = 166`

```rust
#[account] #[derive(Default)]
pub struct LpPosition {
    pub owner: Pubkey,             //   8
    pub pool: Pubkey,              //  40
    pub shares: u64,               //  72  includes pending_shares
    pub pending_shares: u64,       //  80  locked by a withdrawal request
    pub requested_value: u64,      //  88  floor(pending·max(NAV_redeem,0)/lp_shares) at request time
    pub requested_at: i64,         //  96  0 = none
    pub unlock_at: i64,            // 104  requested_at + the delay in force at request time
    pub deposited_cum: u64,        // 112  stat
    pub withdrawn_cum: u64,        // 120  stat
    pub claim_window_secs: u32,    // 128  the claim window in force at request time
    pub class: u8,                 // 132  0 only (§9.6)
    pub bump: u8,                  // 133
}                                  // 134..166 padding (32)
impl LpPosition { pub const SEED: &'static [u8] = b"lp_position"; pub const LEN: usize = 8 + 32*2 + 8*7 + 4 + 1 + 1 + 32; } // 166
```

Shares cannot be transferred. An SPL LP mint can come later.

---

## 2. Pool math (`state/pool.rs`; every function is [NI])

### 2.1 Aggregate maintenance

`pool_add(pool, side, size, entry, funding)` adds `size·entry` (u128) to `{side}_entry_sum` and `size·funding` (i128) to `{side}_funding_sum`, then does `open_positions += 1`. `pool_remove` is the exact inverse. All of it uses checked arithmetic, with `MathOverflow` on failure.

Any change to an open position's `(size, entry_price, last_funding_payment)` follows one rule. It calls `pool_remove(old)` and then `pool_add(new)` with an open-count adjustment of 0 (a helper `pool_replace`). The sums therefore always equal Σ over open positions exactly.

### 2.2 Valuation, O(1)

Inputs: `mark` (the oracle, or `settlement_price` once settled), `now`, `L = m.long_open_interest`, `S = m.short_open_interest`, `F = m.cumulative_funding`. Everything is i128; each u128 is converted with `try_from`, and failure gives `MathOverflow`.

```
pnl_num  = (mark·L − long_entry_sum) − (mark·S − short_entry_sum)
fund_num = −(F·L − long_funding_sum) + (F·S − short_funding_sum)
U        = (pnl_num + fund_num).div_euclid(1_000_000)          // traders' net unrealized PnL incl. funding
slack    = 2·open_positions + 1
unvested = vest_end > now ? floor(vest_amount·(vest_end − now) / (vest_end − vest_start)) : 0   // §10.3
X        = skew_scale == 0 || settled ? 0 : min(ceil(mark·net²/(2·skew_scale·1e6)), ceil(net_notional·max_impact_bps/10_000))   // refundable skew premium (D39)
U'       = U + X                                               // what traders are owed on unwinding
NAV_mint   = cash − U + slack                                  // upper bound: depositors pay it (counts unvested, ignores X)
NAV_redeem = cash − unvested − max(U',0) + floor(h·max(−U',0)/10_000) − slack   // lower bound: withdrawers get it; h = redeem_loss_haircut_bps
nav_mark = cash − unvested − U'                                // drawdown series
nps      = lp_shares == 0 ? 0 : floor(max(nav_mark,0)·1e6 / lp_shares)
dd_bps   = nps_peak == 0 || nps ≥ nps_peak ? 0 : (nps_peak − nps)·10_000 / nps_peak
net      = L − S                                               // contracts; the pool holds −net
net_notional = ceil(|net|·mark, 1e6)                           // u128, rounded against the opener
B        = max(NAV_redeem, 0)                                  // capital still in the pool, pending shares included
capNAV   = lp_shares == 0 ? 0 : floor(B·(lp_shares − pending_withdraw_shares) / lp_shares)
```

Which capital each check uses:
- `capNAV` (pending withdrawals excluded) gates **new risk**: house opens (§4.4). The pool stops adding risk on capital that is leaving.
- `B` (pending included) is used for everything that acts on existing traders or on capital still present: closes that raise `|net|` (§4.5), the ADL triggers and targets (§7.2) and the `lp_withdraw` post-check (§9.4). A free, cancellable withdrawal request therefore cannot trigger ADL of traders or block their closes (red-team F7).

`pool_view(pool, market, mark, now, h) -> PoolView { u, net, net_notional, nav_mint, nav_redeem, b, cap_nav, dd_bps, nps }` returns about 128 bytes by value. `nps` saturates at `u64::MAX` (D30).

**Peak tracking.** [NI] `touch_peak(pool, v, now)` runs in every instruction that holds LpPool mut and computes a `pool_view`: if `nps ≥ nps_peak` or `now ≥ nps_peak_at + DD_WINDOW_SECS`, set `nps_peak = nps` and `nps_peak_at = now`. The rolling reset lets a drawdown heal after a week. Every drawdown check (the open freeze, `dTrig`, the `lp_withdraw` block) reads [NI] `drawdown_now(pool, v)`, the drawdown against the peak as `touch_peak` left it, so a reset takes effect in the instruction that triggers it (D28). Per-share NAV makes deposits and withdrawals neutral (both round in the pool's favour, so `nps` can only tick up from them). Vesting is excluded from `nav_mark`, so a sweep or donation does not jump `nps`.

Bounds: `L, S ≤ MAX_OPEN_INTEREST` (1e15), `mark < 2^64`, and `|F| < 2^63`. Every product is therefore below about 1.8e34, well inside i128.

### 2.3 Proof sketch: matched pairs cancel exactly

1. **Aggregates.** `venue_fill` adds `(s, p, F₀)` to the long side and `(s, p, F₀)` to the short side. The contribution to `pnl_num` is `(mark·s − s·p) − (mark·s − s·p) = 0`. The contribution to `fund_num` is `−(F·s − s·F₀) + (F·s − s·F₀) = 0`. Both are exact integers, so U does not move.
2. **Engine.**
   - `unrealized_pnl` and `accrued_funding` are odd in side, because i128 `/` truncates toward zero and `trunc(−x) = −trunc(x)`. The test `pnl_and_funding_are_odd_in_side` pins this.
   - Two legs closed at the same price and index therefore move cash by `−(pnl_L+f_L) − (pnl_S+f_S) = 0`.
   - Once one leg closes, the survivor is ordinary net exposure, which the aggregate values like any other position. NAV does not change at that moment:
     - cash moves by `−X`;
     - U loses the winner's `+X`, so U is now `−X`, the survivor's term;
     - so `cash − U` is unchanged.
3. **No drift.** By §2.1 the aggregates equal Σᵢ of each position's exact numerator at all times.
4. **Truncation bound.**
   - Let `U_eng = Σᵢ [trunc(pnl_numᵢ/1e6) + trunc(fund_numᵢ/1e6)]`, which is what closing everything at `mark` would actually realize.
   - Each trunc is within 1 of its rational value, and the aggregate floor is within 1 too, so `|U_eng − U| ≤ 2·n_unmatched + 1 ≤ slack`.
   - The truncation errors of matched legs cancel pairwise, so `open_positions` is a safe over-count.
   - Hence `NAV_mint ≥ cash − U_eng ≥ NAV_redeem`; the second inequality uses `h ≤ 1` and `slack`.
   - Minting at `NAV_mint` and redeeming at `NAV_redeem` cannot move value from the remaining LPs to the LP entering or leaving, **as long as no open position is bankrupt**.
   - **Exception: unliquidated bankrupt positions.** A bankrupt leg's `−(C+D)` and its matched partner's `+(C+D)` cancel in `U`, so neither NAV sees the deficit `D` until `liquidate` books it, and `h` does not help because it acts only on net `U < 0`. The mitigations are operational: LP deposits and withdrawals are closed for `lp_reopen_quiet_secs` after each session open (§9), which is when gap deficits appear, so the keeper liquidates first; and `lp_withdraw` pays `min(NAV_now, requested_value)`, so a deficit booked during the delay still reaches the leaver. A deficit created and not yet liquidated mid-session can still be mispriced for the seconds before the keeper acts.
5. **Funding carry.**
   - When `update_funding` raises F by Δ, NAV changes by `(L − S)·Δ/1e6`. That is the pool's carry for holding `−net`.
   - It is realized into `cash` as each position settles its funding at close, liquidation, ADL or `settle_position_pnl` (cash takes `−funding`).
   - So the pool's funding carry goes to LP cash with no extra transfer.

### 2.4 Skew price (house opens and all closes)

```rust
/// delta: +q for a buy (open long / close short), −q for a sell. Returns the execution price.
fn skew_price(oracle: u64, skew_before: i128 /* L−S */, delta: i128, p: &PoolParams) -> Result<u64>
  if p.skew_scale == 0 { return oracle }
  num = oracle·(2·skew_before + delta)           // = oracle·(skew_before + skew_after)
  den = 2·skew_scale
  prem = delta > 0 ? ceil_s(num, den) : num.div_euclid(den)   // buys round up, sells round down
  cap  = floor(oracle·max_impact_bps / 10_000)
  price = oracle + clamp(prem, −cap, cap); require!(price > 0, InvalidOraclePrice)
```

This is SIP-279's average premium. Trades that reduce skew get a better price than the oracle. Because closes also pay it, an open followed by a close in the same skew state nets to zero. The premium an open pays therefore shows in `U` as a trader loss that the unwinding pays back, so the LP-side valuations count it as owed (`X` in §2.2, D39).

**Liquidations and ADL execute at the oracle with no impact.** Deficit-mode ADL still pays the close fee (D22). An owner who liquidates their own position pays the penalty but gets the keeper share back, so the net cost is `liq_penalty_bps·(1 − liq_keeper_share_bps/10_000)`. `PoolParams::validate` requires that to be at least `max_impact_bps + taker_fee` whenever stage 1 is enabled, so self-liquidating never beats a close (defaults: 200 × 0.8 = 160 ≥ 100 + 50). Stage 2 gives the owner nothing, so it needs no rule. Settled markets have no impact.

### 2.5 Fees

`distribute_fee(m, pool, x)` is [NI] and splits a net fee `x` (after referral):
- `t = floor(x·treasury_fee_bps/10_000)` goes to `treasury`;
- `credit_insurance(m, pool, x − t)` takes the rest.

`credit_insurance(m, pool, y)`:
- `room = insurance_target.saturating_sub(insurance)`;
- `insurance += min(y, room)`;
- `cash += y − min(y, room)`.
- This puts insurance first until the target, then the overflow goes to LPs. The stage-2 residual equity uses the same path. Trading is impossible while `lp_shares == 0` (D11), so no `lp_shares == 0` branch is needed.

Fee rounding:
- The open fee is `ceil(notional_exec·open_fee_bps, 10_000)`.
- The close fee becomes `ceil(opened_notional·taker_fee, 10_000)` (D21); it was a floor of entry notional before. A deficit-mode ADL slice `x` pays `ceil(opened_notional(x)·taker_fee, 10_000)` (`slice_close_fee`, D22). `opened_notional == entry_notional` until `settle_position_pnl` runs, and every existing test value divides exactly, so nothing moves.

### 2.6 Deficit waterfall: `absorb_deficit(m, pool, position, D, bankrupt_side, nav_before, now) -> (ins_paid, lp_absorbed, pending)` [NI]

Preconditions: the position's collateral `C` has already been credited to `cash` (in full, since no fee is collected at negative equity), and `D = −equity > 0`.
0. **Loss window (D15).** [NI] `roll_loss_window(pool, nav_before, now)`: if `loss_window_start == 0` or `now ≥ loss_window_start + LOSS_WINDOW_SECS`, set `loss_window_start = now`, `loss_window_nav = max(nav_before, 0)`, `lp_absorbed_window = 0` and `ins_paid_window = 0`. Stage 2 calls it before its keeper top-up, so the top-up counts against the same window. `nav_before` is `NAV_redeem` at the start of the instruction.
1. Margin is exhausted by definition. The stage-1 penalty or buffer does not exist at negative equity.
2. Insurance:
   - `cap_ins = insurance_claim_cap_per_window == 0 ? u64::MAX : cap.saturating_sub(ins_paid_window)` (`ins_paid_window` already includes any stage-2 keeper top-up);
   - `ins_paid = min(D, insurance, cap_ins)`;
   - then `insurance −= ins_paid`, `cash += ins_paid` and `ins_paid_window += ins_paid`.
3. LP:
   - `lp_cap = floor(loss_window_nav·lp_loss_cap_window_bps/10_000).saturating_sub(lp_absorbed_window)`;
   - `lp_absorbed = min(D − ins_paid, lp_cap)` and `lp_absorbed_window += lp_absorbed`.
   - No bucket moves here: cash simply never received this amount.
4. Pending:
   - `pending = D − ins_paid − lp_absorbed` is added to `pending_bad_debt_{bankrupt_side}`;
   - that side's clock `pending_since_{bankrupt_side}` (D36) becomes the debt-weighted time: `now` if that side's pending was 0 before, otherwise `floor((since·old + now·pending)/(old + pending))` (i128), where `old` is that side's pending before. A large new deficit moves the clock almost to `now`; dust barely moves it, so dust cannot be used to keep the write-off clock from running. The other side's clock never moves;
   - `bad_debt_cum += D` (saturating).
5. Emit `BadDebt` for `position` (D29).

While `adl_required()` is true:
- `lp_deposit` and `lp_withdraw` are blocked; `donate_to_pool` is not (§10.4);
- house opens that raise `|net|` are blocked;
- deficit-mode `adl` is enabled for anyone (§7);
- free-collateral withdrawals are **never** gated.

Profitable closes are not blocked by pending debt, only by the profit-close quiet time after each session open (D23). Every pool-funded profit payout pays the opposite side's pending first (D13, §2.8), so a winner who closes ahead of ADL pays what ADL would have taken; the quiet time gives the keeper the seconds after a gap to book the deficit before the winners can close. The flag clears only through those haircuts, ADL, the insurance sweep (§10.1), a donation (§10.4) or the write-off (§7.4). Nothing is only logged any more.

**What the LP loss cap means under D2.** `NAV_redeem` carries the whole `D − ins_paid` from the moment it is booked. `lp_loss_cap_window_bps` bounds how much of it the pool stops *pursuing*; the rest is pursued through the haircuts and ADL for `ADL_WRITE_OFF_SECS` of open time, and only what is still pending then is written off onto LPs. The LP's realised loss from one window is therefore `lp_absorbed_window + written_off`, not `lp_absorbed_window` alone.

### 2.7 `pool_math.rs`: pure integer math shared with the keeper and the sim

`programs/perps-core/src/pool_math.rs` has no Anchor or Solana imports, like `session.rs` and `price.rs`. It holds `pool_view_raw` (over plain integers), `skew_price_raw`, `adl_score`, `adl_floor_now`, the drawdown, trigger and cap predicates, and the constants above. `state/pool.rs` wraps these for the Anchor types. `backend/keeper` (outside the workspace; anchor 0.29 until D48, the solana-v3 client crates since) and `sim/` `#[path]`-include `pool_math.rs` and `session.rs`.

### 2.8 Paying profit: `quote_profit(pool, v, side, P, allow_short) -> Result<Payout>` and `apply_payout(pool, side, &Payout)` [NI]

Every path that pays a positive realized amount `P` out of `cash` (close, both liquidation stages, settled close, ADL) uses this one pair. `quote_profit` is pure, so stage 1 can quote a slice, find that it must close in full instead, and quote again with nothing to undo. `apply_payout` makes the mutations below. `pay_profit(..)` in the text means quote then apply. The steps:
1. **Solvency haircut.** If `U > 0 && U > cash` (the pool is insolvent at mark), `P1 = floor(P·cash/U)`; otherwise `P1 = P`. Every winner paid while the pool is insolvent at mark takes the same pro-rata rule. There is no parameter: the U/cash lines are gone (D12).
2. **Bad-debt haircut (D13).** A **short** winner repays the deficit of bankrupt **longs**, so `opp = side == Short ? pending_bad_debt_long : pending_bad_debt_short`. `hc = min(opp, P1)`; `opp −= hc`; `bad_debt_recovered_cum += hc`; if that side's pending is now 0, its `pending_since_*` is 0 (D36). `P2 = P1 − hc`. The haircut stays in `cash`, which is how the recovery reaches NAV.
3. **Cash.** If `P2 > cash`:
   - `allow_short == false` (close, settled close, ADL): fail with `PoolCashInsufficient`. The keeper settles losers (§6.6) and retries.
   - `allow_short == true` (liquidations): pay `P3 = cash` and record `cash_hc = P2 − cash`. **A liquidation never reverts on cash** (red-team F21).
4. `cash −= P3` (`P3 = P2` when not short). The quote is `{ paid: P3, solvency_hc: P − P1, bad_debt_hc: hc, cash_hc }`; `apply_payout` does the pending reduction of step 2 and this debit. All three haircuts are emitted in the caller's event.

## 3. Caps (all USDC, at mark; capital is `capNAV` or `B`, §2.2)

`W = session::in_pre_close_window(schedule, now, weekend_cap_window_secs)`. `G_bps` is defined in §3.1. `dd` is `drawdown_now(pool, v)` (§2.2, D28).

| Cap | Formula (integers) | Enforced in | When |
|---|---|---|---|
| Gross, contracts | `side_OI + size ≤ max_open_interest` (existing) | `open_position`, `venue_fill` | always |
| Pool has LPs | `lp_shares > 0`, else `PoolHasNoLps` (D11) | `open_position`, `venue_fill` | always |
| Net (open) | `net_notional_after·10_000 ≤ k_net_bps·capNAV` | `open_position` | `\|net_after\| > \|net_before\|` |
| Net (close) | `net_notional_after·10_000 ≤ k_net_bps·B` | `close_position`, `adl` mode 0 (D25) | not settled and `\|net_after\| > \|net_before\|` (D14) |
| Weekend net | `net_notional_after·G_bps ≤ weekend_lp_loss_bps·(X − shortfall)`, with `X = capNAV` for opens and `B` for closes, and `shortfall = premium_shortfall(net, net_after, oracle, exec, size)` the trade's skew-premium shortfall (D44; the mark is `exec` for an ADL slice) | `open_position`, `close_position`, `adl` mode 0 | `weekend_cap_window_secs > 0` (at any time, not only in `W`; D43), not settled for closes and ADL, and `\|net\|` rises |
| Profit-close quiet | `P > 0` rejected (`ProfitCloseQuiet`) while `seconds_since_open < profit_close_quiet_secs`; once settled, while `now − settled_at < profit_close_quiet_secs` (D35) | `close_position`, mode-0 `adl`, `close_settled_position` (settled) | always (D23, D35) |
| Per account | `(user.house_entry_notional + opened_notional)·10_000·10_000 ≤ per_account_cap_bps·k_net_bps·capNAV` | `open_position` | `per_account_cap_bps > 0` (every house open) |
| Bad-debt / drawdown freeze | reject if `adl_required()` (`PoolAdlPending`) or `dd_freeze_bps > 0 && dd ≥ dd_freeze_bps` (`PoolDrawdownFreeze`) | `open_position` | `\|net\|` rises |
| Exposure-ADL triggers | `nTrig`: `hard_net>0 && net_notional·10_000 > hard_net·B`; `dTrig`: `dd_adl>0 && dd ≥ dd_adl`; `wTrig`: `W && net_notional·G_bps > α·B` | `adl` mode 1 (§7.2) | — |
| Hard net (LP exit) | `net_notional·10_000 ≤ hard_net·B'` after the withdrawal | `lp_withdraw` | `hard_net > 0`, not settled |
| Weekend bound (LP exit) | `net_notional·G_bps ≤ α·B'` after the withdrawal (`PoolCapWouldBreach`) | `lp_withdraw` | `weekend_cap_window_secs > 0`, not settled (D43) |
| Solvency haircut | `U > 0 && U > cash` ⇒ `P1 = floor(P·cash/U)` (§2.8) | every profit payout | — |
| LP withdraw block | `dd_withdraw_block_bps > 0 && dd ≥ block` rejects | `lp_withdraw` | not settled |
| Pool size | `max(NAV_mint,0) + amount ≤ max_pool_cash` (if ≠ 0) | `lp_deposit` | — |

### 3.1 Notes on the caps

- **Closes.** A close that lowers `|net|`, or leaves it unchanged, is never blocked by a cap; it pays skew impact. A close that **raises** `|net|` (its counterparty leg is still open) must fit the net cap and, while the weekend bound is on, the weekend cap, both against `B`. Without this check a trader self-crosses a venue pair from two wallets and closes one leg, leaving the pool any amount of naked net for the cost of a clamped skew premium (red-team F1). Liquidations, exposure-mode ADL and settled-market closes are exempt. Deficit-mode ADL that raises `|net|` is not (D25): it leaves the closed position's counterparty open against the pool exactly as a close does.
  - *Liveness cost.* When the pool is at its cap, a trader whose close would raise `|net|` cannot close through `close_position`. They exit by trading the opposite side on a venue (`venue_fill` leaves net unchanged) and close both legs later, or wait for headroom. The frontend must explain the error. The bad-debt freeze and the drawdown freeze do **not** apply to closes or to deficit ADL.
- Venue fills never change `net`, so they are checked only against the gross cap and the has-LPs gate.
- `G_bps = weekend_gap_bps != 0 ? weekend_gap_bps : max(m.initial_margin_ratio, m.weekend_initial_margin_ratio)`. For OIL that is `max(1000, 2000) = 2000`. The market's weekend IM is the gap a position at maximum weekend leverage can absorb before bankruptcy; using it here puts the pool's weekend net under the same stress assumption. With `G = 0` (IM must be > 0, so this cannot happen) the cap is skipped.
- **What the weekend bound actually guarantees (D43).** With `weekend_cap_window_secs > 0` and production params:
  - *By construction.* Every operation that raises `|net|` and is not exempt leaves `G·net_notional_after ≤ α·(X − shortfall)` with `X` the capital before it: a house open against `capNAV ≤ B`, a close or a mode-0 ADL slice against `B`. `shortfall` is what the trade itself takes out of `B` through the skew premium (D44): the rise in the unwind premium `NAV_redeem` owes beyond the premium paid at `exec`. It is 0 up to rounding while the skew stays in the quadratic branch, at most `o·skew_scale·max_impact²/2` (≈ $9.4k at $75 with production skew) for a trade crossing into the clamp, and the whole `Δx` for an ADL slice at the mark. So the capital after the trade still covers the bound, except for the payout term of item 2. `validate` keeps `k_net·G ≤ α·10_000`, so at the `G` the params were validated at, the net cap alone already implies it (production's 0.4× sits 20% under the 0.5× edge, D45); the explicit weekend cap is what still binds after `G` rises. An `lp_withdraw` leaves `G·net_notional ≤ α·B'`, and inside `W` LP exits are shut. So for net put on by capped operations, at the same price and `G`, `G·net_at_close ≤ α·B`: the pool's gap loss on its own net, for a gap of at most `G`, is at most `α` of its capital, with no reliance on the Friday force-reduce.
  - *What can still carry `G·net/B` over `α` (all checked against the code; the sim names which applied, §17.4).*
    1. **Price drift up.** `net_notional = |net|·mark` grows with the price whatever the sign of `net`, and `B` moves by at most the same amount, so the ratio rises with every price rise once `net_notional < B` (it falls with every fall). With `net > 0` (the pool short) the pool also loses, and `B` falls: those net-side positions are winning, and inside `W` `wTrig` cuts them back to `α·B/G`. With `net < 0` (the pool long) `B` rises too, by at most `|net|·Δp`, so after a rise of `x` the ratio is at most `(1 + x)` times the bound; those positions are losing and `wTrig` cannot remove them. Funding never lowers `B`: the rate has the sign of `net` (`update_funding`), so the pool always receives the carry.
    2. **`B` falling after the fact.** Deficits beyond insurance (bankrupt positions, matched-pair bankruptcies above all: §2.3 point 4, §2.6) are booked when liquidated and lower `B` by what insurance did not pay. And `B` is `NAV_redeem`, which counts only `h` of traders' net losses: a profit paid while `U' < 0` lowers `B` by `(1 − h)` of the payout (the growth of the uncounted loss) though `nav_mark` does not move, including in the very close (or mode-0 ADL slice) whose cap was checked on `B` before it. The cap does not subtract this term: it depends on the haircuts the payout gets, and it only arises when traders as a whole are losing.
    3. **Net the caps do not see.** Liquidations (stage 1 and 2) are exempt: liquidating one leg of a venue pair leaves its counterparty against the pool. Net from venue pairs was never opened under the cap.
    4. **The bound tightened after positions were opened.** `update_market_config` raising the IM or weekend IM raises a derived `G`; `apply_pool_params` can turn `W` on, lower α or raise `weekend_gap_bps` (it checks `validate`, not the open net, D44). The always-on cap stops new net at once, but net already open stays above the new `α·B/G` until it closes or `wTrig` cuts its winners inside `W`. An admin tightening the bound should propose it while the net fits the new line.
    5. **Rounding.** `net_notional` is a ceiling, each new position adds 2 units of slack to `NAV_redeem`, `U` is floored and the premium paid is floored.
  - Settled markets have no weekend. A schedule that never closes (`SCHEDULE_ALWAYS_OPEN`) has no `W`, but `W > 0` still turns on the always-on cap and the withdraw check there; set `weekend_cap_window_secs = 0` for such a market if they are not wanted.
  - The force-reduce `wTrig` (D20) stays as the backstop for items 1 and 4 inside `W`; it can only cut **profitable** net-side positions. Matched-pair deficits beyond insurance come on top of `α` (§2.6). The sim checks every weekly close (§17.4).
  - *Exit-liveness cost (D44, D45).* The `lp_withdraw` weekend check keeps `B' ≥ G·net_notional/α`, and with production params the net cap (0.4×) sits 20% under that line (0.5×): the **exit buffer** `1 − k_net·G/α` (§15.1). With traders at the open cap, `net_notional = k_net·capNAV`, so an exit leaves room while `B' ≥ (k_net·G/α)·capNAV = 0.8·capNAV`: at a steady price and `B`, LPs can withdraw `B − 0.8·capNAV`, that is 20% of `B` with nothing pending (measured: $400,000.000092 of `B` = $1,999,999.999992 in `production_exit_buffer_at_the_net_cap`, §17.2; one share more is refused `PoolCapWouldBreach`). With a fraction `p` of shares pending when the net was put on, `capNAV = (1 − p)·B` and the room is `(0.2 + 0.8p)·B`. `capNAV` protects pending capital only **against traders**: new house opens cannot use it. It does not reserve it for the LP who requested it. The room is pool-wide and **first come, first served** among all matured claims, made before or after the fill: `lp_withdraw` checks `B' = nav_redeem − value` and does not know about other pending requests, so a later requester whose claim lands first can take the room a pre-fill request was counted for, and that earlier claim is then refused `PoolCapWouldBreach` (for example `B` = $2.5M with $500k requested before the fill: the room is $900k; a $450k claim requested after the fill lands first and leaves $450k, and the $500k pre-fill claim is refused; `pool.test.ts`). Reserving pending value by request time would close this at a liveness cost and is not done (§18 #15). What changes the room, all checked against the code:
    - *A price rise eats it, whichever the side:* `net_notional = |net|·mark` rises with the price. Starting from `U = 0` at the cap, a rise of `x` leaves `(0.2 − 1.2x)·B` of room when the pool is short (traders' gains lower `B` in full), gone at +16.7%, and `(0.2 − 0.6x)·B` when it is long (only `h` = 50% of traders' losses count in `B`), gone at +33%. A fall widens it (`(0.2 + x)·B` short, `(0.2 + 0.4x)·B` long) only while `|net|` is constant: a fall is a move against the traders' longs, and when those longs are the minority side (the pool long) their liquidations raise `|net|` (next item).
    - *A fall in `B` eats it one for one:* deficits beyond insurance, the `(1 − h)` of a payout made while traders are net losing (§3.1 item 2), and a trade crossing into the impact clamp (up to `o·skew_scale·max_impact²/2` ≈ $9.4k at $75, which the weekend cap subtracts but the net cap does not). Vesting releases and fee overflow to cash raise `B` and the room. The per-position slack (2 units) and `net_notional` rounding are units.
    - *Cap-exempt liquidations of the minority side can wipe it:* liquidations (stage 1 and 2) remove positions with no net or weekend cap check (§3.1 item 3). When the losing side is the minority (the pool long and the price falls, or the pool short and it rises), each liquidated contract raises `|net|`, up to the hard line with no ADL: `nTrig` fires only above 0.6×, `wTrig` only inside `W`, and `dTrig` needs a 15% drawdown. Worked through with the real `pool_math` (a review model, not a repo test; production params, `B` = $10M at $75, pool long at the cap with 80k contracts each side on venue pairs): after a 5% fall the room is $2.20M, as above, but liquidating 12,000 contracts of 20× longs takes `net_notional` to 0.475·`B` and the room to 4.9%, and 15,000 contracts to 0.497·`B` and 0.55%, so a matured 1% claim is refused. In the mirror case (pool short, a rise) the room shrinks well beyond `(0.2 − 1.2x)·B`. Nothing restores it until traders close.
    - *A later raise of `G` shrinks it:* `update_market_config` raising the IM or the weekend IM raises a derived `G` without the pool seeing it (§3.1 item 4, §15.1); the buffer `1 − k_net·G/α` is gone at `G = α/k_net` (25% for production).
    - *Closes and mode-0 ADL are capped on `B`, not `capNAV`:* a net-raising close can take the net to `0.4·B` with requests pending, so pending requests are then protected only up to the same 20% of `B`.
    - *Not a constraint at the cap:* the hard-net post-check (`B' ≥ net_notional/0.6`) leaves a third of `B`, so the weekend line binds first; the per-account cap bounds who holds the net, not the LPs; the cash check binds only if cash is below the room.
    Beyond the buffer, exits wait for traders to close: `nTrig` fires only above `hard_net·B` (0.6×) and cuts to `k_net·B` (which restores the buffer), `wTrig` runs only inside `W` (where exits are shut) and stops at `α·B/G`, and `dTrig` needs a 15% drawdown (down to 0.2×). A matured request the line refuses can be cancelled and re-requested smaller, but that restarts the full `withdrawal_delay_secs` (4 days), the room is not reserved meanwhile, and other matured claims can use it first; a refused request lapses after its claim window. The buffer is a parameter choice: `validate` still accepts `k_net = α/G`, which leaves no exit room at the cap (the D43/D44 lock). Decided by the owner (§18 #15, D45).
- The per-account cap is a concentration bound and is Sybil-able. It stops one account taking the whole net, not a determined actor. It keys on `house_entry_notional`, so it binds house opens only.
- **Profit-close quiet time (D23).** For `profit_close_quiet_secs` after every session open (Sunday 18:00 and each daily 18:00 reopen) a close with `P > 0` fails `ProfitCloseQuiet`. A gap makes matched losers bankrupt at the open, and their winners would otherwise close before the keeper books the deficit, so the deficit would skip the D13 haircut and land on LPs. Mode-0 (deficit) ADL is gated too, since it always pays a profit (D23, second review). On a settled market the quiet time runs from `settled_at` and gates `close_position`, `close_settled_position` and mode-0 ADL alike (D35). Exposure-mode ADL, liquidations, losing closes and venue hedges are not gated, nor are schedules that never close (`seconds_since_open` is `None`). *Liveness cost:* take-profit closes wait up to 5 min after each open; the frontend and TP/SL monitor must retry after it.

---

## 4. Changed instructions

Shorthand used in the account tables:
- `MARKET` = `seeds=[b"market", market.collateral_mint, market.commodity_seed()], bump=market.bump`.
- `POOL` = `mut, seeds=[LpPool::SEED, market.key()], bump=lp_pool.bump`, as `Box<Account<LpPool>>`.
- `VAULT` = `seeds=[b"vault", market], bump=vault.bump`.
- `VT` = `mut, seeds=[b"vault_token", market], bump`.
- `USER(o)` = `mut, seeds=[b"user", o, market], bump=user.bump, constraint user.owner==o`.
- `CFG_ADMIN(s)` = `seeds=[ProtocolConfig::SEED], bump, constraint config.admin==s @Unauthorized`.

New accounts are **appended** to each struct, so existing positional clients keep their indices. Box every typed account that is not boxed yet.

### 4.1 `deposit_collateral(amount)`: + `lp_pool` (POOL), account 8

After the CPI:
- `user.collateral_balance += amount`;
- `pool.free_collateral += amount`;
- `vault.total_deposits += amount`, as today.

All three are checked and use `MathOverflow` on failure; the current `unwrap()` is replaced. Then `assert_backed(vt.amount + amount)` (§11).

### 4.2 `withdraw_collateral(amount)`: + `lp_pool` (POOL), account 8

The existing check is `balance ≥ amount`. After the CPI, `pool.free_collateral −= amount`, using checked subtraction with `BucketUnderflow`. Then `assert_backed(vt.amount − amount)`. Pool state never gates this instruction.

`vault.total_deposits` becomes a statistic: `vault.total_deposits = vault.total_deposits.saturating_sub(amount)` replaces the `checked_sub(..).unwrap()` at `withdraw_collateral.rs:84`. ADL payouts and stage-1 leftovers credit free collateral from inside the pool, so a user's free balance can exceed what was ever deposited, and the old line would panic (red-team F18). `pool.free_collateral` is the real ledger.

### 4.3 `add_margin(amount)`: + `lp_pool` (POOL), account 5

`pool.free_collateral −= amount` and `pool.position_collateral += amount`. These are internal moves.

### 4.4 `open_position(params)` (house path)

`OpenPositionParams { side: u8, size: u64, leverage: u32, acceptable_price: u64 }`. The new field is appended, and 0 means no bound.

Accounts:
1. `owner` (mut, signer)
2. `user_account` (USER(owner), Box)
3. `market` (mut, MARKET, `!is_paused @MarketPaused`, Box)
4. `position` (init as today, Box)
5. `pyth_price_feed`
6. `system_program`
7. `lp_pool` (POOL)

Checks and effects, in order:
1. `0 < leverage ≤ max_leverage` (`ExcessiveLeverage`). `now`. `!settled` (`MarketSettled`). In session (`MarketClosed`). `size > 0` (`PositionTooSmall`). `side` ∈ {0,1} (`InvalidPositionSide`).
2. `oracle = read_price(..)`.
3. Gross OI cap, as today (`OpenInterestCapExceeded`). `lp_shares > 0` (`PoolHasNoLps`).
4. `delta = ±size`. `exec = skew_price(oracle, L−S, delta, params)`. If `acceptable_price ≠ 0`, a buy needs `exec ≤ acceptable_price` and a sell needs `exec ≥ acceptable_price`; otherwise `SlippageExceeded`.
5. `notional_exec = floor(size·exec, 1e6)`, which must be > 0 (`PositionTooSmall`).
   - `margin = venue_margin(market, side, size, exec, oracle, leverage, now)`, with `None` giving `ExcessiveLeverage`.
   - With zero impact this equals `required_collateral`, and in general it guarantees equity at the oracle ≥ IM (venue-settlement §2.2).
   - `fee = ceil(notional_exec·open_fee_bps, 10_000)`.
6. `collateral_balance ≥ margin + fee`, otherwise `InsufficientCollateral`.
7. `v = pool_view(oracle, now)`; `touch_peak`. `net_after = net + delta`. If `|net_after| > |net|`, check in this order, all against `capNAV`:
   - `adl_required()` → `PoolAdlPending`; the drawdown freeze (on `drawdown_now`, D28) → `PoolDrawdownFreeze`;
   - the net cap → `PoolExposureLimit`;
   - the weekend net cap (while `weekend_cap_window_secs > 0`, at any time; D43) → `PoolWeekendExposureLimit`.
8. If `per_account_cap_bps > 0`: the per-account cap, with `opened_notional = notional_exec` (the position's `opened_price` is `exec`) → `AccountExposureLimit`.
9. Effects:
   - `user.collateral_balance −= margin+fee`, `pool.free −= margin+fee`, `pool.position_collateral += margin`, `distribute_fee(fee)`;
   - `position.init_open(.., entry=exec, collateral=margin, source=OrderBook, ..)`;
   - `pool_add(side, size, exec, F)`, `OI_side += size`;
   - the counters as today;
   - `user.house_entry_notional += notional_exec` (= `opened_notional(size)`).
10. Log the same "Position opened:" line, with `exec` as the price. Then `emit!(PositionOpened)`.

### 4.5 `close_position(acceptable_price: u64)`

The instruction gains this argument. Today `ClosePosition` has 12 accounts: 9 required plus the three optional referral accounts (`user_referral`, `referral_code`, `referrer_token_account`). `lp_pool` (POOL) is appended as **account 13**, after the optionals. Anchor's optional accounts are positional, so a client that has no referral passes the program id in each of the three optional slots and still puts `lp_pool` at position 13 (zero-based index 12). `user_account`, `vault_token_account` and the rest are unchanged.

The body of steps 2–7 is the [NI] `settle_close(..) -> CloseOutcome`, which `close_settled_position` (§10.5) reuses.

1. **Price.**
   - Settled: `m = exec = settlement_price`.
   - Otherwise: in session, `m = read_price`, and `exec = skew_price(m, L−S, side==Long ? −size : +size)`.
   - Then check `acceptable_price` as in §4.4: closing a long is a sell, closing a short is a buy.
2. **Value and caps.**
   - `v = pool_view(m, now)`; `touch_peak`. `pnl = position.unrealized_pnl(exec)`, `fnd = accrued_funding(F)` and `P = pnl + fnd`.
   - If `P > 0` and `in_profit_close_quiet`: fails `ProfitCloseQuiet` (D23). Unsettled, that is `seconds_since_open(schedule, now).map_or(false, |t| t < profit_close_quiet_secs)`; settled, `settled_at > 0 && now − settled_at < profit_close_quiet_secs` (D35). This comes before the caps.
   - If not settled: `net_after = net − size` for a long, `net + size` for a short. If `|net_after| > |net|`, the net cap and (with the weekend bound on, at any time; D43) the weekend cap against `B` must hold (`PoolExposureLimit`, `PoolWeekendExposureLimit`; §3.1, D14).
3. **Profit or loss.**
   - If `P > 0`: `po = pay_profit(pool, v, side, P, false)` (§2.8), and `P_eff = po.paid`. This applies the solvency haircut and the bad-debt haircut, and fails with `PoolCashInsufficient` if cash is short. `cash` has already been debited.
   - Otherwise `P_eff = P` (≤ 0).
4. **Fee and referral.**
   - `base_fee = ceil(opened_notional(size)·taker_fee, 10_000)` (D21), then `(fee, reward) = referral_fees(..)` as today.
   - `fee_c = clamp(C + P_eff, 0, fee)`.
   - `referral_paid = referral_reward_paid(reward, fee, C + P_eff)`, as today.
5. **Equity.** `E = C + P_eff − fee_c`.
6. **Pool.**
   - If `E ≥ 0`:
     - `payout = E`;
     - if `P_eff ≤ 0`, `cash += −P_eff` (the profit case was debited in step 3);
     - `distribute_fee(fee_c − referral_paid)`.
   - If `E < 0` (only possible when `P_eff < 0`):
     - `payout = 0`, `D = −E` (fee_c and referral_paid are 0);
     - `cash += C`;
     - `absorb_deficit(D, side, v.nav_redeem, now)`.
   - In both cases `pool.position_collateral −= C` and `pool_remove(side, size, entry, last_funding_payment)`; then `OI_side −= size` (checked, `MathOverflow`, no longer saturating).
   - If `source == OrderBook`: `user.house_entry_notional = user.house_entry_notional.checked_sub(opened_notional(size))` (`BucketUnderflow`).
7. **Position and user.** `status = Closed`. `position.realized_pnl += P_eff − fee_c` (accumulates; settled losses and liquidation slices are already in it). `user.realized_pnl += position.realized_pnl` (the position's lifetime figure; red-team F31). Other stats as today.
8. **Transfers.** As today: `payout` to the user and `referral_paid` to the referrer. Then `assert_backed(vt.amount − payout − referral_paid)`.
9. **Log.** Keep the "Position closed:" line, with `Funding=fnd`, `Fee=fee_c` and `Settlement=payout` (D33). Then `emit!(PositionClosed)` with the three haircuts from step 3.

Bucket check: the bucket total changes by `−payout − referral_paid` in both branches (§11).

### 4.6 `liquidate()`

Accounts as today (10), plus `lp_pool` (POOL) as account 11. `user_account` must be mut, as it is today. The full logic is in §6.

### 4.7 `update_funding()`

Accounts are unchanged, which keeps the keeper's instruction as it is. The logic is in §8.

### 4.8 `venue_fill(params)`: + `lp_pool` (POOL), account 17

`VenueFill` has 16 accounts today; `lp_pool` is appended after `system_program` as account 17, so the CPI from mm-registry and orderbook carries 17 metas.

Steps 1–10 are unchanged, plus one new check before them: `lp_shares > 0` (`PoolHasNoLps`, D11). After `take_margin` for both legs:
- `pool.free −= tc + mc` and `pool.position_collateral += tc + mc`;
- `pool_add(taker_side, size, price, F)` and `pool_add(maker_side, size, price, F)`, so `open_positions += 2`.

No other cap or pool gate applies, because net is unchanged. Position init also sets `opened_price`.

### 4.9 `settle_market(price)`

Accounts are unchanged. The admin must publish the settlement-price rule in advance (this is documentation). First, the funding due since the last crank is charged at the settlement price (`apply_funding`, with the usual "Funding updated:" line and `FundingUpdated` event; D42), since nothing can crank once settled. Then `settlement_price`, `settled_at = now` and `is_paused = true` are set; nothing else in `Market` or `LpPool` changes. Once settled:
- the pool's valuation uses `settlement_price`;
- the session and oracle gates drop for `close_position`, `settle_position_pnl`, `lp_request_withdrawal`, `lp_withdraw` and deficit-mode `adl`; so do the LP window, quiet, funding-current and claim-window gates (§9.5). `lp_withdraw` instead waits out the settled quiet time `max(profit_close_quiet_secs, lp_reopen_quiet_secs)` after `settled_at` (D41), and profitable closes wait `profit_close_quiet_secs` after it (D35);
- `close_settled_position` (§10.5) becomes available, so anyone can close bankrupt or abandoned positions at `settlement_price` (D19);
- once `open_positions == 0`, `sweep_insurance_excess` moves the whole fund to cash (§10.1);
- `open_position`, `venue_fill`, `liquidate`, `update_funding` and `lp_deposit` are refused (settle pauses the market, and `lp_deposit` requires `!is_paused`). `liquidate` keeps returning `MarketSettled`, so `settlement_closes_positions_when_the_oracle_breaks` is unchanged.

### 4.10 `initialize_market`

Accounts are unchanged. One new check: `60 ≤ funding_interval ≤ 86_400` → `InvalidMarketConfig`. Today a 0 interval would make `update_funding` divide by zero. Also check `max_open_interest ≤ MAX_OPEN_INTEREST`. Set `funding_carry_secs = 0`.

### 4.11 `pause_market`, `initialize_user` and the referral instructions

Unchanged.

### 4.12 `update_market_config(params)`

`MarketConfigParams` gains `max_open_interest: u64` at the end. This breaks the IDL; update `scripts/` and `AdminPanel`.
- **`0` means "keep the current value"**, so `MarketConfigParams::default()`, `{ is_paused: false, ..Default::default() }` (`integration.rs` `unpaused()`) and every caller that only changes the schedule or the pause flag keep working unchanged (red-team F19).
- A non-zero value must be `≤ MAX_OPEN_INTEREST` (`InvalidMarketConfig`), and the handler then sets the field. The cap may be set below the current OI, and then it only blocks increases.
- `validate()` therefore still accepts `MarketConfigParams::default()`, and the unit test `accepts_typical_weekend_config` is unchanged.

---

## 5. New instructions

`lib.rs` gains 18 entrypoints, each listed below. All live in `instructions/lp_pool.rs`, `instructions/pool_admin.rs` and `instructions/adl.rs`; the shared close body is in `instructions/settle_close.rs` (D29).

| Instruction | Signer | Accounts (in order; constraints) | Effect |
|---|---|---|---|
| `initialize_pool(params, adl_keeper)` | admin | `admin` (mut, signer); `config` CFG_ADMIN; `market` MARKET; `lp_pool` init payer admin, `space=LpPool::LEN`, seeds; `system_program` | Requires `!settled`, both OIs 0 and `insurance_fund==0` (`MarketNotFresh`). No collateral deposit can precede the pool, because `deposit_collateral` requires POOL. There is **no** check on the vault token balance: anyone can send tokens to the `vault_token` PDA, so `amount == 0` could be made false for ever with one micro-USDC (red-team F14, F29). A donation only widens §11. Then `params.validate(market)`. Zeroes the buckets and aggregates and sets `market`, `adl_keeper`, `params` and `bump`. No migration path exists (redeploy). **Bootstrap:** `scripts/initialize-market.ts` and the harnesses send `initialize_market`, `initialize_pool`, `init_lp_position` and the protocol's seed `lp_deposit` in one transaction (D11) |
| `init_lp_position(class)` | owner | `owner` (mut, signer, payer); `lp_pool` (seeds `[LpPool::SEED, lp_pool.market]`); `lp_position` init, seeds `[LpPosition::SEED, lp_pool, owner, &[class]]`; `system_program` | `class == 0` (`InvalidLpClass`). Sets owner, pool, class and bump |
| `lp_deposit(amount, min_shares_out)` | owner (anyone) | §9.1 | |
| `lp_request_withdrawal(shares)` | owner | §9.2 | |
| `lp_cancel_withdrawal()` | owner | §9.3 | |
| `lp_withdraw()` | owner | §9.4 | |
| `donate_to_pool(amount)` | anyone | §10.4 | |
| `settle_position_pnl()` | anyone | §6.6 | |
| `close_settled_position()` | anyone | §10.5 | |
| `adl(mode, size)` | mode 0: anyone; mode 1: `adl_keeper` or admin | §7 | |
| `write_off_bad_debt()` | anyone | `signer`; `market` MARKET (read, for the schedule); `lp_pool` POOL | §7.4 |
| `sweep_insurance_excess()` | anyone | `signer`; `market` (mut, MARKET); `lp_pool` POOL | §10.1 |
| `withdraw_treasury(amount)` | admin | `admin`; `config` CFG_ADMIN; `market` MARKET; `lp_pool` POOL; `vault` VAULT; `vault_token_account` VT; `destination` (mut `TokenAccount`, `mint == market.collateral_mint`, `!= vault_token_account @VaultInvariantViolated`, D27); `token_program` | §10.2 |
| `propose_pool_params(params)` | admin | `admin`; `config` CFG_ADMIN; `market` MARKET; `lp_pool` POOL | Validates. If `lp_shares == 0` (only before the seed), applies at once. Otherwise sets `pending_params` and `pending_params_at = now + T`, with `T = max(MIN_POOL_TIMELOCK_SECS, max(cur.withdrawal_delay_secs, new.withdrawal_delay_secs) + max(cur.weekend_cap_window_secs, new.weekend_cap_window_secs) + PARAMS_TIMELOCK_MARGIN_SECS)` (i64), where `cur = lp_pool.params`. Emits `PoolParamsProposed`, and `PoolParamsApplied` too when applied at once (D32) |
| `apply_pool_params()` | anyone | `market` MARKET; `lp_pool` POOL | Requires `pending_params_at != 0 && now ≥ pending_params_at` (`ParamsTimelocked`), then re-validates against the market as it is now. Sets `params = pending_params`, `pending_params_at = 0` and `pending_params = default` (D32) |
| `cancel_pool_params()` | admin | `admin`; `config`; `lp_pool` | Requires `pending_params_at != 0` (`NoPendingParams`), then clears it and `pending_params` |
| `set_max_pool_cash(cap)` | admin | `admin`; `config`; `lp_pool` | Immediate, because it cannot hurt existing LPs |
| `set_adl_keeper(key)` | admin | `admin`; `config`; `lp_pool` | Immediate. The keeper signs only exposure-mode ADL, which is bounded on-chain by the triggers, the targets and the timelocked score floor |

**Why the timelock is that long.** "An LP who reacts at once can exit before any change applies" must hold in the worst case. A proposal just before the weekly close can wait up to 49 h for the first in-session `lp_request_withdrawal`. After the delay, `lp_withdraw` can be closed for up to the pre-close window, the 49 h weekend and the reopen quiet time. `49 h + 49 h + 0.5 h ≈ 4.1 d < PARAMS_TIMELOCK_MARGIN_SECS` (5 d). Taking the larger of the current and the proposed delay and window means a proposal cannot shorten the time LPs have to react (red-team F20, F28). With the defaults, `T = 4 d + 1 d + 5 d = 10 d`. A request also stores `unlock_at` and `claim_window_secs` (§9.2), so a change that lands while a request is pending does not move that LP's exit.

There is no instruction that withdraws `insurance`, by design. There is no `set_adl_score_floor` (D16).

---

## 6. Liquidation (`liquidate`, §4.6)

### 6.1 Trigger on mark notional

New method: `Position::equity(mark, F) -> i128 = C + pnl(mark) + accrued_funding(F)`.

`is_liquidatable(mark, funding, mmr)` becomes: with `N_mark = floor(size·mark, 1e6)` (u128), the position is liquidatable when `equity·10_000 < mmr·N_mark`. With `N_mark == 0` it is liquidatable only if `equity < 0`.

`margin_ratio()` keeps entry notional (the venue backstop and the UI use it). A new `margin_ratio_mark()` is for display.

### 6.2 Ordered handler

1. `now`, `!settled` (`MarketSettled`; settled markets use `close_settled_position`, §10.5), in session, `m = read_price`. `(im, mmr) = margin_ratios(now)`.
2. `fnd = accrued_funding(F)`, `E = equity(m)` and `N = N_mark`. Require liquidatable (`NotLiquidatable`).
3. `v = pool_view(m, now)`; `touch_peak`; `nav0 = v.nav_redeem`.
4. `backstop = mmr·backstop_threshold_bps / 10_000` (floor). Stage 1 if `E·10_000 ≥ backstop·N`, otherwise stage 2. With `backstop_threshold_bps = 10_000` every liquidation is stage 2, which is today's behaviour. The stage is decided on `E` before any haircut.

Liquidations never revert on pool cash: a positive realized amount is quoted with `allow_short = true` (§2.8).

### 6.3 Stage 1: partial, with the owner keeping leftover equity

1. **Slice.** Let `p = liq_penalty_bps`.
   - If `im > p`:
     - `x_need = ceil_s(im·size·m − 1e10·E, m·(im − p))` (i128, ≥ 0);
     - this is the smallest slice after which `E − penalty ≥ IM·remaining notional`;
     - the multiplication is checked, and on overflow `x_need = size`.
   - Otherwise `x_need = size`.
   - Then `x = clamp(max(x_need, ceil(size·min_liq_slice_bps, 10_000)), 1, size)`.
   - If `floor((size−x)·m, 1e6) < liq_min_remaining_notional`, set `x = size`.
2. **Penalty.** `pen(x) = min(ceil(floor(x·m,1e6)·p, 10_000), max(E,0))`, `keeper = floor(pen·liq_keeper_share_bps, 10_000)` and `ins_part = pen − keeper`.
3. **Partial (`x < size`).**
   - `pnl_x = Position{size:x, entry, side}.unrealized_pnl(m)` and `realized = pnl_x + slice_funding(x)`, where `slice_funding(x) = fnd(size) − fnd(size − x)`: the slice settles only its own share and slice plus remainder telescope exactly (D24).
   - `r = realized > 0 ? quote_profit(pool, v, side, realized, true).paid : realized`.
   - `C' = C + r − pen(x)` and `E_rest = C' + pnl(size − x) + fnd(size − x)`. If `C' ≤ 0` or `E_rest ≤ 0`, set `x = size` and go to step 4 (nothing has been applied yet).
   - Otherwise:
     - if `realized > 0`, `apply_payout` (it debits `cash` by `r` and applies the bad-debt haircut); else `cash += −realized`;
     - `position_collateral += C' − C` (signed, checked);
     - `credit_insurance(ins_part)`;
     - `pool_replace((size, entry, f_old) → (size−x, entry, f_old))`;
     - the position's `size −= x`, `collateral = C'`, `realized_pnl += r − pen`; `last_funding_payment` is unchanged, so the remainder keeps its unsettled funding (D24);
     - `OI −= x`;
     - if the position is house-opened, `house_entry_notional −= opened_notional(size_old) − opened_notional(size_old − x)` (checked);
     - the status stays `Open`.
   - When a haircut applied, the remainder can sit below IM and may be liquidated again next pass. That is accepted.
4. **Full stage 1 (`x == size`).**
   - `realized = pnl + fnd`; `r = realized > 0 ? pay_profit(.., true).paid : realized`, and if `realized ≤ 0`, `cash += −realized`.
   - `E' = C + r`, which is ≥ 0: if `realized > 0` then `r ≥ 0`, otherwise `E' = E > 0`.
   - `pen = min(pen(size), E')`, with `keeper` and `ins_part` recomputed from it. By the validate rule `p < mmr·backstop`, `E ≥ backstop·N > pen` before any haircut.
   - `leftover = E' − pen ≥ 0`. `position_collateral −= C`, `credit_insurance(ins_part)`.
   - `user.collateral_balance += leftover` and `pool.free += leftover` (D7).
   - `pool_remove`, `OI −= size`, `status = Liquidated`, `realized_pnl += r − pen`, `user.realized_pnl += position.realized_pnl`, and for a house position `house_entry_notional −= opened_notional(size)` (checked).
5. Transfer `keeper` to `liquidator_token_account`.

### 6.4 Stage 2: backstop (full close; the remainder goes to insurance)

1. `target = liquidation_reward_target(opened_notional(size), mmr) = floor(opened_notional·mmr·liquidation_fee, 1e8)`, the existing formula with the MMR **in force** (D8).
2. `realized = pnl + fnd`. If `realized > 0`: `r = pay_profit(.., true).paid` (cash is debited by `r`); otherwise `r = realized`. `E' = C + r`.
3. `roll_loss_window(pool, nav0, now)` (§2.6 step 0), then `cap_ins = insurance_claim_cap_per_window == 0 ? u64::MAX : cap.saturating_sub(ins_paid_window)`.
4. If `E' ≥ 0`:
   - if `realized ≤ 0`, `cash += −realized`;
   - `from_col = min(target, E')` and `credit_insurance(E' − from_col)`;
   - `top = min(target − from_col, insurance, cap_ins)`, `insurance −= top` and `ins_paid_window += top`.
5. If `E' < 0` (so `realized < 0`):
   - `from_col = 0`, `top = min(target, insurance, cap_ins)`, `insurance −= top` and `ins_paid_window += top`;
   - `cash += C`;
   - `absorb_deficit(−E', side, nav0, now)`, whose insurance cap now already counts `top`.
6. `position_collateral −= C`, `pool_remove`, `OI −= size`, `status = Liquidated`, `realized_pnl += r`, `user.realized_pnl += position.realized_pnl`, and for a house position `house_entry_notional −= opened_notional(size)` (checked). Pay `from_col + top` to the liquidator.

### 6.5 Common tail

`assert_backed(vt.amount − keeper_paid)`. Keep the "Position liquidated:" log line and its format, but print the **closed size `x`** in its size field. After a stage-1 partial, `position.size` is the remainder, and printing it would make the indexer record the wrong liquidated size. Emit `PositionLiquidated { position, stage, size_closed, size_remaining, mark, penalty, keeper_reward, owner_leftover, insurance_delta, deficit, solvency_hc, bad_debt_hc, cash_hc }`.

**Old helpers.** `Market::settle_liquidation`, `Market::settle_insurance`, `liquidation_payout` and `apply_insurance` are **deleted**; `credit_insurance`, `absorb_deficit` and the stage functions replace them. `liquidation_reward_target(notional)` becomes `liquidation_reward_target(notional, mmr)`. The unit tests `credit_with_no_deficit_grows_fund` … `empty_fund_limits_reward_and_reports_bad_debt` in `state/market.rs` are rewritten against the stage-2 split above (residual to insurance with overflow to cash, keeper top-up capped by the window, deficit to the waterfall). The TS "Liquidation calculations" tests are rewritten to match §6.3–§6.4 (§16.6).

### 6.6 `settle_position_pnl()` (D3)

Accounts:
1. `signer`
2. `market` (MARKET, read-only)
3. `lp_pool` (POOL)
4. `position` (mut, `market == market`, Open)
5. `pyth_price_feed` (`== market.pyth_price_feed`)

Steps:
1. Settled: `m = settlement_price`. Otherwise: in session and `m = read_price`.
2. `P = pnl(m) + fnd`, which must be `< 0` (`NothingToSettle`). Let `loss = −P`; require `loss < C` (`SettleWouldBankrupt`, so the keeper liquidates instead).
3. Move the money: `C −= loss`, `position_collateral −= loss`, `cash += loss`.
4. Reset the position: `pool_replace((size, entry, f) → (size, m, F))`, `entry_price = m`, `last_funding_payment = F` and `realized_pnl += P`. `touch_peak`.

Equity and `is_liquidatable` are exactly unchanged, because `C + P` is invariant and `N_mark` does not change. `opened_price`, and with it the house notional, the close fee and the stage-2 reward, does not change (D21), so this instruction needs no `user_account`. The settled loss reaches `UserAccount.realized_pnl` when the position finally closes, through `position.realized_pnl`. Emit `PositionPnlSettled`.

`NAV` does not move (cash +loss, U +loss). Under D12 nothing keyed on U/cash exists any more, so a settle before a big winner closes no longer haircuts that winner or blocks LPs (red-team F3).

---

## 7. ADL: `adl(mode: u8, size: u64)`

### 7.1 Accounts (`AdlPosition`; D29)

1. `signer` (signer)
2. `config` (seeds; read for the admin check)
3. `market` (mut, MARKET)
4. `lp_pool` (POOL)
5. `position` (mut, `market == market`, Open)
6. `position_owner` (Unchecked, `address = position.owner`)
7. `user_account` (USER(position_owner))
8. `pyth_price_feed`

No token account is involved: the payout goes to free collateral.

### 7.2 Checked on-chain

1. `mode` ∈ {0 deficit, 1 exposure} (`InvalidAdlMode`). Mode 0 is **permissionless** (D16). Mode 1 needs `signer == lp_pool.adl_keeper || signer == config.admin` (`AdlUnauthorized`).
2. Settled: `m = settlement_price`, and only mode 0 is allowed (mode 1 gives `MarketSettled`, D31). Otherwise: in session and `m = read_price`.
3. Size:
   - `x = size == 0 ? q : size`, with `x ≤ q` (`InvalidPositionReduction`);
   - mode 0: `x = min(x, deficit_slice)`, the smallest slice whose bad-debt haircut covers the opposite pending (D37);
   - if `floor((q−x)·m,1e6) < liq_min_remaining_notional`, set `x = q`;
   - if `x < q` and `remainder_equity = (C − C_x) + pnl(q − x) + fnd(q − x) ≤ 0`, set `x = q` (D24).
4. Profit:
   - `pnl_x = slice pnl(m)` and `P = pnl_x + slice_funding(x)`, the slice's own share of funding (D24);
   - require `P > 0` (`AdlCandidateNotProfitable`);
   - `E = equity(m) > 0`, also `AdlCandidateNotProfitable` (D31).
5. Score and floor, in bps (u128):
   - Long: `score = m·N_mark·10_000 / (entry·E)`. Short: `score = entry·N_mark·10_000 / (m·E)`. This is Hyperliquid's `(m/entry)·(notional/equity)`.
   - Mode 0 floor (on-chain, decaying): `dt = pending_age(since)` with `since` the clock of the side the candidate repays (D36): `open_seconds_between(schedule, since, now)`, or once settled `open_seconds_between(schedule, since, s) + (now − s)` with `s = max(since, settled_at)` (D35); `floor = ADL_FLOOR_START_BPS·max(0, ADL_FLOOR_DECAY_SECS − dt) / ADL_FLOOR_DECAY_SECS`. Right after a deficit only the highest scores qualify; after 6 open hours every profitable opposite-side position does. Highest scores therefore become eligible first, with no ranking to trust.
   - Mode 1 floor: `params.adl_exposure_score_floor_bps` (timelocked, so the keeper cannot move it).
   - Require `score ≥ floor` (`AdlScoreBelowFloor`).
6. `v = pool_view(m, now)`; `touch_peak`. The code does this before steps 3–5, which only read `v` (D31).
7. **Mode 0, deficit.** If neither side has pending, the error is `AdlNotRequired`, checked after steps 4–5 (D31). The candidate must sit opposite the bankrupt side: a Short needs `pending_bad_debt_long > 0` and a Long needs `pending_bad_debt_short > 0` (`AdlWrongSide`). The profit-close quiet time applies (`ProfitCloseQuiet`, D23, D35): mode 0 always pays a profit. Unless settled, a slice that raises `|net|` must pass `check_close_caps` (`PoolExposureLimit`, `PoolWeekendExposureLimit`; D25). The haircut itself is the bad-debt haircut inside `pay_profit` (step 9). The slice owes `fee = ceil(opened_notional(x)·taker_fee, 10_000)` (D22); mode 1 owes 0.
8. **Mode 1, exposure.** Evaluate the three triggers of §3 on `v`, all against `B`:
   - `nTrig` (hard net), target `T_n = floor(k_net_bps·B/10_000)`;
   - `dTrig` (drawdown ≥ `dd_adl_bps`), target `T_d = floor(adl_net_target_bps·B/10_000)`;
   - `wTrig` (inside `W`, `net_notional·G > α·B`; D20), target `T_w = floor(α·B/G_bps)`.
   - At least one must hold (`AdlNotRequired`). `T_net` = the smallest target among those that hold.
   - `net ≠ 0` and the candidate's side equals `sign(net)` (`AdlWrongSide`).
9. Effects, common to both modes:
   - `pay = pay_profit(pool, v, side, P, false).paid` (§2.8): the solvency haircut, then the bad-debt haircut (`min(pending_opp, P)`; this is the whole point of mode 0, and it also applies in mode 1 if the opposite side has pending), then `PoolCashInsufficient` if cash is short;
   - `C_x = x==q ? C : floor(C·x/q)` and `position_collateral −= C_x`;
   - `fee_c = min(fee, C_x + pay)` and `distribute_fee(fee_c)`: the fee comes after the haircuts, from what the slice would receive, so the buckets never underflow (D22);
   - `user.collateral_balance += C_x + pay − fee_c` and `pool.free += C_x + pay − fee_c`;
   - aggregates:
     - partial: `pool_replace((q,e,f) → (q−x, e, f))`, then `size −= x`, `C −= C_x`; `last_funding_payment` is unchanged (D24);
     - full: `pool_remove` and `status = Closed`;
   - `OI −= x`; for a house position `house_entry_notional −= opened_notional(q) − opened_notional(q − x)` (checked);
   - `realized_pnl += pay − fee_c`; on a full close `user.realized_pnl += position.realized_pnl`.
10. Mode 1 post-condition: recompute `v'`. Require `net'` to have the sign of `net` or be 0, and `net_notional' ≥ T_net` (`AdlOvershoot`). ADL stops at the target, and the caller sizes `x` to land on it. Because the targets use `B` (pending withdrawals included), a withdrawal request cannot widen the band (red-team F7, F33).
11. Emit `Adl { mode, position, owner, size_closed, mark, profit, solvency_hc, bad_debt_hc, score, floor, triggers, net_before, net_after, cash_after, b, fee }`; `profit` is `pay`, `fee` is `fee_c` (D22).

### 7.3 Supplied off-chain

- **Mode 0.** Anyone may call it; the keeper's `adl.rs` loop does so as a matter of course, choosing the highest-score eligible position and sizing net-raising slices within the close caps (D25). Each slice pays the close fee (D22). The on-chain floor makes the order self-enforcing: a position becomes eligible only once the floor has decayed to its score.
- **Mode 1.** The keeper ranks the net-side profitable positions by score (all above the timelocked floor) and sizes `x`. The ranking within the eligible set is the keeper's choice and is audited from the `Adl` events. The UI shows each trader their score.

### 7.4 `write_off_bad_debt()`

Requires `adl_required()` (`AdlNotRequired`, D31). Each side is written off on its own clock (D36): a side with pending whose age (§7.2 step 5: open seconds since its debt-weighted `pending_since_*`, and once settled open seconds up to `settled_at` plus wall seconds after it, D35) is `≥ ADL_WRITE_OFF_SECS` has its pending and clock zeroed; `w` is the sum written off, `bad_debt_written_off_cum += w`, and `w == 0` fails `BadDebtNotStale`. Counting open seconds means a Friday deficit cannot be written off on Saturday before ADL had a day of session to work (red-team F27), and settling does not count the closed time before it; the debt-weighted clock means a large new deficit restarts it almost fully, and per-side clocks mean fresh debt on one side never inherits the other side's age.

NAV does not change, because the loss is already in it (D2). This lifts the LP freeze when no profitable counterparty exists. Emits `BadDebtWrittenOff`.

---

## 8. Funding: catch-up and session gating (`update_funding`)

New `session::open_seconds_between(schedule, a, b) -> i64`, 0 when `b ≤ a` (D29). It is O(1) and [NI]:
- `ALWAYS_OPEN`: `b − a`.
- `CME_GLOBEX`: `F(b) − F(a)`.

`F(t)` is cumulative open seconds on the US-Eastern local week grid:
- `local = t + eastern_offset(t)`, `day = local.div_euclid(86_400)`, `s = local.rem_euclid(86_400)`, `wd = weekday(day)`;
- `F = (day − wd).div_euclid(7)·414_000 + W(wd, s)`, where `414_000 = 115 h`, the open time per week;
- `W` by weekday:
  - Sun: `max(0, s − 64_800)`;
  - Mon–Thu: `21_600 + (wd−1)·82_800 + (s < 61_200 ? s : s < 64_800 ? 61_200 : s − 3_600)`;
  - Fri: `21_600 + 4·82_800 + min(s, 61_200)`;
  - Sat: `414_000`.

DST changes happen on Sunday at 02:00 local, while the market is closed, so F is continuous and exact. Holidays are not modelled, as today: the oracle goes stale.

Two more session helpers, also [NI] and also in `session.rs` (so the keeper and the sim get them through `#[path]`):
- `seconds_since_open(schedule, ts) -> Option<i64>`: `None` if closed or `ALWAYS_OPEN`. For CME, seconds since the current session opened (Sunday 18:00 or the daily 18:00 reopen): Sunday `s − 64_800`; Mon–Fri `s < 61_200 ? s + 21_600 : s − 64_800`.
- `funding_current(schedule, settled, last_funding_time, carry_secs, interval, now) -> bool = settled || open_seconds_between(schedule, last_funding_time, now) + carry_secs < interval`, with `Market::funding_current(now)` as the `(m, now)` form (D29). LP flows require it (§9), so an uncranked funding jump cannot be captured by bundling a deposit or request around a permissionless crank (red-team F12). A caller simply cranks `update_funding` first in the same transaction.

Trades crank it themselves (D40): `open_position`, `venue_fill`, `close_position`, `liquidate` and `adl` (the last two unless settled) run the same steps 3–9 as `update_funding` (`apply_funding`, then the log and `FundingUpdated`) when a period is due, before they set or settle any position's funding basis. A new position therefore never takes a stale `cumulative_funding` as its basis, and a close or liquidation never settles at one.

Handler, in order:
1. `!settled` (`MarketSettled`). In session (`MarketClosed`).
2. `oracle = read_price`.
3. `open = open_seconds_between(schedule, last_funding_time, now)` and `total = open + funding_carry_secs` (i64).
4. `total ≥ funding_interval`, otherwise `FundingNotDue`. This is a new error; today the code is `InvalidMarketConfig`.
5. `due = total / interval`, `n = min(due, MAX_FUNDING_CATCHUP_PERIODS)` and `periods_dropped = due − n`. `carry = due > MAX_FUNDING_CATCHUP_PERIODS ? 0 : total − n·interval`. The sub-period remainder carries whenever no whole period was dropped, including when exactly 168 are due (red-team F24).
6. `rate` is today's imbalance formula, unchanged (capped at ±100 per period). `step = funding_per_contract(rate, oracle)`.
7. `cumulative_funding = clamp(cumulative_funding + n·step)` in i128, saturating to i64.
8. `funding_rate = rate`, `last_funding_time = now` and `funding_carry_secs = carry as u32`.
9. Same "Funding updated:" log line. `emit!(FundingUpdated { periods: n, periods_dropped, rate, step, cumulative, carry })`.

Results:
- Missed periods are charged in O(1), at the crank's price and rate. That is an approximation, and it is documented.
- Closed time, including the 1 h daily break, accrues nothing.
- The first crank after the reopen charges only open seconds.
- The pool's carry reaches LP cash through §2.3.5.

---

## 9. LP flows (all in `instructions/lp_pool.rs`)

Common gates. `Q = session::seconds_since_open(schedule, now).map_or(false, |t| t < lp_reopen_quiet_secs)` is the reopen quiet time (D17). Unless the market is settled:
- `lp_deposit` needs `!Q`;
- `lp_withdraw` needs `!Q` and `!W` (no exits in the pre-close window);
- all three of deposit, request and withdraw need `funding_current` (`FundingStale`).
A closed or quiet window gives `LpFlowWindowClosed`.

### 9.1 `lp_deposit(amount, min_shares_out)`

Anyone may deposit; the protocol seeds through this same instruction (D11).

Accounts:
1. `owner` (signer)
2. `market` (MARKET)
3. `lp_pool` (POOL)
4. `lp_position` (mut, seeds `[LpPosition::SEED, lp_pool, owner, &[lp_position.class]]`, `has_one owner`)
5. `vault_token_account` (VT)
6. `owner_token_account` (mut, `owner == owner`, `mint == market.collateral_mint`)
7. `pyth_price_feed`
8. `token_program`

Checks, in order:
1. `amount > 0` (`InvalidLpAmount`).
2. `!market.is_paused` (`MarketPaused`). Settled markets are paused.
3. In session (`MarketClosed`). `m = read_price`: deposits need a fresh oracle and never use a weekend price.
4. `!Q` (`LpFlowWindowClosed`). `funding_current` (`FundingStale`).
5. `!adl_required()` (`PoolAdlPending`). Recapitalising while debt is pending goes through `donate_to_pool` (§10.4).
6. `v = pool_view(m, now)`; `touch_peak`.
7. Shares:
   - **First deposit** (`lp_shares == 0`): require `cash == 0 && open_positions == 0` (`MarketNotFresh`; true by D11) and `amount > LP_DEAD_SHARES` (`InvalidLpAmount`). Mint `amount` shares in total: `LP_DEAD_SHARES` are added to `lp_shares` and owned by nobody, and `amount − LP_DEAD_SHARES` go to the depositor. The seed therefore costs the protocol $1, and `lp_shares` can never return to 0, so no later depositor can mint 1:1 into value left in the pool (red-team F8, F16).
   - Otherwise: require `v.nav_mint > 0` (`PoolInsolvent`), and mint `shares = floor(amount·lp_shares / nav_mint)` (u128). When `NAV_mint ≤ 0` the pool is recapitalised by `donate_to_pool`, not by minting at a floor price, which would make the new depositor pay for the hole.
8. If `max_pool_cash ≠ 0`, require `max(v.nav_mint, 0) + amount ≤ max_pool_cash` (`PoolCashCapExceeded`). The cap measures LP capital, not the `cash` bucket, which also holds losses prepaid by matched traders (red-team F13). The owner named it `max_pool_cash`; the name is kept.
9. The depositor's shares are `> 0` and `≥ min_shares_out` (`SlippageExceeded`).
10. CPI transfer from owner to vault.
11. Effects: `cash += amount`, `lp_shares += minted`, `position.shares += owner_shares` and `deposited_cum += amount`.
12. `assert_backed(vt.amount + amount)`. Emit `LpDeposited { owner, amount, shares, nav_mint, lp_shares }`.

Bootstrap: `initialize_pool`, `init_lp_position` and the protocol's first `lp_deposit` go in the same transaction as `initialize_market` (§5). Before the seed no trading is possible (`PoolHasNoLps`), no fee or residual accrues, `donate_to_pool` and `sweep_insurance_excess` need `lp_shares > 0`, and `insurance_fund` is 0, so there is nothing for a front-runner to capture even if a script sends the seed separately. `lp_deposit`'s gates apply to the seed, so the bootstrap transaction must be sent in session with a fresh oracle, outside `Q`; if it cannot be, the seed follows in a later transaction.

### 9.2 `lp_request_withdrawal(shares)`

Accounts: `owner`, `market`, `lp_pool` (POOL), `lp_position` (mut, as above), `pyth_price_feed`.

1. `0 < shares ≤ position.shares − position.pending_shares` (`InsufficientShares`). `pending_shares == 0` (`WithdrawalPending`).
2. Mark: settled uses `settlement_price`; otherwise in session and `read_price`, and `funding_current` (`FundingStale`).
3. `v = pool_view(m, now)`; `touch_peak`. `requested_value = floor(shares·max(v.nav_redeem,0) / lp_shares)`.
4. Effects: `pending_shares = shares`, `requested_at = now`, `unlock_at = now + withdrawal_delay_secs`, `claim_window_secs = params.withdrawal_claim_window_secs` and `pool.pending_withdraw_shares += shares`. Storing `unlock_at` and the claim window means a later parameter change cannot move this request (red-team F20).

Requests are never gated by ADL, the drawdown lines or the windows: requesting only starts a clock, and `lp_withdraw` pays `min(NAV_now, requested_value)`. Pending shares stop counting toward `capNAV`, so the pool stops adding **new** risk on capital that is leaving; they still count toward `B`, so a request does not trigger ADL or block closes (§2.2). Emit `LpWithdrawalRequested`.

### 9.3 `lp_cancel_withdrawal()`

Accounts: `owner`, `lp_pool` (POOL, mut), `lp_position`. Requires `pending_shares > 0` (`NoWithdrawalPending`). Does `pool.pending −= pending_shares` and clears the request fields (`pending_shares`, `requested_value`, `requested_at`, `unlock_at`, `claim_window_secs`). Needs no price. Works on a lapsed request. Emit `LpWithdrawalCancelled`.

### 9.4 `lp_withdraw()`

Accounts:
1. `owner`
2. `market` (MARKET)
3. `lp_pool` (POOL)
4. `lp_position` (mut)
5. `vault` (VAULT; transfer authority)
6. `vault_token_account` (VT)
7. `owner_token_account` (mut, owner/mint)
8. `pyth_price_feed`
9. `token_program`

Checks, in order:
1. `pending_shares > 0` (`NoWithdrawalPending`). `now ≥ unlock_at` (`WithdrawalDelayNotElapsed`).
2. Unless settled: if `claim_window_secs > 0` and `open_seconds_between(schedule, unlock_at, now) > max(claim_window_secs, W + 5·Q + 86_400)` (`W` and `Q` as in force now, so a later widening of the gates cannot expire the request inside gated time; D38), fail with `WithdrawalExpired`; the owner cancels and re-requests, which restarts the delay. A matured request is therefore not a standing option to leave before a weekend gap (red-team F5).
3. Mark: settled uses `settlement_price`, once `settled_at == 0 || now − settled_at ≥ max(profit_close_quiet_secs, lp_reopen_quiet_secs)` (else `LpFlowWindowClosed`; D41); otherwise in session and `read_price`, then `!W && !Q` (`LpFlowWindowClosed`) and `funding_current` (`FundingStale`).
4. `!adl_required()` (`PoolAdlPending`).
5. `v = pool_view(m, now)`; `touch_peak`. Unless settled, if `dd_withdraw_block_bps > 0 && drawdown_now(pool, v) ≥ dd_withdraw_block_bps`, fail with `PoolWithdrawalsBlocked` (D28).
6. `value = min(floor(pending·max(v.nav_redeem,0)/lp_shares), requested_value)`. The LP bears losses during the delay but gets no gains, so a request gives no free option.
7. `value ≤ cash` (`PoolCashInsufficient`).
8. Post-check, skipped if settled: `B' = max(nav_redeem − value, 0)`; if `hard_net_bps > 0 && net ≠ 0`, require `net_notional·10_000 ≤ hard_net·B'` (`PoolCapWouldBreach`). LP capital cannot leave while it backs net exposure beyond the hard line. With the weekend bound on (`weekend_cap_window_secs > 0`), also require `net_notional·G ≤ α·B'` (`PoolCapWouldBreach`, D43): capital backing the net cannot leave below the weekend bound outside `W` either (inside `W` exits are shut). `B'` counts every remaining share, other LPs' pending shares included, so merely having a request pending does not block another LP's exit; but the room under the line is pool-wide and first come, first served: a claim that lands uses room another matured claim (requested before or after the net was filled) then lacks, and this step does not reserve anything for earlier requests. With production params this line sits 20% above the open cap (0.5× against 0.4×, D45) and binds before the hard line (0.6×): with traders at the cap an exit can take out `B − G·net_notional/α`, about 20% of `B` at a steady price, and beyond that waits until the net shrinks; a price rise, a fall in `B`, a cap-exempt liquidation of the minority side or a later raise of `G` eats the buffer (§3.1 exit-liveness cost, D44, D45).
9. CPI transfer `value` from the vault to the owner (vault PDA signs).
10. Effects: `cash −= value`, `lp_shares −= pending`, `pool.pending −= pending`, `position.shares −= pending`, `withdrawn_cum += value`, and clear the request.
11. `assert_backed(vt.amount − value)`. Emit `LpWithdrawn { owner, shares, value, nav_redeem }`.

### 9.5 Delay and gates, summarised

| Gate | deposit | request | withdraw |
|---|---|---|---|
| Session and fresh oracle | yes | yes (unless settled) | yes (unless settled) |
| `funding_current` | yes | yes (unless settled) | yes (unless settled) |
| Reopen quiet `Q` | blocked | allowed | blocked (unless settled) |
| Settled quiet (D41) | — (paused) | allowed | blocked for `max(profit_close_quiet_secs, Q)` after `settled_at` |
| Pre-close window `W` | allowed | allowed | blocked (unless settled) |
| `adl_required` | blocked | allowed | blocked |
| Drawdown block | — | — | blocked if enabled (unless settled) |
| Hard-net post-check | — | — | yes (unless settled) |
| Delay | — | sets `unlock_at` | `now ≥ unlock_at` (default 4 days) |
| Claim window | — | stores it | lapses after `claim_window_secs` open seconds (unless settled) |
| Value | `NAV_mint`, floor shares | records `requested_value` | `min(now, requested)`, floor |

Deposits stay open inside `W`: capital arriving before a weekend only adds backing, and it cannot leave before the gap because of the delay and the `W` exit block.

### 9.6 Future first-loss tranche (not built)

The tranche would use three things already reserved:
- class byte 1 in the `LpPosition` seeds;
- the LpPool padding, for `junior_shares: u64` and `junior_value: u64`;
- a split function `tranche_nav(NAV) -> (junior, senior)`. The junior tranche absorbs losses first up to `junior_value`, and gains are pro rata or by a coupon.

All NAV reads already go through `pool_view`, so the change would touch only `pool_view` and the share math. The dead shares (§9.1) and the drawdown series would sit in the senior class.

---

## 10. Insurance, treasury, donations and settled markets

### 10.1 `sweep_insurance_excess()`, permissionless

1. `lp_shares > 0` (`PoolHasNoLps`).
2. `x = settled && open_positions == 0 ? insurance_fund : insurance_fund.saturating_sub(insurance_target)`, which must be `> 0` (`NothingToSweep`). A settled market with no positions has nothing left to insure, so the whole fund goes to LPs (D19). That case moves the fund to LPs, not to the protocol, and needs no admin.
3. `insurance −= x` and `cash += x`.
4. Recover pending bad debt first: `r_l = min(x, pending_long)`, then `r_s = min(x − r_l, pending_short)`. Reduce both pending fields and add `r_l + r_s` to `bad_debt_recovered_cum`. Clear the clock of each side whose pending is now 0 (D36).
5. Vest the rest: `add_vesting(x − r_l − r_s, now)` (§10.3).
6. Emit `InsuranceSwept { amount: x, recovered, vested }`.

Fee overflow and stage-2 overflow already go straight to cash (§2.5). The sweep is needed only after `insurance_target` is lowered, at settlement, or when the fund exceeds the target some other way.

### 10.2 `withdraw_treasury(amount)`, admin

1. `amount ≤ treasury` (`InsufficientTreasury`). `destination` has the market's mint (Anchor `ConstraintRaw` otherwise) and is not the vault token account (`VaultInvariantViolated`, D27).
2. CPI from the vault to `destination`.
3. `treasury −= amount`.
4. `assert_backed(vt.amount − amount)`. Emit `TreasuryWithdrawn`.

This is the only route out for protocol revenue. There is no discretionary withdrawal of insurance.

### 10.3 Vesting (D18)

[NI] `add_vesting(pool, y, now)` with `y > 0`: `rem = unvested(now)` (§2.2), `delay = max(withdrawal_delay_secs, 1)`; `vest_amount = rem + y`, `vest_start = now`, `vest_end = now + max(1, floor((rem·(vest_end − now) + y·delay) / (rem + y)))`. The amount-weighted end keeps `unvested(now)` unchanged by the merge and lets dust barely move it; it is floored so a 1-unit donation every second cannot hold the end in place (D26). A merge can slightly speed up a new tranche that joins a large remainder near its end; that does not help the F11 sandwich, because `requested_value` is fixed at request time. The tokens are already in `cash`; only `NAV_redeem` (and `nav_mark`) leave out the unvested part, linearly. `NAV_mint` counts it in full, so a depositor who buys just before a sweep pays for the whole amount, and one who requests just after is paid without it: the 5-day-telegraphed insurance-target cut can no longer be sandwiched (red-team F11). Vesting is a valuation term, not a bucket, so §11 is unchanged.

### 10.4 `donate_to_pool(amount)`, permissionless

Accounts: `donor` (signer), `market` (MARKET), `lp_pool` (POOL), `vault_token_account` (VT), `donor_token_account` (mut, mint), `token_program`.

1. `amount > 0` (`InvalidLpAmount`), `lp_shares > 0` (`PoolHasNoLps`). No session or oracle gate, and **allowed while `adl_required()`**.
2. CPI from donor to vault. `cash += amount`, `donated_cum += amount`.
3. Recover pending first, as in §10.1 step 4; vest the rest (§10.3).
4. `assert_backed(vt.amount + amount)`. Emit `PoolDonated { donor, amount, recovered, vested }`.

This is the protocol's recapitalisation path (red-team F8, F34): it can pay off pending bad debt, lift `NAV_mint` above 0 so deposits reopen, or top up cash so matched winners can close, without minting shares. Tokens sent straight to the vault do not reach `cash` and are not a donation.

### 10.5 `close_settled_position()`, permissionless (D19)

Accounts: `signer`; `market` (mut, MARKET, `settled @MarketNotSettled`); `lp_pool` (POOL); `position` (mut, `market == market`, Open); `position_owner` (Unchecked, `address = position.owner`); `user_account` (USER(position_owner)).

Runs `settle_close` (§4.5 steps 2–7) at `m = exec = settlement_price`, with no referral (the fee all goes to `distribute_fee`). The payout `E ≥ 0` is credited to `user.collateral_balance` and `pool.free` instead of being transferred, so no token account is needed and the bucket step is zero-sum. A position with `E < 0` books its deficit through `absorb_deficit`; a winner is paid through `pay_profit` (`PoolCashInsufficient` until enough losers are closed, so callers close losers first). A winner (`P > 0`) fails `ProfitCloseQuiet` while `now − settled_at < profit_close_quiet_secs` (D35), so the keeper closes the losers the settlement price bankrupted, and books their deficits, before any winner is paid; `settled_at == 0` has no quiet time. Print the "Position closed:" line (D33) and emit `PositionClosed { settled_close: true }`.

Without it, an owner whose position is underwater at settlement never closes (payout 0), its loss stays in `U`, and `NAV_redeem` keeps counting `h·(C+D)` that will never arrive; and a winner who never closes keeps `U > 0` for ever (red-team F15, F26).

---

## 11. Bucket invariant

```
vault_token.amount ≥ free_collateral + position_collateral + market.insurance_fund + cash + treasury   (u128 sum)
```

Every bucket is a u64 (so ≥ 0), and every decrement is `checked_sub` → `BucketUnderflow`.

**[NI] `assert_backed(pool, market, amount_after: u64)`** returns `VaultInvariantViolated` if the check fails. `amount_after` is the pre-CPI `vault_token_account.amount` ± this instruction's transfer, which avoids a `reload()`.

Instructions that assert it (every one that loads the token account):
- `deposit_collateral`
- `withdraw_collateral`
- `close_position`
- `liquidate`
- `lp_deposit`
- `lp_withdraw`
- `withdraw_treasury`
- `donate_to_pool`

The rest only move value between buckets, and each step sums to zero:

| Instruction | Zero-sum step |
|---|---|
| `open_position` | `−(margin+fee) +margin +fee` |
| `venue_fill` | `−(tc+mc) +(tc+mc)` |
| `add_margin` | |
| `settle_position_pnl` | `−loss +loss` |
| `adl` | `−pay −C_x +fee_c split +(C_x+pay−fee_c)` (D22) |
| `close_settled_position` | `−C −P_eff (or +C+ins_paid) +fee split +E` into free |
| `sweep_insurance_excess` | `−x +x` (vesting is not a bucket) |
| `write_off_bad_debt` | none |
| `update_funding` | none |
| `initialize_pool` | none |

Their unit tests assert that sum. Direct token transfers to the vault only widen the inequality (`donate_to_pool` is the way to give value to LPs). The sim asserts **equality**, since nobody there transfers directly.

Per-instruction bucket deltas (Δ = tokens out, shown as negative):
- close: `Δpc = −C`, `Δcash = −P_eff` (profit, after haircuts) or `+(−P_eff)` (loss) or `+C+ins_paid` (deficit), `Δins/Δcash/Δtre = fee split`, `Δins = −ins_paid`, `out = payout + referral_paid`.
- stage 1 partial: `Δpc = r − pen`, `Δcash = −r`, `+ins_part` (via credit), `out = keeper`.
- stage 1 full: `Δpc = −C`, `Δcash = −r`, `+ins_part`, `Δfree = +leftover`, `out = keeper`.
- stage 2: see §6.4; `out = from_col + top`.

---

## 12. Errors (appended after `ApprovalViaCpi`, in this order)

`InvalidPoolParams`, `MarketNotFresh`, `PoolExposureLimit`, `PoolWeekendExposureLimit`, `AccountExposureLimit`, `PoolAdlPending`, `PoolCashInsufficient`, `PoolWithdrawalsBlocked`, `PoolCapWouldBreach`, `PoolCashCapExceeded`, `PoolInsolvent`, `PoolHasNoLps`, `SlippageExceeded`, `InvalidLpAmount`, `InvalidLpClass`, `InsufficientShares`, `WithdrawalPending`, `NoWithdrawalPending`, `WithdrawalDelayNotElapsed`, `NothingToSweep`, `InsufficientTreasury`, `NothingToSettle`, `SettleWouldBankrupt`, `InvalidAdlMode`, `AdlUnauthorized`, `AdlNotRequired`, `AdlWrongSide`, `AdlCandidateNotProfitable`, `AdlScoreBelowFloor`, `AdlOvershoot`, `BadDebtNotStale`, `FundingNotDue`, `ParamsTimelocked`, `NoPendingParams`, `BucketUnderflow`, `VaultInvariantViolated`, `PoolDrawdownFreeze`, `LpFlowWindowClosed`, `FundingStale`, `WithdrawalExpired`, `MarketNotSettled`, `ProfitCloseQuiet` (D23).

That is 42 variants. Existing codes do not move.

## 13. Events (`#[event]`, new)

| Event | Fields |
|---|---|
| `PositionOpened` | position, owner, side, size, oracle, exec_price, collateral, open_fee, net_after, nav_redeem |
| `PositionClosed` | position, owner, mark, exec_price, pnl, funding, solvency_hc, bad_debt_hc, fee, referral_paid, payout, deficit, settled_close |
| `PositionLiquidated` | §6.5 (leads with `position`, D29) |
| `BadDebt` | position, side, deficit, insurance_paid, lp_absorbed, pending_added, pending_long, pending_short, pending_since_long, pending_since_short, window_lp_absorbed, window_ins_paid |
| `PositionPnlSettled` | position, mark, loss, new_collateral |
| `Adl` | §7.2 step 11 (ends with `fee`, D22) |
| `BadDebtWrittenOff` | amount |
| `FundingUpdated` | periods, periods_dropped, rate, step, cumulative_funding, carry_secs |
| `LpDeposited`, `LpWithdrawalRequested`, `LpWithdrawalCancelled`, `LpWithdrawn` | owner, shares, value / amount, nav, lp_shares, unlock_at (request) |
| `InsuranceSwept` | amount, recovered, vested |
| `PoolDonated` | donor, amount, recovered, vested |
| `TreasuryWithdrawn` | amount, destination |
| `PoolParamsProposed`, `PoolParamsApplied`, `PoolParamsCancelled` | params, effective_at (an immediate proposal emits both of the first two, D32) |

NAV and net fields are i128 and `b` is u128, the types `pool_view` computes in. The four LP events' fields are read from the combined row above plus §9.1 and §9.4. `VenueFilled` is unchanged. There is no `AdlFloorSet` (D16).

---

## 14. Compute, stack and the risky spots

CU figures are estimates; assert them on SBF.

| Instruction | Added work | Estimate | Assert |
|---|---|---|---|
| `venue_fill` | +1 account (~0.6 KB Borsh de/ser ~3.5 k), 2 `pool_add` | today + 5 k | < 100 k (existing) |
| `fill_quote` / `match_orders` | +1 pass-through account | today + 6 k | < 200 k (existing) |
| `open_position` | LpPool, `pool_view` (~14 i128 ops + 4 divisions, ~2.5 k), `skew_price`, caps, `touch_peak` | ~45 k | < 120 k |
| `close_position` | `pool_view`, skew, close-path caps (one `in_pre_close_window`), `pay_profit`, waterfall | ~65 k (with 2 token CPIs) | < 150 k |
| `liquidate` | stage logic, two quotes, waterfall | ~60 k | < 150 k |
| `update_funding` | 2 × `eastern_offset` (civil date math) | ~18 k | < 60 k |
| `lp_deposit` / `lp_withdraw` | `pool_view`, `funding_current` and `open_seconds_between` (up to 4 × `eastern_offset`), window checks, 1 CPI | ~45 k | < 100 k |
| `adl` | 2 × `pool_view`, `open_seconds_between` | ~40 k | < 100 k |

Stack (4 KB frames; `scripts/build-sbf.sh` fails on a frame-overflow message, D48):
- Every account in every touched `Accounts` struct is `Box`ed. That includes `OpenPosition`, `DepositCollateral`, `WithdrawCollateral` and `AddMargin`, which are not boxed today.
- `PoolParams` (104 B) is passed by `&`.
- `PoolView` (~128 B) and `Payout` (32 B) are returned by value from [NI] fns.
- `close_position`'s handler is already large. Steps 2–7 live in [NI] `settle_close(..) -> CloseOutcome` (shared with `close_settled_position`), and `liquidate`'s stages in [NI] `liq_stage1` / `liq_stage2`. The close-path cap check is its own [NI] fn.

Risky spots:

| # | Risk | Mitigation |
|---|---|---|
| R1 | The `ClosePosition` `try_accounts` frame (13 accounts, 3 optional) | Box all; fall back to `UncheckedAccount` for the referrer's token account if needed |
| R2 | i128 overflow | Bounded by `MAX_OPEN_INTEREST` (§2.2); everything is checked. The `pending_since_*` weighting and the vesting product are i128 |
| R3 | LpPool is write-locked by deposits, withdrawals and add_margin as well as trades | Per-market serialization. Market was already locked by trades |
| R4 | DST and week-boundary errors in `open_seconds_between` and `seconds_since_open` | Test against brute force (§17.1). They now gate funding, LP claims, ADL decay and the write-off, so an error is no longer cosmetic |
| R5 | Matched winners blocked by `PoolCashInsufficient` when their losers have not paid in | `settle_position_pnl` loop in the keeper; `donate_to_pool` for emergencies. Liquidations never revert on cash (§2.8) |
| R6 | ADL trust | Mode 0 is permissionless, ordered by an on-chain decaying floor. Mode 1 is keeper-signed, bounded by on-chain triggers, targets on `B`, and a timelocked floor; the keeper still chooses within the eligible set, which the `Adl` events let anyone audit |
| R7 | `settle_position_pnl` rewrites `entry_price` | Everything keyed on notional for accounting (house cap, close fee, stage-2 reward) uses `opened_price` (D21). ADL and haircuts measure profit since the last settlement (variation-margin semantics). UI and keeper show `opened_price` |
| R8 | LPs trapped by the hard-net or weekend-bound post-check, pending debt or a drawdown block | Write-off after 24 open hours; donations; the drawdown block is off by default. D45: production `k_net` (0.4×) sits 20% under the weekend withdraw line (0.5×), so with traders at the net cap LPs keep about 20% of `B` of exit room at a steady price. `capNAV` keeps traders off capital pending before the net was filled, but the room is shared first come, first served among all matured claims, so a later requester who claims first can take it. A price rise (whichever the side), a fall in `B`, cap-exempt liquidations of the minority side (a fall with the pool long, a rise with it short: they raise `|net|` up to the hard line with no ADL) or a later raise of `G` eats the buffer, and beyond it exits wait for traders to close: exposure ADL is not run for LP exits (`nTrig` above 0.6× cuts to 0.4×, `wTrig` only inside `W`, `dTrig` at a 15% drawdown). Decided (§18 #15) |
| R9 | Weekend gap on net carried into the close | D43: `k_net ≤ α/G` enforced by `validate` (production 0.4×, 20% under the edge since D45); the weekend cap always on for net-raising opens, closes and mode-0 ADL; `lp_withdraw` keeps `G·net ≤ α·B'`; LP exits shut in `W`; force-reduce `wTrig` as the backstop. The cap counts the trade's own skew-premium shortfall (D44). Hard for net put on under the caps; what can still exceed it (price drift up, `B` falling from deficits beyond insurance or `(1 − h)` of payouts while traders are net losing, cap-exempt liquidations, a later raise of `G` or a params change tightening the bound over open net, rounding) is listed in §3.1 and attributed by size, per weekly close, by the sim (§17.4) |
| R10 | Close-path caps trap a matched trader when the pool is at its cap | Exit through a venue hedge; the frontend explains `PoolExposureLimit` on close. Owner trade-off (§18) |
| R11 | Under-collection when `x_need` truncates, or after a haircut in a slice | The ceil keeps the slice ≥ the exact need; a remainder that is still under MM is re-liquidatable |
| R12 | Unliquidated bankrupt positions hidden from NAV mid-session | Reopen quiet time, `min(now, requested)` and a fast keeper; not eliminated (§2.3) |
| R13 | Drawdown series manipulation | `nps` uses mark NAV less vesting; raising it needs real money given to LPs; lowering it needs real pool losses. The 7-day reset can hide a slow bleed from the lines (owner may lengthen `DD_WINDOW_SECS`) |

---

## 15. Parameters

### 15.1 Production defaults (OIL; launch seed assumed at $1M; production `taker_fee` 50 bps, weekday IM/MM 10%/5%, weekend 20%/10%)

| Param | Default | Rationale |
|---|---|---|
| `k_net_bps` | 4_000 (0.4×) | Owner decisions D43 and D45: 20% under `α/G` = 0.5×, the largest net cap `validate` accepts with the weekend bound on (`k_net·G ≤ α·10_000`). Under the edge, a G-sized gap on net at the cap costs at most 0.8·α = 8% of NAV whether or not the Friday force-reduce succeeds (§3.1), and the gap to the weekend withdraw line is the LPs' exit buffer (below). A 5% adverse day moves NAV by at most 2%; CL's daily σ is about 2–3%. Weekday capacity is 0.4× (0.5× under D43, 1× before it) |
| `hard_net_bps` | 6_000 | 1.5 × `k_net`, as before: the `nTrig` exposure-ADL line (down to `k_net`). With the weekend bound on, the LP-exit block is the weekend line `α·B'/G` = 0.5 × `B'`, which binds first (with the net at the cap the hard line leaves a third of `B`, the weekend line 20%), so the hard-net post-check matters only with `W = 0` (D44) |
| G (`weekend_gap_bps`) | 0 → derived `max(IM, weekend IM)` = 2000 for OIL | The weekend margin already assumes this gap. Historical caveat: roughly +15% at the Abqaiq reopen (Sep 2019), and more than 20% at the Mar-2020 reopen. Verify against CL data |
| `weekend_lp_loss_bps` (α) | 1_000 | Weekend cap, force-reduce target and LP withdraw line = `α/G` = 0.5 × capital, 1.25 × `k_net` (D45). A G-sized gap on net at that level costs 10% of NAV |
| `weekend_cap_window_secs` | 86_400 | `> 0` turns the weekend bound on (D43): the weekend cap on every net-raising open, close and mode-0 ADL at any time, the `lp_withdraw` weekend check and the `validate` rule. From Thursday 17:00 ET (`W`) LP exits are also shut and the force-reduce runs |
| `per_account_cap_bps` | 2_500 | One account ≤ 25% of the net cap (relative to `k_net`, so 0.1 × `capNAV`: $100k of house notional on the $1M seed) |
| `dd_freeze_bps` | 1_000 | A 10% drawdown of per-share NAV (7-day peak) stops new net-raising house risk |
| `dd_adl_bps` | 1_500 | A 15% drawdown enables exposure ADL down to `adl_net_target_bps` |
| `adl_net_target_bps` | 2_000 | Drawdown ADL takes net down to 0.2 × `B`, half of `k_net` as before (D45) |
| `dd_withdraw_block_bps` | 0 (off) | `NAV_redeem` already prices `U`, and the weekend-bound post-check (the hard-net one with `W = 0`) keeps capital behind the net; a drawdown block mostly traps LPs. Owner may enable |
| `adl_exposure_score_floor_bps` | 0 | Exposure ADL may pick any net-side winner; the triggers and targets bound it. Raise to restrict it to leveraged winners |
| `redeem_loss_haircut_bps` (h) | 5_000 | Counts only half of traders' net losses for exits; gap losses can exceed collateral |
| `skew_scale` | 2_500_000_000_000 (2.5M contracts) | 1k contracts of average skew = 4 bps. At the net cap (~5.3k contracts at $75 on $1M) it is about 21 bps, near a CL spread plus a hedge cost. Retune with TVL: `≈ net_cap_contracts / target_impact` |
| `max_impact_bps` | 100 | Bounds a mis-set scale |
| `open_fee_bps` | 5 | Same as the test `taker_fee`. House flow pays open + close |
| `liq_penalty_bps` | 200 | Net of the keeper rebate (160 bps) it exceeds `max_impact + taker_fee` = 150 bps, so self-liquidating never beats a close (validate rule, §1.2); well under MM × backstop (333 bps) |
| `liq_keeper_share_bps` | 2_000 | 40 bps of notional to the keeper, 160 to insurance |
| `backstop_threshold_bps` | 6_667 | Hyperliquid's ⅔ MM |
| `min_liq_slice_bps` | 2_000 | Hyperliquid's 20% |
| `liq_min_remaining_notional` | 100_000_000 ($100) | Avoids dust positions |
| `insurance_target` | 250_000_000_000 ($250k) | `(G − weekend MM) × losing-side gross × ρ` = 10% × $5M × 50%, assuming $10M gross and half at minimum margin |
| `insurance_claim_cap_per_window` | 0 | Insurance sits fully ahead of LPs |
| `lp_loss_cap_window_bps` | 500 | LPs stop pursuing at most 5% of window-start NAV per 24 h loss window; the rest goes to haircuts and ADL (Hyperliquid's post-JELLY Liquidator cap). Written-off debt comes on top (§2.6) |
| `treasury_fee_bps` | 1_000 | 10% of net fees is protocol revenue |
| `withdrawal_delay_secs` | 345_600 (4 d) | Hyperliquid's HLP lock |
| `withdrawal_claim_window_secs` | 259_200 (3 open days) | Must be ≥ window + 5 × quiet + 1 open day = 181_800 (validate, D38). Long enough to claim around the shut windows, short enough that a matured request is not a standing weekend put |
| `lp_reopen_quiet_secs` | 1_800 | 30 min after each session open for the keeper to liquidate gap deficits before LP flows reprice |
| `profit_close_quiet_secs` | 300 | 5 min after each session open in which winners cannot close, so the keeper books gap deficits first and they reach the D13 haircut (D23). `≤ 3_600` |
| `max_pool_cash` | 0 | Owner sets it at launch (compared with LP NAV) |
| Timelock | `max(1 d, max delay + max window + 5 d)` = 10 d | LPs can exit before any change applies, even if it is proposed just before the weekly close (§5) |
| Constants | `ADL_FLOOR_START_BPS` 1_000_000, `ADL_FLOOR_DECAY_SECS` 6 open h, `ADL_WRITE_OFF_SECS` 24 open h, `LOSS_WINDOW_SECS` 24 h, `DD_WINDOW_SECS` 7 d, `LP_DEAD_SHARES` 1e6 ($1) | §1.2 |

**Exit buffer (D45).** `1 − k_net·G/α` = `1 − 4_000·2_000/(1_000·10_000)` = 20% for OIL. It is the share of `B` LPs can still withdraw with traders at the net cap, at a steady price and `B`, before `lp_withdraw`'s weekend check (`G·net_notional ≤ α·B'`) refuses: the open cap holds `net_notional ≤ 0.4·capNAV` and the withdraw line needs `B' ≥ 2·net_notional`. Rationale: under D43 the two lines coincided, so an LP who requested after traders filled the cap could not claim anything until traders closed (§18 #15); the owner chose a parameter buffer over a keeper-run exit ADL, giving up a fifth of the weekday capacity. The buffer is not a guarantee: a price rise raises `net_notional` whichever the side, a fall in `B` (deficits, the `(1 − h)` of payouts while traders are losing) lowers the line, cap-exempt liquidations of the minority side raise `|net|` up to the hard line with no ADL (so a move against the minority side, a fall with the pool long or a rise with it short, can erase the buffer), and a later raise of `G` shrinks it (§3.1, exit-liveness cost); beyond it, exits wait for traders to close. Nor is it reserved: it is shared first come, first served by every matured claim, and `capNAV` keeps only traders, not other LPs, off capital pending before the fill. `k_net = α/G` (no buffer) is still valid in `validate` and restores the 0.5× capacity at the cost of the exit lock at the cap. A market with a higher `G` keeps the same buffer through `scripts/initialize-market.ts` (`k_net = floor(0.8·α·10_000/G)`, §16.6); a later `update_market_config` that raises `G` shrinks it, and at `G = α/k_net` (25% for production) it is gone.

### 15.2 Neutral values (test harness `PoolParams::neutral()`; reproduce today's numbers)

- `skew_scale = 0`, `open_fee_bps = 0`, `treasury_fee_bps = 0` and `insurance_target = u64::MAX`, so every fee goes to insurance.
- Every cap that could bite an existing test is off or wide: `k_net_bps = 100_000` (10×, so $10M of net on the $1M seed, including on closes), `hard_net_bps = 0`, `per_account_cap_bps = 0`, `weekend_cap_window_secs = 0` (no weekend cap, force-reduce or `W` exit gate).
- The drawdown lines are 0 (off). `backstop_threshold_bps = 10_000` (stage 2 only). `liq_penalty_bps = 0`.
- `lp_loss_cap_window_bps = 10_000`, `withdrawal_delay_secs = 0`, `withdrawal_claim_window_secs = 0` (never lapses), `lp_reopen_quiet_secs = 0`, `profit_close_quiet_secs = 0`.
- `adl_net_target_bps = 2_500`, its value before D45 (production's is 2_000 since D45), so neutral did not change.
- Every other field takes its production value.

The harness seeds $1M of LP cash in the same transaction as `initialize_pool` (so `$1M − LP_DEAD_SHARES` of shares go to the protocol key). Tests that need other params pass them to `start_with(params)`, which applies them before the seed (immediately, since `lp_shares == 0`), or propose them and warp past the timelock.

---

## 16. Impact outside perps-core

Account counts below were checked against HEAD: `ClosePosition` 12, `VenueFill` 16, mm-registry `FillQuote` 19, orderbook `MatchOrders` 20, `Liquidate` 10, `OpenPosition` 6, `DepositCollateral` / `WithdrawCollateral` 7, `AddMargin` 4.

### 16.1 mm-registry `fill_quote`

- Add `/// CHECK: perps-core validates` `#[account(mut)] pub perps_lp_pool: UncheckedAccount` after `mm_position`. Accounts go from 19 to 20.
- The CPI struct gains `lp_pool: a.perps_lp_pool` (last, matching §4.8), so the VenueFill CPI carries 17 metas.
- The transaction grows by about 32 bytes, still under 1232.
- CU about +6 k, still under the 200 k assert.
- The tests' `fill_ix` adds the PDA, and `Env` initialises and seeds the pool for its market.

### 16.2 orderbook `match_orders`

Add `perps_lp_pool` (mut, Unchecked) after `maker_position`. Accounts go from 20 to 21, and the CPI carries 17 metas. CU about +6 k (< 200 k). The tests are the same as for mm-registry.

### 16.3 SDK (`sdk/mm-registry`)

- The hand-coded offsets do not change: Market 104/144/152/247 and Venue 160..165.
- `sdk_layout_is_pinned` gains one assertion, the Market `funding_carry_secs` at 283. The SDK does not read it.
- Add `findLpPoolPda(market, perpsProgramId)` (`[b"lp_pool", market]`) to `pda.ts`, and `perpsLpPool` to the `.accounts({...})` of `buildFillQuoteIx`.
- `FILL_COMPUTE_UNITS` is unchanged (300 k).
- `approve_venue` / `update_venue_approval` are unchanged.
- Pool-param builders gain `profit_close_quiet_secs` (Borsh, last field, D23). LpPool offsets after `params` move: `pending_params` 440, `bump` 544.
- Since D47/D51: Market is 434 bytes; `decodeMarketOracleFields` reads `pyth_feed_id` (295), `oracle_program` (327) and the pending rotation (359..431), and `PerpsMarketInfo` carries the feed id and owner. The fill still passes `pyth_price_feed` (104).

### 16.4 Keeper (`backend/keeper`)

- `MarketData` appends `funding_carry_secs: u32`. Its Borsh prefix decode keeps working.
- `calculate_margin_ratio` keeps entry notional for logging. `is_liquidatable` mirrors §6.1 (`equity·10_000 < mmr·N_mark`).
- The `liquidate` ix appends `lp_pool`, so it has 11 metas. Liquidations no longer fail on pool cash (§2.8), so the liquidator needs no new error handling.
- After a stage-1 slice the position stays Open, and the loop re-checks it next pass.
- Funding: due when `open_seconds_between(..) + carry ≥ interval`. `open_seconds_between`, `seconds_since_open` and `funding_current` go into perps-core `session.rs`, which the keeper already `#[path]`-includes.
- **Pool math.** The keeper cannot depend on perps-core (outside the workspace; anchor 0.29 when this was written, the solana-v3 client crates since D48). It `#[path]`-includes the new dependency-free `pool_math.rs` (§2.7) for `pool_view_raw`, the triggers, `adl_score` and `adl_floor_now`.
- **Layout pin.** New `LpPoolData` Borsh mirror of §1.2 (prefix through `pending_since_short`, which follows `bump` at 545 (D36); `PoolParams` is 104 bytes, D23). The pin works like Market's today: a perps-core unit test serialises a fully populated `LpPool` with distinct field values and asserts it equals the checked-in fixture `programs/perps-core/tests/fixtures/lp_pool.bin`; the keeper's `keeper_pool_layout_is_pinned` decodes the same fixture into `LpPoolData` and asserts each field. The same pattern covers `LpPosition`.
- New loops:
  - `adl.rs`: mode 0 whenever `adl_required()`: pick the highest-score opposite-side winner whose score ≥ `adl_floor_now`, and call `adl(0, ..)`. Slice math mirrors §7.2: slice-only funding and the remainder-equity fallback (D24), net-raising slices sized within the close caps (D25; `close_cap_room` uses the always-on weekend cap while `W > 0`, D43, and bisects below its closed form for the slice's premium shortfall at the mark, D44), and the slice's close fee (D22). Mode 1 when a trigger holds: rank net-side winners above the param floor and size `x` to land on `T_net`;
  - `settle.rs`: when `cash` < (Σ positive P of positions likely to close), or every 15 min, `settle_position_pnl` the largest losers; on a `PoolCashInsufficient` close seen in logs, settle then retry;
  - `close_settled_position` on every open position of a settled market, losers first;
  - `sweep_insurance_excess` when excess > 0, or when settled and empty;
  - `write_off_bad_debt` when stale.

### 16.5 Frontend

- Regenerate `target/idl/perps_core.json` and `target/types/perps_core.ts` (`scripts/gen-idl.sh`; `--sync` refreshes the frontend's committed copy in `frontend/src/idl`, D49) together with the program; this is a redeploy.
- `useTrading.ts`:
  - deposit, withdraw, open, close and add_margin add `lpPool`;
  - open passes `acceptablePrice` (exec ± slippage tolerance);
  - close passes `acceptablePrice`; `PoolExposureLimit` / `PoolWeekendExposureLimit` on a close gets an explanation and the venue-hedge exit (§3.1); `ProfitCloseQuiet` shows the time left (D23).
- `useTriggerOrderMonitor.ts` (TP/SL): pass a default `acceptablePrice` from a configurable slippage bound (e.g. 1% beyond `max_impact_bps`), never 0. A take-profit that hits `ProfitCloseQuiet` retries after the quiet time (D23).
- `useOnChainMarket.ts`: the next funding time is no longer `last + interval`. Port `open_seconds_between` to `utils/session.ts`, test it against `SESSION_VECTORS`, and compute the time at which open seconds since `last_funding_time` plus `funding_carry_secs` reach the interval. Otherwise funding shows as overdue every weekend and daily break.
- `OrderEntry` shows the skew-impact exec price, the open fee, and the net / weekend / per-account headroom (from LpPool + Market); the weekend headroom applies at any time while `W > 0` (D43) and is shown when it is the tighter one (e.g. after a weekend-IM raise). Its `liquidationPrice` uses the mark-notional rule of §6.1.
- `positionHealth.ts` uses mark-notional liquidation, the backstop line and the partial-liquidation note.
- `PositionTable` shows `opened_price`.
- `LpVault.tsx` / `useVault.ts`:
  - re-point from prop-amm to `init_lp_position`, `lp_deposit`, `lp_request_withdrawal`, `lp_cancel_withdrawal` and `lp_withdraw`;
  - show `NAV_redeem / share`, the pending request, `requested_value`, `unlock_at`, the claim deadline (lapsed requests offer cancel + re-request), the active gate (`W`, `Q`, the settled quiet time (D41), funding not current, pending debt, drawdown), and a claim that the post-checks would refuse (`withdrawRefusal`: cash, the hard-net line, or the weekend bound `G·net ≤ α·B'` with what to do, D43, and that exposure ADL does not free it, D44). Since D45 the vault also shows the **LP exit room now** (`withdrawRoom`: the largest payout the post-checks let out, which is the exit buffer with traders at the cap), and the weekend-bound notice gives that amount, states the buffer from the live params (`exitBufferBps`: `1 − k_net·G/α` at the market's current `G`, or that there is none when `k_net·G ≥ α`), says the room is shared first come, first served and that a smaller re-request waits the full delay again, says the rest waits until traders close, and with no room left says only that exits wait until traders close or the price moves back;
  - before an LP flow, decide on the funding crank from a freshly fetched `Market` at the cluster's clock (Clock sysvar), not from a periodic snapshot and the browser clock: any trade cranks inline (D40), so a stale snapshot makes the client prepend an `update_funding` that fails `FundingNotDue`, or omit one the flow needs (`FundingStale`). When a period is due the crank goes in its own transaction, with `FundingNotDue` ignored; within a few seconds of the due time the client waits until it is clearly due (`frontend/src/utils/fundingCrank.ts`, third review).
- `AdminPanel` adds `max_open_interest` (now editable; 0 keeps it), propose/apply/cancel pool params, max pool cash, the ADL keeper, donations and the treasury withdrawal. It shows insurance, cash, treasury, vesting, drawdown and pending bad debt.
- Add `lp_pool` and `lp_position` to `utils/pda.ts` (+ tests).
- Since D47/D51: the oracle account is a Pyth Receiver `PriceUpdateV2` (`utils/oracleAccount.ts`); transactions pass the market's pinned `pyth_price_feed`, and the configured feed is the push oracle's shard-0 account of the commodity's feed id (`config/pythFeeds.ts`).

### 16.6 TS suite (`tests/oil-perps.ts`)

- After `initialize_market`, call `initialize_pool(neutral params)` and the seed `lp_deposit` in the same transaction.
- `deposit` / `withdraw` add `lpPool`.
- The pure-calculation tests switch:
  - "detect liquidatable" uses mark notional;
  - the "Liquidation calculations" tests that mirror `liquidation_payout` (including the step where leftover collateral goes to the fund before the deficit) are rewritten for §6.3–§6.4: the in-force MMR, `opened_notional`, the stage-2 split with overflow to cash, and stage-1 slice and penalty cases.
- "define all error codes" adds the 42 variants. `scripts/initialize-market.ts` adds `initialize_pool`, `init_lp_position` and the protocol seed `lp_deposit` to the `initialize_market` transaction, and fixes the "10M USDC" comment (D6). The pool params it sends are `PoolParams::production()`, except that a commodity whose `G = max(IM, weekend IM)` exceeds 20% gets `k_net = floor(0.8·α·10_000/G)` (and `hard_net`, the drawdown-ADL target at 1.5× and 0.5× of it), so the params stay valid once `applyRiskConfig` sets the weekend IM (D43) and keep production's 20% exit buffer (D45; `EXIT_BUFFER_BPS`). Flooring keeps `k_net·G` under the edge and `adl ≤ k_net ≤ hard_net`: SILVER and COPPER (G 24%) get 3_333 / 4_999 / 1_666, NATGAS (G 30%) 2_666 / 3_999 / 1_333.

### 16.7 Sim (`sim/`)

- `chain.rs`: `initialize_pool(production defaults)` plus the protocol's $1M seed in one transaction at t0. The sim's pool params override `withdrawal_delay_secs` to 1 day so the unwind fits (below).
- `model.rs`:
  - pool aggregates, `pool_view`, `skew_price`, fee split, `pay_profit` with both haircuts, close-path caps;
  - staged liquidation, windowed waterfall, ADL with the decaying floor, `open_seconds_between` funding catch-up, LP share math with dead shares, vesting, drawdown;
  - `model::close` switches the close fee from floor to `ceil(opened_notional·taker_fee, 10_000)` (sim sizes do not divide evenly, `model.rs:139`);
  - `model::liquidation` uses the in-force MMR for the reward (`model.rs:172-174`);
  - slice-only funding with the remainder-equity fallback (D24), the mode-0 ADL fee (D22), the profit-close quiet time (D23) and the weighted vesting end (D26).
  - Share code with the chain through `pool_math.rs` and `session.rs` (`#[path]`) where possible.
- `predict.rs` gets every new rejection: the §12 codes per instruction, `FundingNotDue` replacing `InvalidMarketConfig`, `FundingStale` / `LpFlowWindowClosed` / `WithdrawalExpired` for LP flows, and `acceptable_price`.
- `keeper.rs`:
  - `crank_funding` (`keeper.rs:222-243`) gates on `open_seconds_between(last, now) + carry ≥ interval` instead of wall-clock elapsed, checks `cumulative == before + n·step` and the new carry, and takes `periods_dropped` from the `FundingUpdated` event instead of `elapsed/interval − 1` (which counts closed weekend hours and would fail §17.4 in every run);
  - the ADL, settle_pnl, settled-close, sweep and write-off loops.
- `agents.rs`:
  - public LP agents (deposit, request, withdraw, cancel; some try to exit around gaps, which the gates must stop);
  - house traders pass `acceptable_price`;
  - the stress scenario's house gapper sizes its open to its per-account headroom, as the order entry shows it (D45 lowered that cap to 0.1 × `capNAV`, under what a 20× gapper can margin; refused outright it carried nothing over the gap and stress seed 2 lost its LP-absorbed deficit).
- `adversary.rs` (`adversary.rs:512` and the new probes):
  - **self-cross then close one leg** before the weekend (two wallets, one venue pair, close the short leg): the close must fail `PoolExposureLimit` / `PoolWeekendExposureLimit` past the cap (red-team F1);
  - LP sandwich around a liquidation, a sweep, a donation and a funding crank;
  - a non-keeper mode-1 `adl`, and a mode-0 `adl` below the current floor;
  - a free withdrawal request followed by a mode-1 `adl` (must fail `AdlNotRequired` when only `capNAV` moved);
  - pushing the net to the cap and the weekend cap, inside `W` and outside it (D43: with the weekend IM raised so the weekend cap is the tighter), and an LP claim with the net at the weekend cap (`PoolCapWouldBreach`);
  - pushing the net to the production net cap (production `G`) and claiming inside the exit buffer (D45): lp-buffer's matured claim must land, and the room the model computes must sit exactly on the weekend line and, while the net is within `k_net·B`, be at least the promised `(1 − k_net·G/α)·B` (like the weekend probes, it first closes lowering legs when the net is over `k_net·B` after a price move or a fall in `B`, and skips the claim if it is still over);
  - a self-liquidation-to-dodge-impact probe: expected outcome is a net loss versus closing (validate rule, §1.2);
  - a withdrawal during `adl_required`, inside `W`, inside `Q`, and after the claim window;
  - an opposite-side winner closing while pending > 0 (must be haircut).
- `ledger.rs`: drop `vault_basis`. Invariant 9 now also accepts changes from partial liquidation, `adl`, `settle_position_pnl` and `close_settled_position`.
- `invariants.rs` adds:
  - **2a** `bucket_identity`: vault == Σ buckets exactly, and every bucket == the model's;
  - **2c** `pool_aggregates`: the aggregates equal Σ over open positions exactly, `|U_eng − U| ≤ slack`, and `NAV_mint ≥ cash − U_eng ≥ NAV_redeem`;
  - **2** `solvency`, **fatal by default**: `vault − free − Σ max(0, C + φ·P⁺ + P⁻)` (φ = the solvency-haircut factor) `≥ 0` **or** `adl_required()`;
  - **7**, at the end: all free collateral and all non-dead LP shares withdraw after the keeper clears ADL. The run's unwind phase warps into open sessions past `withdrawal_delay_secs` (1 day in the sim), outside `W` and `Q`, and within the claim window.
  - `EXEMPT` needs no `PoolCashInsufficient` entry, because liquidations no longer revert on cash.
- `main.rs`: `--strict-solvency` is the default; the flag is accepted as a no-op. Add `--lenient-solvency`. `README` is updated.
- `report.rs`: NAV path, drawdown, `capNAV` and `B` use, haircuts by kind, `PoolCashInsufficient` count, ADL events by mode and trigger, `periods_dropped` (should be 0), and per weekly close: `net_notional_at_close`, `G·net/B`, carried-in excess over `α·B/G` with the exceptions that explain it (D43, §17.4), and deficits beyond insurance; `weekend_bound_breaks` totals the steps that broke the bound by exception.

---

## 17. Test plan

### 17.1 Unit (`cargo test -p perps-core --lib`)

1. LEN pins:
   - `LpPool::LEN == 601 == 8 + ser + 56`, `LpPosition::LEN == 166 == 8 + ser + 32`;
   - `Market` padding 11, `Position` padding 24, `UserAccount` padding 8;
   - `PoolParams` serializes to 104 bytes;
   - byte offsets from §1.2 and §1.3, and the `lp_pool.bin` / `lp_position.bin` fixtures (§16.4).
2. Aggregates:
   - property test over random matched pairs plus unmatched positions with random entries, indices and marks;
   - matched-only gives `U == 0` exactly and `Σ U_eng == 0`;
   - in general `|U_eng − U| ≤ slack` and `NAV_mint ≥ cash − U_eng ≥ NAV_redeem`;
   - `pool_replace` round-trips.
3. `skew_price`:
   - scale 0 returns the oracle;
   - buys round up and sells round down (±1 unit);
   - the clamp;
   - an open followed by a close in the same state nets to 0;
   - the reduce-skew sign.
4. Fees: `distribute_fee` / `credit_insurance` at, below and above the target.
5. `absorb_deficit`: each layer exhausted in turn; the window: two deficits in one window share one LP cap and one insurance cap, a deficit after `LOSS_WINDOW_SECS` rolls it; the debt-weighted per-side `pending_since_*` (large-after-small moves it to ≈ now, dust barely moves it, the other side's clock never moves; D36).
6. `quote_profit` / `apply_payout`: solvency haircut only when `U > cash`; bad-debt haircut on the correct opposite side; `allow_short` true and false; a quote has no side effects.
7. Liquidation:
   - the `x_need` grid (`im`, `p`, `E`, `size`): post-slice `E − pen ≥ IM·N_rem` − 1 unit;
   - min slice, min remainder, the `im ≤ p` fallback;
   - the penalty cap; a slice whose haircut makes `C' ≤ 0` becomes a full stage-1 close.
8. `is_liquidatable` on mark notional, including a short at a loss where the entry and mark rules disagree.
9. `open_seconds_between` and `seconds_since_open`:
   - brute force at 60 s steps across 2026 (both DST changes) against `is_open`;
   - additive over random splits;
   - `SESSION_VECTORS`.
10. Funding catch-up: `due`, `n`, `carry`, the 168 cap including exactly 168 due with a remainder (carry kept, 0 dropped) and 169 due (carry 0, 1 dropped), and i64 saturation.
11. `PoolParams::validate`: every bound, ±1, including the claim-window and self-liquidation rules, and the D43 rule `k_net·G ≤ α·10_000` with the bound on (5_000 accepted and 5_001 refused at production; an overridden `weekend_gap_bps`; a derived `G` from a higher weekend IM; off with `W = 0`).
12. Every zero-sum bucket step listed in §11.
13. Drawdown: `touch_peak` raises the peak, resets after `DD_WINDOW_SECS`; deposits and withdrawals never lower `nps`; a sweep does not move `nps`.
14. Vesting: `unvested` is linear, merges on a second `add_vesting` at the weighted end, dust donations do not restart it (D26), and `NAV_mint − NAV_redeem` widens by exactly the unvested part.
15. `adl_floor_now` at 0, half and full decay, and with settled wall-clock time.
16. `opened_notional` telescopes: random partial slices sum to `opened_notional(size)`.
17. Slices (D24): `slice_funding` telescopes; a partial ADL cannot pay out the remainder's funding, and keeps its basis; a stage-1 slice settles only its share.
18. Deficit ADL (D22, D25): the fee per slice, capped by `C_x + paid`, through `distribute_fee`; mode 1 owes none; a net-raising slice meets the close caps.
19. Profit-close quiet (D23): a winner fails `ProfitCloseQuiet` inside the quiet time and passes at its end; a loser, a 0 setting and a 24/7 schedule are never gated; a settled market is gated from `settled_at` (D35).

### 17.2 Integration (in-process)

Harness changes, **in both harnesses**:
- `tests/support/market_env.rs`: `start()` sends `initialize_pool(PoolParams::neutral())`, `init_lp_position` and a protocol `lp_deposit` of $1M in the `initialize_market` transaction. `start_with(params)` does the same with other params.
- `programs/perps-core/tests/integration.rs` has its own `Env` (`Env::new` / `bare` / `init_market` / `init_market_with` / `init_market_as`, lines 163–330, with OIL and GOLD markets). `init_market_as` gets the same pool initialisation and $1M seed for each market it creates. Its local builders for `deposit`, `withdraw`, `open`, `close`, `liquidate` and `update_funding` (lines 363–459 and 770–800) add `lp_pool` at the positions of §4, and `acceptable_price = 0` for open and close. `user_accounts_cannot_be_used_in_another_market` asserts the GOLD vault holds 0: it now holds the GOLD seed, so it asserts `seed` instead (or GOLD is created with `init_market_unseeded` for that test).
- mm-registry and orderbook test `Env`s seed the pool of the perps market they use (§16.1–16.2).
- New helpers:
  - `lp_pool_pda`, `lp_position_pda`, `pool(base)`;
  - `lp_deposit`, `lp_request`, `lp_withdraw`, `lp_cancel`, `donate`;
  - `set_pool_params` (through `start_with`, or proposed and warped past the timelock);
  - `adl`, `settle_pnl`, `close_settled`, `sweep`.
- `open_ix` and `close_ix` gain `acceptable_price = 0` and `lp_pool`. `close_ix` passes the program id for absent optional referral accounts, with `lp_pool` at account 13.
- `assert_conservation` becomes:
  - the bucket identity (exact);
  - aggregates == Σ open positions;
  - and, when every pair is intact, `U == 0`.
- Call sites of `MarketConfigParams` (`integration.rs` `weekend_config()` / `unpaused()`, `market_env.rs` `cme_config()`, the `update_market_config` unit tests, `sim/src/adversary.rs`) need no change, because `max_open_interest = 0` keeps the current cap (§4.12).

Existing assertions that change:
- `weekend_margins_liquidate…` reward: 375_000 → **750_000** (in-force MMR 10%).
- venue `worked_example` is unchanged, because `margin_ratio` keeps entry notional.
- Any absolute vault balance includes the $1M seed.
- `settlement_closes_positions_when_the_oracle_breaks` still expects `liquidate → MarketSettled`; add a `close_settled_position` step to it.
- Every other existing number is unchanged under the neutral params (D10).

New tests:
1. `initialize_pool`:
   - admin only; requires a fresh market (`MarketNotFresh`); **succeeds after someone transfers 1 unit to the vault token PDA** (red-team F14);
   - every trading instruction fails `AccountNotInitialized` without a pool; `open_position` and `venue_fill` fail `PoolHasNoLps` with a pool but no seed.
2. LP deposits:
   - the first deposit mints `amount`, of which `LP_DEAD_SHARES` are unowned; `amount ≤ LP_DEAD_SHARES` fails; later deposits floor;
   - `min_shares_out`; `max_pool_cash` against NAV (a matched loser settling in does not use up the cap);
   - paused, closed session, stale oracle, `Q`, funding not current (`FundingStale`, then crank + deposit in one transaction succeeds), `adl_required`, `PoolInsolvent`.
3. LP withdrawals:
   - the delay, ±1 s, read from `unlock_at` (a params change raising the delay does not move a pending request);
   - the claim window: withdraw at the last open second succeeds, one second later `WithdrawalExpired`; cancel and re-request;
   - `W` and `Q` block (`LpFlowWindowClosed`); deposits inside `W` succeed;
   - `min(requested, now)` when NAV rises and when it falls;
   - `PoolCashInsufficient`, `PoolCapWouldBreach` (with another LP's pending shares still counted in `B'`; and from the weekend bound `G·net ≤ α·B'`, not with `W = 0` or once settled, D43), the drawdown block when enabled;
   - the exit buffer (D45, `production_exit_buffer_at_the_net_cap`): production params at G 20%, traders filled to the net cap by real house opens (one more contract refused `PoolExposureLimit`); a request made before the fill is paid in full (less its share of the per-position slack); after the fill, a claim of exactly `B − 2·net_notional` (20.00% of `B`) lands and one share more is refused `PoolCapWouldBreach`, after which the pool sits on the weekend line;
   - exit from a settled market with no session.
4. `matched_pairs_leave_nav_unchanged`:
   - venue fills at band edges, funding cranks, prices up and down;
   - `NAV_mint` and `NAV_redeem` constant within slack;
   - after both legs close, cash is back to its start value plus the fee overflow, and insurance is +fees.
5. `orphan_leg_is_valued`: close the winner first and NAV is unchanged; close the loser first and NAV is unchanged.
6. `settled_loser_does_not_haircut_winner` (red-team F3): a matched pair, the loser is settled with `settle_position_pnl`, then the winner closes with **no** haircut and LP withdrawals stay open.
7. House open:
   - the exec price and margin with impact (adverse top-up), the open fee, `acceptable_price`;
   - `SlippageExceeded` in both directions.
8. Caps:
   - net cap at the limit and +1 unit (`PoolExposureLimit`);
   - a reducing open beyond the cap succeeds;
   - weekend cap inside and outside the window (always on while `W > 0`, off with `W = 0`, D43), with G derived (20%) and overridden, and tightening on its own when the weekend IM is raised after the params were applied;
   - per account on `opened_notional`; after `settle_position_pnl` on the user's position the headroom is unchanged (red-team F10, F17, F30);
   - frozen while pending debt (`PoolAdlPending`) or in drawdown (`PoolDrawdownFreeze`);
   - pending withdrawals shrink `capNAV` for opens but not `B` for closes.
9. **Self-cross bypass** (red-team F1): two wallets cross a 100k-contract venue pair, then one closes its leg: rejected with `PoolExposureLimit` (and `PoolWeekendExposureLimit` wherever the weekend cap is the tighter, inside or outside `W`, D43); hedging with an opposite venue fill succeeds; closing the leg whose close lowers `|net|` succeeds.
10. Close:
    - skew impact on close; the solvency haircut when `U > cash`;
    - `PoolCashInsufficient`, then `settle_position_pnl` on the loser, then the close succeeds;
    - `settle_position_pnl` rejections (`NothingToSettle`, `SettleWouldBankrupt`) and its equity invariance;
    - settle −$300, then close at +$10 with a $1 fee: `position.realized_pnl` and `user.realized_pnl` both −$291 (red-team F31).
11. **Opposite winner closes while pending** (red-team F4, F25): a gap deficit books pending on the long side, an opposite short winner closes before any ADL, its payout is cut by `min(pending, P_eff)`, pending falls by that amount and `bad_debt_recovered_cum` rises.
12. Liquidation stage 1:
    - the slice size;
    - the owner's leftover goes to free collateral, and a later `withdraw_collateral` of it all succeeds (red-team F18);
    - keeper and insurance shares;
    - the post-slice ratio is at least IM − 1 unit;
    - a second call gives `NotLiquidatable`;
    - min slice and min remainder go to a full stage-1 close.
13. Liquidation stage 2:
    - the residual goes to insurance, overflowing to cash at the target;
    - a gap deficit runs insurance → LP cap → pending, `adl_required` is set, and LP deposits and withdrawals are frozen;
    - thirty bankrupt positions in one window share one LP cap (red-team F2);
    - free-collateral withdrawal still works;
    - a liquidatable position with positive realized PnL and `cash` drained to 0 still liquidates, with `cash_hc` emitted (red-team F21).
14. ADL:
    - mode 0 is permissionless; below `adl_floor_now` fails `AdlScoreBelowFloor`, and the same call succeeds after the floor decays;
    - deficit mode clears pending through the haircut;
    - exposure net-mode, drawdown-mode and weekend force-reduce, each stopping at its target (`AdlOvershoot` past it);
    - an LP's withdrawal request alone does not make mode 1 eligible (red-team F7, F33);
    - `AdlWrongSide`, `AdlCandidateNotProfitable`, `AdlUnauthorized` (mode 1 by a non-keeper);
    - a settled market allows mode 0 only;
    - each mode-0 slice pays its close fee, partial and full, into insurance; mode 1 pays none (D22);
    - a deficit ADL of one leg of a self-cross is capped (D25);
    - write-off before and after 24 open hours; a Friday deficit cannot be written off on Saturday (`BadDebtNotStale`); a large second deficit restarts the clock almost fully (red-team F27).
15. Funding:
    - 3 missed periods are charged once;
    - Friday close to Sunday reopen charges 0;
    - the daily break is excluded;
    - the carry crosses cranks; the cap is 168, and exactly 168 due keeps the remainder;
    - `FundingNotDue`;
    - the pool's carry lands in cash on close.
16. Insurance, treasury, donations:
    - the sweep moves the excess, recovers pending, and vests the rest; a deposit just before a sweep and a request just after earns nothing from it (red-team F11);
    - `PoolHasNoLps`, `NothingToSweep`;
    - settled and empty: the whole fund sweeps;
    - `donate_to_pool` while `adl_required` clears pending and reopens LP deposits; with `NAV_mint ≤ 0` it brings deposits back (red-team F34);
    - the fee split; `withdraw_treasury` is admin-only and bounded.
17. Settled market (red-team F15, F26): an underwater position and a never-closed winner are both closed by `close_settled_position` from a third party; `U == 0` after; the last LP withdraws in full; insurance sweeps whole.
18. Pool params: applied immediately before the seed; timelocked with LPs at `max delay + max window + 5 d`; a proposal that cuts the delay still waits for the current one; apply is permissionless after the timelock; cancel; re-validated at apply.
19. `bucket_identity_property`: 200 random actions across all instructions with exact identity after each one.
20. `max_open_interest` editable via `update_market_config`; `0` keeps it; `unpaused()` still unpauses (red-team F19, F32).
21. Profit-close quiet (D23): at the Sunday reopen after a gap the bankrupt loser's winner fails `ProfitCloseQuiet`, a losing close and the liquidation go through, and after the quiet time the winner repays the pending deficit; the daily reopen also gates; a settled market gates from `settled_at` (D35).
22. Regression: every existing perps-core, venue, mm-registry and orderbook test, with only the listed number changes.

### 17.3 SBF (`scripts/build-sbf.sh` then `SBF_OUT_DIR=$PWD/target/deploy cargo test -p perps-core -p mm-registry -p orderbook --test integration`)

- The build must show no frame-overflow lines ("Stack offset" / "overflows the maximum allowed frame space", D48) for any program.
- The CU asserts in the §14 table use `simulate`.
- Existing asserts: `fill_compute_units` < 100 k, `fill_quote_compute_units` < 200 k, `match_compute_units` < 200 k.
- A `liquidate` worst case: stage 1 at the min-slice boundary whose haircut forces a full close, plus a stage-2 deficit that rolls the loss window.
- `lp_withdraw` worst case: settled = false, CME schedule, claim-window and funding checks (4 `eastern_offset` calls).

### 17.4 Sim

- Default (strict) runs must exit 0:
  - `normal`, `stress` and `adversarial` × seeds 1–5, native and SBF;
  - `--days 10` stress across two weekends.
- The report must show 0 `periods_dropped` and 0 silent bad debt.
- **Weekend criterion (restated; red-team F2, F35, D43).** At each Sunday reopen with a gap of at most G, the per-share NAV drawdown across the weekend must satisfy `drawdown ≤ G·net_notional_at_close/B_at_close + (deficits beyond insurance and ADL recovery)/B_at_close`, up to rounding. The sim checks it as the fatal invariant `15_weekend_criterion` (third review), including weekly closes the unwind's warps cross (at a zero gap), and CI runs one sim (`stress`, seed 1, 5 days) that spans a weekly close and its reopen. Since D43 the same invariant also requires, fatally:
  - at every weekly close, `carried_excess = net_notional_at_close − α·B/G` is 0 unless documented exceptions of §3.1 cover it, **by size**. The sim tracks `E = G·net_notional − α·B` across every transaction and tick. From the last moment the bound held, each step that raises `E` above `max(E_prev, 0)` is covered by what it shows: wholly by `settled`, `g_raised`, `params_tightened` (W on, α down, G up through `apply_pool_params`), `price_rise` (a pure price tick: net, cash and bad debt unchanged) or `liquidation` (one that raised `|net|`); in part by `redeem_haircut_payout` (`α·` the growth of the loss `NAV_redeem` leaves uncounted), `deficit` (`α·min(fall of the full-count NAV, Δbad_debt_cum)`) and `rounding` (`α·64 + 2G` per step). Whatever is left is recorded under `other:<label>` with its size. A weekly close with an excess fails if no documented exception was seen **or any step is uncovered**, however many documented ones there were (D44, review of D43); each close's record lists `excess_explained_by` and `excess_unexplained_steps` (sizes in `G·nn − α·B` units), and the report totals `weekend_bound_breaks`;
  - every landed net-raising transaction other than a liquidation or settled close keeps `G·net_notional_after ≤ α·min(B_before, B_after + Δhc + 64)`: after the trade, short of the `B` only the redeem haircut took (§3.1 item 2) and rounding, since the skew-premium shortfall is in the cap (D44); and every landed `lp_withdraw` keeps `G·net_notional ≤ α·B'`.
  - The adversary probes the bound outside `W` (`selfcross_close_weekend_bound`, `house_open_weekend_bound`, required) and inside it (`selfcross_close_weekend_cap`, `house_open_weekend_cap`): it first raises the weekend IM to about 36% with `update_market_config` (G above `α/(0.7·k_net)`, and 10% above `α·B/(k_net·capNAV)` for the requests pending then), so the always-on weekend cap must tighten below the net cap by itself, and restores it before the probe returns. `check_open_caps` tests the net cap on `capNAV` first, and with `k_net` 20% under `α/G` (D45) net put on before LPs requested can already be over `0.4·capNAV`; before the house open the probe therefore closes legs of its pair on the side that lowers `|net|` (never capped) until the net is back under the net cap on `capNAV`, so the weekend cap is what answers, and requires `PoolExposureLimit` only if those legs run out. The net-raising closes are prepared the same way: `check_close_caps` tests the net cap on `B` first, so when the net is already over `k_net·B` or within a leg of it (after a gap or a fall in `B`) the pair is built larger by the excess and its lowering legs are closed first, until two legs ($90k) fit under the net cap, so a raising close cannot cross both caps. `lp_withdraw_weekend_bound` (lp-bound's matured claim with the net at the cap, refused `PoolCapWouldBreach`) needs a claimable request with no debt pending outside `W`; in a two-day run the forced gap's pending debt covers that time, so it is reached only in longer runs. The other side (D45): `lp_withdraw_within_buffer` pushes the net to the production net cap at the production `G` and claims lp-buffer's matured $50k request inside the exit buffer; it must land, and `exit_buffer_at_net_cap` checks the model's room sits on the weekend line and is at least `(1 − k_net·G/α)·B`. It needs the same calm, claimable state, so it too is reached only in longer runs (adversarial seed 1 over 5 days reaches both: room 25.6% of `B` with the net about a leg under the cap, lp-buffer's $48.5k claim paid; lp-bound refused `PoolCapWouldBreach`).
- Gaps above G can produce `adl_required`. Those are allowed but must clear by the end of the run.

### 17.5 Live localnet run

The real SBF binaries on `solana-test-validator` with the real keeper, live bots and a live checker, on the localnet-only `SCHEDULE_TEST_HOURLY` (an hour-long week) and the test-only mock-pyth oracle (D46). `scripts/localnet/run.sh` runs it end to end and writes a report (invariants I1–I11 plus the checker's own coverage check C0, liquidations, bad debt, the weekend-bound ratio at every weekly close, the LP NAV path); see [localnet.md](localnet.md) for how to run it, what it covers and the time constants a few-hour run cannot reach.

---

## 18. Still for the owner to decide

1. Protocol seed size, and `max_pool_cash` (now an LP-NAV cap) at launch.
2. **Decided (D43, D45):** `k_net ≤ α/G` for a hard weekend bound, enforced by `validate` with the bound on; production `k_net_bps` 4_000 (5_000 under D43), `hard_net_bps` 6_000, `adl_net_target_bps` 2_000, `per_account_cap_bps` 2_500 (of the net cap).
3. G: keep the derived 20% (weekend IM) or set `weekend_gap_bps` from CL Sunday-open history. Also α (`weekend_lp_loss_bps`) and `weekend_cap_window_secs` (which now also shuts LP exits and runs the Friday force-reduce).
4. **Decided (D43):** the `k_net ≤ α/G` route, with the weekend cap always on and the `lp_withdraw` weekend check. The Friday force-reduce (D20) stays as the backstop for what §3.1 lists (price drift, a raised `G`), so it now rarely binds.
5. The close-path caps (D14): accept that a matched trader whose close would raise `|net|` past the cap must exit by a venue hedge.
6. The drawdown lines (`dd_freeze_bps`, `dd_adl_bps`, `adl_net_target_bps`, `dd_withdraw_block_bps`, `DD_WINDOW_SECS`), which replace research's 60/55/50/45, and the redeem haircut h.
7. `skew_scale`, `max_impact_bps` and `open_fee_bps`. Also whether the production `taker_fee` stays at 50 bps as in `scripts/initialize-market.ts` (it now feeds the self-liquidation rule).
8. `liq_penalty_bps` (now 200), `liq_keeper_share_bps`, `backstop_threshold_bps`, `min_liq_slice_bps` and `liq_min_remaining_notional`.
9. `insurance_target`, `insurance_claim_cap_per_window` and `lp_loss_cap_window_bps`.
10. Sweeping the **whole** insurance fund to LPs once a settled market has no positions (D19).
11. `treasury_fee_bps`.
12. `withdrawal_delay_secs`, `withdrawal_claim_window_secs`, `lp_reopen_quiet_secs`, `profit_close_quiet_secs` (300 s), and with them the timelock (10 d by default).
13. ADL: `adl_exposure_score_floor_bps`, `ADL_FLOOR_START_BPS` / `ADL_FLOOR_DECAY_SECS`, and who holds `adl_keeper` at launch.
14. The constants: `ADL_WRITE_OFF_SECS` (24 open h), `LOSS_WINDOW_SECS` (24 h), `MAX_FUNDING_CATCHUP_PERIODS` (168), `LP_DEAD_SHARES` and `MAX_OPEN_INTEREST`, plus OIL's `max_open_interest` in contracts.
15. **Decided (D45): LP exit liveness at the net cap, option 1, a 20% buffer.** Under D43/D44 the open cap and the `lp_withdraw` weekend check were one line, so an LP who requested after traders filled the net could not claim any amount until traders closed. The owner chose a slack between the lines (option (b) as first listed): production `k_net` 0.4× under the 0.5× weekend line, so with traders at the full cap LPs keep about 20% of `B` of exit room at a steady price (`B' ≥ G·net/α = 0.8·B`); a price rise, a fall in `B`, cap-exempt liquidations of the minority side or a later raise of `G` eats it, the room is shared first come, first served among matured claims (not reserved for requests made before the fill), and beyond it exits wait for traders to close (§3.1). Not done, for a later decision if wanted: reserving room for pending requests by request time in `lp_withdraw` (checking the lines against `B'` less the value of earlier pending requests); it would let a pre-fill request claim in full, but every later claim would wait behind every earlier one, matured or not. Weekday capacity is 0.4× (0.5× under D43, 1× before). Rejected: (a) a keeper-run exit ADL that cuts net-side winners to `α·(B − claim)/G` when a matured claim is refused, for bad trader UX (it closes winners on LP timing), because an LP who also trades can grief it (requesting to force closes of the opposite side), and because it cannot touch a losing net; (c) accepting the lock. A utilization or holding fee on the net (paid by the net side to LPs while the net is near the cap) remains a possible later addition.
16. Whether `apply_pool_params` should refuse a change that tightens the weekend bound over net already open (needs the oracle account, D44); for now it is a stated exception (§3.1 item 4).

17. Oracle (D47): the OIL feed. Pyth's `Commodities.USOILSPOT` (feed id `925ca92f…b3e6`, a non-expiring WTI CFD: no monthly roll, but its hours, Sun 23:00 to Fri 21:45 GMT with a daily 22:00–23:00 GMT break, differ from CME's, so in the gaps the market fails closed with `StaleOraclePrice`) or the CME-hours WTI futures feeds (dated, so a roll every month through the 10-day rotation; ids not collected). The tests use USOILSPOT's id; check the id on Pyth before launch (D52: XAU, XAG and USOILSPOT's ids are corroborated by several independent public integrations, not yet by Pyth's own list).
18. Oracle (D47): which Receiver / push-oracle pair `ProtocolConfig.oracle_program` and the feed accounts use on mainnet: the legacy `rec5EKMG…` / `pythWSns…` or the pro-compatible `rec2HHDD…` / `pyt2F414…` (Pyth's upgrade guide points to the latter; their shard-0 feed addresses differ). Check on chain before launch; not checked here. `scripts/initialize-market.ts` and `oracle-feed.ts` take the pair from `PYTH_PROGRAMS` (required, no default); the frontend defaults to `pro` (`VITE_PYTH_PROGRAMS=legacy` switches) (D51).
19. Oracle (D47): Pyth sponsors no commodity push feed on Solana, so the protocol must run and fund a price pusher (Hermes needs an API key) for every market feed, before `initialize_market` (the first push creates the account); NATGAS and COPPER have no Pyth Core feed id found yet and cannot launch.
20. Oracle (D47): the rotation timelock is the pool-params timelock (10 d at production) and `update_roles` no longer moves existing markets' oracle program; an oracle emergency is handled with `pause_market` / `settle_market` meanwhile.
21. Oracle (D52): **launch requirement on the price pusher**, and an open owner decision. Anyone can push a newer signed Hermes update into a market's pinned feed, so whoever knows a newer price than the stored one can open, close, deposit or withdraw at the stored price and then push the newer one (and a liquidator can pick the update in (stored, now] that hurts the position most). The edge is the move between the stored price and Hermes; the program bounds it only by `MAX_PRICE_AGE_SECS` (60 s). Required before launch: the protocol's pusher pushes whenever Hermes moves more than a deviation threshold well under the round-trip fee (the pool's open fee plus the market's close fee: 5 + 50 bps with `initialize-market.ts`'s values; a threshold of at most 10 bps), with a heartbeat well under the age limit (at most 10 s), from more than one RPC; the lag between the stored price and Hermes is monitored, and the market is paused while it exceeds the threshold. A news jump faster than the pusher's latency still leaves an edge. Open: an on-chain bound, either a shorter age for trade, LP and liquidation reads (it shrinks the window but makes every read depend on a push at least that often, else the market fails closed; it would also change the localnet price driver, the sim and the tests) or delayed (commit, then execute at a price published after the commit) opens and closes, which removes the edge at a UX cost. Neither is done.
22. Oracle (D52): the admin stays trusted for prices. `settle_market(settlement_price)` is admin-only, immediate and takes any positive price, so the 10-day rotation timelock (D47) protects LPs only against a feed change, not against the admin, and an LP's request after a proposal can still be refused at claim time by the pool's caps and gates. Open: a timelock or band on `settle_market` (for example: only after the market has been unpriceable for N seconds, or within a band of the last oracle price), at the cost of a slower emergency path.

Decided since: the mode-0 ADL close fee (D22), the profit-close quiet time (D23), the hard weekend bound (D43, items 2 and 4; its review, D44, adds items 15 and 16) and the LP exit buffer (D45, item 15).

---

## 19. Red-team log

Findings are numbered in the order received (F1–F13 economics, F14–F24 accounts, F25–F43 integration). "Real" means the problem holds against this spec and the code at HEAD; the account counts were re-checked against the source.

| # | Finding (short) | Decision | Change |
|---|---|---|---|
| F1 | Self-cross a venue pair, close one leg: net/weekend caps bypassed | Real (critical). Research §3.3 had the close-path check; the draft dropped it | D14; §3 rows "Net (close)" and "Weekend net"; §3.1 closes note with the liveness cost; §4.5 step 2; adversary probe (§16.7); test 17.2 #9. The ADL/drawdown freezes are deliberately **not** applied to closes (trap risk); caps use `B` so LP requests cannot block closes |
| F2 | Weekend bound α does not hold (carried net, matched deficits, per-call caps) | Real (high) | Windowed loss caps (D15, §2.6 step 0, new LpPool fields); LP exits shut in `W` (§9); Friday force-reduce `wTrig` (D20, §7.2); §3.1 states what the bound guarantees; §17.4 criterion restated; `k_net ≤ α/G` offered as owner choice (§18 #2, #4). Not adopted then: forcing `k_net ≤ α/G` in validate, since it halves weekday capacity and is the owner's call. **Since adopted by the owner (D43):** `validate` forces it with the bound on, production `k_net` 0.5×, the weekend cap always on and the `lp_withdraw` weekend check; the sim's weekend criterion now fails a weekly close whose excess no documented exception explains. **D43 review (D44):** the cap counts the trade's skew-premium shortfall, the sim attributes the excess by size (one uncovered step fails the close), params changes that tighten the bound are a stated exception, and the exit lock at the cap is stated and left to the owner (§18 #15). **D45:** the owner's answer is a 20% buffer, production `k_net` 0.4× under the 0.5× line |
| F3 | U/cash lines distorted by settled or closed-first matched losers | Real (high). `U/cash = U/(NAV+U)`, so prepaid claims inflate it | D12: solvency haircut only when `U > cash` (§2.8); freeze/ADL/withdraw lines on per-share NAV drawdown (§2.2, §3); `max_pnl_traders_bps`, `adl_trigger_bps`, `adl_target_bps`, `lp_withdraw_block_bps` removed; new `dd_*` and `adl_net_target_bps`; test 17.2 #6. Not adopted: a variation-margin bucket (not well defined per pair in O(1)) |
| F4 | Opposite winners close at full profit before ADL; LP cap ineffective | Real (high) | D13: bad-debt haircut inside `pay_profit` on every pool-funded payout (§2.8, §4.5, §6.3, §6.4, §7.2, §10.5); §2.6 explains what the LP cap means under D2; test 17.2 #11 |
| F5 | Matured withdrawal never expires: standing weekend put | Real (high) | `withdrawal_claim_window_secs` in open seconds, stored per request; `WithdrawalExpired` (§9.4 step 2); `lp_withdraw` shut in `W`; validate keeps one usable open day. Deposits are **not** shut in `W` (arriving capital only adds backing; §9.5) |
| F6 | Matched-pair deficits invisible to NAV until liquidation | Real (medium) | `lp_reopen_quiet_secs` blocks deposits and withdrawals after every session open (§9); `seconds_since_open` (§8); §2.3 point 4 corrected; R12. Not adopted: blocking on keeper-reported liquidatable positions (not checkable on-chain) |
| F7 | A free withdrawal request shrinks capNAV and triggers exposure ADL | Real (medium) | `B` (pending included) for ADL triggers, targets, close caps and the LP post-check; `capNAV` only for new opens (§2.2, §7.2, §9.4); test 17.2 #14 |
| F8 | Bootstrap: pre-seed trading bricks seeding; dust first deposit captures value; no recap path | Real (medium) | D11: `PoolHasNoLps` gate on `open_position` and `venue_fill`; dead shares on the first deposit; seed in the `initialize_market` transaction; `donate_to_pool` (§10.4) allowed while pending; donate/sweep need `lp_shares > 0`. Not adopted: minting at a floor price when `NAV_mint ≤ 0` (makes the new LP fund the hole; donation is the recap path). Not adopted: a separate admin seed path in `initialize_pool`, because the owner decided the protocol seeds through `lp_deposit` |
| F9 | ADL score floor set by the target-picking key | Real (medium) | D16: mode 0 permissionless with on-chain decaying floor (§7.2 step 5); mode 1 keeper-signed with a timelocked param floor; `set_adl_score_floor`, `adl_score_floor`, `AdlFloorSet` removed; R6 rewritten. Mode 1 keeps keeper choice within the eligible set, evented |
| F10 | `settle_position_pnl` rewrites entry, house notional drifts and can be griefed | Real (medium) | D21: `opened_notional` on every add/remove, telescoping, `checked_sub` (§1.1, §4.4, §4.5, §6.3, §6.4, §7.2) |
| F11 | Insurance-target cut is telegraphed, sweep can be sandwiched | Real (low) | D18: swept and donated value vests into `NAV_redeem` over the delay while `NAV_mint` counts it (§2.2, §10.3); R10 replaced. Not adopted: a per-day sweep cap (vesting covers it without a rate state) |
| F12 | Funding crank jump capturable by bundling | Real (low) | `funding_current` gate on deposit, request and withdraw (`FundingStale`; §8, §9) |
| F13 | `max_pool_cash` caps cash, which includes prepaid trader losses | Real (low) | Compare `max(NAV_mint,0) + amount` (§9.1 step 8). Name kept as the owner decided; documented as an LP-capital cap |
| F14 | 1 micro-USDC to the vault blocks `initialize_pool` for ever | Real (high) | `amount == 0` constraint and the VT account dropped from `initialize_pool` (§5); same-transaction bootstrap; test 17.2 #1 |
| F15 | Settled markets cannot close bankrupt positions; NAV_redeem overstated | Real (high) | D19: `close_settled_position` (§10.5), `MarketNotSettled`; `liquidate` stays refused when settled |
| F16 | First deposit at `lp_shares == 0` mints 1:1 and captures pool value | Real (medium) | Dead shares (`lp_shares` never returns to 0) plus the trading gate and fresh-pool check (§9.1 step 7) |
| F17 | `house_entry_notional` not reduced on full liquidations; drifts on settle | Real (medium) | Same as F10; every exit path now subtracts (§6.3 step 4, §6.4 step 6, §7.2 step 9) |
| F18 | `withdraw_collateral` panics when ADL/liquidation credits exceed `total_deposits` | Real (medium); verified at `withdraw_collateral.rs:84` | `saturating_sub`; `total_deposits` is a statistic (§4.2); deposit uses `checked_add` (§4.1) |
| F19 | New `max_open_interest` param breaks `Default` callers and `unpaused()` | Real (medium) | `0` keeps the current value (§4.12) |
| F20 | Timelock uses the proposed delay; `lp_withdraw` reads the delay live | Real (medium) | Timelock uses the max of current and proposed delay and window (§5); `unlock_at` and `claim_window_secs` stored per request (§1.3, §9.2) |
| F21 | Liquidations can revert with `PoolCashInsufficient` | Real (medium) | Liquidations quote profit with `allow_short` and never revert on cash (§2.8, §6.2); `cash_hc` evented; test 17.2 #13. Not adopted: an extra `cash − value ≥ U⁺` check in `lp_withdraw` (already implied by `NAV_redeem` when `U ≥ 0`) |
| F22 | `integration.rs` has its own Env not updated | Real (low) | §17.2 harness changes cover both harnesses, the builders and the GOLD vault assertion |
| F23 | Account positions off by one | Real (low); verified | close 12→13, venue_fill 16→17 (17 metas), fill_quote 19→20, match_orders 20→21 with 17 metas; optional-account placeholders stated (§4.5, §4.8, §16) |
| F24 | Funding carry dropped when exactly 168 periods are due | Real (low) | `due`/`periods_dropped` formula (§8 step 5); unit test 17.1 #10 |
| F25 | Deficit ADL dodged by closing; departure from research unlisted | Real (high); same root as F4 | D13 (listed in §0) |
| F26 | Settled market: no way to close bankrupt/abandoned positions; LPs and insurance stuck | Real (high); same root as F15 | D19; whole-fund sweep when settled and empty (§10.1); settled test 17.2 #17 |
| F27 | Write-off clock runs over the weekend and is not reset by new debt | Real (medium) | Open-seconds clock (§7.4) and debt-weighted `pending_since_*`, one per side (D36) (§2.6 step 4). Not adopted: restricting write-off to keeper/admin (with D13 the write-off only stops future recovery, so permissionless is fine) |
| F28 | Timelock does not let LPs exit if proposed near the weekly close | Real (medium) | Timelock adds `PARAMS_TIMELOCK_MARGIN_SECS` (5 d) and the window; worst case worked in §5 |
| F29 | 1-unit donation blocks `initialize_pool` | Real (medium); duplicate of F14 | As F14 |
| F30 | Settle rewrites entry: house notional, close fee and stage-2 reward drift | Real (medium) | D21: close fee and stage-2 reward also use `opened_notional` (§2.5, §4.5, §6.4, D8); `margin_ratio()` stays on entry notional (display and venue backstop) |
| F31 | `realized_pnl` bookkeeping overwrites and loses settled losses | Real (medium) | Every writer uses `+=`; `UserAccount` gets the position's lifetime figure at the terminal event (§4.5 step 7, §6.3, §6.4, §6.6, §7.2); test 17.2 #10 |
| F32 | `max_open_interest` with `0 < x` breaks callers | Real (medium); duplicate of F19 | As F19 |
| F33 | Exposure ADL bound lost through a withdrawal request | Real (medium); duplicate of F7 | As F7 |
| F34 | No recapitalisation while bad debt is pending | Real (medium) | `donate_to_pool` (§10.4), allowed while pending, recovers pending first |
| F35 | Sim criterion "drawdown ≤ α" contradicts the design | Real (medium); same root as F2 | §17.4 restated; carried excess reported (§16.7 `report.rs`) |
| F36 | Sim funding, fee and liquidation checks pin old behaviour | Real (medium) | Listed in §16.7: crank gate, `n·step` check, `periods_dropped` from the event, ceil close fee, in-force MMR reward, unwind past the delay (sim delay 1 d) |
| F37 | Fate of old liquidation/insurance helpers and pinned tests unstated | Real (low) | §6.5 "Old helpers": deleted, `liquidation_reward_target(notional, mmr)`, tests rewritten; §16.6 for TS |
| F38 | `integration.rs` harness left out | Real (low); duplicate of F22 | As F22 |
| F39 | Account counts wrong | Real (low); duplicate of F23 | As F23 |
| F40 | Keeper layout pin and pool math cannot be built as described | Real (low); keeper was anchor 0.29 outside the workspace (the solana-v3 client crates since D48, still outside) | `pool_math.rs` (§2.7), `#[path]`-included; layout pinned through checked-in fixtures (§16.4) |
| F41 | Liquidations revert on cash; sim/keeper do not handle it | Real (low); same root as F21 | Fixed at the root (no revert), so no `EXEMPT` entry is needed (§16.7) |
| F42 | Self-liquidation rationale ignores the keeper rebate | Real (low) | Validate rule `penalty·(1 − keeper_share) ≥ max_impact + taker_fee` when stage 1 is on (§1.2); default penalty 100 → 200 bps; §2.4 and the probe's expected outcome updated |
| F43 | Off-chain display items missing | Real (low) | §16.5: funding due time via ported `open_seconds_between`, TP/SL `acceptablePrice`, `OrderEntry` liquidation price; §6.5: the log prints the closed size |

No finding was rejected outright. Partial rejections are noted in the Decision column (F2, F3, F5, F6, F8, F11, F13, F21, F27).

Review of the implementation (eba23dc):

| # | Finding (short) | Decision | Change |
|---|---|---|---|
| F44 | A partial ADL or stage-1 slice settles the whole position's funding and can leave a bankrupt remainder | Real | D24 |
| F45 | Permissionless mode-0 ADL is a fee-free, impact-free, cap-free close | Real. Caps fixed at once; the fee was an owner decision | D25 (caps), D22 (fee). Not adopted: skew impact on ADL |
| F46 | Mode-0 ADL bypasses the net and weekend caps (self-cross plus self-ADL) | Real | D25 |
| F47 | A 1-unit donation restarts the vesting schedule | Real | D26 |
| F48 | A winner closes before its bankrupt matched counterparty is liquidated and escapes the D13 haircut | Real; a spec gap. Owner decision | D23. Not adopted: gating only net-raising profitable closes (dodgeable by book skew) |
| F49 | `withdraw_treasury` accepts the vault as destination | Real | D27 |
| F50 | Coverage gaps: §17.2 #1 for every trading instruction; `bucket_identity_property` missing venue fills, settled closes, mode-1 ADL, the treasury | Real; no code defect | Tests added. Pool-param changes are still not in the random loop (nor in the sim; invariant 15 attributes one to `params_tightened` if it lands, D44) |

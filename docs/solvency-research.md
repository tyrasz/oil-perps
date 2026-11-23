# Solvency for oil perps: how HLP works, what exchanges do with insurance funds, and what to build

## 1. Short answer

**How Hyperliquid's HLP works**
- **What it is.** HLP is a community-owned protocol vault. It runs market-making and liquidation strategies, supplies USDC to Earn, and takes a share of fees that the docs do not state ([Protocol vaults](https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/vaults/protocol-vaults)).
- **Depositors.** Depositors share PnL pro rata and pay no leader commission. Losses go straight into vault equity, with no tranche and no first-loss layer. Deposits lock for 4 days ([depositor doc](https://hyperliquid.gitbook.io/hyperliquid-docs/hypercore/vaults/for-vault-depositors-legacy)).
- **Structure.** The info API (`vaultDetails`, snapshot 2026-11-23) shows 7 child vaults and an account value of $149.3M, against a peak of $603.9M on 2025-09-03.
  - The four Liquidator children hold $1M to $30M each. Most capital sits in "Strategy X" (~$100.9M).
  - Keeping the liquidators small is deliberate.
- **It is not GLP.** HLP trades on a central limit order book. Jeff Yan says its market-making share is "negligible", under 1%. Its real solvency job is backstop liquidation, and each liquidation goes to one child vault ([Yan, 2025-10-11](https://x.com/chameleon_jeff/status/1977066751717429516)).
- **Waterfall** ([Liquidations](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/liquidations), [ADL](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/auto-deleveraging)):
  1. **Book liquidation** starts below maintenance margin (MM). MM is half the initial margin at max leverage, so 1.25% to 16.7%. There is no clearance fee, and the trader keeps any leftover margin.
     - For positions over $100k, only 20% is sent as a market order.
     - For 30 seconds after a block with any partial liquidation, all liquidation orders for that user are for the whole position. The cooldown does not space out 20% slices.
  2. **Backstop** starts below 2/3 of MM. The Liquidator vault takes over the position and its collateral (all cross positions, or just the isolated one). MM is not returned, "because the liquidator vault requires a buffer to make sure backstop liquidations are profitable on average."
  3. **Auto-deleveraging (ADL)** fires when an account's value goes negative.
     - Opposite-side winners are ranked by `(mark/entry) × (notional/account value)` and closed at the previous mark.
     - A user with no open positions never socializes losses.
     - ADL, not HLP, is what makes "no bad debt" true.
- **No insurance fund.** Hyperliquid relies on three things: the MM buffer, HLP depositors, and ADL.
  - The Assistance Fund buys and burns HYPE. Validators voted in Dec 2025 to treat it as burned ([Fees](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/fees), [The Block](https://www.theblock.co/news/defi/2025-12-17-hyper-foundation-proposes-removing-1-billion-hype-tokens-from-circulating-supply-by-zeroing-out-hyperliquids-assistance-fund-383091)).
  - The JELLY refunds were at the Foundation's discretion.
- **Exposure bounds.**
  - Tiered leverage ([margin tiers](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/margin-tiers)).
  - OI caps that reject orders at the cap. After JELLY these were made dynamic relative to market cap; the formula is not published.
  - Funding capped at 0.5% per hour.
  - Isolated-only modes for thin assets.
  - A transfer-margin rule for withdrawing unrealized PnL: `max(IM, 10% × notional)` per the current [Margining](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/margining) docs. 20% was announced in March 2025. I found no source dating the change.
- **The model closest to a third-party oil market is HIP-3** ([HIP-3](https://hyperliquid.gitbook.io/hyperliquid-docs/hyperliquid-improvement-proposals-hips/hip-3-builder-deployed-perpetuals), [deployer actions](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/hip-3-deployer-actions)).
  - Each DEX has its own independent backstop liquidator at `0x400…0 + dex_index`. It takes only cross-margin assets and "falls back to ADL."
  - Depositors fund it in multiples of 1,000 tokens, and "only principal amount is withdrawable, not pnl."
  - That HLP does not backstop HIP-3 markets follows from this design; the docs do not say it outright.
  - Oracle clamps: mark moves at most 1% per update, prices at most 10x the start-of-day value, and a mark update cannot push OI to 10x the cap.
  - The deployer stakes 500k HYPE, and slashed stake is burned rather than paid to victims. It enforces honest oracle inputs; it is not insurance.
  - The OI-cap floor text is ambiguous; do not read it as $1M.

**Incidents and the rules that followed**

| Date | Event | HLP impact | What changed |
|---|---|---|---|
| 2025-03-12 | ETH whale withdrew unrealized PnL and fell to the backstop | about −$4M | Leverage cut to 40x BTC / 25x ETH; 20% transfer margin announced ([The Block](https://www.theblock.co/news/markets/2025-03-12-hype-drop-hlp-vault-loss-hyperliquid-whale-liquidation-345866)) |
| 2025-03-26 | JELLY: attacker self-liquidated a short into HLP | about $12–13.5M unrealized loss | See below |
| 2025-10-10 | Market crash | about +$40–41.5M (third-party estimates, [CoinGecko](https://www.coingecko.com/learn/hyperliquid-hlp-vault-analysis); no official figure) | None. First cross-margin ADL in more than 2 years, zero bad debt (Yan) |
| 2025-11-12 | POPCAT manipulated long of about $30M | about −$4.9M | None found. Unrelated: a platform-wide Arbitrum bridge pause of about an hour, labelled "maintenance". Hyperliquid did not link it to POPCAT, and HLP flows were not specifically paused ([Cointelegraph](https://cointelegraph.com/news/hyperliquid-hlp-popcat-attack-3m-wipeout)) |
| 2026-04-09 | FARTCOIN longs | about −$1.2–1.5M | None. ADL fired, consistent with the post-JELLY loss-threshold rule ([CoinDesk](https://www.coindesk.com/markets/2026/04/09/someone-tried-to-manipulate-fartcoin-s-price-with-a-usd145-million-long-it-crashed-50)) |

- **JELLY detail.**
  - Validators delisted the market and settled it at $0.0095, overriding an oracle price of about $0.50.
  - The often-quoted "+$700k" was HLP's whole-vault 24h PnL, not the profit on the short.
  - The Foundation refunded non-flagged users. The refund price of 0.037555 is single-sourced.
  - Changes announced 2025-03-27 ([cryptotimes](https://www.cryptotimes.io/2025/03/28/hyperliquid-to-refund-jelly-traders-updates-its-risk-strategy/)):
    - the Liquidator is capped at a small share of HLP and rebalanced rarely;
    - **ADL fires once Liquidator losses pass a threshold, instead of drawing on the other HLP children**;
    - OI caps became dynamic;
    - delisting moved to on-chain validator votes.
- **The 10-10 ADL debate.** Tarun Chitra's paper (Gauntlet's CEO) first claimed about $650M of excess ADL losses. Its Feb 2026 revision cut the estimate to **$45.0–51.7M** ([arXiv 2512.01112v3](https://arxiv.org/html/2512.01112v3)), so cite the revision.

**What exchanges do with insurance funds**
- **Where the money comes from.**
  - Mainly the gap between the liquidation fill and the bankruptcy price (the price at which margin is zero). Fills better than bankruptcy add to the fund, and fills worse draw on it ([Coinbase Intl rules](https://www.coinbase.com/international-exchange/legal/trading-rules); [BitMEX](https://www.bitmex.com/blog/the-bitmex-insurance-fund)).
  - Some venues charge an explicit fee. Deribit set a 1% liquidation fee across all products effective 1 Aug 2026, alongside a new Liquidity Support Program. That comes from Deribit's own announcement, but I found no page restating it after implementation ([Deribit KB](https://support.deribit.com/hc/en-us/articles/25944777576477-Insurance-Fund), [corroboration](https://blogs.apievangelist.com/blogs/deribit-2026-06-29-new-fee-schedule-on-deribit/)).
  - Binance's clearance fee goes to the fund only "when Binance considers it necessary" ([FAQ](https://www.binance.com/en/support/faq/introduction-to-futures-insurance-funds-360033525371)).
  - The house seeds or tops up the fund: Binance from company funds; Deribit injected 500 BTC in Mar 2020; OKEx 2,500 BTC in 2018.
  - Coinbase Intl seeds its fund, then adds "a portion of transaction fees" ([help](https://support.coinbase.com/en/international-exchange/liquidations/what-is-the-insurance-fund)).
- **What it pays for.** It covers deficits on positions taken over at the bankruptcy price.
  - Coinbase's waterfall is: book liquidation → Liquidity Support Program (LP takeover) → fund → ADL and clawbacks ([liquidations](https://help.coinbase.com/en/coinbase/derivatives/liquidations)). Its fund also compensates LSP participants.
- **How funds are pooled.** Pools are set per contract or per correlated group, not just as one headline total.
  - BitMEX hit ADL in 2019 on contracts with tiny allocations while its total fund was huge ([post-mortem](https://www.bitmex.com/blog/xbtu19-and-ethm19-auto-deleveraging-events-2-april-2019)).
  - On 25 Dec 2025 Bybit replaced per-contract funds with a New Listing pool (first 30 days, minimum $8M) and Portfolio pools of up to 9 contracts ([Bybit](https://www.bybit.com/en/press/post/bybit-insurance-fund-mechanism-enhances-risk-coverage-capacity-per-trading-pair-by-over-200--blt83baa61d02a83677)).
- **When the fund runs out: ADL**, ranked by profit and leverage, at the bankruptcy price.
  - Bybit's trigger is per pair: ADL starts when a pair's 8h PnL drawdown hits a trigger line (30% in Bybit's example) relative to the pool's peak, stops at a lower line (25% in the example), and also fires when the pool is at or below zero ([ADL](https://www.bybit.com/en/help-center/article/Auto-Deleveraging-ADL)).
  - OKX uses a similar drawdown trigger ([OKX](https://www.okx.com/help-center/360054418331)).
  - Deribit uses session-based socialized loss instead, and says it has never needed it.
  - OKX's 2018 clawback of about 17.7% of the week's profits is the cautionary case ([OKX notice](https://www.okx.com/help/360011941512)).
- **What happens to excess: it is house capital.**
  - Binance may deploy excess above its minimum "for other purposes … in its sole discretion" ([FAQ](https://www.binance.com/en/support/faq/introduction-to-futures-insurance-funds-360033525371)). It used "a portion of the futures insurance fund" in its Oct 2026 hack recovery ([crypto.news](https://crypto.news/binance-tapped-its-insurance-fund-after-the-hack-whats-left/)).
  - BitMEX announced on 18 Nov 2025 a cut to about 3,600 XBT plus 30M USDT ([BitMEX](https://www.bitmex.com/blog/insurance-fund-changes)). The pre-cut balance is not verified; 36.4k BTC was the March 2020 peak. Where the excess went was never disclosed.
  - About $270M (about 3,694 BTC plus 30.84M USDT) had no announced owner when BitMEX closed on 23 Sep 2026 ([Protos](https://protos.com/inside-the-mystery-of-bitmexs-insurance-fund/)).

**For your engine.** Every venue studied either (a) has no market risk (CME, dYdX), or (b) bounds the pool's exposure relative to the pool's own capital and ends in ADL (HLP, GMX, Drift). Your vault is neither: it has full market risk on net OI, no capital bound, and no final layer.

## 2. Comparison

| | Counterparty | What bounds it | Loss waterfall | Surplus |
|---|---|---|---|---|
| **HLP (validator perps)** | Order-book makers. HLP is counterparty only for its MM flow and backstop takeovers | Tiered leverage, dynamic OI caps, transfer margin, capped Liquidator children | Book liquidation (no fee) → Liquidator vault keeps MM → Liquidator loss threshold → ADL | Accrues to HLP depositors pro rata |
| **HIP-3 DEX** | Order book; a per-DEX backstop liquidator | Deployer OI caps per asset and DEX, oracle clamps, stake slashing | Book → DEX backstop → ADL | Backstop depositors withdraw principal only, not PnL |
| **CEX funds (Binance, Bybit, OKX, BitMEX)** | Order book; the exchange's fund takes over at bankruptcy | Risk limits, per-pool sizing, discretionary top-ups | Partial liquidation → fund at bankruptcy price → ADL (OKX 2018: clawback) | Owned by the exchange. Excess redeployed at discretion (Binance, BitMEX) |
| **Deribit / Coinbase Intl** | Order book; LSP liquidity providers take over first | Liquidation fee, LSP | Book → LSP → fund → socialized loss (Deribit) or ADL and clawbacks (Coinbase) | Exchange-owned. Per-asset split unverified |
| **GMX v2** ([gmx-synthetics](https://github.com/gmx-io/gmx-synthetics)) | GM LP pool | `reserve ≤ reserveFactor × pool`, `maxOpenInterest`, convex price impact, borrow and funding. Synthetic defaults: max trader PnL 60%, ADL 55% down to 50%, withdrawals blocked at 45% | Collateral → pool → profit haircut on close above the PnL cap → ADL | Trader losses and fees go to LPs |
| **Jupiter JLP** ([IDL](https://github.com/julianfssen/jupiter-perps-anchor-idl-parsing)) | JLP pool | Per-custody max global long and short, max position size, `maxAumUsd`, jump-rate borrow on utilization | Collateral → pool. No ADL found in sources | To LPs |
| **gTrade gToken** | gToken vault | `maxDailyAccPnlDelta` and `maxAccOpenPnlDelta` (they exist; values unverified). Withdrawals take 1, 2 or 3 epochs at >120%, >110% or lower collateralization | Vault. Recapitalization mechanics unverified | To vault stakers |
| **Drift v2** ([protocol-v2](https://github.com/drift-labs/protocol-v2)) | DLOB plus protocol AMM; winners paid from a `pnl_pool` that cannot go negative | Per-market insurance claim by tier (A $100M, B $1M, C $100k, speculative tiers 0), AMM hourly drawdown stop, down-weighted unrealized PnL | Collateral → insurance fund (up to the cap) → market fee pool → socialized via a funding-rate delta | Revenue pool → insurance stakers (APR-capped) and protocol |
| **dYdX v4** ([v4-chain](https://github.com/dydxprotocol/v4-chain)) | Matched order book; the protocol has no market risk | Margin. Per-market fund for isolated markets, shared fund for cross | Liquidation fee to fund (governance-set, 1.5% per docs) → fund (never negative) → deleveraging from a pseudo-random start at bankruptcy price. Withdrawals gated while any negative account with open positions exists | Stays in fund. Disposition not found |
| **CME (NYMEX CL)** | CCP via novation, holding a matched book. Directional only after a default, which it hedges and auctions | SPAN 2 initial margin (≥99%, ≥1-day liquidation horizon for futures and energy swaps, [17 CFR 39.13](https://www.ecfr.gov/current/title-17/chapter-I/part-39/subpart-B/section-39.13)); cash variation margin at least twice a day | Defaulter's margin → defaulter's guaranty fund → CME $100M → non-defaulters' guaranty fund (about $8.31B, sized to Cover 2) → assessments (about $22.84B) ([Safeguards](https://www.cmegroup.com/solutions/risk-management/financial-safeguards.html)) | None. The CCP makes no trading P&L |

- **Notes on dYdX.** The YFI attack in 2023 took about $9M, roughly 40% of the v3 fund. The same attacker had taken about $5M from SUSHI a week earlier, after which dYdX had preemptively raised SUSHI initial margin to 100%. After YFI, dYdX raised initial margin on thin markets, cut position sizes, and planned an automatic IMF adjustment ([The Block](https://www.theblock.co/post/263632/dydxs-insurance-fund-lost-9-million-as-a-result-of-targeted-attack-ceo)).
- **Notes on CME.** CME let CL settle at **−$37.63** on 20 Apr 2020, after testing negative prices the week before ([20-160](https://www.cmegroup.com/notices/clearing/2020/04/Chadv20-160.html)). The broker took the losses beyond customer equity: Interactive Brokers booked about $104M ([10-K](https://www.sec.gov/Archives/edgar/data/1381197/000138119721000008/ibkr-20201231x10k.htm)).
- **Nasdaq Clearing 2018.** A €114M shortfall ate €107M of a €166M members' default fund, because Nasdaq's own capital in the waterfall was only €7M ([BIS](https://bis.org/publ/qtrpdf/r_qt1812x.htm)).

## 3. What this means for oil perps

### 3.1 Diagnosis (perps-core HEAD 3b76521)
- **No counterparty on the house path.** `open_position` fills at the oracle with no spread and no fee, and adds OI on one side only (`open_position.rs:116,127-138`).
- **Orphaned legs.** `venue_fill` pairs are zero-sum only while both legs are open. Once one closes, the survivor's counterparty is the shared `vault_token_account` (`venue-settlement.md:824`).
- **Net exposure is known but not valued.** Net vault exposure is `L − S` contracts, but no Σsize·entry or funding basis is stored, so its PnL cannot be computed on-chain.
- **The only bound is in contracts.** `max_open_interest` counts contracts, is not tied to any capital, and cannot be changed after init.
- **Insurance is only a number.** `insurance_fund` is a `u64` that can exceed its real backing. Uncovered bad debt is only `msg!`-logged. Liquidation confiscates all remaining equity.
- **Withdrawals are first-come.** `withdraw_collateral` takes a read-only Market and does no checks, so the last withdrawer eats the hole.
- **Stuck funds and dropped funding.** Surplus can never leave the vault, and funding drops every period after the first between cranks.

### 3.2 Viable designs

| Design | What it is | For | Against |
|---|---|---|---|
| **A. Matched-only (CME/dYdX)** | Remove the house path. A venue leg can close only through a reduce-only venue fill. The LP pool is only a liquidation backstop, as in HIP-3 | The pool has no market risk except backstop takeovers | Needs a two-sided book all session and at the Sunday reopen, so traders are trapped when the book is thin. Still needs a backstop and ADL. Reworks the close path through orderbook and mm-registry, and adds CU to closes |
| **B. Pool as universal counterparty (GMX v2 / JLP)** | Every trade is against the LP pool at oracle ± price impact | Always-on liquidity; simple | LPs hold the full oil delta, including weekend gaps. Oracle-priced fills invite toxic flow (GMX AVAX 2022, about $565k). Capacity scales only with TVL. Needs borrow fees to pay LPs |
| **C. Hybrid (recommended)** | Venue fills stay matched. An explicit per-market LP pool is the settlement counterparty for everything, but only the **net** (house opens, orphans, liquidations) costs it risk, because matched pairs cancel exactly in its books. Caps tie to LP equity, insurance sits ahead of LPs, ADL is last | Smallest change from today's code. Bounded LP risk. Liquidity when the book is thin | Parameters need care. LPs carry the residual net. The ADL candidate is chosen by a keeper |

**Why C fits the existing code.** If the pool is the counterparty to every realized PnL, then a matched pair costs it nothing.
- When the winning leg closes, pool cash pays +X.
- The surviving leg's −X now sits in the pool's unrealized book.
- So pool NAV does not change. Orphans become bounded net exposure instead of an unowned hole.

**Do not CPI into prop-amm.** It trusts caller-supplied close params, credits `total_assets` with no token inflow, never marks `unrealized_pnl`, mixes contracts with USDC, is one vault per mint rather than per market, and each SPL CPI costs about 4–5k CU. Fold the pool into perps-core instead, and port prop-amm's share math, withdrawal delay, skew spread and `can_accept_trade` idea with the units fixed.

### 3.3 Recommended design (C), concretely

**Accounts (new PDAs; do not `realloc` Market, which has 15 bytes of padding and a test-pinned LEN of 298)**
- **`LpPool` `[b"lp_pool", market]`.** Tokens stay in the existing `vault_token_account`, held as ledger buckets, so no extra CPI.
  - **Buckets (u64):** `cash` (LP-owned), `insurance`, `treasury`, `total_free_collateral`, `total_position_collateral`.
  - **Aggregates:**
    - per side, `size`, `Σsize·entry` and `Σsize·funding_index_at_entry` (i128/u128);
    - `open_positions` (a real count, separate from the `total_positions` nonce);
    - `bad_debt_cum`, `pending_bad_debt`, `adl_required: bool`;
    - drawdown window: `peak_nav` and `window_start`.
  - **Parameters:** `k_net_bps`, `k_net_weekend_bps`, `hard_net_bps`, `max_pnl_traders_bps`, `adl_trigger_bps`, `adl_target_bps`, `lp_withdraw_block_bps`, `per_account_cap_bps`, `skew_scale`, `open_fee_bps`, `liq_penalty_bps`, `backstop_threshold_bps`, `insurance_target`, `insurance_claim_cap_per_event`, `lp_loss_cap_per_event_bps`, `withdrawal_delay_secs`, `fee_split` (insurance, LP, treasury).
  - **Shares:** `lp_shares` and `pending_withdraw_shares`.
- **`LpPosition` `[b"lp_position", lp_pool, owner]`:** shares, locked shares, and the request timestamp. Shares are non-transferable at first; an SPL LP mint can come later.
- **Invariant asserted in every instruction that loads the token account:**
  `vault_token.amount ≥ free + position_collateral + insurance + cash + treasury`, with every bucket ≥ 0.
  This also replaces the sim's invariant 2 on-chain.

**Valuing the pool's book in O(1)**
- Let m be the mark (oracle price). Trader net PnL against the pool is:
  `U = (m·L − ΣLe) − (m·S − ΣSe) − funding_owed(L,S)`
  where the funding term is built from the Market's per-side cumulative indices and the `Σsize·index` aggregates. Matched legs share the same size and entry, so they cancel exactly.
- `NAV_mint = cash − U`.
- `NAV_redeem = cash − max(U,0) − h·min(U,0) − pending_bad_debt − open_positions`, with h around 0.5. Trader losses count at a haircut, and one unit is deducted per open position to cover the gap between aggregate and per-position truncation. Always round against the LP who is withdrawing.

**Exposure caps (in USDC, relative to the pool)**
- **Net cap.** `|L−S|·m ≤ k_net·NAV_redeem`. Enforced in `open_position` and on any pool close that increases |net|. Venue fills never change net, so they are not checked against it.
- **Weekend net cap.** Inside `pre_close_window_secs`:
  `|L−S|·m ≤ min(k_net_weekend·NAV, (insurance + α·NAV)/G)`
  where G is the stress Sunday-open gap you choose from CL history and α is the share of LP equity you accept losing on a gap. This ties weekend exposure to real capital, which the margin bump alone does not.
- **Hard cap.** `|L−S|·m > hard_net·NAV` makes exposure-ADL eligible. Closes are never blocked; closes that orphan a leg pay skew impact instead.
- **PnL cap** (GMX synthetic defaults as starting points: 60/55/50/45):
  - if `U > max_pnl_traders·cash`, profits paid on close are scaled by `cap·cash/U`;
  - ADL is allowed while `U > adl_trigger·cash`, but must not take U below `adl_target·cash`;
  - LP withdrawals are blocked while `U > lp_withdraw_block·cash`.
  - Keep the deposit threshold at or below the trader threshold.
- **Gross cap.** `max_open_interest` becomes notional and editable through `update_market_config` (it is init-only today).
- **Per-account cap.** No single account may exceed `per_account_cap` of the net capacity (the concentration lesson from Nasdaq 2018 and IBKR 2020).
- **Pricing.** House opens fill at `oracle × (1 + 0.5(skew_before + skew_after)/skew_scale)` plus an open fee, using the Synthetix SIP-279 premium ([SIP-279](https://github.com/Synthetixio/SIPs/blob/master/content/sips/sip-279.md)). Do not offer zero-impact oracle fills.

**Loss waterfall when a close or liquidation leaves deficit D (equity < 0)**
1. The position's margin (already exhausted by definition).
2. The liquidation penalty and buffer (below).
3. `insurance` pays `min(D, insurance, insurance_claim_cap_per_event)`.
4. `cash` absorbs up to `lp_loss_cap_per_event·NAV`. This ring-fences the pool the way Hyperliquid's post-JELLY Liquidator cap does.
5. Any remainder goes to `pending_bad_debt` and sets `adl_required`.
   - While the flag is set, pool-funded payouts stop: profitable closes and LP withdrawals.
   - Free-collateral withdrawals continue, because that bucket is segregated.
   - The `adl` instruction must clear the flag.
   - Nothing is ever only logged again.

**Liquidation (change `liquidate`)**
- **Trigger on mark notional.** `equity < MMR_in_force · m·q`. Today `margin_ratio` uses entry notional.
- **Stage 1, partial.** For MM > equity ≥ `backstop_threshold` (e.g. 2/3 MM): close a slice against the pool at oracle, enough to restore IM, with a minimum of 20–25%.
  - The penalty is `liq_penalty_bps × closed notional`, split between the keeper reward and insurance.
  - **Leftover equity stays with the trader.** This replaces full confiscation.
- **Stage 2, backstop.** Below the threshold, close the full position and send all remaining equity to insurance (Hyperliquid keeps MM for the same reason). The keeper reward is paid from it, based on the MMR in force (it uses base MMR today).
- **Tests.** The unit tests at `state/market.rs:409-436` change, and insurance accrual falls; size `liq_penalty_bps` to compensate.

**ADL (new `adl` instruction, keeper-signed)**
- **Two modes.**
  - **Deficit mode:** close an opposite-side profitable position against `pending_bad_debt` at the bankrupt position's bankruptcy price, so the winner bears the deficit up to their profit (the CEX standard).
  - **Exposure mode:** close profitable positions on the side the pool is short, at mark, while `U > adl_trigger` or net is above the hard cap. Stop at the target (GMX `MIN_PNL_FACTOR_AFTER_ADL`).
- **On-chain checks.** Solana cannot rank positions on-chain. The program verifies only that the candidate is on the opposite side, is in profit, and has a score of at least the published floor, using Hyperliquid's score `(m/entry)×(notional/equity)`.
- **Audit trail.** Emit an event per ADL so the keeper's off-chain ranking can be audited. Show each trader their ADL score; the 10-10 fight was about how visible and fair the ranking was.

**Insurance fund: sources, governance, excess**
- **Sources:**
  - stage-2 residual equity;
  - the insurance share of the stage-1 penalty;
  - a `fee_split` share of `taker_fee` and the new open fee. Today all fees minus referral go to insurance; change that to insurance first until the fund reaches `insurance_target`, then LP and treasury.
- **Funding surplus is LP income.** The funding the vault "silently keeps" today, `|L−S|·step`, becomes the pool's legitimate carry for holding the net, so it goes to `cash`.
- **Target size.** `insurance_target ≥ G × hard_net_cap_notional` plus a floor. Size it to stress, not average loss: YFI took 40% of dYdX's fund in a day.
- **Governance.** Parameters change only through `update_pool_config` behind a timelock. **There is no discretionary withdraw instruction for `insurance`.** This is the deliberate contrast with Binance and BitMEX.
- **Excess.** A permissionless `sweep_insurance_excess` moves anything above `insurance_target` into `cash`, where it accrues to LP shares. Protocol revenue leaves only through `withdraw_treasury` (admin, treasury bucket only).

**Funding (change `update_funding`)**
- **Catch-up.** Compute `n = floor((now − last)/interval)`, apply `n × step` by multiplication (O(1)), and set `last += n·interval`.
- **Closed sessions.** No accrual while the session is closed. The first crank after reopen resets `last` to the session-open time without charging.
- **Rate.** The current rate depends only on imbalance and is capped at ±0.01% per period. With the pool holding the net, consider raising the cap or adding skew-velocity funding to pull the imbalance in (owner's call, see 4).

**LP flows (new `lp_deposit`, `lp_request_withdrawal`, `lp_withdraw`, `lp_cancel_withdrawal`)**
- Allowed only with the session open and a fresh oracle; no weekend pricing.
- Deposits mint at `NAV_mint`. Withdrawals redeem at `NAV_redeem` after a delay (Hyperliquid uses 4 days).
- Shares are locked during the delay.
- Withdrawals are blocked while `adl_required` is set or while `U > lp_withdraw_block`. This is the dYdX withdrawal gate, applied to LPs only.

**Instruction changes**

| Instruction | Change |
|---|---|
| `open_position` | Add `LpPool` (mut): skew price and open fee, net/weekend/gross/per-account caps, aggregates, buckets |
| `venue_fill` | Add `LpPool` (mut): aggregates for both legs, free → position bucket. The account count goes from 16 to 17, so update the hand-encoded order in `sdk/mm-registry` and re-check the 100k CU assertion (`tests/venue/mod.rs:830-841`) |
| `close_position` | Add `LpPool`: settle PnL against `cash`, PnL-cap haircut, skew impact when the close orphans a leg, fee split, `adl_required` check. Optionally credit `collateral_balance` instead of the wallet, which saves a CPI |
| `liquidate` | Mark-based trigger, two stages, penalty instead of confiscation, waterfall, `pending_bad_debt` |
| `update_funding` | Catch-up and session-aware clock |
| `deposit_collateral`, `add_margin`, `withdraw_collateral` | Bucket counters. `withdraw_collateral` takes `LpPool` (mut) and asserts the invariant; free collateral is never gated |
| `update_market_config` | `max_open_interest` becomes editable and notional |
| `settle_market` | Settles the pool's aggregates. Publish the settlement-price rule in advance (the JELLY lesson) |
| New | `initialize_pool`, LP instructions, `adl`, `sweep_insurance_excess`, `withdraw_treasury`, `update_pool_config` (timelocked) |
| Migration | `begin_pool_migration` (pauses the market), a permissionless `migrate_position` / `migrate_user_account` crank (one padding byte as a `migrated` flag) that backfills aggregates and bucket totals, and `finish_pool_migration` |

- **What `finish_pool_migration` does:**
  - checks that migrated sizes equal L and S;
  - computes the backed surplus as `token_balance − free − position_collateral`;
  - sets `insurance = min(old insurance_fund, surplus)`;
  - assigns the remainder (owner's call);
  - records any shortfall as bad debt.
- **Constraints.**
  - Box every new account and keep math in `#[inline(never)]` helpers (4 KB SBF stack; `build-sbf.sh` fails on stack offset).
  - Append fields only; the SDK decodes by byte offset.
  - Keep `ExecutionSource` order and the `total_positions` nonce as they are.
  - Update the keeper, frontend, sim and mm-registry/orderbook together.

### 3.4 Simulation findings this resolves

| Finding | Resolved by |
|---|---|
| Insolvent at the mark in most seeds | Exposure is now owned by LP equity, bounded by the net, weekend and PnL caps, and ended by ADL. It becomes a measurable NAV drawdown, not a hole. Run the sim with `--strict-solvency` as the default |
| Final withdrawal failed in 2 runs | Buckets plus "cash never goes negative": free collateral is always backed, and payouts stop at `adl_required` instead of draining other users |
| `vault − insurance` stuck after unwind | Surplus becomes labelled `cash` (LP-owned) or `treasury`, and insurance excess is swept |
| `unbacked_insurance_ticks` | Migration backs insurance only with real surplus |
| Bad debt only logged | `pending_bad_debt`, `bad_debt_cum` and ADL |
| `funding_periods_dropped`; silent funding surplus | Catch-up periods; the surplus becomes the pool's carry |
| Net exposure not computable on-chain | Per-side aggregates, which mirror the sim's `vault_basis` |

**Only partly resolved.**
- **Weekend reopen gap.** The gap still lands on the pool, but its size is now bounded by the weekend cap.
- **Oracle manipulation.** The existing staleness, confidence and minimum-price checks remain; consider adding a per-update deviation band.
- **Negative or zero WTI.** The `min_oracle_price` halt and the contract-roll policy need to be stated explicitly.

## 4. Owner's calls
1. **Who the LPs are.** Public, protocol-seeded, or both? Does the protocol put first-loss capital of its own ahead of LPs (CME's $100M; Nasdaq's €7M was too thin)?
2. **External hedging.** Should the pool hedge its net in CME CL? A hedge held over the weekend does offset the Sunday gap, but it brings in an off-chain operator and custody trust.
3. **Insurance excess.** LPs (recommended), treasury, or buyback?
4. **Pool scope.** One pool and fund per market, or shared across markets with per-market claim caps (Drift tiers, Bybit portfolio pools)?
5. **House path.** Keep `open_position` as cap-bounded, skew-priced flow, or go matched-only (design A)?
6. **ADL details.** Bankruptcy price or mark in exposure mode? Is keeper-supplied candidate selection with an on-chain score floor enough trust?
7. **Liquidation economics.** `liq_penalty_bps` and the backstop threshold, i.e. how much leftover equity returns to traders.
8. **Weekend.** Force-reduce before Friday close, or rely on caps and margin? Accrue funding while closed?
9. **Funding.** Magnitude (the current ±0.01% cap) and whether to add a mark/premium term once venue prices are reliable.
10. **Starting parameters.** `k_net`, weekend α and G, the PnL thresholds (60/55/50/45 suggested), withdrawal delay, insurance target.
11. **Migration.** Who gets today's unlabelled surplus (insurance, LP seed, or treasury), and how is any pre-existing shortfall handled?
12. **Discretionary powers.** Publish rules in advance for `settle_market` prices, pauses and any compensation. JELLY saved the pool but drew lasting criticism of centralization.
13. **Open regulatory questions.** CME's weekend and holiday margin scaling is unverified. The SPAN 2 lookback is "minimum 10 years" per [CME](https://www.cmegroup.com/span-2.html), not 5. Treat both as open when you calibrate G.

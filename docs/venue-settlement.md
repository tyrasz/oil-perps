# Venue settlement design

How mm-registry quote fills and orderbook matches settle into real perps-core
positions (`venue_fill`). This is the design the implementation was built
from. Section 11 amends sections 0–10, and the code then changed further
after two adversarial reviews. Where they differ, the code wins:

> Historical, pre-D48: toolchain and oracle details below (Rust 1.75, no
> `div_ceil`, pyth-sdk-solana's `get_price_no_older_than`, `scripts/idlgen`)
> describe the stack this was built on. Since [lp-pool.md](lp-pool.md) D47–D52
> the programs build with Rust 1.95 (platform-tools v1.57, `div_ceil` allowed)
> and read Pyth Receiver `PriceUpdateV2` accounts with `pyth_receiver.rs`.

| Spec says | Code does | Why |
|---|---|---|
| Pin the venue's ProgramData `slot` (A9 `expected_program_slot`) | Pin its upgrade **authority** (`Venue.upgrade_authority`, `expected_upgrade_authority`; default = immutable) | Loader `ExtendProgram` needs no authority and rewrites the slot, so anyone could halt every venue's fills for a few thousand lamports |
| `VenueApproval` bounds price, leverage, allowance, expiry | Also `allowed_sides` (`SIDE_LONG` / `SIDE_SHORT`) | Otherwise a one-shot buy approval let a buggy venue open a short at a band-edge price |
| Approvals are written by their owner | Narrowing and revoking: from anywhere. Granting: top level, or one CPI deep under a program other than the venue (instructions sysvar) | A taker signs the venue's transaction, so a compromised venue could otherwise widen the taker's approval mid-transaction. The one-CPI allowance keeps multisig and PDA owners working |
| `match_orders(position_nonce)` | `match_orders(position_nonce, max_fill_size)` | Lets the matcher split a cross that is larger than the venue's per-fill cap |
| `close_expired_order` closes Open/PartiallyFilled orders | It also closes legacy Filled/Cancelled orders | The old program left them unclosed |

The tests that pin this behaviour are in `programs/perps-core/tests/venue/`,
`programs/mm-registry/tests/integration.rs` and
`programs/orderbook/tests/integration.rs`. The SDK's hand-coded layout is
pinned by `sdk_layout_is_pinned` in `programs/perps-core/src/state/venue.rs`.

---

**Original spec: venue fills open real perps-core positions**

Scope: one perps-core primitive (`venue_fill`), plus admin and user instructions around it; mm-registry `fill_quote`; orderbook `match_orders`. vamm and prop-amm are out of scope (section 9).

## 0. Decisions, and what reading the code changed

| Question | Decision | Taken from | Why (checked against the code) |
|---|---|---|---|
| Primitive | `venue_fill`: one instruction opens a taker position and an opposite maker position, same `size`, same `entry_price`. It is reachable only by CPI from a registered venue program signing with its PDA `[b"perps_venue", market]`. | all three; seeds from security / mm-ux | A per-market signer seed ties every venue signature to one market. Minimal's global `[b"venue_authority"]` does not. |
| Venue record | PDA `[b"venue", market, venue_program]`, created by the admin. Authority and ProgramData are derived on-chain. The venue's ProgramData `slot` is pinned. | security, mm-ux | Keying by program needs no id table. The slot pin means any upgrade of a venue program halts its fills until the perps-core admin re-attests. This is fail-safe. |
| Consent | A `VenueApproval` PDA per (owner, venue). It is signed inside perps-core and carries an allowance that is spent down, an expiry, `max_leverage`, a limit price and an adverse-deviation bound. It is required for **both** sides. `Venue.min_signers` additionally requires real signatures. | security | A signer bit only proves consent to the venue's own instruction. A buggy venue can still rewrite size, price and leverage. Perps-core can only enforce bounds that come from data the user signed in perps-core. Minimal's and mm-ux's bitmask leaves an opted-in MM exposed up to their entire free collateral. |
| MM stale-quote guard | `VenueApproval.max_price_deviation_bps`, enforced by perps-core against the oracle. | security (mm-ux put it on the quote) | It is enforced by perps-core, not trusted to the venue, and it covers every quote at once. Setting allowance to 0 with `update_venue_approval` is the MM's one-transaction mass cancel, so mm-ux's `quote_epoch` is not needed. |
| Margin | `max(notional_fill/lev, IM × notional_oracle) + adverse`, where adverse is the side's immediate loss at the oracle. A backstop asserts `margin_ratio(oracle) ≥ IM`. | mm-ux formula, minimal top-up, security backstop | Security's reject-if-below-IM-at-oracle rule makes a 10x taker fail when crossing a 1 bp spread. The top-up keeps the same invariant: no position is born below IM, so collusion creates no bad debt. Proof in 2.2. |
| Band and oracle | Per-venue band `max_price_deviation_bps` (≤ 500) around `read_price_max_age`, with a per-venue oracle age of 1–60 s. | all; oracle age from mm-ux | `read_price` hard-codes `MAX_PRICE_AGE_SECS = 60` (`price.rs`), which is too loose for MMs. |
| Blast-radius caps | Per-venue `max_fill_notional` and a notional window circuit breaker. A guardian `disable_venue`, and a global `venue_fills_paused` in `ProtocolConfig` padding. | security | Cheap. Market is already write-locked on every fill, so writing to Venue adds no contention. |
| Position PDAs | Taker: `[b"vposition", taker, market, taker_user.total_positions (u32 LE)]`. Maker: `[b"vmaker", taker_position]`. | mm-ux | With `market.total_positions` / `+1` (security, minimal), the client pre-derives N, and **any** open in the same market landing first makes the fill fail with `ConstraintSeeds`. That is not just serialization: the transaction is lost. Seeds do not matter elsewhere: `close_position`, `liquidate` and `add_margin` never check them; the frontend finds positions by memcmp on owner (`useOnChainPositions.ts:129`); the keeper uses memcmp on market and status (`liquidator.rs:318-321`). `UserAccount.total_positions` only ever increases, and the `v*` prefixes cannot collide with `[b"position", …]`. |
| Collateral | Each party's own `UserAccount.collateral_balance`, with no token movement. The mm-registry vault becomes a registration bond only. Quotes and orders lock nothing. | all | The money is already in the market Vault, and close and liquidate pay out from there unchanged. |
| Fees | None at fill. `close_position` already charges `taker_fee` on the entry notional of each leg. | all | Avoids double-charging and keeps token or fee logic out of the most exposed instruction. |
| Orderbook rent | `place_order` takes a deposit of `max_fills × rent(Position)`. The matcher is reimbursed per fill. A filled or exhausted order is closed back to its owner. | security, mm-ux | The matcher no longer pays rent it never gets back, and `max_fills` stops an order being split into many dust positions. |
| Orderbook failures | Hard errors. The keeper simulates and skips the pair. Anyone can call `close_expired_order`. | security, minimal | mm-ux's cancel-instead-of-error needs the orderbook to duplicate perps-core's margin math. |
| Matcher | Live `config.admin`, or `OrderBook.matcher`, a hot key set by the admin. | mm-ux | Keeps the upgrade-authority key out of a crank. |

Claims in the source designs that turned out wrong:
- `Position::LEN` is **168**, not 165 (minimal).
- The test harness's `start()` writes a Pyth `timestamp` of 0, so any `read_price` in orderbook or mm-registry tests is stale today. Tests need a `set_price` that stamps the clock time.
- With `SBF_OUT_DIR` set, `ProgramTest::add_program` panics for a program that has no `.so`. A test-only relay venue must use `add_builtin_program`.
- `fill_quote` today does not check `market_maker.status` or `quote.registry`.
- Anchor's `Account::close()` inside a handler is already used by `fill_quote`, so closing orders in `match_orders` follows existing practice.

---

## 1. perps-core

### 1.1 State and byte accounting

| Account | Change | LEN | Padding |
|---|---|---|---|
| `ProtocolConfig` | add `pub venue_fills_paused: bool` after `bump` | 201 → 201 | 64 → 63 |
| `Position` | add `ExecutionSource::Venue` as the **last** variant (Borsh tag 2) | 168 | 32 (unchanged) |
| `Venue` | new account | 267 | 64 |
| `VenueApproval` | new account | 191 | 32 |
| `Market`, `UserAccount`, `Vault` | unchanged | 298 (434 since lp-pool.md D47) / 85 / 145 | — |

Existing accounts read the new bytes as 0: `venue_fills_paused = false`. No realloc or migration is needed.

`state/venue.rs` (new; add `pub mod venue; pub use venue::*;` in `state/mod.rs`):

```rust
#[account]
#[derive(Default)]
pub struct Venue {
    pub market: Pubkey,               // 32
    pub program: Pubkey,              // 32  venue program id
    pub authority: Pubkey,            // 32  find_program_address([b"perps_venue", market], program).0
    pub program_data: Pubkey,         // 32  find_program_address([program], bpf_loader_upgradeable).0
    pub program_slot: u64,            // 8   ProgramData.slot pinned at register/update
    pub max_fill_notional: u64,       // 8   per fill, at fill price (6 dp USDC)
    pub max_window_notional: u64,     // 8
    pub window_start: i64,            // 8
    pub window_notional: u64,         // 8
    pub total_fills: u64,             // 8
    pub total_notional: u64,          // 8   saturating
    pub window_secs: u32,             // 4
    pub max_price_deviation_bps: u16, // 2   |fill - oracle| band
    pub max_oracle_age_secs: u16,     // 2
    pub min_signers: u8,              // 1   0..=2 of {taker, maker} must be tx signers
    pub enabled: bool,                // 1
    pub bump: u8,                     // 1
}
impl Venue {
    pub const SEED: &'static [u8] = b"venue";
    pub const AUTHORITY_SEED: &'static [u8] = b"perps_venue";
    pub const LEN: usize = 8 + 32 * 4 + 8 * 7 + 4 + 2 + 2 + 1 + 1 + 1 + 64; // 267
}

#[account]
#[derive(Default)]
pub struct VenueApproval {
    pub owner: Pubkey,                // 32
    pub venue: Pubkey,                // 32
    pub market: Pubkey,               // 32  = venue.market (for indexers)
    pub allowance: u64,               // 8   collateral the venue may still commit; spent down, never refilled
    pub max_buy_price: u64,           // 8   0 = none; applies when owner goes long
    pub min_sell_price: u64,          // 8   0 = none; applies when owner goes short
    pub expires_at: i64,              // 8
    pub fills: u64,                   // 8
    pub collateral_committed: u64,    // 8   lifetime stat, saturating
    pub max_leverage: u32,            // 4   3 dp
    pub max_price_deviation_bps: u16, // 2   adverse-only bound vs oracle; 0 = off
    pub bump: u8,                     // 1
}
impl VenueApproval {
    pub const SEED: &'static [u8] = b"venue_approval";
    pub const LEN: usize = 8 + 32 * 3 + 8 * 6 + 4 + 2 + 1 + 32; // 191
}

pub const MAX_VENUE_BAND_BPS: u16 = 500;
pub const MAX_APPROVAL_SECS: i64 = 90 * 86_400;

/// Inclusive band check, u128.
pub fn within_band(price: u64, oracle: u64, bps: u16) -> bool {
    let diff = if price > oracle { price - oracle } else { oracle - price } as u128;
    diff * 10_000 <= oracle as u128 * bps as u128
}

/// Collateral a venue fill posts for one side. Public so venues and the SDK use the same math.
/// None => leverage is above what the in-force IM allows (or overflow).
pub fn venue_margin(m: &Market, side: Side, size: u64, price: u64, oracle: u64,
                    leverage: u32, now: i64) -> Option<u64> {
    let notional_fill = u64::try_from(size as u128 * price as u128 / 1_000_000).ok()?;
    let notional_oracle = size as u128 * oracle as u128 / 1_000_000;
    let base = m.required_collateral(notional_fill, leverage, now)?;   // weekend-aware, rounds up
    let (im, _) = m.margin_ratios(now);
    let im_oracle = u64::try_from(ceil_div(notional_oracle * im as u128, 10_000)).ok()?;
    // Same truncation as close/liquidate, so the two legs net exactly
    let probe = Position { side, size, entry_price: price, ..Default::default() };
    let pnl = probe.unrealized_pnl(oracle);
    let adverse = if pnl < 0 { pnl.unsigned_abs() } else { 0 };
    base.max(im_oracle).checked_add(adverse)
}
```

Make `ceil_div` in `state/market.rs` `pub(crate)`. Use no `div_ceil` and no let-chains, so the code builds with Rust 1.75.

`oracle.rs`: `read_price` becomes `read_price_max_age(feed, market, now, MAX_PRICE_AGE_SECS)`. The new function is `pub fn read_price_max_age(feed: &AccountInfo, market: &Market, now: i64, max_age_secs: u64) -> Result<u64>`. Its body is today's body with the age passed to `get_price_no_older_than`. Behaviour of every existing caller is unchanged.

### 1.2 Errors (append after `InvalidOracleAccount`, so existing codes stay put)

`VenueUnauthorized, VenueDisabled, VenueProgramChanged, VenueFillsPaused, InvalidVenueParams, InvalidFillParams, FillPriceOutOfBand, FillTooLarge, VenueRateLimited, SelfTrade, MissingSigner, ApprovalExpired, ApprovalAllowanceExceeded, ApprovalLeverageExceeded, ApprovalLimitPrice, ApprovalPriceDeviation, InvalidApprovalParams, VenueMarginInvariant`

### 1.3 Admin and guardian instructions (`instructions/venue_admin.rs`)

```rust
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct VenueParams {
    pub max_price_deviation_bps: u16, // 0..=MAX_VENUE_BAND_BPS
    pub max_oracle_age_secs: u16,     // 1..=60
    pub min_signers: u8,              // 0..=2
    pub max_fill_notional: u64,       // > 0
    pub max_window_notional: u64,     // >= max_fill_notional
    pub window_secs: u32,             // 60..=86_400
}
impl VenueParams { pub fn validate(&self) -> bool { /* exactly the ranges above */ } }
```

**`register_venue(params: VenueParams)`**

```rust
#[derive(Accounts)]
pub struct RegisterVenue<'info> {
    #[account(mut)] pub admin: Signer<'info>,
    #[account(seeds = [ProtocolConfig::SEED], bump = config.bump,
              constraint = config.admin == admin.key() @ PerpsError::Unauthorized)]
    pub config: Box<Account<'info, ProtocolConfig>>,
    #[account(seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()], bump = market.bump)]
    pub market: Box<Account<'info, Market>>,
    /// CHECK: executable, not perps-core
    #[account(constraint = venue_program.executable && venue_program.key() != crate::ID @ PerpsError::InvalidVenueParams)]
    pub venue_program: UncheckedAccount<'info>,
    #[account(seeds = [venue_program.key().as_ref()], bump, seeds::program = bpf_loader_upgradeable::ID)]
    pub venue_program_data: Box<Account<'info, ProgramData>>,
    #[account(init, payer = admin, space = Venue::LEN,
              seeds = [Venue::SEED, market.key().as_ref(), venue_program.key().as_ref()], bump)]
    pub venue: Box<Account<'info, Venue>>,
    pub system_program: Program<'info, System>,
}
```

Handler:
1. `require!(!market.is_settled(), MarketSettled)`.
2. `require!(params.validate(), InvalidVenueParams)`.
3. `authority = Pubkey::find_program_address(&[b"perps_venue", market.key().as_ref()], venue_program.key).0`.
4. Set `program_data = venue_program_data.key()` and `program_slot = venue_program_data.slot`.
5. Copy the params, set `window_start = now` and `enabled = true`, zero the stats, set `bump`.

**`update_venue(params: VenueParams, enabled: bool)`**: accounts `admin: Signer`, `config` (admin), `venue` (mut, seeds `[Venue::SEED, venue.market, venue.program]`), and `venue_program_data: Box<Account<ProgramData>>` with `address = venue.program_data`. The handler validates the params, overwrites them, sets `enabled`, and **re-pins** `program_slot = venue_program_data.slot`. After a venue upgrade this is the only way to re-enable fills. `program` and `market` never change.

**`disable_venue()`**: accounts `signer`, `config` with `config.can_pause(&signer)`, `venue` (mut). Sets `enabled = false`.

**`set_venue_fills_paused(paused: bool)`**: accounts `signer` and `config` (mut). If `paused`, it requires `can_pause`; otherwise it requires `admin`. Otherwise `Unauthorized`. The guardian can stop fills but cannot restart them.

### 1.4 User instructions (`instructions/venue_approval.rs`)

```rust
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct VenueApprovalParams {
    pub allowance: u64,
    pub max_leverage: u32,            // 1..=market.max_leverage
    pub max_buy_price: u64,           // 0 = none
    pub min_sell_price: u64,          // 0 = none
    pub max_price_deviation_bps: u16, // 0..=10_000
    pub expires_at: i64,              // now < expires_at <= now + MAX_APPROVAL_SECS
}
```

**`approve_venue(params)`**:
- Accounts:
  - `owner: Signer` (mut, pays)
  - `market: Box<Account<Market>>` with market seeds
  - `venue: Box<Account<Venue>>` with seeds `[Venue::SEED, market, venue.program]`
  - `approval: init, payer = owner, space = VenueApproval::LEN, seeds = [VenueApproval::SEED, owner, venue]`
  - `system_program`
- Validates the params and sets every field (`market = venue.market`, stats 0) → `InvalidApprovalParams`.

**`update_venue_approval(params)`**:
- Accounts: `owner: Signer`; `market`; `venue`; `approval` (mut, same seeds, `bump = approval.bump`, `has_one = owner`).
- Same validation. Overwrites the six param fields and keeps the stats.

**`revoke_venue_approval()`**:
- Accounts: `owner: Signer` (mut); `approval` (mut, seeds `[VenueApproval::SEED, owner, approval.venue]`, `has_one = owner`, `close = owner`).
- It does not depend on the venue program or the Venue account.

### 1.5 `venue_fill` (`instructions/venue_fill.rs`)

```rust
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy)]
pub struct VenueFillParams {
    pub size: u64,           // base units, > 0
    pub price: u64,          // 6 dp, > 0
    pub taker_side: u8,      // 0 Long, 1 Short; maker gets the opposite
    pub taker_leverage: u32, // 3 dp
    pub maker_leverage: u32,
}

#[derive(Accounts)]
pub struct VenueFill<'info> {
    pub venue_authority: Signer<'info>,
    #[account(mut)] pub payer: Signer<'info>,
    #[account(seeds = [ProtocolConfig::SEED], bump = config.bump,
              constraint = !config.venue_fills_paused @ PerpsError::VenueFillsPaused)]
    pub config: Box<Account<'info, ProtocolConfig>>,
    #[account(mut, seeds = [b"market", market.collateral_mint.as_ref(), market.commodity_seed()], bump = market.bump,
              constraint = !market.is_paused @ PerpsError::MarketPaused)]
    pub market: Box<Account<'info, Market>>,
    #[account(mut, seeds = [Venue::SEED, market.key().as_ref(), venue.program.as_ref()], bump = venue.bump,
              constraint = venue.authority == venue_authority.key() @ PerpsError::VenueUnauthorized,
              constraint = venue.enabled @ PerpsError::VenueDisabled)]
    pub venue: Box<Account<'info, Venue>>,
    #[account(address = venue.program_data @ PerpsError::VenueProgramChanged,
              constraint = venue_program_data.slot == venue.program_slot @ PerpsError::VenueProgramChanged)]
    pub venue_program_data: Box<Account<'info, ProgramData>>,
    /// CHECK: key pinned by the market
    #[account(constraint = pyth_price_feed.key() == market.pyth_price_feed)]
    pub pyth_price_feed: AccountInfo<'info>,

    /// CHECK: wallet; bound to its UserAccount/approval by seeds; is_signer read for min_signers
    pub taker: UncheckedAccount<'info>,
    #[account(mut, seeds = [b"user", taker.key().as_ref(), market.key().as_ref()], bump = taker_user.bump,
              constraint = taker_user.owner == taker.key())]
    pub taker_user: Box<Account<'info, UserAccount>>,
    #[account(mut, seeds = [VenueApproval::SEED, taker.key().as_ref(), venue.key().as_ref()], bump = taker_approval.bump)]
    pub taker_approval: Box<Account<'info, VenueApproval>>,
    #[account(init, payer = payer, space = Position::LEN,
              seeds = [b"vposition", taker.key().as_ref(), market.key().as_ref(), &taker_user.total_positions.to_le_bytes()], bump)]
    pub taker_position: Box<Account<'info, Position>>,

    /// CHECK: as taker. Distinct keys also prevent aliasing the two mut UserAccounts/approvals.
    #[account(constraint = maker.key() != taker.key() @ PerpsError::SelfTrade)]
    pub maker: UncheckedAccount<'info>,
    #[account(mut, seeds = [b"user", maker.key().as_ref(), market.key().as_ref()], bump = maker_user.bump,
              constraint = maker_user.owner == maker.key())]
    pub maker_user: Box<Account<'info, UserAccount>>,
    #[account(mut, seeds = [VenueApproval::SEED, maker.key().as_ref(), venue.key().as_ref()], bump = maker_approval.bump)]
    pub maker_approval: Box<Account<'info, VenueApproval>>,
    #[account(init, payer = payer, space = Position::LEN,
              seeds = [b"vmaker", taker_position.key().as_ref()], bump)]
    pub maker_position: Box<Account<'info, Position>>,

    pub system_program: Program<'info, System>,
}
```

That is 17 accounts. Only the venue program can sign for `venue_authority`, and the admin bound that program and PDA together in `Venue`. No instruction introspection is needed. The admin cannot call this directly.

**Handler, in this order.** Every failure aborts the whole instruction, so there are no partial effects. The math lives in `#[inline(never)]` helpers.

1. `now = Clock::get()?.unix_timestamp`.
2. `require!(!market.is_settled(), MarketSettled)` and `require!(market.is_in_session(now), MarketClosed)`.
3. `require!(params.size > 0 && params.price > 0, InvalidFillParams)`. Map `taker_side` to a `Side`, else `InvalidPositionSide`. `maker_side` is its opposite.
4. `require!(taker.is_signer as u8 + maker.is_signer as u8 >= venue.min_signers, MissingSigner)`.
5. `oracle = read_price_max_age(feed, market, now, venue.max_oracle_age_secs as u64)?` (staleness, confidence, floor).
6. `require!(within_band(price, oracle, venue.max_price_deviation_bps), FillPriceOutOfBand)`.
7. `notional = u64::try_from(size as u128 * price as u128 / 1e6)` → `MathOverflow`. Then `require!(notional <= venue.max_fill_notional, FillTooLarge)`.
8. Window: if `now >= venue.window_start + venue.window_secs as i64`, set `window_start = now` and `window_notional = 0`. Then `require!(window_notional + notional <= max_window_notional, VenueRateLimited)`, using checked addition.
9. `require!(market.can_increase_long_oi(size) && market.can_increase_short_oi(size), OpenInterestCapExceeded)`.
10. `take_margin(market, user, approval, side, lev, size, price, oracle, now) -> Result<u64>`, `#[inline(never)]`, runs for the taker and then the maker:
    1. `require!(lev > 0 && lev <= market.max_leverage, ExcessiveLeverage)`, then `require!(lev <= approval.max_leverage, ApprovalLeverageExceeded)`.
    2. `require!(now <= approval.expires_at, ApprovalExpired)`.
    3. Limit price: a Long requires `max_buy_price == 0 || price <= max_buy_price`; a Short requires `min_sell_price == 0 || price >= min_sell_price`. Otherwise `ApprovalLimitPrice`.
    4. Adverse deviation, when `d = approval.max_price_deviation_bps > 0`: a Long requires `price*10_000 <= oracle*(10_000+d)`; a Short requires `price*10_000 >= oracle*(10_000-d)` (u128, saturating). Otherwise `ApprovalPriceDeviation`.
    5. `c = venue_margin(market, side, size, price, oracle, lev, now)`, with `None` → `ExcessiveLeverage`.
    6. `require!(user.collateral_balance >= c, InsufficientCollateral)` and `require!(approval.allowance >= c, ApprovalAllowanceExceeded)`.
    7. `user.collateral_balance -= c` and `approval.allowance -= c`. `approval.fills += 1`. `approval.collateral_committed = collateral_committed.saturating_add(c)`. Return `c`.
11. Initialise both positions, with the same fields `open_position` sets:
    - `owner`, `market = market.key()`, `side`, `size`, `collateral = c`, `entry_price = price`, `leverage`
    - `realized_pnl = 0`, `last_funding_payment = market.cumulative_funding`, `opened_at = last_updated_at = now`
    - `status = Open`, `execution_source = Venue`, `bump` from `ctx.bumps`
    - Add a helper `Position::init_open(&mut self, …)`. `open_position` may adopt it, but that is not required: leave `open_position` untouched in this change.
12. Backstop: `let (im, _) = market.margin_ratios(now);` and for each new position `require!(pos.margin_ratio(oracle, 0) >= im, VenueMarginInvariant)`. By 2.2 this always holds; it guards against future edits.
13. Effects:
    - `long_open_interest += size` and `short_open_interest += size`
    - `market.total_positions += 2` and `market.total_trades += 1`
    - each `UserAccount`: `total_positions += 1` and `total_trades += 1`
    - `venue.window_notional += notional`, `total_fills += 1`, `total_notional` saturating
    - All checked, except where marked saturating.
14. `emit!(VenueFilled { venue, market, taker, maker, taker_position, maker_position, taker_side, size, price, oracle_price, taker_collateral, maker_collateral, timestamp: now })`. Positions are one per fill, so this event is the trade tape.

No tokens move: the collateral is already in the market Vault, and `vault.total_deposits` is unchanged.

`lib.rs` gets seven new entrypoints: `register_venue`, `update_venue`, `disable_venue`, `set_venue_fills_paused`, `approve_venue`, `update_venue_approval`, `revoke_venue_approval`, `venue_fill`. That is eight names in total, including `venue_fill`.

---

## 2. Pricing safety

### 2.1 Threats and what stops each one

| Threat | Defence |
|---|---|
| Colluders fill far off-market to extract from the vault or insurance fund | Band. Inside the band, the adverse top-up plus the IM invariant (2.2) means value only moves between the colluders. |
| Inflated `notional_value()` makes a position instantly liquidatable, so `settle_liquidation` tops up the reward from insurance | No position is born below IM at either oracle notional or fill notional, and IM > MM (enforced by `initialize_market` and `MarketConfigParams::validate`). |
| Buggy or compromised venue | Each victim is bounded by their own approval: `allowance`, leverage, limit, adverse deviation, expiry. Fills also stay inside the band and `max_fill_notional`, pass the window breaker, and stop when the slot pin trips after an upgrade. The guardian can call `disable_venue` or `set_venue_fills_paused`, or pause the market. |
| Stale MM quote picked off | `max_oracle_age_secs` on the venue and `max_price_deviation_bps` on the MM's approval. |
| MM front-runs a taker with `update_quote` | The taker's `limit_price` in mm-registry. The taker's one-shot approval also sets `max_buy_price` or `min_sell_price`, which perps-core enforces. |
| Self-trade or aliasing | `maker != taker` in perps-core, and again in each venue for a clearer error. |
| Settled, paused or out-of-session market | The existing gates. A settled market is also paused, so `MarketPaused` fires first. |
| OI | Caps are checked on both sides. A matched fill still adds gross exposure, and gross exposure bounds liquidation cascades. |

### 2.2 Invariant

For each new position, with fill price F, oracle P and size s:

- Base margin: `B = ceil(s·F/lev) ≥ IM·s·F`, guaranteed by `required_collateral`.
- Margin posted: `c = max(B, ceil(IM·s·P)) + adverse`, where `adverse = max(0, −pnl(P))`.
- The losing side's equity at P is `c + pnl = max(B, IM·s·P)`. That is at least `IM·s·P` and at least `IM·s·F`.
- The winning side's equity is `max(B, IM·s·P) + gain`, which is greater still.
- So `margin_ratio(P, 0) ≥ IM > MM`: neither position is born liquidatable.

Zero-sum:
- `unrealized_pnl` and `accrued_funding` are odd functions of side, and i128 division truncates toward zero, so `pnl_L = −pnl_S` and `funding_L = −funding_S` exactly for an identical `size` and `entry_price`.
- `gain = adverse` exactly.
- Closing both pays out `c_T + c_M − fee_T − fee_M`.
- The only route to bad debt is a later oracle gap, which `settle_insurance` handles exactly as for `open_position` positions.

Worked example (oracle $75, IM 10%, taker fee 5 bps; the taker buys 100 at the $75.03 ask, both sides at 10x):

| Quantity | Taker (long) | MM (short) |
|---|---|---|
| Base | 750_300_000 | 750_300_000 |
| `im_oracle` | 750_000_000 | 750_000_000 |
| Adverse | 3_000_000 | 0 |
| Collateral posted | **753_300_000** | **750_300_000** |

Backstop: the taker's ratio is exactly 1_000 (equal to IM) and the MM's is 1_004.

Close at $78:
- PnL is ±297_000_000 and each fee is 3_751_500.
- Payouts: taker 1_046_548_500, MM 449_548_500.
- The sum is 1_496_097_000 = 1_503_600_000 − 7_503_000.
- `insurance_fund` grows by 7_503_000.

---

## 3. Collateral

- Both parties margin from their own perps-core `UserAccount [b"user", owner, market]`, funded with the existing `deposit_collateral`.
  - The MM uses its **wallet** key, so it closes with the existing `close_position`, and the keeper liquidates its positions like any other.
- The mm-registry `mm_collateral` vault becomes a registration bond only: `register_mm`, `min_collateral`, deposit, withdraw and deregister are unchanged.
- No slashing in v1.
- Quotes and orders reserve nothing. Every bound is re-checked atomically at fill time.
  - A user who withdraws after quoting or placing only makes that fill fail.
  - That is a liveness issue for the keeper, not a solvency one.
- No netting: an MM filled on both bid and ask holds a fully margined long and a fully margined short. This is the main v1 capital cost; see section 10.

---

## 4. Fees

- No fee is charged at fill. `close_position` charges `taker_fee` on the entry notional for each leg, and that fee goes to insurance minus the referral share.
- `Market.maker_fee` stays unused.
- `MmRegistry.mm_fee` is ignored: `FillQuoteResult.fee = 0` and `total_fees` stops growing. Document the field as reserved.
- v2 option: in `take_margin`, debit a fill fee from `collateral_balance` and credit `market.insurance_fund`, with no token CPI.

---

## 5. mm-registry

### 5.1 State

`TwoSidedQuote`: add `pub leverage: u32` after `bump`. Padding goes from 32 to 28 and LEN stays 194. It holds the MM's leverage for fills of this quote, in 3 decimals.

`MarketMaker` and `MmRegistry` are unchanged.
- `inventory`, `avg_inventory_price`, `unrealized_pnl` and `realized_pnl` are never written again; document them as deprecated (always 0).
- `deregister_mm`'s `inventory == 0` check stays. It is always true because fills were never enabled.
- `collateral_locked` and `collateral_available` only change when legacy locks are released.

`pub const MAX_QUOTE_TTL_SECS: i64 = 86_400;`

### 5.2 `post_quote` and `update_quote`

- `PostQuoteParams` gains `leverage: u32`, which must be > 0. `expires_in` becomes mandatory: `0 < expires_in <= MAX_QUOTE_TTL_SECS`. Either violation → `InvalidQuoteParams`.
  - Set `quote.leverage`, `collateral_locked = 0`, and `expires_at = now + expires_in`.
  - Delete the collateral-requirement check and the `lock_collateral` call.
- `UpdateQuoteParams` gains `leverage: Option<u32>` (> 0) and `expires_in: Option<i64>` (same range; sets `expires_at = now + x`).
  - The collateral block becomes: `market_maker.unlock_collateral(quote.collateral_locked); quote.collateral_locked = 0;`.
  - This also migrates legacy quotes.
- `cancel_quote` is unchanged; it still releases legacy locks.

### 5.3 `fill_quote`

```rust
pub struct FillQuoteParams {
    pub size: u64,
    pub is_buy: bool,         // true: taker long vs ask (MM short); false: taker short vs bid (MM long)
    pub limit_price: u64,     // > 0; buy: ask <= limit; sell: bid >= limit
    pub taker_leverage: u32,
    pub allow_partial: bool,  // if size > remaining, fill remaining instead of failing
}
// FillQuoteResult unchanged: { fill_price, fill_size, fee: 0 }

#[derive(Accounts)]
pub struct FillQuote<'info> {
    #[account(mut)] pub taker: Signer<'info>,                       // also perps-core payer
    #[account(mut, seeds = [b"mm_registry", registry.market.as_ref()], bump = registry.bump,
              constraint = registry.is_trading_enabled @ MmRegistryError::TradingDisabled)]
    pub registry: Box<Account<'info, MmRegistry>>,
    #[account(mut, seeds = [b"market_maker", registry.key().as_ref(), market_maker.owner.as_ref()], bump = market_maker.bump,
              constraint = market_maker.status == MmStatus::Active @ MmRegistryError::MarketMakerNotActive)]
    pub market_maker: Box<Account<'info, MarketMaker>>,
    #[account(mut,
              constraint = quote.market_maker == market_maker.key() @ MmRegistryError::Unauthorized,
              constraint = quote.registry == registry.key() @ MmRegistryError::Unauthorized,
              constraint = quote.is_active @ MmRegistryError::QuoteNotActive)]
    pub quote: Box<Account<'info, TwoSidedQuote>>,
    /// CHECK: MM wallet = maker position owner
    #[account(address = market_maker.owner @ MmRegistryError::Unauthorized)]
    pub mm_owner: UncheckedAccount<'info>,
    /// CHECK: signs the CPI
    #[account(seeds = [b"perps_venue", registry.market.as_ref()], bump)]
    pub venue_authority: UncheckedAccount<'info>,
    /// CHECK: perps-core validates
    pub perps_config: UncheckedAccount<'info>,
    /// CHECK: perps-core validates
    #[account(mut)] pub perps_venue: UncheckedAccount<'info>,
    /// CHECK: perps-core validates
    pub venue_program_data: UncheckedAccount<'info>,
    /// CHECK: must be this registry's market; perps-core validates the rest
    #[account(mut, address = registry.market @ MmRegistryError::WrongMarket)]
    pub market: UncheckedAccount<'info>,
    /// CHECK: perps-core validates
    pub pyth_price_feed: UncheckedAccount<'info>,
    /// CHECK ×6: perps-core validates / inits
    #[account(mut)] pub taker_user: UncheckedAccount<'info>,
    #[account(mut)] pub taker_approval: UncheckedAccount<'info>,
    #[account(mut)] pub taker_position: UncheckedAccount<'info>,
    #[account(mut)] pub mm_user: UncheckedAccount<'info>,
    #[account(mut)] pub mm_approval: UncheckedAccount<'info>,
    #[account(mut)] pub mm_position: UncheckedAccount<'info>,
    pub perps_core_program: Program<'info, perps_core::program::PerpsCore>,
    pub system_program: Program<'info, System>,
}
```

That is 20 accounts. `mm_collateral_account`, `taker_token_account` and `token_program` are removed.

Never hold a typed `Account<Market>` here: the CPI mutates it.

Handler:
1. `now`. `require!(quote.is_valid(now), QuoteExpired)`. `require!(quote.leverage > 0 && quote.expires_at > 0, QuoteNotFillable)`; legacy quotes fail here.
2. `require!(taker.key() != market_maker.owner, SelfTrade)`. `require!(params.limit_price > 0, InvalidPrice)`.
3. `(price, remaining) = if is_buy { (ask_price, ask_remaining) } else { (bid_price, bid_remaining) }`. If `remaining == 0` → `FillSizeExceedsRemaining`.
4. Limit check: a buy requires `price <= limit_price`; a sell requires `price >= limit_price`. Otherwise `PriceWorseThanLimit`.
5. `fill = if size <= remaining { size } else if allow_partial { remaining } else { FillSizeExceedsRemaining }`. `require!(fill > 0 && (fill >= quote.min_fill_size || fill == remaining), FillSizeTooSmall)`.
6. `cpi_venue_fill(&ctx.accounts, bump, VenueFillParams { size: fill, price, taker_side: if is_buy {0} else {1}, taker_leverage, maker_leverage: quote.leverage })`. This is an `#[inline(never)]` fn:

   ```rust
   let m = a.registry.market; let b = [bump];
   let seeds: &[&[u8]] = &[b"perps_venue", m.as_ref(), &b];
   perps_core::cpi::venue_fill(CpiContext::new_with_signer(a.perps_core_program.to_account_info(),
       perps_core::cpi::accounts::VenueFill {
           venue_authority: a.venue_authority, payer: a.taker, config: a.perps_config, market: a.market,
           venue: a.perps_venue, venue_program_data: a.venue_program_data, pyth_price_feed: a.pyth_price_feed,
           taker: a.taker, taker_user: a.taker_user, taker_approval: a.taker_approval, taker_position: a.taker_position,
           maker: a.mm_owner, maker_user: a.mm_user, maker_approval: a.mm_approval, maker_position: a.mm_position,
           system_program: a.system_program,
       } /* each .to_account_info() */, &[seeds]), p)
   ```

7. After the CPI:
   - Subtract `fill` from the side's remaining. Set `updated_at = now`.
   - MM `total_volume += notional`, `total_fills += 1`, `last_active_at`. `registry.total_volume += notional`.
   - When both sides are 0, run the existing exhaust path: `is_active = false`, decrement both `active_quotes` counters, close the quote to `market_maker`.
   - No collateral, inventory or token logic: delete the transfer branches and the lock recompute. A fillable quote always has `collateral_locked == 0`.
8. Return `FillQuoteResult { fill_price: price, fill_size: fill, fee: 0 }`.

Delete `FILLS_ENABLED`. Keep the `FillsDisabled` variant so later codes do not shift.

`MmRegistryError`, appended: `PriceWorseThanLimit, QuoteNotFillable, SelfTrade, WrongMarket`.

Venue registration for mm-registry: `min_signers = 1`, so a venue bug cannot match two non-signing MMs.

Taker transaction built by the SDK:
1. `ComputeBudget(300_000)`
2. `[initialize_user, deposit_collateral]`, if needed
3. `approve_venue` or `update_venue_approval`, with:
   - `allowance` = the worst-case margin: `venue_margin` evaluated at oracle = price × (1 ± band), taking the max
   - `max_buy_price` / `min_sell_price` = `limit_price`
   - `max_leverage = taker_leverage`
   - `expires_at = now + 120`
4. `fill_quote`

Position PDAs: `vposition(taker, market, taker_user.total_positions + i)` for the i-th fill in the transaction, and `vmaker(that)`. A two-quote sweep fits in a legacy transaction; three or more need v0 with an address lookup table.

MM setup: `register_mm` (bond), `perps_core::initialize_user` + `deposit_collateral`, then `approve_venue(mm-registry venue)` with, for example:
- `allowance` = the risk budget
- `max_price_deviation_bps = 30`
- `expires_at` ≤ 90 days

To kill all fills at once, call `update_venue_approval { allowance: 0 }` or `revoke_venue_approval`.

---

## 6. orderbook

### 6.1 State

| Account | Change | LEN | Padding |
|---|---|---|---|
| `Order` | add `leverage: u32`, `max_fills: u8`, `fills: u8` after `bump` | 156 | 32 → 26 |
| `OrderBook` | add `matcher: Pubkey` after `bump`. It reads as default on existing books, which grants nothing. | 145 | 32 → 0 |

Constants: `MAX_ORDER_TTL_SECS: i64 = 30 * 86_400`, `MAX_FILLS: u8 = 16`.

Errors, appended: `OrderExpired, LegacyOrder, UnsupportedOrderType, SelfTrade, InvalidOrderParams, OrderNotExpired, RentDepositExhausted`.

### 6.2 Instructions

**`place_order(params: PlaceOrderParams { side, order_type, price, size, leverage: u32, expires_at: i64, max_fills: u8 })`**. This is a breaking change to the IDL; nothing off-chain calls it today.
- New account: `market: Box<Account<'info, Market>>` with `address = orderbook.market @ WrongOrderBook`.
- Checks, in order:
  - `order_type == Limit` → `UnsupportedOrderType`
  - `price > 0 && size > 0` → `InvalidOrderParams`
  - `0 < leverage <= market.max_leverage` → `InvalidOrderParams`
  - `now < expires_at <= now + MAX_ORDER_TTL_SECS` → `InvalidOrderParams`
  - `1 <= max_fills <= MAX_FILLS` → `InvalidOrderParams`
- Effects:
  - Set the fields, with `fills = 0`.
  - Transfer `max_fills as u64 * Rent::get()?.minimum_balance(perps_core::state::Position::LEN)` lamports from `owner` to `order` (`system_program::transfer`).
  - Sequence number and counters as today.

**`cancel_order()`**: add `close = owner`, which refunds the rent and the unused deposit. Otherwise as today.

**`close_expired_order()`** (permissionless):
- Accounts: `orderbook` (mut), `order` (mut, `market == orderbook.market`, `close = owner`), `owner` (mut, `address = order.owner`).
- Requires status Open or PartiallyFilled and `now > order.expires_at`, else `OrderNotExpired`.
- Decrements `total_bids` or `total_asks`.

**`set_matcher(matcher: Pubkey)`**:
- Accounts: `admin: Signer`, `config` (perps-core, `seeds::program = perps_core::ID`, `admin == signer`), `orderbook` (mut).
- Sets `orderbook.matcher`.

**`match_orders()`**:

| # | Account | Constraint |
|---|---|---|
| 1 | `matcher: Signer` (mut) | payer for both Positions |
| 2 | `config: Box<Account<ProtocolConfig>>` | `seeds::program = perps_core::ID`; also forwarded to the CPI |
| 3 | `orderbook` (mut) | seeds as today |
| 4–5 | `bid_order`, `ask_order` (mut, Box) | side and `market == orderbook.market`, as today |
| 6–7 | `bid_owner`, `ask_owner` (mut, Unchecked) | `address = order.owner`; receive closes |
| 8 | `venue_authority` (Unchecked) | `seeds = [b"perps_venue", orderbook.market]`, bump |
| 9–10 | `perps_venue` (mut), `venue_program_data` (Unchecked) | perps-core validates |
| 11 | `market` (mut, Unchecked) | `address = orderbook.market` |
| 12 | `pyth_price_feed` (Unchecked) | |
| 13–18 | `taker_user`, `taker_approval`, `taker_position`, `maker_user`, `maker_approval`, `maker_position` (mut, Unchecked) | perps-core validates |
| 19–20 | `perps_core_program`, `system_program` | |

The taker is the order with the **higher** `sequence_number`; the maker is the resting order.

Handler:
1. `require!(matcher == config.admin || (orderbook.matcher != Pubkey::default() && matcher == orderbook.matcher), Unauthorized)`.
2. Both orders must be Open or PartiallyFilled (`InvalidOrder`). Both must be `Limit` (`UnsupportedOrderType`). Both must have `leverage > 0` (`LegacyOrder`). Both must have `now <= expires_at` (`OrderExpired`). Both must have `fills < max_fills` (`RentDepositExhausted`).
3. `bid.owner != ask.owner` → `SelfTrade`. `bid.price >= ask.price` → `PriceMismatch`.
4. `(maker, taker)` by `sequence_number`. `price = maker.price`, which satisfies both limits. `taker_side = if taker is bid { 0 } else { 1 }`. `fill = min(bid_remaining, ask_remaining)`.
5. CPI `venue_fill`, `#[inline(never)]`:
   - `taker` = the taker order's owner account and `maker` = the maker order's
   - `payer = matcher`, `config = config`
   - `taker_leverage` / `maker_leverage` from the two orders
6. For each order: `filled_size += fill`, `fills += 1`, status Filled or PartiallyFilled.
7. Reimburse rent, after the CPI. `r = Rent::get()?.minimum_balance(Position::LEN)`. Move `r` lamports from each order account to `matcher` by direct lamport arithmetic; the orderbook owns the orders. Use checked arithmetic, and require the order to stay ≥ `rent.minimum_balance(Order::LEN)` (`RentDepositExhausted`).
8. For each order that is Filled, or has `fills == max_fills` (any remainder becomes `Cancelled`):
   - decrement `total_bids` or `total_asks`;
   - `order.close(owner_info)?`, which refunds the account rent plus the unused deposit.

`best_bid` and `best_ask` stay informational; document them as unmaintained.

Venue registration for orderbook: `min_signers = 0`, since both sides are resting; the approvals are the bound. Both order owners need an approval for the orderbook venue. The SDK bundles `approve_venue` with the first `place_order`.

Permissionless matching is deferred. It would be safe for funds, since perps-core bounds everything. But orders are loose accounts with no on-chain price-time priority, so an open matcher could choose which orders trade. The permissionless path is a later taker-signed `take_order`.

---

## 7. Limits, build, IDL, deploy

- **CPI depth.** Venue at height 1 → perps-core at 2 → system `create_account` ×2 at 3. The max stack height is 5, so a router on top still fits. perps-core never calls a venue, so there is no reentrancy.
- **Signer propagation.** The taker's signature carries into the CPI, so `is_signer` is true for `min_signers`. A venue can satisfy `min_signers` only with its own PDA as a party, and that PDA would need its own UserAccount, collateral and approval.
- **Compute (estimate; assert in tests).**
  - `venue_fill`: about 50–80k CU. That covers 7 stored-bump PDA checks, 2 `find_program_address` calls for init, the Pyth parse, about 9 deserialisations and 2 `create_account` calls.
  - `fill_quote` in total: about 110k. `match_orders`: about 120k.
  - The SDK requests 300k.
- **Stack (4 KB SBF frames).**
  - Every typed account in `VenueFill`, `FillQuote` and `MatchOrders` is `Box`ed, including `ProgramData`.
  - Pass-through accounts are `UncheckedAccount`.
  - `take_margin`, `venue_margin` and the CPI builders are `#[inline(never)]`.
  - Known risk: two `init`s in one `try_accounts`. If cargo-build-sbf reports a stack offset for `VenueFill`, make both positions `UncheckedAccount` with the same `seeds` constraints, and create them in an `#[inline(never)]` helper (`system_program::create_account` with `invoke_signed` + `Position::try_serialize`).
- **Transaction size.** `fill_quote` with compute budget and approval-update instructions uses about 21 unique accounts and 1 signature, about 950 bytes, under the 1232 limit.
- **`scripts/build-sbf.sh`.**
  - Loop over `perps-core prop-amm mm-registry orderbook`, in that order. In `$WORK`, mm-registry's and orderbook's `path = "../perps-core"` resolves to the stripped copy. Verify this, and that its `[workspace]` table is accepted for a path dependency.
  - Pipe `cargo-build-sbf` output through `tee "$dst/build.log"`, then `! grep -q "Stack offset" "$dst/build.log"`, so stack warnings fail the build.
- **CI (`.github/workflows/ci.yml`).** The SBF step becomes `SBF_OUT_DIR=$PWD/target/deploy cargo test -p perps-core -p prop-amm -p mm-registry -p orderbook --test integration`. Remove `pt.prefer_bpf(false)` from `programs/orderbook/tests/integration.rs`.
- **IDL and clients** (regenerate with `scripts/gen-idl.sh`; `scripts/idlgen` before D49).
  - perps-core: 8 instructions, the `Venue` and `VenueApproval` accounts, the `VenueFilled` event, `ExecutionSource::Venue`. **Ship the frontend IDL before the first fill**: Anchor TS fails to decode tag 2, and `program.account.position.all` would throw. Frontend `OnChainPosition.executionSource` gains `venue?: object`.
  - Keeper: no change. `PositionData` decodes the prefix up to `status`, and status sits at offset 133 either way.
  - `sdk/mm-registry` `buildFillQuoteIx` drops `mmCollateralAccount`, `takerTokenAccount` and `tokenProgram`. It gains the perps accounts, PDA helpers (`venue`, `perps_venue` authority, ProgramData, `venue_approval`, `vposition`, `vmaker`), the one-shot approval builder and the compute-budget instruction. `PostQuoteParams` / `UpdateQuoteParams` / `TwoSidedQuote` types gain `leverage` and `expires_in`.
  - Frontend `utils/pda.ts` adds `vposition` and `vmaker`.
- **Deploy order.**
  1. Upgrade perps-core. It is inert with no Venue.
  2. Ship the frontend IDL.
  3. Upgrade mm-registry and orderbook. Fills fail with `AccountNotInitialized` on the Venue until it is registered.
  4. `register_venue` per (market, program), **after** the venue deploy so the slot pin is current. Defaults:
     - mm-registry: band 50 bps, oracle age 15 s, `min_signers` 1, $50k per fill, $1M per 3_600 s window
     - orderbook: band 100 bps, oracle age 30 s, `min_signers` 0, same caps
  5. `set_matcher`.
  6. MMs cancel legacy quotes, deposit into perps-core, approve, and repost.

  Every later venue upgrade needs `update_venue` to re-pin. It can go in the same transaction as the final loader `Upgrade` instruction.

  Rollback: `disable_venue`, `set_venue_fills_paused(true)` or `pause_market`; the guardian can trigger all three.

---

## 8. Test plan (solana-program-test, in-process; the same tests on SBF)

### 8.1 Harness (`programs/perps-core/tests/support/market_env.rs`)

- Expose `oracle` and `mint` on `Base`.
- `set_price(base, dollars)`: rewrite the Pyth account with the current clock `timestamp` (it is 0 today). `set_time(base, ts)`: warp the clock and refresh the feed.
- `trader(base, usdc) -> Trader { kp, tokens, user }`: token account, `initialize_user`, `deposit_collateral`.
- `add_venue_program_data(base, program_id, slot)`: `set_account` a fake `UpgradeableLoaderState::ProgramData` at `[program_id]` under the upgradeable loader. `bump_program_slot` rewrites it.
- `register_venue(base, program_id, VenueParams)`, `approve(base, trader, venue, VenueApprovalParams)`, `revoke(...)`.
- `vposition(owner, market, n)`, `vmaker(taker_pos)`, `close(base, trader, pos)`, `liquidate(base, keeper, owner, pos)`, `update_funding`.
- `load<T: AccountDeserialize>`, `token_balance`, `vault_token_balance`.
- `assert_conservation(base, mark)`: vault tokens equal the sum of free balances, plus the sum over open positions of (collateral + PnL at mark + accrued funding), plus `insurance_fund`, within 1 unit per position. This holds when every position is venue-opened.
- Switch `programs/mm-registry/tests/integration.rs` to `#[path]`-include this harness, as orderbook already does.

### 8.2 perps-core

The tests use a **relay venue**: a native processor in `tests/integration.rs`, added with `pt.add_builtin_program("relay", RELAY_ID, processor!(relay))` so it also runs under `SBF_OUT_DIR`. It forwards arbitrary `venue_fill` data and accounts and `invoke_signed`s `[b"perps_venue", market, bump]`. This models a fully compromised venue.

Unit tests:
- `within_band`: edge, ±1, band 0.
- `venue_margin` grid over (F, P, lev, weekend window): equity at P ≥ IM×s×P and ≥ IM×s×F. Rounding is up. At F == P the result equals `required_collateral`.
- Odd-symmetry property tests for `unrealized_pnl` and `accrued_funding`.
- Window reset.
- Size pins: `ProtocolConfig::LEN == 201` with `8 + ser + 63`; `Position::LEN == 168`; `Venue::LEN == 267` with `8 + ser + 64`; `VenueApproval::LEN == 191` with `8 + ser + 32`.

Integration tests:
1. `venue_fill_opens_matched_positions`. Oracle $75, fill 100 @ $75.03 at 10x/10x.
   - Collateral is 753_300_000 / 750_300_000.
   - Sides, `entry_price`, `execution_source == Venue`, PDAs `vposition(taker, 0)` and `vmaker`.
   - Balances and allowances debited; `fills == 1`.
   - Both OI sides +100; `total_positions` +2.
   - Vault token balance unchanged.
2. `round_trip_is_zero_sum`. Close both at $78: payouts 1_046_548_500 and 449_548_500; insurance +7_503_000; `assert_conservation`. Repeat with an `update_funding` in between and check `funding_T == −funding_M`.
3. `venue_positions_liquidate`. The MM short at 10x, oracle to $79: `liquidate` succeeds with reward 3_751_500. The taker then closes and is paid in full.
4. `not_born_liquidatable`. At the band edge, `liquidate` on either new position returns `NotLiquidatable`.
5. `concurrent_fills_do_not_race`.
   - An `open_position` by a third party lands between deriving and sending; the fill still succeeds.
   - Two fills by different takers against the same maker both succeed.
   - A stale taker index gives `ConstraintSeeds`.
6. Rejections. Each asserts the exact code, that both UserAccounts and approvals are unchanged, and that neither position account exists:
   - an unregistered relay (`AccountNotInitialized`)
   - a keypair as `venue_authority` (`VenueUnauthorized`)
   - venue disabled by the guardian (`VenueDisabled`); the guardian cannot re-enable it
   - `set_venue_fills_paused(true)` by the guardian (`VenueFillsPaused`); the guardian cannot unpause
   - slot bumped (`VenueProgramChanged`), then `update_venue` re-pins and the fill succeeds
   - band +1 unit, both directions (`FillPriceOutOfBand`)
   - oracle older than the venue age but under 60 s (`StaleOraclePrice`), while `open_position` still succeeds
   - no approval; an approval for another venue; an expired approval (`ApprovalExpired`)
   - allowance 1 unit short (`ApprovalAllowanceExceeded`)
   - leverage above the approval (`ApprovalLeverageExceeded`) or above IM (`ExcessiveLeverage`)
   - `max_buy_price` / `min_sell_price` (`ApprovalLimitPrice`)
   - adverse deviation: oracle $75.30 vs ask $75.03 with the MM at 30 bps (`ApprovalPriceDeviation`)
   - self-trade (`SelfTrade`)
   - `min_signers = 1` with neither owner signing (`MissingSigner`)
   - paused or settled market (`MarketPaused`); Saturday on the CME schedule (`MarketClosed`)
   - OI headroom on one side only (`OpenInterestCapExceeded`)
   - one side short of collateral (`InsufficientCollateral`)
   - over `max_fill_notional` (`FillTooLarge`)
   - window full (`VenueRateLimited`), then warp past the window and the fill succeeds
   - size or price 0 (`InvalidFillParams`)
   - after revoke the fill fails and the rent is refunded
7. Admin:
   - `register_venue`, `update_venue` and `set_venue_fills_paused(false)` reject non-admins.
   - Registering `perps_core::ID` or a non-executable account fails (`InvalidVenueParams`).
   - Each param out of range fails (`InvalidVenueParams`).
   - The stored `authority` equals the relay's PDA.
8. Regression: every existing perps-core test passes unchanged.

### 8.3 mm-registry

Replaces `fills_are_disabled_and_move_no_funds`.

Setup:
- MM: $1,000 bond, $1,000 in perps-core, approval `max_price_deviation_bps = 30`.
- Quote: $74.97 / $75.03, 10 contracts per side, 10x, TTL 300.
- Taker: $1,000 in perps-core, plus a one-shot approval in the same transaction.

Tests:
1. A buy opens taker long / MM short at $75.03. A sell opens the reverse at $74.97. The bond vault and both SPL balances are unchanged; the taker's lamports fall by 2 × Position rent plus the transaction fee.
2. Partial fills create 2 positions each. Exhausting both sides closes the quote and decrements `active_quotes`. A tail below `min_fill_size` equal to the remainder is accepted. `allow_partial` fills exactly the remainder.
3. `update_quote` raises the ask to $75.50 in an earlier transaction; a fill with limit $75.03 fails with `PriceWorseThanLimit`.
4. Stale quote: oracle $75.30 → `ApprovalPriceDeviation`.
5. Rejections:
   - expired (`QuoteExpired`); legacy with leverage 0 or `expires_at` 0 (`QuoteNotFillable`), while still cancellable
   - MM not Active; trading disabled
   - quote from another registry (`Unauthorized`)
   - `market != registry.market` (`WrongMarket`)
   - taker is the MM (`SelfTrade`)
   - MM not approved
   - MM's perps-core balance too low (`InsufficientCollateral`)
6. Substitution: another approved user's `mm_user` / `mm_owner` passed as the MM side is rejected by `address = market_maker.owner` and the perps-core seeds.
7. The MM closes with plain `close_position`; a third party liquidates an MM position.
8. `deregister_mm` with open perps positions succeeds; the bond is refunded and the positions stay closable.
9. A legacy quote with `collateral_locked > 0` is migrated by `update_quote { leverage, expires_in }`; the lock is released.
10. SBF only: `fill_quote` uses < 200_000 CU (`simulate_transaction` units consumed) and builds with no stack warnings.

### 8.4 orderbook

Replaces the "only the live admin matches" test, keeping its assertions.

Setup: oracle $75, band 100 bps; alice and bob each deposit $1,000 and approve the orderbook venue.

1. Bob asks 10 @ $75.00 at 5x first; alice bids 10 @ $75.10 at 5x.
   - They match at $75.00. Each posts 150_000_000. Alice is the taker (`vposition(alice, 0)`); bob holds the `vmaker` position.
   - Both orders are closed and their owners refunded. The matcher's lamports are net-neutral apart from the transaction fee.
2. The reverse placement order executes at the bid price.
3. Adverse resting price: bob asks $76 first, alice bids $76.50, oracle $75.
   - Alice posts 162_000_000 and bob 152_000_000.
   - Closing both at $75 pays 151_620_000 and 161_620_000; the sum equals the collateral minus 760_000 of fees.
4. Partial: bid 10 against ask 4, then ask 6. Statuses are Partial then Filled. The keeper is reimbursed one rent per order per fill.
5. `max_fills = 1`: after one partial fill the order is cancelled and closed with its remainder.
6. Rejections: not crossing (`PriceMismatch`); expired (`OrderExpired`); self-trade; legacy leverage 0 (written with `set_account`, `LegacyOrder`); a non-Limit legacy order; a random matcher (`Unauthorized`); the old admin after `rotate_admin`; `market != orderbook.market`.
7. `set_matcher`: the hot key matches. A non-admin cannot call `set_matcher`.
8. `place_order` rejects non-Limit types, zero price, size or leverage, leverage above max, a bad expiry, and `max_fills` of 0 or 17. The deposit is transferred.
9. `close_expired_order` called by a stranger refunds the owner; before expiry it fails with `OrderNotExpired`. `cancel_order` refunds the deposit.
10. Bob withdraws his collateral after placing; the match fails atomically and both orders are unchanged.
11. Close and liquidate positions opened through the book. SBF only: `match_orders` uses < 200_000 CU.

---

## 9. vamm (and prop-amm)

- Do not register either one as a Venue. Without a `Venue` PDA a program physically cannot open positions.
- vamm has no consenting counterparty account, and its x·y=k price is not anchored to the oracle.
- Mark vamm deprecated in `Anchor.toml` and the README, drop it from production deploys, and delete it in a follow-up.
- If curve liquidity is wanted, run it as a bot. The bot posts quotes through mm-registry from its own perps-core UserAccount and approval, so its fills are real and margined.

## 10. Product owner decisions (recommended default in bold)

1. **Require a VenueApproval for signing takers too.** The alternative, signature-only consent, saves one instruction and about 0.0022 SOL of rent (refundable). The cost is that the taker's size, price and leverage are then trusted to the venue program.
2. Who pays position rent in `fill_quote`: **the taker pays for both positions, about 0.0041 SOL per fill.** The alternative is mm-ux's MM rent buffer, where the MM pays for its own position: it adds `withdraw_rent_buffer` and a lamport debit from the `market_maker` PDA.
3. Reclaiming position rent: **deferred.** Closing Position accounts would erase the closed-position history the frontend reads from chain (`useOnChainPositions.ts:172`). It needs an indexer first.
4. Fees at fill: **none in v1.** The close fee applies to both legs.
5. Quotes lock nothing: **yes.** The alternative keeps minimal's 10×-bond quoting-capacity limit as anti-spoofing.
6. Pinning the venue program's slot: **on.** Every venue upgrade then needs an admin `update_venue`. Turning it off saves one account per fill.
7. Default bands, oracle ages and notional caps: **as in section 7.**
8. Matching: **admin or `OrderBook.matcher` only.** This uses all 32 bytes of OrderBook padding; the alternative puts a global matcher in `ProtocolConfig` padding.
9. OI caps count both legs of a matched fill: **yes.** Net-OI caps can come later.
10. No position netting: **accepted for v1.** A flat MM holds a fully margined long and short, pays two close fees, and the losing leg can be liquidated. Proposed follow-ups:
    - v1.1: same-side increase into an existing position.
    - v2: reduce or flip at the fill price. At that point, charge `maker_fee` at fill.

---

## 11. AMENDMENTS (binding; they override anything above that conflicts)

These come from a red-team pass over the spec.

**A1. No venue-wide notional window.** Two colluding wallets could fill it with wash trades and shut the venue down. Remove `max_window_notional`, `window_start`, `window_notional`, `window_secs` and `VenueRateLimited` from `Venue` and `VenueParams`, and add their bytes to Venue padding. Keep `max_fill_notional` as a per-fill sanity cap. The emergency stops are guardian `disable_venue`, `set_venue_fills_paused` and `pause_market`.

**A2. Separate approvals for each role.** Seed `VenueApproval` as `[b"venue_approval", owner, venue, &[role]]`, with `role` 0 = taker and 1 = maker. Store `role: u8` in the account, taking the byte from padding. `venue_fill` loads `taker_approval` with role 0 and `maker_approval` with role 1. `approve_venue`, `update_venue_approval` and `revoke_venue_approval` take the role as a param, or read it from the account. In the orderbook, the taker/maker split in `match_orders` decides which role's approval each order owner needs. An order owner may therefore need both approvals, and the SDK creates both.

**A3. `venue_fill` never touches `market.total_positions`.** It is the PDA nonce for `open_position`. Bumping it would break every `open_position` in flight. Leave `UserAccount.total_positions` and `total_trades` updates as they are, since they are stats only. `market.total_trades += 1` is fine.

**A4. The taker position PDA comes from a nonce the caller picks.** Add `position_nonce: u64` to `VenueFillParams`. Seed the taker position as `[b"vposition", taker, market, position_nonce.to_le_bytes()]` and the maker position as `[b"vmaker", taker_position]`. `init` rejects a reused nonce. Clients pick a random u64. mm-registry `FillQuoteParams` gains `position_nonce: u64` and passes it through. For the orderbook, use `[bid_order.key() XOR …]`. Simpler: `match_orders` takes a `position_nonce: u64` arg from the matcher. Tests must check two things. First, a fill in which W is the maker does not invalidate W's own pre-derived taker fill. Second, a venue fill landing between deriving and sending an `open_position` does not break that `open_position`.

**A5. Orderbook `min_fill_size`.** Add `min_fill_size: u64` to `Order`, from padding, set in `place_order`. It must be `<= size`, and 0 means size/max_fills. `match_orders` requires `fill >= min_fill_size || fill == remaining` for **both** orders.

**A6. Test 8.4 #3 must stay inside the band.** Use oracle $75, bob asking $75.60 first, alice bidding $76.00, fill at $75.60. Recompute the numbers; do not loosen the band check.

**A7. Zero-sum holds only while both legs are open.** Once one leg is closed or liquidated, the other has the vault as its counterparty, like any `open_position` position. Assert conservation only while every pair is intact, or at the closing mark.

**A8. Fallback if a stack overflow forces hand-rolled position creation.** Copy Anchor 0.28's `init`: `create_account` if lamports == 0, otherwise transfer the shortfall, then allocate and assign with `invoke_signed`. Prefer Box plus `init` first. Only fall back if `cargo-build-sbf` actually reports a stack-offset error.

**A9. The slot pin needs an expected value.** `register_venue` and `update_venue` take `expected_program_slot: u64`, and require `venue_program_data.slot == expected_program_slot`, otherwise `InvalidVenueParams`.

**A10. Signers.** Anchor CPI metas mark UncheckedAccount `taker`/`maker` as non-signers. A party counts as a signer in perps-core only if the venue's meta marks it so, or if the same key is also in a Signer slot. In mm-registry, the taker is also `payer`, so `min_signers = 1` works. Document this in code. Test that the mm-registry venue rejects a fill CPI'd with neither party signing; the relay test covers this.

**A11.** Register the relay test program with `pt.add_builtin_program("relay", RELAY_ID, processor!(relay).unwrap())`.

**A12. `build-sbf.sh`.** Capture stderr too: `2>&1 | tee build.log`, then `! grep -q "Stack offset" build.log`.

**A13. Product decisions.** Take the section 10 defaults (all bold choices).

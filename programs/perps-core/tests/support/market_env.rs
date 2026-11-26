//! Shared harness for perps-core's venue tests and the programs that sit beside
//! perps-core (orderbook, vamm, mm-registry): a perps-core config + OIL market
//! whose admin is the upgrade authority, with a separate guardian, a fresh
//! oracle and helpers for traders, venues, approvals and positions.
//! Included with `#[path]` from those crates' integration tests.

use anchor_lang::{AccountDeserialize, AccountSerialize, InstructionData, ToAccountMetas};
use perps_core::errors::PerpsError;
use perps_core::instructions::{
    InitializeMarketParams, MarketConfigParams, OpenPositionParams, ProtocolRolesParams, VenueApprovalParams, VenueParams,
};
use perps_core::state::{LpPool, LpPosition, Market, PoolParams, Position, PositionStatus, UserAccount, Venue};
use perps_core::pyth_receiver::{set_publish_times, PriceUpdateV2View};
pub use solana_program_test::{BanksClientError, ProgramTest, ProgramTestContext};
#[allow(unused_imports)]
pub use solana_sdk::{
    account::{Account, AccountSharedData},
    bpf_loader_upgradeable::{self, UpgradeableLoaderState},
    clock::Clock,
    compute_budget::ComputeBudgetInstruction,
    instruction::{AccountMeta, Instruction, InstructionError},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction, system_program, sysvar,
    transaction::{Transaction, TransactionError},
};

/// Stands in for the Pyth Solana Receiver: owner of the test feeds and the
/// configured `oracle_program`.
pub const PYTH_PROGRAM: Pubkey = Pubkey::new_from_array([0x50; 32]);
/// The OIL market's Pyth feed id: Commodities.USOILSPOT's
/// (925ca92f…b3e6; docs/lp-pool.md D47).
pub const OIL_FEED_ID: [u8; 32] = [
    0x92, 0x5c, 0xa9, 0x2f, 0xf0, 0x05, 0xae, 0x94, 0x3c, 0x15, 0x8e, 0x35, 0x63, 0xf5, 0x96, 0x98,
    0xce, 0x7e, 0x75, 0xc5, 0xa8, 0xc8, 0xdd, 0x43, 0x30, 0x3a, 0x0a, 0x15, 0x48, 0x87, 0xb3, 0xe6,
];
pub const USDC: u64 = 1_000_000;
pub const CONTRACT: u64 = 1_000_000;
pub const OIL_PRICE: f64 = 75.0;
/// LP cash the protocol seeds every market's pool with (docs/lp-pool.md §15.2).
pub const LP_SEED: u64 = 1_000_000 * USDC;

// Fri 2026-11-13 17:00 ET (22:00 UTC) is a weekly close on the CME schedule.
pub const FRIDAY_CLOSE: i64 = 1_794_607_200;
/// Wed 2026-11-11 12:00 ET: in session, outside any pre-close window. `start` sets the clock here.
pub const WEDNESDAY: i64 = 1_794_416_400;
pub const SATURDAY: i64 = FRIDAY_CLOSE + 86_400;

pub fn perps_process(program_id: &Pubkey, accounts: &[solana_sdk::account_info::AccountInfo], data: &[u8]) -> solana_sdk::entrypoint::ProgramResult {
    // Anchor 1.2's entry wants one lifetime for the id, the accounts and the data
    let program_id: &Pubkey = Box::leak(Box::new(*program_id));
    let data: &[u8] = Box::leak(data.to_vec().into_boxed_slice());
    let accounts = Box::leak(Box::new(accounts.to_vec()));
    perps_core::entry(program_id, accounts, data)
}

pub fn pda(seeds: &[&[u8]], program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(seeds, program).0
}

pub fn config_pda() -> Pubkey {
    pda(&[b"config"], &perps_core::ID)
}

pub fn user_pda(owner: &Pubkey, market: &Pubkey) -> Pubkey {
    pda(&[b"user", owner.as_ref(), market.as_ref()], &perps_core::ID)
}

/// `[b"position", owner, market, market.total_positions]`: open_position's PDA.
pub fn position_pda(owner: &Pubkey, market: &Pubkey, index: u64) -> Pubkey {
    pda(&[b"position", owner.as_ref(), market.as_ref(), &index.to_le_bytes()], &perps_core::ID)
}

pub fn vault_pda(market: &Pubkey) -> Pubkey {
    pda(&[b"vault", market.as_ref()], &perps_core::ID)
}

pub fn vault_token_pda(market: &Pubkey) -> Pubkey {
    pda(&[b"vault_token", market.as_ref()], &perps_core::ID)
}

/// `[b"lp_pool", market]`.
pub fn lp_pool_pda(market: &Pubkey) -> Pubkey {
    pda(&[LpPool::SEED, market.as_ref()], &perps_core::ID)
}

/// `[b"lp_position", lp_pool, owner, &[class]]`.
pub fn lp_position_pda(lp_pool: &Pubkey, owner: &Pubkey, class: u8) -> Pubkey {
    pda(&[LpPosition::SEED, lp_pool.as_ref(), owner.as_ref(), &[class]], &perps_core::ID)
}

/// The upgradeable loader's ProgramData address for `program`.
pub fn program_data_pda(program: &Pubkey) -> Pubkey {
    pda(&[program.as_ref()], &bpf_loader_upgradeable::id())
}

/// perps-core's Venue record for `program` in `market`.
pub fn venue_pda(market: &Pubkey, program: &Pubkey) -> Pubkey {
    pda(&[b"venue", market.as_ref(), program.as_ref()], &perps_core::ID)
}

/// The PDA `program` signs venue_fill with: `[b"perps_venue", market]` under `program`.
pub fn venue_authority(market: &Pubkey, program: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"perps_venue", market.as_ref()], program)
}

pub const ROLE_TAKER: u8 = perps_core::state::ROLE_TAKER;
pub const ROLE_MAKER: u8 = perps_core::state::ROLE_MAKER;
pub const SIDE_LONG: u8 = perps_core::state::SIDE_LONG;
pub const SIDE_SHORT: u8 = perps_core::state::SIDE_SHORT;

pub fn approval_pda(owner: &Pubkey, venue: &Pubkey, role: u8) -> Pubkey {
    pda(&[b"venue_approval", owner.as_ref(), venue.as_ref(), &[role]], &perps_core::ID)
}

/// Taker position of a venue fill: `[b"vposition", taker, market, nonce]`.
pub fn vposition(owner: &Pubkey, market: &Pubkey, nonce: u64) -> Pubkey {
    pda(&[b"vposition", owner.as_ref(), market.as_ref(), &nonce.to_le_bytes()], &perps_core::ID)
}

/// Maker position of a venue fill: `[b"vmaker", taker_position]`.
pub fn vmaker(taker_position: &Pubkey) -> Pubkey {
    pda(&[b"vmaker", taker_position.as_ref()], &perps_core::ID)
}

pub fn ix(program_id: Pubkey, accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    Instruction { program_id, accounts: accounts.to_account_metas(None), data: data.data() }
}

fn perps_ix(accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
    ix(perps_core::ID, accounts, data)
}

pub fn compute_budget_ix(units: u32) -> Instruction {
    ComputeBudgetInstruction::set_compute_unit_limit(units)
}

/// Whether the tests run against SBF binaries (`SBF_OUT_DIR` set).
pub fn is_sbf() -> bool {
    std::env::var("SBF_OUT_DIR").is_ok() || std::env::var("BPF_OUT_DIR").is_ok()
}

pub async fn send(ctx: &mut ProgramTestContext, ixs: &[Instruction], signers: &[&Keypair]) -> Result<(), BanksClientError> {
    let payer = ctx.payer.insecure_clone();
    let mut all: Vec<&Keypair> = vec![&payer];
    all.extend_from_slice(signers);
    let blockhash = ctx.get_new_latest_blockhash().await.unwrap();
    let result = ctx.banks_client.process_transaction(Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &all, blockhash)).await;
    if result.is_ok() {
        watch_buckets(ctx, ixs).await;
    }
    result
}

// --- §11 bucket invariant, checked after every transaction --------------------------
//
// Every successful transaction sent through `send` (and the other harnesses'
// senders that call `watch_buckets`) is followed by a check of each LP pool it
// touched (directly, through its market, or through its vault token account):
//
// - `surplus = vault tokens − Σ buckets` is ≥ 0 (§11) and is exactly what it
//   was after the previous transaction, so every instruction is zero-sum
//   across the buckets plus the tokens it moved. Only a direct SPL transfer to
//   the vault (a top-level spl-token instruction naming it) or a `patch` may
//   move it; both re-baseline.
// - `free_collateral` == Σ UserAccount.collateral_balance and
//   `position_collateral`, the entry / funding aggregates, `open_positions`
//   and both open interests == the sums over open positions, and each user's
//   `house_entry_notional` == Σ `opened_notional` of its open house positions,
//   over every UserAccount and Position any transaction has named. A test that
//   writes those sums directly (`patch`) turns this part off for that pool.

#[cfg(test)]
mod bucket_watch {
    use super::*;
    use perps_core::state::ExecutionSource;
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};

    #[derive(Default)]
    pub struct Watch {
        pub market: Pubkey,
        pub surplus: Option<i128>,
        pub users: BTreeSet<Pubkey>,
        pub positions: BTreeSet<Pubkey>,
        pub sums_off: bool,
        /// Transactions checked
        pub checks: u64,
    }

    thread_local! {
        pub static WATCH: RefCell<BTreeMap<Pubkey, Watch>> = RefCell::new(BTreeMap::new());
    }

    async fn fetch(ctx: &mut ProgramTestContext, address: Pubkey) -> Option<Account> {
        ctx.banks_client.get_account(address).await.unwrap()
    }

    async fn fetch_as<T: AccountDeserialize>(ctx: &mut ProgramTestContext, address: Pubkey) -> Option<T> {
        let a = fetch(ctx, address).await?;
        if a.owner != perps_core::ID {
            return None;
        }
        T::try_deserialize(&mut a.data.as_slice()).ok()
    }

    async fn token_amount(ctx: &mut ProgramTestContext, address: Pubkey) -> u64 {
        fetch(ctx, address).await.map(|a| spl_token::state::Account::unpack(&a.data).unwrap().amount).unwrap_or(0)
    }

    pub async fn surplus(ctx: &mut ProgramTestContext, pool_key: Pubkey) -> Option<(i128, LpPool, Market)> {
        let p: LpPool = fetch_as(ctx, pool_key).await?;
        let m: Market = fetch_as(ctx, p.market).await?;
        let vault = token_amount(ctx, vault_token_pda(&p.market)).await as i128;
        Some((vault - perps_core::state::bucket_total(&p, &m) as i128, p, m))
    }

    /// The sums of §1.2 over the watched accounts, or a description of the first mismatch.
    pub async fn check_sums(ctx: &mut ProgramTestContext, p: &LpPool, m: &Market, users: &BTreeSet<Pubkey>, positions: &BTreeSet<Pubkey>) -> Result<(), String> {
        let mut free = 0u128;
        let mut house: BTreeMap<Pubkey, u128> = BTreeMap::new();
        let mut owners: BTreeMap<Pubkey, Pubkey> = BTreeMap::new();
        for &u in users {
            if let Some(acct) = fetch_as::<UserAccount>(ctx, u).await {
                free += acct.collateral_balance as u128;
                house.insert(acct.owner, 0);
                owners.insert(acct.owner, u);
            }
        }
        let (mut open, mut pc, mut le, mut se, mut lf, mut sf, mut loi, mut soi) = (0u64, 0u128, 0u128, 0u128, 0i128, 0i128, 0u128, 0u128);
        for &a in positions {
            let q: Position = match fetch_as(ctx, a).await {
                Some(q) => q,
                None => continue,
            };
            if q.status != PositionStatus::Open {
                continue;
            }
            open += 1;
            pc += q.collateral as u128;
            let (e, f) = (q.size as u128 * q.entry_price as u128, q.size as i128 * q.last_funding_payment as i128);
            match q.side {
                perps_core::state::Side::Long => { le += e; lf += f; loi += q.size as u128; }
                perps_core::state::Side::Short => { se += e; sf += f; soi += q.size as u128; }
            }
            if q.execution_source == ExecutionSource::OrderBook {
                *house.entry(q.owner).or_insert(0) += q.opened_notional(q.size) as u128;
            }
        }
        if free != p.free_collateral as u128 {
            return Err(format!("free_collateral {} != Σ users {}", p.free_collateral, free));
        }
        if (open, pc) != (p.open_positions, p.position_collateral as u128) {
            return Err(format!("(open_positions, position_collateral) {:?} != Σ positions {:?}", (p.open_positions, p.position_collateral), (open, pc)));
        }
        if (le, se, lf, sf) != (p.long_entry_sum, p.short_entry_sum, p.long_funding_sum, p.short_funding_sum) {
            return Err(format!("aggregates {:?} != Σ positions {:?}", (p.long_entry_sum, p.short_entry_sum, p.long_funding_sum, p.short_funding_sum), (le, se, lf, sf)));
        }
        if (loi, soi) != (m.long_open_interest as u128, m.short_open_interest as u128) {
            return Err(format!("open interest {:?} != Σ positions {:?}", (m.long_open_interest, m.short_open_interest), (loi, soi)));
        }
        for (owner, h) in house {
            if let Some(&u) = owners.get(&owner) {
                let acct: UserAccount = fetch_as(ctx, u).await.unwrap();
                if acct.house_entry_notional as u128 != h {
                    return Err(format!("house_entry_notional of {} is {} != Σ opened_notional {}", owner, acct.house_entry_notional, h));
                }
            }
        }
        Ok(())
    }
}

/// Check the §11 bucket invariant (and the §1.2 sums) for every pool `ixs`
/// touched, after they succeeded. See the comment above `bucket_watch`.
#[cfg(test)]
pub async fn watch_buckets(ctx: &mut ProgramTestContext, ixs: &[Instruction]) {
    use bucket_watch::*;
    let mut keys: Vec<Pubkey> = Vec::new();
    for i in ixs {
        for k in std::iter::once(&i.program_id).chain(i.accounts.iter().map(|a| &a.pubkey)) {
            if !keys.contains(k) {
                keys.push(*k);
            }
        }
    }
    // Pools touched: named, or through their market or vault token account
    let mut pools: Vec<Pubkey> = Vec::new();
    let mut found_positions: Vec<(Pubkey, Pubkey)> = Vec::new();
    let mut found_users: Vec<(Pubkey, Pubkey)> = Vec::new();
    for &k in &keys {
        let acct = match ctx.banks_client.get_account(k).await.unwrap() {
            Some(a) if a.owner == perps_core::ID => a,
            _ => {
                let pool = WATCH.with(|w| w.borrow().iter().find(|(_, x)| vault_token_pda(&x.market) == k).map(|(p, _)| *p));
                if let Some(p) = pool {
                    if !pools.contains(&p) {
                        pools.push(p);
                    }
                }
                continue;
            }
        };
        let mut data = acct.data.as_slice();
        if LpPool::try_deserialize(&mut data).is_ok() {
            if !pools.contains(&k) {
                pools.push(k);
            }
        } else if Market::try_deserialize(&mut acct.data.as_slice()).is_ok() {
            let p = lp_pool_pda(&k);
            if ctx.banks_client.get_account(p).await.unwrap().is_some() && !pools.contains(&p) {
                pools.push(p);
            }
        } else if let Ok(q) = Position::try_deserialize(&mut acct.data.as_slice()) {
            found_positions.push((lp_pool_pda(&q.market), k));
        } else if let Ok(u) = UserAccount::try_deserialize(&mut acct.data.as_slice()) {
            found_users.push((u.owner, k));
        }
    }
    for (pool, position) in found_positions {
        if !pools.contains(&pool) && ctx.banks_client.get_account(pool).await.unwrap().is_some() {
            pools.push(pool);
        }
        WATCH.with(|w| w.borrow_mut().entry(pool).or_default().positions.insert(position));
    }
    for &pool in &pools {
        let market = match surplus(ctx, pool).await {
            Some((_, p, _)) => p.market,
            None => continue,
        };
        WATCH.with(|w| w.borrow_mut().entry(pool).or_default().market = market);
    }
    for (owner, address) in found_users {
        WATCH.with(|w| {
            let mut w = w.borrow_mut();
            for x in w.values_mut() {
                if user_pda(&owner, &x.market) == address {
                    x.users.insert(address);
                }
            }
        });
        // A user account is named with its market, so its pool is in `pools`
    }
    for pool in pools {
        let (s, p, m) = match surplus(ctx, pool).await {
            Some(x) => x,
            None => continue,
        };
        let vt = vault_token_pda(&p.market);
        let direct = ixs.iter().any(|i| i.program_id == spl_token::ID && i.accounts.iter().any(|a| a.pubkey == vt));
        let (prev, users, positions, sums_off) = WATCH.with(|w| {
            let w = w.borrow();
            let x = &w[&pool];
            (x.surplus, x.users.clone(), x.positions.clone(), x.sums_off)
        });
        match prev {
            Some(prev) if !direct => assert_eq!(s, prev, "§11: vault − Σ buckets moved from {} to {} (pool {:?}, market {:?})", prev, s, (p.free_collateral, p.position_collateral, p.cash, p.treasury), m.insurance_fund),
            _ => assert!(s >= 0 || direct, "§11: vault < Σ buckets by {}", -s),
        }
        WATCH.with(|w| {
            let mut w = w.borrow_mut();
            let x = w.get_mut(&pool).unwrap();
            x.surplus = Some(s);
            x.checks += 1;
        });
        if !sums_off {
            if let Err(e) = check_sums(ctx, &p, &m, &users, &positions).await {
                panic!("§1.2 sums after a transaction: {}", e);
            }
        }
    }
}

#[cfg(not(test))]
pub async fn watch_buckets(_ctx: &mut ProgramTestContext, _ixs: &[Instruction]) {}

/// After a test writes state directly: re-read every watched pool's surplus,
/// and stop checking the §1.2 sums of any pool whose sums no longer hold.
#[cfg(test)]
pub async fn rebaseline_buckets(ctx: &mut ProgramTestContext) {
    use bucket_watch::*;
    let pools: Vec<Pubkey> = WATCH.with(|w| w.borrow().keys().copied().collect());
    for pool in pools {
        let (s, p, m) = match surplus(ctx, pool).await {
            Some(x) => x,
            None => continue,
        };
        let (users, positions) = WATCH.with(|w| {
            let w = w.borrow();
            (w[&pool].users.clone(), w[&pool].positions.clone())
        });
        let off = check_sums(ctx, &p, &m, &users, &positions).await.is_err();
        WATCH.with(|w| {
            let mut w = w.borrow_mut();
            let x = w.get_mut(&pool).unwrap();
            x.surplus = Some(s);
            x.sums_off |= off;
        });
    }
}

/// How many transactions `watch_buckets` has checked for `pool`, and whether
/// it still checks the §1.2 sums (false once a test wrote them directly).
#[cfg(test)]
pub fn buckets_watched(pool: &Pubkey) -> (u64, bool) {
    bucket_watch::WATCH.with(|w| w.borrow().get(pool).map(|x| (x.checks, !x.sums_off)).unwrap_or((0, false)))
}

#[cfg(not(test))]
pub async fn rebaseline_buckets(_ctx: &mut ProgramTestContext) {}

/// `send`, returning the `T` events the transaction emitted, logged as
/// "Program data: <base64>". perps-core's events reach this natively too:
/// its `emit_event!` (lib.rs) also logs that line through the forwarded
/// `sol_log` stub natively, which program-test records as "Program log:
/// Program data: …" (stripped below), so perps-core event asserts need no
/// `is_sbf()` gate. Natively there are still no `msg!` lines, no return data
/// and no events from programs that use Anchor's `emit!` directly
/// (mm-registry, orderbook): gate those on `is_sbf()` (docs/lp-pool.md D48).
pub async fn send_events<T: anchor_lang::Event>(ctx: &mut ProgramTestContext, ixs: &[Instruction], signers: &[&Keypair]) -> Result<Vec<T>, BanksClientError> {
    use anchor_lang::__private::base64;
    let payer = ctx.payer.insecure_clone();
    let mut all: Vec<&Keypair> = vec![&payer];
    all.extend_from_slice(signers);
    let blockhash = ctx.get_new_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &all, blockhash);
    let out = ctx.banks_client.process_transaction_with_metadata(tx).await?;
    out.result.map_err(BanksClientError::TransactionError)?;
    watch_buckets(ctx, ixs).await;
    let mut events = Vec::new();
    for line in out.metadata.map(|m| m.log_messages).unwrap_or_default() {
        // "Program data: …" on SBF; "Program log: Program data: …" natively
        // (perps-core's emit_event! through the forwarded sol_log stub)
        let b64 = line.strip_prefix("Program data: ").or_else(|| line.strip_prefix("Program log: Program data: "));
        if let Some(b64) = b64 {
            let data = match base64::decode(b64) {
                Ok(d) => d,
                Err(_) => continue,
            };
            if data.len() >= 8 && &data[..8] == T::DISCRIMINATOR {
                if let Ok(e) = T::try_from_slice(&data[8..]) {
                    events.push(e);
                }
            }
        }
    }
    Ok(events)
}

/// Native runs only: let the Anchor 1.2 programs reach program-test's syscall
/// stubs (clock, CPI, return data, stack height; test-sdk's native_stubs.rs).
/// Call after the first `ProgramTest::start*`. Natively `msg!` lines and
/// return data stay unavailable, and so do Anchor `emit!` events; perps-core's
/// events reach the log through `emit_event!` (see `send_events`).
#[cfg(test)]
pub fn install_native_stubs() {
    solana_sdk::native_stubs::install();
}
#[cfg(not(test))]
pub fn install_native_stubs() {}

/// Simulate, returning the result and the compute units consumed.
pub async fn simulate(ctx: &mut ProgramTestContext, ixs: &[Instruction], signers: &[&Keypair]) -> (Result<(), TransactionError>, u64) {
    let payer = ctx.payer.insecure_clone();
    let mut all: Vec<&Keypair> = vec![&payer];
    all.extend_from_slice(signers);
    let blockhash = ctx.get_new_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &all, blockhash);
    let sim = ctx.banks_client.simulate_transaction(tx).await.unwrap();
    let units = sim.simulation_details.map(|d| d.units_consumed).unwrap_or(0);
    (sim.result.expect("simulation ran"), units)
}

pub fn custom_error(result: Result<(), BanksClientError>) -> u32 {
    match result.expect_err("transaction should fail").unwrap() {
        TransactionError::InstructionError(_, InstructionError::Custom(c)) => c,
        other => panic!("unexpected error: {:?}", other),
    }
}

pub fn perps_err(e: PerpsError) -> u32 {
    e.into()
}

pub fn anchor_err(e: anchor_lang::error::ErrorCode) -> u32 {
    e.into()
}

/// A Full Pyth Receiver `PriceUpdateV2` for `OIL_FEED_ID`: `price * 10^expo`
/// with confidence `conf`, published at `timestamp`.
pub fn pyth_account(price: i64, conf: u64, expo: i32, timestamp: i64) -> Account {
    feed_account(PriceUpdateV2View::new(OIL_FEED_ID, price, conf, expo, timestamp).to_bytes(), PYTH_PROGRAM)
}

/// A feed account holding `data`, owned by `owner`.
pub fn feed_account(data: Vec<u8>, owner: Pubkey) -> Account {
    Account { lamports: 1_000_000_000, data, owner, executable: false, rent_epoch: 0 }
}

/// Dollars as a Pyth price with expo -8.
pub fn pyth_dollars(d: f64) -> i64 {
    (d * 1e8).round() as i64
}

/// Dollars in the program's 6-decimal format.
pub fn usd(d: f64) -> u64 {
    (d * 1e6).round() as u64
}

pub struct Base {
    pub ctx: ProgramTestContext,
    /// perps-core upgrade authority and admin
    pub admin: Keypair,
    /// Can pause markets and venues, nothing else
    pub guardian: Keypair,
    pub market: Pubkey,
    pub oracle: Pubkey,
    pub mint: Pubkey,
    pub vault: Pubkey,
    pub vault_token: Pubkey,
    pub lp_pool: Pubkey,
    /// The protocol's seed LP (the admin key): `LP_SEED − LP_DEAD_SHARES` shares
    pub seed_lp: Lp,
}

/// An LP wallet: a token account and an LpPosition (class 0) in the market's pool.
pub struct Lp {
    pub kp: Keypair,
    pub tokens: Pubkey,
    pub position: Pubkey,
}

impl Lp {
    pub fn key(&self) -> Pubkey {
        self.kp.pubkey()
    }
}

/// `pt` must already have the program under test added. Leaves the clock at
/// `WEDNESDAY` with a fresh $75 oracle price, and the market's LP pool on
/// `PoolParams::neutral()` seeded with `LP_SEED`.
pub async fn start(pt: ProgramTest) -> Base {
    start_with(pt, PoolParams::neutral()).await
}

/// `start` with other pool params, applied before the seed.
pub async fn start_with(pt: ProgramTest, pool_params: PoolParams) -> Base {
    start_seeded(pt, pool_params, LP_SEED).await
}

/// `start_with` and a seed deposit of `seed` (0 = none: the pool has no LPs
/// and `seed_lp` only has its LpPosition and `LP_SEED` in its wallet).
pub async fn start_seeded(mut pt: ProgramTest, pool_params: PoolParams, seed: u64) -> Base {
    let admin = Keypair::new();
    let guardian = Keypair::new();
    let oracle = Pubkey::new_unique();
    pt.add_program("perps_core", perps_core::ID, solana_program_test::processor!(perps_process));
    // Fresh at WEDNESDAY: the seed lp_deposit in the market's transaction reads it
    pt.add_account(oracle, pyth_account(pyth_dollars(OIL_PRICE), 0, -8, WEDNESDAY));
    let state = UpgradeableLoaderState::ProgramData { slot: 0, upgrade_authority_address: Some(admin.pubkey()) };
    let program_data = program_data_pda(&perps_core::ID);
    pt.add_account(program_data, Account { lamports: 1_000_000_000, data: bincode::serialize(&state).unwrap(), owner: bpf_loader_upgradeable::id(), executable: false, rent_epoch: 0 });
    let mut ctx = pt.start_with_context().await;
    install_native_stubs();
    // Before the market exists, so its funding clock starts here too
    let mut clock: Clock = ctx.banks_client.get_sysvar().await.unwrap();
    clock.unix_timestamp = WEDNESDAY;
    ctx.set_sysvar(&clock);
    let payer = ctx.payer.insecure_clone();
    fund(&mut ctx, &admin.pubkey()).await;
    fund(&mut ctx, &guardian.pubkey()).await;

    let mint = Keypair::new();
    let rent = ctx.banks_client.get_rent().await.unwrap();
    send(&mut ctx, &[
        system_instruction::create_account(&payer.pubkey(), &mint.pubkey(), rent.minimum_balance(spl_token::state::Mint::LEN), spl_token::state::Mint::LEN as u64, &spl_token::ID),
        spl_token::instruction::initialize_mint(&spl_token::ID, &mint.pubkey(), &payer.pubkey(), None, 6).unwrap(),
    ], &[&mint]).await.unwrap();
    let mint = mint.pubkey();

    send(&mut ctx, &[perps_ix(perps_core::accounts::InitializeConfig {
        upgrade_authority: admin.pubkey(), config: config_pda(), program_data, system_program: system_program::ID,
    }, perps_core::instruction::InitializeConfig { params: ProtocolRolesParams { guardian: guardian.pubkey(), oracle_program: PYTH_PROGRAM } })], &[&admin]).await.unwrap();
    let market = pda(&[b"market", mint.as_ref(), b"OIL"], &perps_core::ID);
    let params = InitializeMarketParams {
        commodity: "OIL".into(), max_leverage: 20_000, maintenance_margin_ratio: 500, initial_margin_ratio: 1_000,
        taker_fee: 5, maker_fee: 2, liquidation_fee: 100, max_open_interest: 1_000_000_000_000, funding_interval: 3_600,
        pyth_feed_id: OIL_FEED_ID,
    };
    let (vault, vault_token) = (vault_pda(&market), vault_token_pda(&market));
    let lp_pool = lp_pool_pda(&market);
    // The protocol's seed, funded from a wallet the admin owns
    let seed_tokens = Keypair::new();
    let len = spl_token::state::Account::LEN;
    send(&mut ctx, &[
        system_instruction::create_account(&payer.pubkey(), &seed_tokens.pubkey(), rent.minimum_balance(len), len as u64, &spl_token::ID),
        spl_token::instruction::initialize_account(&spl_token::ID, &seed_tokens.pubkey(), &mint, &admin.pubkey()).unwrap(),
        spl_token::instruction::mint_to(&spl_token::ID, &mint, &seed_tokens.pubkey(), &payer.pubkey(), &[], LP_SEED).unwrap(),
    ], &[&seed_tokens]).await.unwrap();
    let seed_lp = Lp { kp: admin.insecure_clone(), tokens: seed_tokens.pubkey(), position: lp_position_pda(&lp_pool, &admin.pubkey(), 0) };

    // Bootstrap (D11): market, pool, the seed LpPosition and the seed deposit in one transaction
    let mut ixs = vec![perps_ix(perps_core::accounts::InitializeMarket {
        authority: admin.pubkey(), config: config_pda(), market, vault, vault_token_account: vault_token,
        collateral_mint: mint, pyth_price_feed: oracle, system_program: system_program::ID, token_program: spl_token::ID, rent: sysvar::rent::ID,
    }, perps_core::instruction::InitializeMarket { params })];
    ixs.extend(pool_bootstrap_ixs(&admin.pubkey(), market, oracle, &seed_lp, pool_params, seed));
    send(&mut ctx, &ixs, &[&admin]).await.unwrap();
    let mut base = Base { ctx, admin, guardian, market, oracle, mint, vault, vault_token, lp_pool, seed_lp };
    set_time(&mut base, WEDNESDAY).await;
    base
}

/// `initialize_pool(params)` by `admin`, `init_lp_position(0)` for `seed` and
/// (if `amount > 0`) the seed `lp_deposit`, for appending to the
/// `initialize_market` transaction.
pub fn pool_bootstrap_ixs(admin: &Pubkey, market: Pubkey, oracle: Pubkey, seed: &Lp, params: PoolParams, amount: u64) -> Vec<Instruction> {
    let lp_pool = lp_pool_pda(&market);
    let mut ixs = vec![
        perps_ix(perps_core::accounts::InitializePool {
            admin: *admin, config: config_pda(), market, lp_pool, system_program: system_program::ID,
        }, perps_core::instruction::InitializePool { params, adl_keeper: *admin }),
        perps_ix(perps_core::accounts::InitLpPosition {
            owner: seed.key(), lp_pool, lp_position: seed.position, system_program: system_program::ID,
        }, perps_core::instruction::InitLpPosition { class: 0 }),
    ];
    if amount > 0 {
        ixs.push(lp_deposit_ix_for(market, oracle, seed, amount, 0));
    }
    ixs
}

/// lp_deposit accounts for `lp` in `market` (vault token and pool derived).
pub fn lp_deposit_ix_for(market: Pubkey, oracle: Pubkey, lp: &Lp, amount: u64, min_shares_out: u64) -> Instruction {
    perps_ix(perps_core::accounts::LpDeposit {
        owner: lp.key(), market, lp_pool: lp_pool_pda(&market), lp_position: lp.position,
        vault_token_account: vault_token_pda(&market), owner_token_account: lp.tokens, pyth_price_feed: oracle,
        token_program: spl_token::ID,
    }, perps_core::instruction::LpDeposit { amount, min_shares_out })
}

pub async fn fund(ctx: &mut ProgramTestContext, who: &Pubkey) {
    let payer = ctx.payer.insecure_clone();
    send(ctx, &[system_instruction::transfer(&payer.pubkey(), who, 10_000_000_000)], &[]).await.unwrap();
}

/// A new keypair holding 10 SOL.
pub async fn funded_keypair(base: &mut Base) -> Keypair {
    let k = Keypair::new();
    fund(&mut base.ctx, &k.pubkey()).await;
    k
}

/// Hand the perps-core admin role from `base.admin` to a new key, returning it.
pub async fn rotate_admin(base: &mut Base) -> Keypair {
    let new_admin = Keypair::new();
    fund(&mut base.ctx, &new_admin.pubkey()).await;
    let old = base.admin.insecure_clone();
    send(&mut base.ctx, &[perps_ix(perps_core::accounts::AdminOnly { admin: old.pubkey(), config: config_pda() },
        perps_core::instruction::ProposeAdmin { new_admin: new_admin.pubkey() })], &[&old]).await.unwrap();
    send(&mut base.ctx, &[perps_ix(perps_core::accounts::AcceptAdmin { new_admin: new_admin.pubkey(), config: config_pda() },
        perps_core::instruction::AcceptAdmin {})], &[&new_admin]).await.unwrap();
    new_admin
}

// --- clock and oracle ---------------------------------------------------------

pub async fn now(base: &mut Base) -> i64 {
    base.ctx.banks_client.get_sysvar::<Clock>().await.unwrap().unix_timestamp
}

/// Warp the clock to `unix_timestamp` and restamp the oracle so its price stays fresh.
pub async fn set_time(base: &mut Base, unix_timestamp: i64) {
    let mut clock: Clock = base.ctx.banks_client.get_sysvar().await.unwrap();
    clock.unix_timestamp = unix_timestamp;
    base.ctx.set_sysvar(&clock);
    let mut acct = base.ctx.banks_client.get_account(base.oracle).await.unwrap().unwrap();
    set_publish_times(&mut acct.data, unix_timestamp);
    base.ctx.set_account(&base.oracle, &AccountSharedData::from(acct));
}

/// Set the oracle to `dollars`, published now.
pub async fn set_price(base: &mut Base, dollars: f64) {
    let t = now(base).await;
    set_price_raw(base, pyth_dollars(dollars), 0, t).await;
}

/// Set the oracle to a raw expo -8 price and confidence, published at `timestamp`.
pub async fn set_price_raw(base: &mut Base, price: i64, conf: u64, timestamp: i64) {
    base.ctx.set_account(&base.oracle, &AccountSharedData::from(pyth_account(price, conf, -8, timestamp)));
}

/// Replace the market feed account's data and owner (negative oracle tests).
pub async fn set_feed(base: &mut Base, data: Vec<u8>, owner: Pubkey) {
    base.ctx.set_account(&base.oracle, &AccountSharedData::from(feed_account(data, owner)));
}

// --- accounts -----------------------------------------------------------------

pub async fn raw(base: &mut Base, address: Pubkey) -> Option<Account> {
    base.ctx.banks_client.get_account(address).await.unwrap()
}

pub async fn exists(base: &mut Base, address: Pubkey) -> bool {
    raw(base, address).await.map(|a| a.lamports > 0).unwrap_or(false)
}

pub async fn lamports(base: &mut Base, address: Pubkey) -> u64 {
    raw(base, address).await.map(|a| a.lamports).unwrap_or(0)
}

pub async fn load<T: AccountDeserialize>(base: &mut Base, address: Pubkey) -> T {
    try_load(base, address).await.expect("account exists")
}

pub async fn try_load<T: AccountDeserialize>(base: &mut Base, address: Pubkey) -> Option<T> {
    let acct = raw(base, address).await?;
    T::try_deserialize(&mut acct.data.as_slice()).ok()
}

/// Rewrite an Anchor account in place (e.g. to fake legacy state or an OI level).
pub async fn patch<T: AccountDeserialize + AccountSerialize>(base: &mut Base, address: Pubkey, f: impl FnOnce(&mut T)) {
    let mut acct = raw(base, address).await.expect("account exists");
    let mut value = T::try_deserialize(&mut acct.data.as_slice()).unwrap();
    f(&mut value);
    let mut data = Vec::with_capacity(acct.data.len());
    value.try_serialize(&mut data).unwrap();
    acct.data[..data.len()].copy_from_slice(&data);
    base.ctx.set_account(&address, &AccountSharedData::from(acct));
    rebaseline_buckets(&mut base.ctx).await;
}

pub async fn market(base: &mut Base) -> Market {
    let m = base.market;
    load(base, m).await
}

pub async fn user(base: &mut Base, owner: &Pubkey) -> UserAccount {
    let u = user_pda(owner, &base.market);
    load(base, u).await
}

pub async fn token_balance(base: &mut Base, account: Pubkey) -> u64 {
    let acct = raw(base, account).await.expect("token account exists");
    spl_token::state::Account::unpack(&acct.data).unwrap().amount
}

pub async fn vault_token_balance(base: &mut Base) -> u64 {
    let v = base.vault_token;
    token_balance(base, v).await
}

// --- LP pool ----------------------------------------------------------------------

pub async fn pool(base: &mut Base) -> LpPool {
    let p = base.lp_pool;
    load(base, p).await
}

pub async fn lp_position(base: &mut Base, lp: &Lp) -> LpPosition {
    load(base, lp.position).await
}

/// A wallet holding `usdc` and an initialised LpPosition, nothing deposited.
pub async fn lp(base: &mut Base, usdc: u64) -> Lp {
    let kp = funded_keypair(base).await;
    let tokens = token_account(base, &kp.pubkey(), usdc).await;
    let position = lp_position_pda(&base.lp_pool, &kp.pubkey(), 0);
    let ix = perps_ix(perps_core::accounts::InitLpPosition {
        owner: kp.pubkey(), lp_pool: base.lp_pool, lp_position: position, system_program: system_program::ID,
    }, perps_core::instruction::InitLpPosition { class: 0 });
    send(&mut base.ctx, &[ix], &[&kp]).await.unwrap();
    Lp { kp, tokens, position }
}

pub fn lp_deposit_ix(base: &Base, lp: &Lp, amount: u64, min_shares_out: u64) -> Instruction {
    lp_deposit_ix_for(base.market, base.oracle, lp, amount, min_shares_out)
}

pub async fn lp_deposit(base: &mut Base, lp: &Lp, amount: u64, min_shares_out: u64) -> Result<(), BanksClientError> {
    let ix = lp_deposit_ix(base, lp, amount, min_shares_out);
    send(&mut base.ctx, &[ix], &[&lp.kp]).await
}

pub fn lp_request_ix(base: &Base, lp: &Lp, shares: u64) -> Instruction {
    perps_ix(perps_core::accounts::LpRequestWithdrawal {
        owner: lp.key(), market: base.market, lp_pool: base.lp_pool, lp_position: lp.position, pyth_price_feed: base.oracle,
    }, perps_core::instruction::LpRequestWithdrawal { shares })
}

pub async fn lp_request(base: &mut Base, lp: &Lp, shares: u64) -> Result<(), BanksClientError> {
    let ix = lp_request_ix(base, lp, shares);
    send(&mut base.ctx, &[ix], &[&lp.kp]).await
}

pub async fn lp_cancel(base: &mut Base, lp: &Lp) -> Result<(), BanksClientError> {
    let ix = perps_ix(perps_core::accounts::LpCancelWithdrawal {
        owner: lp.key(), lp_pool: base.lp_pool, lp_position: lp.position,
    }, perps_core::instruction::LpCancelWithdrawal {});
    send(&mut base.ctx, &[ix], &[&lp.kp]).await
}

pub fn lp_withdraw_ix(base: &Base, lp: &Lp) -> Instruction {
    perps_ix(perps_core::accounts::LpWithdraw {
        owner: lp.key(), market: base.market, lp_pool: base.lp_pool, lp_position: lp.position, vault: base.vault,
        vault_token_account: base.vault_token, owner_token_account: lp.tokens, pyth_price_feed: base.oracle,
        token_program: spl_token::ID,
    }, perps_core::instruction::LpWithdraw {})
}

pub async fn lp_withdraw(base: &mut Base, lp: &Lp) -> Result<(), BanksClientError> {
    let ix = lp_withdraw_ix(base, lp);
    send(&mut base.ctx, &[ix], &[&lp.kp]).await
}

/// `donor` gives `amount` from `tokens` to the pool.
pub async fn donate(base: &mut Base, donor: &Keypair, tokens: Pubkey, amount: u64) -> Result<(), BanksClientError> {
    let ix = perps_ix(perps_core::accounts::DonateToPool {
        donor: donor.pubkey(), market: base.market, lp_pool: base.lp_pool, vault_token_account: base.vault_token,
        donor_token_account: tokens, token_program: spl_token::ID,
    }, perps_core::instruction::DonateToPool { amount });
    send(&mut base.ctx, &[ix], &[donor]).await
}

/// sweep_insurance_excess, cranked by the test payer.
pub async fn sweep(base: &mut Base) -> Result<(), BanksClientError> {
    let signer = base.ctx.payer.pubkey();
    let ix = perps_ix(perps_core::accounts::SweepInsuranceExcess { signer, market: base.market, lp_pool: base.lp_pool },
        perps_core::instruction::SweepInsuranceExcess {});
    send(&mut base.ctx, &[ix], &[]).await
}

/// close_settled_position of `owner`'s `position`, by the test payer.
pub async fn close_settled(base: &mut Base, owner: &Pubkey, position: Pubkey) -> Result<(), BanksClientError> {
    let signer = base.ctx.payer.pubkey();
    let ix = perps_ix(perps_core::accounts::CloseSettledPosition {
        signer, market: base.market, lp_pool: base.lp_pool, position, position_owner: *owner,
        user_account: user_pda(owner, &base.market),
    }, perps_core::instruction::CloseSettledPosition {});
    send(&mut base.ctx, &[ix], &[]).await
}

pub async fn propose_pool_params(base: &mut Base, signer: &Keypair, params: PoolParams) -> Result<(), BanksClientError> {
    let ix = perps_ix(perps_core::accounts::ProposePoolParams {
        admin: signer.pubkey(), config: config_pda(), market: base.market, lp_pool: base.lp_pool,
    }, perps_core::instruction::ProposePoolParams { params });
    send(&mut base.ctx, &[ix], &[signer]).await
}

/// apply_pool_params, by the test payer (permissionless).
pub async fn apply_pool_params(base: &mut Base) -> Result<(), BanksClientError> {
    let ix = perps_ix(perps_core::accounts::ApplyPoolParams { market: base.market, lp_pool: base.lp_pool },
        perps_core::instruction::ApplyPoolParams {});
    send(&mut base.ctx, &[ix], &[]).await
}

fn pool_admin_accounts(base: &Base, signer: &Keypair) -> perps_core::accounts::PoolAdminOnly {
    perps_core::accounts::PoolAdminOnly { admin: signer.pubkey(), config: config_pda(), lp_pool: base.lp_pool }
}

pub async fn cancel_pool_params(base: &mut Base, signer: &Keypair) -> Result<(), BanksClientError> {
    let ix = perps_ix(pool_admin_accounts(base, signer), perps_core::instruction::CancelPoolParams {});
    send(&mut base.ctx, &[ix], &[signer]).await
}

// --- oracle feed rotation (D47) ------------------------------------------------

pub fn propose_oracle_feed_ix(base: &Base, signer: &Pubkey, new_price_feed: Pubkey, feed_id: [u8; 32]) -> Instruction {
    perps_ix(perps_core::accounts::ProposeOracleFeed {
        admin: *signer, config: config_pda(), market: base.market, lp_pool: base.lp_pool, new_price_feed,
    }, perps_core::instruction::ProposeOracleFeed { feed_id })
}

pub async fn propose_oracle_feed(base: &mut Base, signer: &Keypair, new_price_feed: Pubkey, feed_id: [u8; 32]) -> Result<(), BanksClientError> {
    let ix = propose_oracle_feed_ix(base, &signer.pubkey(), new_price_feed, feed_id);
    send(&mut base.ctx, &[ix], &[signer]).await
}

pub fn apply_oracle_feed_ix(base: &Base, new_price_feed: Pubkey) -> Instruction {
    perps_ix(perps_core::accounts::ApplyOracleFeed { market: base.market, config: config_pda(), new_price_feed }, perps_core::instruction::ApplyOracleFeed {})
}

/// Permissionless: sent by the payer alone.
pub async fn apply_oracle_feed(base: &mut Base, new_price_feed: Pubkey) -> Result<(), BanksClientError> {
    let ix = apply_oracle_feed_ix(base, new_price_feed);
    send(&mut base.ctx, &[ix], &[]).await
}

pub fn cancel_oracle_feed_ix(base: &Base, signer: &Pubkey) -> Instruction {
    perps_ix(perps_core::accounts::CancelOracleFeed { admin: *signer, config: config_pda(), market: base.market }, perps_core::instruction::CancelOracleFeed {})
}

pub async fn cancel_oracle_feed(base: &mut Base, signer: &Keypair) -> Result<(), BanksClientError> {
    let ix = cancel_oracle_feed_ix(base, &signer.pubkey());
    send(&mut base.ctx, &[ix], &[signer]).await
}

pub async fn set_max_pool_cash(base: &mut Base, signer: &Keypair, cap: u64) -> Result<(), BanksClientError> {
    let ix = perps_ix(pool_admin_accounts(base, signer), perps_core::instruction::SetMaxPoolCash { cap });
    send(&mut base.ctx, &[ix], &[signer]).await
}

pub async fn set_adl_keeper(base: &mut Base, signer: &Keypair, key: Pubkey) -> Result<(), BanksClientError> {
    let ix = perps_ix(pool_admin_accounts(base, signer), perps_core::instruction::SetAdlKeeper { key });
    send(&mut base.ctx, &[ix], &[signer]).await
}

/// Propose `params` as the admin and warp past the timelock, then apply.
/// Leaves the clock at the effective time (restamping the oracle).
pub async fn set_pool_params(base: &mut Base, params: PoolParams) {
    let admin = base.admin.insecure_clone();
    propose_pool_params(base, &admin, params).await.unwrap();
    let at = pool(base).await.pending_params_at;
    if at != 0 {
        set_time(base, at).await;
        apply_pool_params(base).await.unwrap();
    }
}

pub async fn withdraw_treasury(base: &mut Base, signer: &Keypair, destination: Pubkey, amount: u64) -> Result<(), BanksClientError> {
    let ix = perps_ix(perps_core::accounts::WithdrawTreasury {
        admin: signer.pubkey(), config: config_pda(), market: base.market, lp_pool: base.lp_pool, vault: base.vault,
        vault_token_account: base.vault_token, destination, token_program: spl_token::ID,
    }, perps_core::instruction::WithdrawTreasury { amount });
    send(&mut base.ctx, &[ix], &[signer]).await
}

// --- traders --------------------------------------------------------------------

/// A wallet with 10 SOL, a USDC token account and a perps-core UserAccount.
pub struct Trader {
    pub kp: Keypair,
    pub tokens: Pubkey,
    pub user: Pubkey,
}

impl Trader {
    pub fn key(&self) -> Pubkey {
        self.kp.pubkey()
    }
}

/// A token account for `owner` holding `amount` newly minted USDC.
pub async fn token_account(base: &mut Base, owner: &Pubkey, amount: u64) -> Pubkey {
    let payer = base.ctx.payer.insecure_clone();
    let account = Keypair::new();
    let rent = base.ctx.banks_client.get_rent().await.unwrap();
    let len = spl_token::state::Account::LEN;
    let mut ixs = vec![
        system_instruction::create_account(&payer.pubkey(), &account.pubkey(), rent.minimum_balance(len), len as u64, &spl_token::ID),
        spl_token::instruction::initialize_account(&spl_token::ID, &account.pubkey(), &base.mint, owner).unwrap(),
    ];
    if amount > 0 {
        ixs.push(spl_token::instruction::mint_to(&spl_token::ID, &base.mint, &account.pubkey(), &payer.pubkey(), &[], amount).unwrap());
    }
    send(&mut base.ctx, &ixs, &[&account]).await.unwrap();
    account.pubkey()
}

pub async fn mint_to(base: &mut Base, tokens: Pubkey, amount: u64) {
    let payer = base.ctx.payer.insecure_clone();
    let ix = spl_token::instruction::mint_to(&spl_token::ID, &base.mint, &tokens, &payer.pubkey(), &[], amount).unwrap();
    send(&mut base.ctx, &[ix], &[]).await.unwrap();
}

/// A trader with `usdc` deposited in the market (and nothing left in the wallet).
pub async fn trader(base: &mut Base, usdc: u64) -> Trader {
    let kp = funded_keypair(base).await;
    let tokens = token_account(base, &kp.pubkey(), usdc).await;
    let user = user_pda(&kp.pubkey(), &base.market);
    send(&mut base.ctx, &[perps_ix(perps_core::accounts::InitializeUser {
        owner: kp.pubkey(), market: base.market, user_account: user, system_program: system_program::ID,
    }, perps_core::instruction::InitializeUser {})], &[&kp]).await.unwrap();
    let t = Trader { kp, tokens, user };
    if usdc > 0 {
        deposit(base, &t, usdc).await.unwrap();
    }
    t
}

pub async fn deposit(base: &mut Base, t: &Trader, amount: u64) -> Result<(), BanksClientError> {
    let accounts = perps_core::accounts::DepositCollateral {
        owner: t.key(), user_account: t.user, market: base.market, vault: base.vault,
        vault_token_account: base.vault_token, user_token_account: t.tokens, token_program: spl_token::ID,
        lp_pool: base.lp_pool,
    };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::DepositCollateral { amount })], &[&t.kp]).await
}

pub async fn withdraw(base: &mut Base, t: &Trader, amount: u64) -> Result<(), BanksClientError> {
    let accounts = perps_core::accounts::WithdrawCollateral {
        owner: t.key(), user_account: t.user, market: base.market, vault: base.vault,
        vault_token_account: base.vault_token, user_token_account: t.tokens, token_program: spl_token::ID,
        lp_pool: base.lp_pool,
    };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::WithdrawCollateral { amount })], &[&t.kp]).await
}

// --- market admin ------------------------------------------------------------------

pub async fn pause(base: &mut Base, signer: &Keypair) -> Result<(), BanksClientError> {
    let accounts = perps_core::accounts::PauseMarket { signer: signer.pubkey(), config: config_pda(), market: base.market };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::PauseMarket {})], &[signer]).await
}

/// update_market_config as the admin.
pub async fn configure(base: &mut Base, params: MarketConfigParams) -> Result<(), BanksClientError> {
    let admin = base.admin.insecure_clone();
    let accounts = perps_core::accounts::UpdateMarketConfig { authority: admin.pubkey(), config: config_pda(), market: base.market };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::UpdateMarketConfig { params })], &[&admin]).await
}

/// CME Globex hours with 20% / 10% weekend margins from two hours before the weekly close.
pub fn cme_config() -> MarketConfigParams {
    MarketConfigParams {
        is_paused: false,
        session_schedule: perps_core::session::SCHEDULE_CME_GLOBEX,
        pre_close_window_secs: 7_200,
        weekend_initial_margin_ratio: 2_000,
        weekend_maintenance_margin_ratio: 1_000,
        max_confidence_bps: 100,
        min_oracle_price: 0,
        max_open_interest: 0,
    }
}

pub async fn settle(base: &mut Base, settlement_price: u64) -> Result<(), BanksClientError> {
    let admin = base.admin.insecure_clone();
    let accounts = perps_core::accounts::SettleMarket { authority: admin.pubkey(), config: config_pda(), market: base.market };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::SettleMarket { settlement_price })], &[&admin]).await
}

/// update_funding, cranked by the test payer.
pub async fn update_funding(base: &mut Base) -> Result<(), BanksClientError> {
    let keeper = base.ctx.payer.pubkey();
    let accounts = perps_core::accounts::UpdateFunding { keeper, market: base.market, pyth_price_feed: base.oracle };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::UpdateFunding {})], &[]).await
}

// --- positions ----------------------------------------------------------------------

/// open_position for `owner` at index `market.total_positions` as of now.
/// Returns the instruction and the position it creates, for sending later.
pub async fn open_ix(base: &mut Base, owner: &Pubkey, side: u8, size: u64, leverage: u32) -> (Instruction, Pubkey) {
    open_ix_with(base, owner, side, size, leverage, 0).await
}

/// `open_ix` with an `acceptable_price` bound (0 = none).
pub async fn open_ix_with(base: &mut Base, owner: &Pubkey, side: u8, size: u64, leverage: u32, acceptable_price: u64) -> (Instruction, Pubkey) {
    let index = market(base).await.total_positions;
    let position = position_pda(owner, &base.market, index);
    let accounts = perps_core::accounts::OpenPosition {
        owner: *owner, user_account: user_pda(owner, &base.market), market: base.market, position,
        pyth_price_feed: base.oracle, system_program: system_program::ID, lp_pool: base.lp_pool,
    };
    let params = OpenPositionParams { side, size, leverage, acceptable_price };
    (perps_ix(accounts, perps_core::instruction::OpenPosition { params }), position)
}

/// open_position of `contracts` whole contracts at `leverage_x`.
pub async fn open(base: &mut Base, t: &Trader, side: u8, contracts: u64, leverage_x: u32) -> Result<Pubkey, BanksClientError> {
    let (ix, position) = open_ix(base, &t.key(), side, contracts * CONTRACT, leverage_x * 1_000).await;
    send(&mut base.ctx, &[ix], &[&t.kp]).await?;
    Ok(position)
}

pub fn close_ix(base: &Base, t: &Trader, position: Pubkey) -> Instruction {
    close_ix_with(base, t, position, 0)
}

/// close_position with an `acceptable_price` bound (0 = none). No referral:
/// the three optional slots carry the program id, `lp_pool` is account 13.
pub fn close_ix_with(base: &Base, t: &Trader, position: Pubkey, acceptable_price: u64) -> Instruction {
    let accounts = perps_core::accounts::ClosePosition {
        owner: t.key(), user_account: t.user, market: base.market, position, vault: base.vault,
        vault_token_account: base.vault_token, user_token_account: t.tokens, pyth_price_feed: base.oracle,
        token_program: spl_token::ID, user_referral: None, referral_code: None, referrer_token_account: None,
        lp_pool: base.lp_pool,
    };
    perps_ix(accounts, perps_core::instruction::ClosePosition { acceptable_price })
}

/// Close `position`; the payout goes to `t.tokens`.
pub async fn close(base: &mut Base, t: &Trader, position: Pubkey) -> Result<(), BanksClientError> {
    let ix = close_ix(base, t, position);
    send(&mut base.ctx, &[ix], &[&t.kp]).await
}

pub fn liquidate_ix(base: &Base, keeper: &Trader, owner: &Pubkey, position: Pubkey) -> Instruction {
    let accounts = perps_core::accounts::Liquidate {
        liquidator: keeper.key(), position_owner: *owner, user_account: user_pda(owner, &base.market), market: base.market,
        position, vault: base.vault, vault_token_account: base.vault_token, liquidator_token_account: keeper.tokens,
        pyth_price_feed: base.oracle, token_program: spl_token::ID, lp_pool: base.lp_pool,
    };
    perps_ix(accounts, perps_core::instruction::Liquidate {})
}

/// `keeper` liquidates `owner`'s `position`; the reward goes to `keeper.tokens`.
pub async fn liquidate(base: &mut Base, keeper: &Trader, owner: &Pubkey, position: Pubkey) -> Result<(), BanksClientError> {
    let ix = liquidate_ix(base, keeper, owner, position);
    send(&mut base.ctx, &[ix], &[&keeper.kp]).await
}

pub async fn add_margin(base: &mut Base, t: &Trader, position: Pubkey, amount: u64) -> Result<(), BanksClientError> {
    let accounts = perps_core::accounts::AddMargin {
        owner: t.key(), user_account: t.user, market: base.market, position, lp_pool: base.lp_pool,
    };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::AddMargin { amount })], &[&t.kp]).await
}

pub fn settle_pnl_ix(base: &Base, signer: &Pubkey, position: Pubkey) -> Instruction {
    perps_ix(perps_core::accounts::SettlePositionPnl {
        signer: *signer, market: base.market, lp_pool: base.lp_pool, position, pyth_price_feed: base.oracle,
    }, perps_core::instruction::SettlePositionPnl {})
}

/// settle_position_pnl of `position`, cranked by the test payer.
pub async fn settle_pnl(base: &mut Base, position: Pubkey) -> Result<(), BanksClientError> {
    let signer = base.ctx.payer.pubkey();
    let ix = settle_pnl_ix(base, &signer, position);
    send(&mut base.ctx, &[ix], &[]).await
}

pub fn adl_ix(base: &Base, signer: &Pubkey, owner: &Pubkey, position: Pubkey, mode: u8, size: u64) -> Instruction {
    perps_ix(perps_core::accounts::AdlPosition {
        signer: *signer, config: config_pda(), market: base.market, lp_pool: base.lp_pool, position,
        position_owner: *owner, user_account: user_pda(owner, &base.market), pyth_price_feed: base.oracle,
    }, perps_core::instruction::Adl { mode, size })
}

/// adl of `owner`'s `position` signed by `signer` (`None`: the test payer).
pub async fn adl(base: &mut Base, signer: Option<&Keypair>, owner: &Pubkey, position: Pubkey, mode: u8, size: u64) -> Result<(), BanksClientError> {
    let key = signer.map(|k| k.pubkey()).unwrap_or(base.ctx.payer.pubkey());
    let ix = adl_ix(base, &key, owner, position, mode, size);
    match signer {
        Some(k) => send(&mut base.ctx, &[ix], &[k]).await,
        None => send(&mut base.ctx, &[ix], &[]).await,
    }
}

/// write_off_bad_debt, cranked by the test payer.
pub async fn write_off(base: &mut Base) -> Result<(), BanksClientError> {
    let signer = base.ctx.payer.pubkey();
    let ix = perps_ix(perps_core::accounts::WriteOffBadDebt { signer, market: base.market, lp_pool: base.lp_pool },
        perps_core::instruction::WriteOffBadDebt {});
    send(&mut base.ctx, &[ix], &[]).await
}

/// The pool's valuation at `mark` now (docs/lp-pool.md §2.2).
pub async fn pool_view_at(base: &mut Base, mark: u64) -> perps_core::state::PoolView {
    let m = market(base).await;
    let p = pool(base).await;
    let t = now(base).await;
    perps_core::state::pool_view(&p, &m, mark, t, p.params.redeem_loss_haircut_bps).unwrap()
}

/// The bucket identity, exactly (§11): vault tokens == free collateral +
/// position collateral + insurance + LP cash + treasury. Holds whenever no
/// test has sent tokens straight to the vault.
pub async fn assert_buckets(base: &mut Base) {
    let m = market(base).await;
    let p = pool(base).await;
    let vault = vault_token_balance(base).await as u128;
    assert_eq!(vault, perps_core::state::bucket_total(&p, &m), "vault != Σ buckets: {:?}", (p.free_collateral, p.position_collateral, m.insurance_fund, p.cash, p.treasury));
}

/// §17.2: the bucket identity (exact); the listed owners' free balances are
/// inside `free_collateral`; and when `positions` lists every open position,
/// the aggregates equal the sums over them exactly and the pool's U is
/// within slack of what closing them all at `mark` would realize.
pub async fn assert_conservation(base: &mut Base, mark: u64, owners: &[Pubkey], positions: &[Pubkey]) {
    assert_buckets(base).await;
    let m = market(base).await;
    let p = pool(base).await;
    let mut free = 0u128;
    for owner in owners {
        free += user(base, owner).await.collateral_balance as u128;
    }
    assert!(free <= p.free_collateral as u128, "owners' free {} > pool free {}", free, p.free_collateral);
    let (mut open, mut le, mut se, mut lf, mut sf, mut pc, mut u_eng) = (0u64, 0u128, 0u128, 0i128, 0i128, 0u128, 0i128);
    for &address in positions {
        let q: Position = load(base, address).await;
        if q.status == PositionStatus::Open {
            open += 1;
            pc += q.collateral as u128;
            u_eng += q.unrealized_pnl(mark) as i128 + q.accrued_funding(m.cumulative_funding) as i128;
            let (e, f) = (q.size as u128 * q.entry_price as u128, q.size as i128 * q.last_funding_payment as i128);
            match q.side {
                perps_core::state::Side::Long => { le += e; lf += f; }
                perps_core::state::Side::Short => { se += e; sf += f; }
            }
        }
    }
    if open == p.open_positions {
        assert_eq!((p.long_entry_sum, p.short_entry_sum, p.long_funding_sum, p.short_funding_sum), (le, se, lf, sf), "aggregates");
        assert_eq!(p.position_collateral as u128, pc, "position_collateral");
        let v = pool_view_at(base, mark).await;
        let slack = 2 * open as i128 + 1;
        assert!((v.u - u_eng).abs() <= slack, "U {} vs engine {}", v.u, u_eng);
    }
}

// --- venues -----------------------------------------------------------------------

/// mm-registry's defaults from the spec: 50 bps band, 15 s oracle, $50k per fill.
/// `min_signers` is 0 here; set it per venue.
pub fn default_venue_params() -> VenueParams {
    VenueParams { max_price_deviation_bps: 50, max_oracle_age_secs: 15, min_signers: 0, max_fill_notional: 50_000 * USDC }
}

/// Write a fake upgradeable-loader ProgramData for `program` deployed at
/// `slot`, with `authority` as its upgrade authority (None = immutable).
/// Works for any program id, including builtins and programs loaded by `add_program`.
pub async fn set_program_data(base: &mut Base, program: &Pubkey, slot: u64, authority: Option<Pubkey>) {
    let state = UpgradeableLoaderState::ProgramData { slot, upgrade_authority_address: authority };
    let account = Account { lamports: 1_000_000_000, data: bincode::serialize(&state).unwrap(), owner: bpf_loader_upgradeable::id(), executable: false, rent_epoch: 0 };
    base.ctx.set_account(&program_data_pda(program), &AccountSharedData::from(account));
}

/// Fake ProgramData for `program` at `slot`, upgradeable by the admin.
pub async fn add_venue_program_data(base: &mut Base, program: &Pubkey, slot: u64) {
    let admin = base.admin.pubkey();
    set_program_data(base, program, slot, Some(admin)).await;
}

fn program_data_state(acct: &Account) -> (u64, Option<Pubkey>) {
    match bincode::deserialize(&acct.data).unwrap() {
        UpgradeableLoaderState::ProgramData { slot, upgrade_authority_address } => (slot, upgrade_authority_address),
        other => panic!("not ProgramData: {:?}", other),
    }
}

/// `program`'s upgrade authority as venue records pin it (default = immutable).
pub async fn program_upgrade_authority(base: &mut Base, program: &Pubkey) -> Pubkey {
    let acct = raw(base, program_data_pda(program)).await.expect("ProgramData exists");
    program_data_state(&acct).1.unwrap_or_default()
}

/// Simulate an upgrade of `program` (new slot, same authority), as loader
/// Upgrade or the permissionless ExtendProgram would.
pub async fn bump_program_slot(base: &mut Base, program: &Pubkey) -> u64 {
    let acct = raw(base, program_data_pda(program)).await.expect("ProgramData exists");
    let (slot, authority) = program_data_state(&acct);
    set_program_data(base, program, slot + 1, authority).await;
    slot + 1
}

/// Simulate loader SetAuthority on `program`.
pub async fn set_upgrade_authority(base: &mut Base, program: &Pubkey, authority: Option<Pubkey>) {
    let acct = raw(base, program_data_pda(program)).await.expect("ProgramData exists");
    let (slot, _) = program_data_state(&acct);
    set_program_data(base, program, slot, authority).await;
}

pub fn register_venue_ix(base: &Base, admin: &Pubkey, program: &Pubkey, params: VenueParams, expected_upgrade_authority: Pubkey) -> Instruction {
    let accounts = perps_core::accounts::RegisterVenue {
        admin: *admin, config: config_pda(), market: base.market, venue_program: *program,
        venue_program_data: program_data_pda(program), venue: venue_pda(&base.market, program), system_program: system_program::ID,
    };
    perps_ix(accounts, perps_core::instruction::RegisterVenue { params, expected_upgrade_authority })
}

pub async fn try_register_venue(base: &mut Base, signer: &Keypair, program: &Pubkey, params: VenueParams, expected_upgrade_authority: Pubkey) -> Result<(), BanksClientError> {
    let ix = register_venue_ix(base, &signer.pubkey(), program, params, expected_upgrade_authority);
    send(&mut base.ctx, &[ix], &[signer]).await
}

/// Register `program` as a venue of the market as the admin, creating its fake
/// ProgramData (slot 1) if there is none. Returns the Venue address.
pub async fn register_venue(base: &mut Base, program: &Pubkey, params: VenueParams) -> Pubkey {
    if !exists(base, program_data_pda(program)).await {
        add_venue_program_data(base, program, 1).await;
    }
    let authority = program_upgrade_authority(base, program).await;
    let admin = base.admin.insecure_clone();
    try_register_venue(base, &admin, program, params, authority).await.unwrap();
    venue_pda(&base.market, program)
}

pub async fn update_venue(base: &mut Base, signer: &Keypair, program: &Pubkey, params: VenueParams, enabled: bool, expected_upgrade_authority: Pubkey) -> Result<(), BanksClientError> {
    let accounts = perps_core::accounts::UpdateVenue {
        admin: signer.pubkey(), config: config_pda(), venue: venue_pda(&base.market, program), venue_program_data: program_data_pda(program),
    };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::UpdateVenue { params, enabled, expected_upgrade_authority })], &[signer]).await
}

pub async fn disable_venue(base: &mut Base, signer: &Keypair, program: &Pubkey) -> Result<(), BanksClientError> {
    let accounts = perps_core::accounts::DisableVenue { signer: signer.pubkey(), config: config_pda(), venue: venue_pda(&base.market, program) };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::DisableVenue {})], &[signer]).await
}

pub async fn set_venue_fills_paused(base: &mut Base, signer: &Keypair, paused: bool) -> Result<(), BanksClientError> {
    let accounts = perps_core::accounts::SetVenueFillsPaused { signer: signer.pubkey(), config: config_pda() };
    send(&mut base.ctx, &[perps_ix(accounts, perps_core::instruction::SetVenueFillsPaused { paused })], &[signer]).await
}

pub async fn load_venue(base: &mut Base, program: &Pubkey) -> Venue {
    let v = venue_pda(&base.market, program);
    load(base, v).await
}

// --- approvals ----------------------------------------------------------------------

/// No limit price or deviation bound, the market's max leverage, a day to expiry.
pub fn approval_params(allowance: u64, now: i64) -> VenueApprovalParams {
    VenueApprovalParams { allowance, max_leverage: 20_000, max_buy_price: 0, min_sell_price: 0, max_price_deviation_bps: 0, expires_at: now + 86_400, allowed_sides: SIDE_LONG | SIDE_SHORT }
}

pub fn approve_ix(base: &Base, owner: &Pubkey, venue: &Pubkey, role: u8, params: VenueApprovalParams) -> Instruction {
    let accounts = perps_core::accounts::ApproveVenue {
        owner: *owner, market: base.market, venue: *venue, approval: approval_pda(owner, venue, role), system_program: system_program::ID,
        instructions: sysvar::instructions::ID,
    };
    perps_ix(accounts, perps_core::instruction::ApproveVenue { role, params })
}

/// Returns the approval address.
pub async fn approve(base: &mut Base, owner: &Keypair, venue: &Pubkey, role: u8, params: VenueApprovalParams) -> Result<Pubkey, BanksClientError> {
    let ix = approve_ix(base, &owner.pubkey(), venue, role, params);
    send(&mut base.ctx, &[ix], &[owner]).await?;
    Ok(approval_pda(&owner.pubkey(), venue, role))
}

pub fn update_approval_ix(base: &Base, owner: &Pubkey, venue: &Pubkey, role: u8, params: VenueApprovalParams) -> Instruction {
    let accounts = perps_core::accounts::UpdateVenueApproval {
        owner: *owner, market: base.market, venue: *venue, approval: approval_pda(owner, venue, role),
        instructions: sysvar::instructions::ID,
    };
    perps_ix(accounts, perps_core::instruction::UpdateVenueApproval { params })
}

pub async fn update_approval(base: &mut Base, owner: &Keypair, venue: &Pubkey, role: u8, params: VenueApprovalParams) -> Result<(), BanksClientError> {
    let ix = update_approval_ix(base, &owner.pubkey(), venue, role, params);
    send(&mut base.ctx, &[ix], &[owner]).await
}

pub fn revoke_ix(owner: &Pubkey, venue: &Pubkey, role: u8) -> Instruction {
    let accounts = perps_core::accounts::RevokeVenueApproval { owner: *owner, approval: approval_pda(owner, venue, role) };
    perps_ix(accounts, perps_core::instruction::RevokeVenueApproval {})
}

pub async fn revoke(base: &mut Base, owner: &Keypair, venue: &Pubkey, role: u8) -> Result<(), BanksClientError> {
    send(&mut base.ctx, &[revoke_ix(&owner.pubkey(), venue, role)], &[owner]).await
}

// --- venue fills ----------------------------------------------------------------------

/// Every account venue_fill takes, derived for `venue_program` in the market.
/// Venue programs forward these through their own CPI accounts.
#[derive(Clone, Copy, Debug)]
pub struct FillAccounts {
    pub venue_authority: Pubkey,
    pub venue_authority_bump: u8,
    pub config: Pubkey,
    pub market: Pubkey,
    pub venue: Pubkey,
    pub venue_program_data: Pubkey,
    pub pyth_price_feed: Pubkey,
    pub taker: Pubkey,
    pub taker_user: Pubkey,
    pub taker_approval: Pubkey,
    pub taker_position: Pubkey,
    pub maker: Pubkey,
    pub maker_user: Pubkey,
    pub maker_approval: Pubkey,
    pub maker_position: Pubkey,
    /// The market's LP pool (venue_fill account 17, docs/lp-pool.md §4.8)
    pub lp_pool: Pubkey,
}

pub fn fill_accounts(base: &Base, venue_program: &Pubkey, taker: &Pubkey, maker: &Pubkey, position_nonce: u64) -> FillAccounts {
    let market = base.market;
    let (venue_authority, venue_authority_bump) = venue_authority(&market, venue_program);
    let venue = venue_pda(&market, venue_program);
    let taker_position = vposition(taker, &market, position_nonce);
    FillAccounts {
        venue_authority,
        venue_authority_bump,
        config: config_pda(),
        market,
        venue,
        venue_program_data: program_data_pda(venue_program),
        pyth_price_feed: base.oracle,
        taker: *taker,
        taker_user: user_pda(taker, &market),
        taker_approval: approval_pda(taker, &venue, ROLE_TAKER),
        taker_position,
        maker: *maker,
        maker_user: user_pda(maker, &market),
        maker_approval: approval_pda(maker, &venue, ROLE_MAKER),
        maker_position: vmaker(&taker_position),
        lp_pool: lp_pool_pda(&market),
    }
}

impl FillAccounts {
    /// perps-core's account struct, in venue_fill's order.
    pub fn perps_accounts(&self, payer: &Pubkey) -> perps_core::accounts::VenueFill {
        perps_core::accounts::VenueFill {
            venue_authority: self.venue_authority,
            payer: *payer,
            config: self.config,
            market: self.market,
            venue: self.venue,
            venue_program_data: self.venue_program_data,
            pyth_price_feed: self.pyth_price_feed,
            taker: self.taker,
            taker_user: self.taker_user,
            taker_approval: self.taker_approval,
            taker_position: self.taker_position,
            maker: self.maker,
            maker_user: self.maker_user,
            maker_approval: self.maker_approval,
            maker_position: self.maker_position,
            system_program: system_program::ID,
            lp_pool: self.lp_pool,
        }
    }

    /// The perps-core state a fill reads or writes, for asserting a rejected
    /// fill changed nothing: both UserAccounts and approvals (raw bytes) and
    /// whether each position account exists.
    pub async fn snapshot(&self, base: &mut Base) -> FillSnapshot {
        let mut accounts = Vec::new();
        for a in [self.taker_user, self.maker_user, self.taker_approval, self.maker_approval] {
            accounts.push(raw(base, a).await.map(|a| a.data));
        }
        let positions = [exists(base, self.taker_position).await, exists(base, self.maker_position).await];
        FillSnapshot { accounts, positions }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct FillSnapshot {
    pub accounts: Vec<Option<Vec<u8>>>,
    pub positions: [bool; 2],
}

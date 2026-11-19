//! Orderbook end-to-end, in-process with perps-core loaded alongside: matched
//! orders settle through perps-core's venue_fill into real positions. With
//! SBF_OUT_DIR set, both programs run from their SBF binaries.

#[allow(dead_code)]
#[path = "../../perps-core/tests/support/market_env.rs"]
mod support;

use anchor_lang::error::ErrorCode as AnchorError;
use orderbook::{ErrorCode, Order, OrderBook, OrderSide, OrderStatus, OrderType, PlaceOrderParams};
use perps_core::errors::PerpsError;
use perps_core::instructions::VenueParams;
use perps_core::state::{ExecutionSource, Position, PositionStatus, Side};
use solana_program_test::{processor, ProgramTest};
use support::*;

fn book_process(program_id: &Pubkey, accounts: &[solana_sdk::account_info::AccountInfo], data: &[u8]) -> solana_sdk::entrypoint::ProgramResult {
    let accounts = Box::leak(Box::new(accounts.to_vec()));
    orderbook::entry(program_id, accounts, data)
}

fn err(e: ErrorCode) -> u32 {
    e.into()
}

fn unauthorized() -> u32 {
    err(ErrorCode::Unauthorized)
}

const DEPOSIT: u64 = 1_000 * USDC;

/// The orderbook venue's deploy defaults: 100 bps band, 30 s oracle, no
/// signers (both sides are resting orders; the approvals are the bound).
fn book_venue_params() -> VenueParams {
    VenueParams { max_price_deviation_bps: 100, max_oracle_age_secs: 30, min_signers: 0, max_fill_notional: 50_000 * USDC }
}

struct Env {
    base: Base,
    book: Pubkey,
    venue: Pubkey,
    alice: Trader,
    bob: Trader,
    next_nonce: u64,
    /// Passed to match_orders; 0 = no cap
    max_fill_size: u64,
    position_rent: u64,
    order_rent: u64,
}

impl Env {
    /// perps-core and the orderbook, with no book yet.
    async fn bare() -> Base {
        let pt = ProgramTest::new("orderbook", orderbook::ID, processor!(book_process));
        start(pt).await
    }

    /// A book matched by the admin and registered as a venue; alice and bob
    /// with $1,000 each, approved as taker and maker.
    async fn new() -> Env {
        let mut base = Env::bare().await;
        let book = pda(&[b"orderbook", base.market.as_ref()], &orderbook::ID);
        let admin = base.admin.insecure_clone();
        init_book(&mut base, &admin, book).await.unwrap();
        let venue = register_venue(&mut base, &orderbook::ID, book_venue_params()).await;
        let alice = trader(&mut base, DEPOSIT).await;
        let bob = trader(&mut base, DEPOSIT).await;
        let rent = base.ctx.banks_client.get_rent().await.unwrap();
        let mut env = Env {
            base, book, venue, alice, bob, next_nonce: 0, max_fill_size: 0,
            position_rent: rent.minimum_balance(Position::LEN), order_rent: rent.minimum_balance(Order::LEN),
        };
        for kp in [env.alice.kp.insecure_clone(), env.bob.kp.insecure_clone()] {
            env.approve_both(&kp).await;
        }
        env
    }

    async fn approve_both(&mut self, kp: &Keypair) {
        let t = now(&mut self.base).await;
        for role in [ROLE_TAKER, ROLE_MAKER] {
            approve(&mut self.base, kp, &self.venue, role, approval_params(DEPOSIT, t)).await.unwrap();
        }
    }

    fn nonce(&mut self) -> u64 {
        self.next_nonce += 1;
        self.next_nonce.wrapping_mul(0x9e37_79b9_7f4a_7c15)
    }

    /// A limit order of `contracts` at `dollars` and `lev_x`, one hour to
    /// expiry, 4 fills, default minimum fill.
    async fn params(&mut self, side: OrderSide, dollars: f64, contracts: u64, lev_x: u32) -> PlaceOrderParams {
        let t = now(&mut self.base).await;
        PlaceOrderParams {
            side, order_type: OrderType::Limit, price: usd(dollars), size: contracts * CONTRACT, leverage: lev_x * 1_000,
            expires_at: t + 3_600, max_fills: 4, min_fill_size: 0,
        }
    }

    async fn place_ix(&mut self, owner: &Pubkey, params: PlaceOrderParams) -> (Instruction, Pubkey) {
        let seq = load::<OrderBook>(&mut self.base, self.book).await.sequence_number;
        let order = pda(&[b"order", self.book.as_ref(), owner.as_ref(), &seq.to_le_bytes()], &orderbook::ID);
        let accounts = orderbook::accounts::PlaceOrder {
            owner: *owner, orderbook: self.book, market: self.base.market, order, system_program: system_program::ID,
        };
        (ix(orderbook::ID, accounts, orderbook::instruction::PlaceOrder { params }), order)
    }

    async fn try_place(&mut self, owner: &Keypair, params: PlaceOrderParams) -> Result<Pubkey, BanksClientError> {
        let (ix, order) = self.place_ix(&owner.pubkey(), params).await;
        send(&mut self.base.ctx, &[ix], &[owner]).await?;
        Ok(order)
    }

    async fn place(&mut self, owner: &Keypair, params: PlaceOrderParams) -> Pubkey {
        self.try_place(owner, params).await.unwrap()
    }

    /// Limit order with the defaults of `params`.
    async fn quick(&mut self, owner: &Keypair, side: OrderSide, dollars: f64, contracts: u64, lev_x: u32) -> Pubkey {
        let p = self.params(side, dollars, contracts, lev_x).await;
        self.place(owner, p).await
    }

    async fn order(&mut self, order: Pubkey) -> Order {
        load(&mut self.base, order).await
    }

    async fn book_state(&mut self) -> OrderBook {
        load(&mut self.base, self.book).await
    }

    /// match_orders for `bid` and `ask`, with the taker (the later order) and
    /// maker derived the way a keeper would.
    async fn match_ix(&mut self, matcher: &Pubkey, bid: Pubkey, ask: Pubkey, nonce: u64) -> (Instruction, FillAccounts) {
        let (b, a) = (self.order(bid).await, self.order(ask).await);
        let (taker, maker) = if b.sequence_number > a.sequence_number { (b.owner, a.owner) } else { (a.owner, b.owner) };
        let fa = fill_accounts(&self.base, &orderbook::ID, &taker, &maker, nonce);
        let accounts = orderbook::accounts::MatchOrders {
            matcher: *matcher,
            config: config_pda(),
            orderbook: self.book,
            bid_order: bid,
            ask_order: ask,
            bid_owner: b.owner,
            ask_owner: a.owner,
            venue_authority: fa.venue_authority,
            perps_venue: fa.venue,
            venue_program_data: fa.venue_program_data,
            market: fa.market,
            pyth_price_feed: fa.pyth_price_feed,
            taker_user: fa.taker_user,
            taker_approval: fa.taker_approval,
            taker_position: fa.taker_position,
            maker_user: fa.maker_user,
            maker_approval: fa.maker_approval,
            maker_position: fa.maker_position,
            perps_core_program: perps_core::ID,
            system_program: system_program::ID,
        };
        (ix(orderbook::ID, accounts, orderbook::instruction::MatchOrders { position_nonce: nonce, max_fill_size: self.max_fill_size }), fa)
    }

    async fn send_match(&mut self, matcher: &Keypair, ix: Instruction) -> Result<(), BanksClientError> {
        // Under SBF a match is a CPI-heavy instruction; the CU budget itself is
        // asserted separately in match_compute_units.
        send(&mut self.base.ctx, &[compute_budget_ix(400_000), ix], &[matcher]).await
    }

    /// Match with a fresh nonce; returns the fill accounts used.
    async fn try_match(&mut self, matcher: &Keypair, bid: Pubkey, ask: Pubkey) -> (Result<(), BanksClientError>, FillAccounts) {
        let n = self.nonce();
        let (ix, fa) = self.match_ix(&matcher.pubkey(), bid, ask, n).await;
        (self.send_match(matcher, ix).await, fa)
    }

    async fn do_match(&mut self, bid: Pubkey, ask: Pubkey) -> FillAccounts {
        let admin = self.base.admin.insecure_clone();
        let (result, fa) = self.try_match(&admin, bid, ask).await;
        result.unwrap();
        fa
    }

    /// Raw bytes and lamports of the book and both orders, plus perps-core's
    /// fill state: everything a failed match must leave as it was.
    async fn snapshot(&mut self, orders: &[Pubkey], fa: &FillAccounts) -> (Vec<Option<(u64, Vec<u8>)>>, FillSnapshot) {
        let mut accounts = Vec::new();
        for &a in [self.book].iter().chain(orders) {
            accounts.push(raw(&mut self.base, a).await.map(|a| (a.lamports, a.data)));
        }
        (accounts, fa.snapshot(&mut self.base).await)
    }

    /// Assert `ix` (a match of `bid`/`ask` with accounts `fa`) fails with `code`
    /// and changes nothing.
    async fn rejects_ix(&mut self, matcher: &Keypair, ix: Instruction, fa: &FillAccounts, orders: &[Pubkey], code: u32) {
        let before = self.snapshot(orders, fa).await;
        assert_eq!(custom_error(self.send_match(matcher, ix).await), code);
        assert_eq!(self.snapshot(orders, fa).await, before);
    }

    async fn rejects(&mut self, matcher: &Keypair, bid: Pubkey, ask: Pubkey, code: u32) {
        let n = self.nonce();
        let (ix, fa) = self.match_ix(&matcher.pubkey(), bid, ask, n).await;
        self.rejects_ix(matcher, ix, &fa, &[bid, ask], code).await;
    }

    async fn admin_rejects(&mut self, bid: Pubkey, ask: Pubkey, code: u32) {
        let admin = self.base.admin.insecure_clone();
        self.rejects(&admin, bid, ask, code).await;
    }

    async fn lamports(&mut self, who: Pubkey) -> u64 {
        lamports(&mut self.base, who).await
    }

    async fn set_matcher(&mut self, signer: &Keypair, matcher: Pubkey) -> Result<(), BanksClientError> {
        let accounts = orderbook::accounts::SetMatcher { admin: signer.pubkey(), config: config_pda(), orderbook: self.book };
        send(&mut self.base.ctx, &[ix(orderbook::ID, accounts, orderbook::instruction::SetMatcher { matcher })], &[signer]).await
    }

    async fn cancel(&mut self, owner: &Keypair, order: Pubkey) -> Result<(), BanksClientError> {
        let accounts = orderbook::accounts::CancelOrder { owner: owner.pubkey(), orderbook: self.book, order };
        send(&mut self.base.ctx, &[ix(orderbook::ID, accounts, orderbook::instruction::CancelOrder {})], &[owner]).await
    }

    /// close_expired_order needs no signer; the test payer (a third party) sends it.
    async fn close_expired(&mut self, order: Pubkey, owner: Pubkey) -> Result<(), BanksClientError> {
        let accounts = orderbook::accounts::CloseExpiredOrder { orderbook: self.book, order, owner };
        send(&mut self.base.ctx, &[ix(orderbook::ID, accounts, orderbook::instruction::CloseExpiredOrder {})], &[]).await
    }

    /// Order account rent plus `max_fills` Position rents.
    fn order_cost(&self, max_fills: u8) -> u64 {
        self.order_rent + max_fills as u64 * self.position_rent
    }
}

async fn init_book(base: &mut Base, signer: &Keypair, book: Pubkey) -> Result<(), BanksClientError> {
    let accounts = orderbook::accounts::InitializeOrderBook {
        authority: signer.pubkey(), config: config_pda(), market: base.market, orderbook: book, system_program: system_program::ID,
    };
    send(&mut base.ctx, &[ix(orderbook::ID, accounts, orderbook::instruction::InitializeOrderbook {})], &[signer]).await
}

#[tokio::test]
async fn only_the_perps_admin_creates_a_book() {
    let mut base = Env::bare().await;
    let book = pda(&[b"orderbook", base.market.as_ref()], &orderbook::ID);
    let stranger = funded_keypair(&mut base).await;
    assert_eq!(custom_error(init_book(&mut base, &stranger, book).await), unauthorized());
    let admin = base.admin.insecure_clone();
    init_book(&mut base, &admin, book).await.unwrap();
}

#[tokio::test]
async fn only_the_live_admin_matches_orders() {
    let mut env = Env::new().await;
    let admin = env.base.admin.insecure_clone();
    let stranger = funded_keypair(&mut env.base).await;
    let (alice, bob) = (env.alice.kp.insecure_clone(), env.bob.kp.insecure_clone());
    let bid = env.quick(&alice, OrderSide::Bid, 75.20, 3, 5).await;
    let ask1 = env.quick(&bob, OrderSide::Ask, 75.00, 1, 5).await;
    let ask2 = env.quick(&bob, OrderSide::Ask, 75.00, 1, 5).await;

    // Orders are loose accounts with no price-time priority: a public matcher
    // could choose which of them trade
    env.rejects(&stranger, bid, ask1, unauthorized()).await;
    env.do_match(bid, ask1).await;
    assert!(!exists(&mut env.base, ask1).await, "a filled order is closed");
    assert_eq!(env.order(bid).await.status, OrderStatus::PartiallyFilled);

    // The gate follows an admin handover; the old admin loses it
    let new_admin = rotate_admin(&mut env.base).await;
    env.rejects(&admin, bid, ask2, unauthorized()).await;
    let (result, _) = env.try_match(&new_admin, bid, ask2).await;
    result.unwrap();
    assert!(!exists(&mut env.base, ask2).await);
    let b = env.order(bid).await;
    assert_eq!((b.status, b.filled_size, b.fills), (OrderStatus::PartiallyFilled, 2 * CONTRACT, 2));
}

#[tokio::test]
async fn match_opens_positions_at_the_resting_ask() {
    let mut env = Env::new().await;
    let (alice, bob) = (env.alice.kp.insecure_clone(), env.bob.kp.insecure_clone());
    let admin = env.base.admin.pubkey();
    let (alice_lamports, bob_lamports) = (env.lamports(alice.pubkey()).await, env.lamports(bob.pubkey()).await);
    let ask = env.quick(&bob, OrderSide::Ask, 75.00, 10, 5).await;
    let bid = env.quick(&alice, OrderSide::Bid, 75.10, 10, 5).await;
    assert_eq!(env.lamports(alice.pubkey()).await, alice_lamports - env.order_cost(4));
    let admin_lamports = env.lamports(admin).await;
    let vault = vault_token_balance(&mut env.base).await;

    let fa = env.do_match(bid, ask).await;

    // Alice placed later, so she is the taker; both trade at bob's resting $75.00
    assert_eq!((fa.taker, fa.maker), (alice.pubkey(), bob.pubkey()));
    let taker: Position = load(&mut env.base, fa.taker_position).await;
    let maker: Position = load(&mut env.base, fa.maker_position).await;
    assert_eq!(fa.maker_position, vmaker(&fa.taker_position));
    assert_eq!((taker.owner, maker.owner), (alice.pubkey(), bob.pubkey()));
    assert!(taker.side == Side::Long && maker.side == Side::Short);
    for p in [&taker, &maker] {
        assert_eq!((p.size, p.entry_price, p.leverage, p.collateral), (10 * CONTRACT, 75_000_000, 5_000, 150_000_000));
        assert!(p.execution_source == ExecutionSource::Venue && p.status == PositionStatus::Open);
    }
    assert_eq!(user(&mut env.base, &alice.pubkey()).await.collateral_balance, DEPOSIT - 150_000_000);
    assert_eq!(user(&mut env.base, &bob.pubkey()).await.collateral_balance, DEPOSIT - 150_000_000);
    assert_eq!(vault_token_balance(&mut env.base).await, vault);

    // Both orders are closed. Each owner ends down exactly the rent of their
    // own position; the matcher (not the fee payer here) is made whole.
    assert!(!exists(&mut env.base, bid).await && !exists(&mut env.base, ask).await);
    let book = env.book_state().await;
    assert_eq!((book.total_bids, book.total_asks), (0, 0));
    assert_eq!(env.lamports(alice.pubkey()).await, alice_lamports - env.position_rent);
    assert_eq!(env.lamports(bob.pubkey()).await, bob_lamports - env.position_rent);
    assert_eq!(env.lamports(admin).await, admin_lamports);
}

#[tokio::test]
async fn reverse_placement_executes_at_the_bid() {
    let mut env = Env::new().await;
    let (alice, bob) = (env.alice.kp.insecure_clone(), env.bob.kp.insecure_clone());
    let bid = env.quick(&alice, OrderSide::Bid, 75.10, 10, 5).await;
    let ask = env.quick(&bob, OrderSide::Ask, 75.00, 10, 5).await;
    let fa = env.do_match(bid, ask).await;

    assert_eq!((fa.taker, fa.maker), (bob.pubkey(), alice.pubkey()));
    let taker: Position = load(&mut env.base, fa.taker_position).await;
    let maker: Position = load(&mut env.base, fa.maker_position).await;
    assert!(taker.side == Side::Short && maker.side == Side::Long);
    assert_eq!((taker.entry_price, maker.entry_price), (75_100_000, 75_100_000));
    // $751 / 5 = 150_200_000; alice's long is also $1 under water at the $75 oracle
    assert_eq!(taker.collateral, 150_200_000);
    assert_eq!(maker.collateral, 151_200_000);
}

#[tokio::test]
async fn adverse_resting_price_is_margined_and_nets_out() {
    // Spec A6: bob asks $75.60 first (80 bps off the $75 oracle, inside the
    // 100 bps band), alice bids $76.00; they trade at $75.60
    let mut env = Env::new().await;
    let (alice, bob) = (env.alice.kp.insecure_clone(), env.bob.kp.insecure_clone());
    let ask = env.quick(&bob, OrderSide::Ask, 75.60, 10, 5).await;
    let bid = env.quick(&alice, OrderSide::Bid, 76.00, 10, 5).await;
    let insurance = market(&mut env.base).await.insurance_fund;
    let fa = env.do_match(bid, ask).await;

    // Base $756 / 5 = 151_200_000 each; alice's long also posts its $6 loss at the oracle
    let taker: Position = load(&mut env.base, fa.taker_position).await;
    let maker: Position = load(&mut env.base, fa.maker_position).await;
    assert_eq!((taker.owner, taker.entry_price, taker.collateral), (alice.pubkey(), 75_600_000, 157_200_000));
    assert_eq!((maker.owner, maker.entry_price, maker.collateral), (bob.pubkey(), 75_600_000, 151_200_000));
    let owners = [alice.pubkey(), bob.pubkey()];
    let positions = [fa.taker_position, fa.maker_position];
    assert_eq!(market(&mut env.base).await.insurance_fund, insurance);
    assert_conservation(&mut env.base, usd(75.0), &owners, &positions).await;

    // Close both at $75: -/+ $6 PnL, a 378_000 fee on each $756 leg
    close(&mut env.base, &env.alice, fa.taker_position).await.unwrap();
    close(&mut env.base, &env.bob, fa.maker_position).await.unwrap();
    let (paid_alice, paid_bob) = (token_balance(&mut env.base, env.alice.tokens).await, token_balance(&mut env.base, env.bob.tokens).await);
    assert_eq!((paid_alice, paid_bob), (150_822_000, 156_822_000));
    assert_eq!(paid_alice + paid_bob, 157_200_000 + 151_200_000 - 756_000);
    assert_eq!(market(&mut env.base).await.insurance_fund, insurance + 756_000);
    assert_conservation(&mut env.base, usd(75.0), &owners, &positions).await;
}

#[tokio::test]
async fn partial_fills_reimburse_the_matcher_per_fill() {
    let mut env = Env::new().await;
    let (alice, bob) = (env.alice.kp.insecure_clone(), env.bob.kp.insecure_clone());
    let admin = env.base.admin.pubkey();
    let (alice_lamports, bob_lamports) = (env.lamports(alice.pubkey()).await, env.lamports(bob.pubkey()).await);
    let bid = env.quick(&alice, OrderSide::Bid, 75.00, 10, 5).await;
    // 0 resolves to size / max_fills
    assert_eq!(env.order(bid).await.min_fill_size, 10 * CONTRACT / 4);
    let ask1 = env.quick(&bob, OrderSide::Ask, 75.00, 4, 5).await;
    let admin_lamports = env.lamports(admin).await;
    let bid_lamports = env.lamports(bid).await;
    assert_eq!(bid_lamports, env.order_cost(4));

    let fa1 = env.do_match(bid, ask1).await;
    let b = env.order(bid).await;
    assert_eq!((b.status, b.filled_size, b.fills), (OrderStatus::PartiallyFilled, 4 * CONTRACT, 1));
    // One Position rent from each order, matching the two the matcher paid
    assert_eq!(env.lamports(bid).await, bid_lamports - env.position_rent);
    assert_eq!(env.lamports(admin).await, admin_lamports);
    assert!(!exists(&mut env.base, ask1).await);
    assert_eq!(env.book_state().await.total_bids, 1);

    let ask2 = env.quick(&bob, OrderSide::Ask, 75.00, 6, 5).await;
    let fa2 = env.do_match(bid, ask2).await;
    assert!(!exists(&mut env.base, bid).await && !exists(&mut env.base, ask2).await);
    assert_eq!(env.lamports(admin).await, admin_lamports);
    let book = env.book_state().await;
    assert_eq!((book.total_bids, book.total_asks), (0, 0));
    // Two fills, two positions each side, each owner paid exactly their positions' rent
    assert_eq!(env.lamports(alice.pubkey()).await, alice_lamports - 2 * env.position_rent);
    assert_eq!(env.lamports(bob.pubkey()).await, bob_lamports - 2 * env.position_rent);
    let m1: Position = load(&mut env.base, fa1.maker_position).await;
    let m2: Position = load(&mut env.base, fa2.maker_position).await;
    assert_eq!((m1.owner, m1.size, m2.owner, m2.size), (alice.pubkey(), 4 * CONTRACT, alice.pubkey(), 6 * CONTRACT));
}

#[tokio::test]
async fn min_fill_size_binds_both_orders() {
    let mut env = Env::new().await;
    let (alice, bob) = (env.alice.kp.insecure_clone(), env.bob.kp.insecure_clone());
    let mut p = env.params(OrderSide::Bid, 75.00, 10, 5).await;
    p.min_fill_size = 6 * CONTRACT;
    let bid = env.place(&alice, p).await;

    // 4 is the whole ask but below the bid's minimum
    let small = env.quick(&bob, OrderSide::Ask, 75.00, 4, 5).await;
    env.admin_rejects(bid, small, err(ErrorCode::FillTooSmall)).await;
    let six = env.quick(&bob, OrderSide::Ask, 75.00, 6, 5).await;
    env.do_match(bid, six).await;

    // 4 is now the bid's whole remainder, but below this ask's minimum
    let mut p = env.params(OrderSide::Ask, 75.00, 8, 5).await;
    p.min_fill_size = 6 * CONTRACT;
    let big = env.place(&bob, p).await;
    env.admin_rejects(bid, big, err(ErrorCode::FillTooSmall)).await;

    // Against an ask whose minimum it meets, the bid's remainder fills below its own minimum
    let ask = env.quick(&bob, OrderSide::Ask, 75.00, 8, 5).await;
    let fa = env.do_match(bid, ask).await;
    assert_eq!(load::<Position>(&mut env.base, fa.taker_position).await.size, 4 * CONTRACT);
    assert!(!exists(&mut env.base, bid).await);
    assert_eq!(env.order(ask).await.filled_size, 4 * CONTRACT);
}

#[tokio::test]
async fn max_fills_cancels_the_remainder() {
    let mut env = Env::new().await;
    let (alice, bob) = (env.alice.kp.insecure_clone(), env.bob.kp.insecure_clone());
    let alice_lamports = env.lamports(alice.pubkey()).await;
    let mut p = env.params(OrderSide::Bid, 75.00, 10, 5).await;
    p.max_fills = 1;
    p.min_fill_size = 1;
    let bid = env.place(&alice, p).await;
    assert_eq!(env.lamports(bid).await, env.order_cost(1));
    let ask = env.quick(&bob, OrderSide::Ask, 75.00, 4, 5).await;
    let fa = env.do_match(bid, ask).await;

    assert_eq!(load::<Position>(&mut env.base, fa.maker_position).await.size, 4 * CONTRACT);
    assert!(!exists(&mut env.base, bid).await, "remainder cancelled and the order closed");
    assert_eq!(env.book_state().await.total_bids, 0);
    assert_eq!(env.lamports(alice.pubkey()).await, alice_lamports - env.position_rent);
}

#[tokio::test]
async fn unfillable_orders_are_rejected() {
    let mut env = Env::new().await;
    let (alice, bob) = (env.alice.kp.insecure_clone(), env.bob.kp.insecure_clone());

    let low_bid = env.quick(&alice, OrderSide::Bid, 74.90, 10, 5).await;
    let ask = env.quick(&bob, OrderSide::Ask, 75.00, 10, 5).await;
    env.admin_rejects(low_bid, ask, err(ErrorCode::PriceMismatch)).await;

    let alice_ask = env.quick(&alice, OrderSide::Ask, 75.00, 10, 5).await;
    let bid = env.quick(&alice, OrderSide::Bid, 75.00, 10, 5).await;
    env.admin_rejects(bid, alice_ask, err(ErrorCode::SelfTrade)).await;

    // Legacy orders (no leverage, no deposit) never match; a legacy non-limit
    // order reports its type first
    patch::<Order>(&mut env.base, ask, |o| o.leverage = 0).await;
    env.admin_rejects(bid, ask, err(ErrorCode::LegacyOrder)).await;
    patch::<Order>(&mut env.base, ask, |o| o.order_type = OrderType::Market).await;
    env.admin_rejects(bid, ask, err(ErrorCode::UnsupportedOrderType)).await;
    patch::<Order>(&mut env.base, ask, |o| { o.order_type = OrderType::Limit; o.leverage = 5_000; o.fills = o.max_fills; }).await;
    env.admin_rejects(bid, ask, err(ErrorCode::RentDepositExhausted)).await;
    patch::<Order>(&mut env.base, ask, |o| { o.fills = 0; o.status = OrderStatus::Cancelled; }).await;
    env.admin_rejects(bid, ask, err(ErrorCode::InvalidOrder)).await;
    patch::<Order>(&mut env.base, ask, |o| o.status = OrderStatus::Open).await;

    // An order from another market's book
    let real_market = env.base.market;
    patch::<Order>(&mut env.base, ask, |o| o.market = Pubkey::new_unique()).await;
    env.admin_rejects(bid, ask, err(ErrorCode::WrongOrderBook)).await;
    patch::<Order>(&mut env.base, ask, |o| o.market = real_market).await;

    // Expired, one second after expires_at
    let expires_at = env.order(ask).await.expires_at;
    set_time(&mut env.base, expires_at + 1).await;
    env.admin_rejects(bid, ask, err(ErrorCode::OrderExpired)).await;
}

#[tokio::test]
async fn match_accounts_are_bound_to_the_book_and_orders() {
    let mut env = Env::new().await;
    let admin = env.base.admin.insecure_clone();
    let (alice, bob) = (env.alice.kp.insecure_clone(), env.bob.kp.insecure_clone());
    let ask = env.quick(&bob, OrderSide::Ask, 75.00, 10, 5).await;
    let bid = env.quick(&alice, OrderSide::Bid, 75.10, 10, 5).await;
    let carol = trader(&mut env.base, DEPOSIT).await;

    // market != orderbook.market (index 10 in MatchOrders)
    let n = env.nonce();
    let (mut ix, fa) = env.match_ix(&admin.pubkey(), bid, ask, n).await;
    assert_eq!(ix.accounts[10].pubkey, env.base.market);
    ix.accounts[10].pubkey = env.base.oracle;
    env.rejects_ix(&admin, ix, &fa, &[bid, ask], err(ErrorCode::WrongOrderBook)).await;

    // An owner account that is not the order's owner
    let (mut ix, fa) = env.match_ix(&admin.pubkey(), bid, ask, n).await;
    ix.accounts[5].pubkey = carol.key();
    env.rejects_ix(&admin, ix, &fa, &[bid, ask], aerr(AnchorError::ConstraintAddress)).await;

    // perps-core binds the forwarded accounts to the order owners: carol's
    // user account in alice's place fails its seeds
    let (mut ix, fa) = env.match_ix(&admin.pubkey(), bid, ask, n).await;
    ix.accounts[12].pubkey = carol.user;
    env.rejects_ix(&admin, ix, &fa, &[bid, ask], aerr(AnchorError::ConstraintSeeds)).await;

    // A nonce alice already used fails in perps-core's init (system AccountAlreadyInUse)
    let (ix, used) = env.match_ix(&admin.pubkey(), bid, ask, n).await;
    env.send_match(&admin, ix).await.unwrap();
    let ask2 = env.quick(&bob, OrderSide::Ask, 75.00, 10, 5).await;
    let bid2 = env.quick(&alice, OrderSide::Bid, 75.10, 10, 5).await;
    let (ix, fa) = env.match_ix(&admin.pubkey(), bid2, ask2, n).await;
    assert_eq!(fa.taker_position, used.taker_position);
    let before = env.snapshot(&[bid2, ask2], &fa).await;
    assert_eq!(custom_error(env.send_match(&admin, ix).await), 0);
    assert_eq!(env.snapshot(&[bid2, ask2], &fa).await, before);
}

fn aerr(e: AnchorError) -> u32 {
    anchor_err(e)
}

#[tokio::test]
async fn each_owner_needs_the_approval_for_its_role() {
    let mut env = Env::new().await;
    let bob = env.bob.kp.insecure_clone();
    let carol = trader(&mut env.base, DEPOSIT).await;
    let t = now(&mut env.base).await;
    let venue = env.venue;
    approve(&mut env.base, &carol.kp, &venue, ROLE_TAKER, approval_params(DEPOSIT, t)).await.unwrap();

    // Resting first makes carol the maker, which she has not approved
    let carol_ask = env.quick(&carol.kp, OrderSide::Ask, 75.00, 10, 5).await;
    let bob_bid = env.quick(&bob, OrderSide::Bid, 75.00, 10, 5).await;
    env.admin_rejects(bob_bid, carol_ask, aerr(AnchorError::AccountNotInitialized)).await;

    // As the later order she is the taker, which she has
    let bob_ask = env.quick(&bob, OrderSide::Ask, 75.00, 10, 5).await;
    let carol_bid = env.quick(&carol.kp, OrderSide::Bid, 75.00, 10, 5).await;
    let fa = env.do_match(carol_bid, bob_ask).await;
    assert_eq!(fa.taker, carol.key());
}

#[tokio::test]
async fn set_matcher_hands_matching_to_a_hot_key() {
    let mut env = Env::new().await;
    let admin = env.base.admin.insecure_clone();
    let (alice, bob) = (env.alice.kp.insecure_clone(), env.bob.kp.insecure_clone());
    let (hot, hot2) = (funded_keypair(&mut env.base).await, funded_keypair(&mut env.base).await);
    assert_eq!(env.book_state().await.matcher, Pubkey::default());

    assert_eq!(custom_error(env.set_matcher(&hot, hot.pubkey()).await), unauthorized());
    let mut p = env.params(OrderSide::Bid, 75.00, 10, 5).await;
    p.min_fill_size = 1;
    let bid = env.place(&alice, p).await;
    let ask = env.quick(&bob, OrderSide::Ask, 75.00, 1, 5).await;
    env.rejects(&hot, bid, ask, unauthorized()).await;

    env.set_matcher(&admin, hot.pubkey()).await.unwrap();
    assert_eq!(env.book_state().await.matcher, hot.pubkey());
    let hot_lamports = env.lamports(hot.pubkey()).await;
    let (result, _) = env.try_match(&hot, bid, ask).await;
    result.unwrap();
    assert_eq!(env.lamports(hot.pubkey()).await, hot_lamports);

    // The admin keeps matching; replacing the hot key revokes the old one
    let ask = env.quick(&bob, OrderSide::Ask, 75.00, 3, 5).await;
    env.set_matcher(&admin, hot2.pubkey()).await.unwrap();
    env.rejects(&hot, bid, ask, unauthorized()).await;
    env.do_match(bid, ask).await;
    let ask = env.quick(&bob, OrderSide::Ask, 75.00, 3, 5).await;
    let (result, _) = env.try_match(&hot2, bid, ask).await;
    result.unwrap();
}

#[tokio::test]
async fn place_order_validates_and_takes_the_deposit() {
    let mut env = Env::new().await;
    let alice = env.alice.kp.insecure_clone();
    let base = env.params(OrderSide::Bid, 75.00, 10, 5).await;
    let t = now(&mut env.base).await;
    let invalid = err(ErrorCode::InvalidOrderParams);

    let mut cases: Vec<(PlaceOrderParams, u32)> = Vec::new();
    for order_type in [OrderType::Market, OrderType::StopLoss, OrderType::TakeProfit] {
        cases.push((PlaceOrderParams { order_type, ..base }, err(ErrorCode::UnsupportedOrderType)));
    }
    for p in [
        PlaceOrderParams { price: 0, ..base },
        PlaceOrderParams { size: 0, ..base },
        PlaceOrderParams { leverage: 0, ..base },
        PlaceOrderParams { leverage: 20_001, ..base },
        PlaceOrderParams { expires_at: t, ..base },
        PlaceOrderParams { expires_at: t + orderbook::MAX_ORDER_TTL_SECS + 1, ..base },
        PlaceOrderParams { max_fills: 0, ..base },
        PlaceOrderParams { max_fills: orderbook::MAX_FILLS + 1, ..base },
        PlaceOrderParams { min_fill_size: base.size + 1, ..base },
    ] {
        cases.push((p, invalid));
    }
    let lamports_before = env.lamports(alice.pubkey()).await;
    for (p, code) in cases {
        assert_eq!(custom_error(env.try_place(&alice, p).await.map(|_| ())), code, "{:?}", p);
    }
    assert_eq!(env.lamports(alice.pubkey()).await, lamports_before);
    assert_eq!(env.book_state().await.sequence_number, 0);

    // Another account as the market
    let (mut ix, _) = env.place_ix(&alice.pubkey(), base).await;
    ix.accounts[2].pubkey = env.base.oracle;
    assert_eq!(custom_error(send(&mut env.base.ctx, &[ix], &[&alice]).await), aerr(AnchorError::AccountOwnedByWrongProgram));

    // The edges are accepted, and the deposit is max_fills Position rents
    let p = PlaceOrderParams {
        leverage: 20_000, expires_at: t + orderbook::MAX_ORDER_TTL_SECS, max_fills: orderbook::MAX_FILLS, min_fill_size: base.size, ..base
    };
    let order = env.place(&alice, p).await;
    assert_eq!(env.lamports(order).await, env.order_cost(orderbook::MAX_FILLS));
    assert_eq!(env.lamports(alice.pubkey()).await, lamports_before - env.order_cost(orderbook::MAX_FILLS));
    let o = env.order(order).await;
    assert_eq!((o.leverage, o.max_fills, o.fills, o.min_fill_size, o.status), (20_000, 16, 0, base.size, OrderStatus::Open));
}

#[tokio::test]
async fn expired_orders_close_permissionlessly_and_cancel_refunds() {
    let mut env = Env::new().await;
    let (alice, bob) = (env.alice.kp.insecure_clone(), env.bob.kp.insecure_clone());
    let stranger = funded_keypair(&mut env.base).await;
    let alice_lamports = env.lamports(alice.pubkey()).await;
    let mut p = env.params(OrderSide::Bid, 75.00, 10, 5).await;
    let t = now(&mut env.base).await;
    p.expires_at = t + 100;
    let bid = env.place(&alice, p).await;

    let not_expired = err(ErrorCode::OrderNotExpired);
    assert_eq!(custom_error(env.close_expired(bid, alice.pubkey()).await), not_expired);
    set_time(&mut env.base, t + 100).await;
    assert_eq!(custom_error(env.close_expired(bid, alice.pubkey()).await), not_expired);
    set_time(&mut env.base, t + 101).await;
    // The refund goes to the owner only
    assert_eq!(custom_error(env.close_expired(bid, stranger.pubkey()).await), unauthorized());
    env.close_expired(bid, alice.pubkey()).await.unwrap();
    assert!(!exists(&mut env.base, bid).await);
    assert_eq!(env.lamports(alice.pubkey()).await, alice_lamports);
    assert_eq!(env.book_state().await.total_bids, 0);

    let bob_lamports = env.lamports(bob.pubkey()).await;
    let ask = env.quick(&bob, OrderSide::Ask, 75.00, 10, 5).await;
    assert_eq!(env.book_state().await.total_asks, 1);
    env.cancel(&bob, ask).await.unwrap();
    assert!(!exists(&mut env.base, ask).await);
    assert_eq!(env.lamports(bob.pubkey()).await, bob_lamports);
    assert_eq!(env.book_state().await.total_asks, 0);
}

#[tokio::test]
async fn match_fails_atomically_when_collateral_is_gone() {
    let mut env = Env::new().await;
    let (alice, bob) = (env.alice.kp.insecure_clone(), env.bob.kp.insecure_clone());
    let ask = env.quick(&bob, OrderSide::Ask, 75.00, 10, 5).await;
    let bid = env.quick(&alice, OrderSide::Bid, 75.10, 10, 5).await;
    // Orders reserve nothing; the fill re-checks collateral
    withdraw(&mut env.base, &env.bob, DEPOSIT).await.unwrap();
    env.admin_rejects(bid, ask, perps_err(PerpsError::InsufficientCollateral)).await;
    let o = env.order(bid).await;
    assert_eq!((o.status, o.filled_size, o.fills), (OrderStatus::Open, 0, 0));
}

#[tokio::test]
async fn pre_built_transactions_survive_each_other() {
    // Spec A4: the taker position comes from the matcher's nonce, so nothing
    // landing first invalidates a transaction already built
    let mut env = Env::new().await;
    let admin = env.base.admin.insecure_clone();
    let (alice, bob) = (env.alice.kp.insecure_clone(), env.bob.kp.insecure_clone());
    let carol = trader(&mut env.base, DEPOSIT).await;

    // Match 1: alice is the maker. Match 2: alice is the taker.
    let alice_ask = env.quick(&alice, OrderSide::Ask, 75.00, 2, 5).await;
    let bob_bid = env.quick(&bob, OrderSide::Bid, 75.00, 2, 5).await;
    let bob_ask = env.quick(&bob, OrderSide::Ask, 75.00, 2, 5).await;
    let alice_bid = env.quick(&alice, OrderSide::Bid, 75.00, 2, 5).await;
    let (n1, n2) = (env.nonce(), env.nonce());
    let (ix1, fa1) = env.match_ix(&admin.pubkey(), bob_bid, alice_ask, n1).await;
    let (ix2, fa2) = env.match_ix(&admin.pubkey(), alice_bid, bob_ask, n2).await;
    let (open, position) = open_ix(&mut env.base, &carol.key(), 0, CONTRACT, 5_000).await;
    assert_eq!((fa1.maker, fa2.taker), (alice.pubkey(), alice.pubkey()));

    env.send_match(&admin, ix1).await.unwrap();
    env.send_match(&admin, ix2).await.unwrap();
    send(&mut env.base.ctx, &[open], &[&carol.kp]).await.unwrap();
    for p in [fa1.taker_position, fa1.maker_position, fa2.taker_position, fa2.maker_position, position] {
        assert!(load::<Position>(&mut env.base, p).await.status == PositionStatus::Open);
    }
}

#[tokio::test]
async fn book_positions_close_and_liquidate() {
    let mut env = Env::new().await;
    let (alice, bob) = (env.alice.kp.insecure_clone(), env.bob.kp.insecure_clone());
    let keeper = trader(&mut env.base, 0).await;
    let ask = env.quick(&bob, OrderSide::Ask, 75.00, 10, 10).await;
    let bid = env.quick(&alice, OrderSide::Bid, 75.00, 10, 10).await;
    let fa = env.do_match(bid, ask).await;
    assert_eq!(load::<Position>(&mut env.base, fa.maker_position).await.collateral, 75_000_000);

    // $79: bob's short has $35 of equity on $750, under the 5% maintenance margin
    set_price(&mut env.base, 79.0).await;
    liquidate(&mut env.base, &keeper, &bob.pubkey(), fa.maker_position).await.unwrap();
    assert!(load::<Position>(&mut env.base, fa.maker_position).await.status == PositionStatus::Liquidated);
    // notional x maintenance margin x liquidation fee = 750_000_000 x 5% x 1%
    assert_eq!(token_balance(&mut env.base, keeper.tokens).await, 375_000);

    // Alice's long is paid in full: collateral + $40 - 375_000 fee
    close(&mut env.base, &env.alice, fa.taker_position).await.unwrap();
    assert_eq!(token_balance(&mut env.base, env.alice.tokens).await, 75_000_000 + 40_000_000 - 375_000);
}

#[tokio::test]
async fn match_compute_units() {
    let mut env = Env::new().await;
    let admin = env.base.admin.insecure_clone();
    let (alice, bob) = (env.alice.kp.insecure_clone(), env.bob.kp.insecure_clone());
    let ask = env.quick(&bob, OrderSide::Ask, 75.00, 10, 5).await;
    let bid = env.quick(&alice, OrderSide::Bid, 75.10, 10, 5).await;
    let n = env.nonce();
    let (ix, _) = env.match_ix(&admin.pubkey(), bid, ask, n).await;
    let (result, units) = simulate(&mut env.base.ctx, &[compute_budget_ix(400_000), ix], &[&admin]).await;
    result.unwrap();
    println!("match_orders: {} CU", units);
    if is_sbf() {
        assert!(units > 0 && units < 200_000, "{} CU", units);
    }
}

/// A cross bigger than the venue's per-fill cap can be split by the matcher.
#[tokio::test]
async fn big_crosses_split_under_the_fill_cap() {
    let mut env = Env::new().await;
    let admin = env.base.admin.insecure_clone();
    let ua = program_upgrade_authority(&mut env.base, &orderbook::ID).await;
    let capped = VenueParams { max_fill_notional: 500 * USDC, ..book_venue_params() };
    update_venue(&mut env.base, &admin, &orderbook::ID, capped, true, ua).await.unwrap();
    let (alice, bob) = (env.alice.kp.insecure_clone(), env.bob.kp.insecure_clone());
    let ask = env.quick(&bob, OrderSide::Ask, 75.0, 10, 5).await;
    let bid = env.quick(&alice, OrderSide::Bid, 75.0, 10, 5).await;
    // All 10 at once is $750, over the $500 cap
    env.admin_rejects(bid, ask, perps_err(PerpsError::FillTooLarge)).await;

    env.max_fill_size = 5 * CONTRACT;
    env.do_match(bid, ask).await;
    let (b, a) = (env.order(bid).await, env.order(ask).await);
    assert_eq!((b.filled_size, a.filled_size), (5 * CONTRACT, 5 * CONTRACT));
    assert_eq!((b.status, a.status), (OrderStatus::PartiallyFilled, OrderStatus::PartiallyFilled));
    env.do_match(bid, ask).await;
    // Both filled and closed back to their owners
    assert!(!exists(&mut env.base, bid).await && !exists(&mut env.base, ask).await);

    // A cap below an order's minimum fill is still refused
    let ask = env.quick(&bob, OrderSide::Ask, 75.0, 4, 5).await;
    let bid = env.quick(&alice, OrderSide::Bid, 75.0, 4, 5).await;
    env.max_fill_size = 1;
    env.admin_rejects(bid, ask, err(ErrorCode::FillTooSmall)).await;
}

/// The old program marked orders Filled or Cancelled without closing them;
/// their rent is reclaimable, without touching the book's counters again.
#[tokio::test]
async fn legacy_finished_orders_can_be_closed() {
    let mut env = Env::new().await;
    let alice = env.alice.kp.insecure_clone();
    for status in [OrderStatus::Cancelled, OrderStatus::Filled] {
        let order = env.quick(&alice, OrderSide::Bid, 74.0, 1, 5).await;
        // As the old program left it: cancel_order took the order off the
        // counters, match_orders did not
        patch::<Order>(&mut env.base, order, |o| o.status = status).await;
        if status == OrderStatus::Cancelled {
            patch::<OrderBook>(&mut env.base, env.book, |b| b.total_bids -= 1).await;
        }
        let before = env.book_state().await.total_bids;
        let expected_after = if status == OrderStatus::Filled { before - 1 } else { before };
        let owner_before = env.lamports(alice.pubkey()).await;
        let rent = lamports(&mut env.base, order).await;
        env.close_expired(order, alice.pubkey()).await.unwrap();
        assert!(!exists(&mut env.base, order).await);
        assert_eq!(env.book_state().await.total_bids, expected_after);
        assert_eq!(env.lamports(alice.pubkey()).await, owner_before + rent);
    }
}

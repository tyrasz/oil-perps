//! Orderbook access control, in-process with perps-core loaded alongside.

#[allow(dead_code)]
#[path = "../../perps-core/tests/support/market_env.rs"]
mod support;

use orderbook::{OrderSide, OrderStatus, OrderType};
use solana_program_test::{processor, ProgramTest};
use support::*;

fn book_process(program_id: &Pubkey, accounts: &[solana_sdk::account_info::AccountInfo], data: &[u8]) -> solana_sdk::entrypoint::ProgramResult {
    let accounts = Box::leak(Box::new(accounts.to_vec()));
    orderbook::entry(program_id, accounts, data)
}

fn unauthorized() -> u32 {
    orderbook::ErrorCode::Unauthorized.into()
}

struct Env {
    base: Base,
    book: Pubkey,
    next_seq: u64,
}

impl Env {
    async fn new() -> Env {
        let mut pt = ProgramTest::new("orderbook", orderbook::ID, processor!(book_process));
        pt.prefer_bpf(false);
        let base = start(pt).await;
        let book = pda(&[b"orderbook", base.market.as_ref()], &orderbook::ID);
        Env { base, book, next_seq: 0 }
    }

    async fn init_book(&mut self, signer: &Keypair) -> Result<(), BanksClientError> {
        let accounts = orderbook::accounts::InitializeOrderBook {
            authority: signer.pubkey(), config: config_pda(), market: self.base.market, orderbook: self.book, system_program: system_program::ID,
        };
        send(&mut self.base.ctx, &[ix(orderbook::ID, accounts, orderbook::instruction::InitializeOrderbook {})], &[signer]).await
    }

    async fn place(&mut self, owner: &Keypair, side: OrderSide, price: u64, size: u64) -> Pubkey {
        let order = pda(&[b"order", self.book.as_ref(), owner.pubkey().as_ref(), &self.next_seq.to_le_bytes()], &orderbook::ID);
        self.next_seq += 1;
        let accounts = orderbook::accounts::PlaceOrder { owner: owner.pubkey(), orderbook: self.book, order, system_program: system_program::ID };
        let data = orderbook::instruction::PlaceOrder { side, order_type: OrderType::Limit, price, size, expires_at: 0 };
        send(&mut self.base.ctx, &[ix(orderbook::ID, accounts, data)], &[owner]).await.unwrap();
        order
    }

    async fn do_match(&mut self, matcher: &Keypair, bid: Pubkey, ask: Pubkey) -> Result<(), BanksClientError> {
        let accounts = orderbook::accounts::MatchOrders { matcher: matcher.pubkey(), config: config_pda(), orderbook: self.book, bid_order: bid, ask_order: ask };
        send(&mut self.base.ctx, &[ix(orderbook::ID, accounts, orderbook::instruction::MatchOrders {})], &[matcher]).await
    }

    async fn status(&mut self, order: Pubkey) -> OrderStatus {
        let acct = self.base.ctx.banks_client.get_account(order).await.unwrap().unwrap();
        let order: orderbook::Order = anchor_lang::AccountDeserialize::try_deserialize(&mut acct.data.as_slice()).unwrap();
        order.status
    }
}

#[tokio::test]
async fn only_the_perps_admin_creates_a_book() {
    let mut env = Env::new().await;
    let stranger = Keypair::new();
    fund(&mut env.base.ctx, &stranger.pubkey()).await;
    assert_eq!(custom_error(env.init_book(&stranger).await), unauthorized());
    let admin = env.base.admin.insecure_clone();
    env.init_book(&admin).await.unwrap();
}

#[tokio::test]
async fn only_the_live_admin_matches_orders() {
    let mut env = Env::new().await;
    let admin = env.base.admin.insecure_clone();
    env.init_book(&admin).await.unwrap();
    let (alice, bob, stranger) = (Keypair::new(), Keypair::new(), Keypair::new());
    for k in [&alice, &bob, &stranger] {
        fund(&mut env.base.ctx, &k.pubkey()).await;
    }
    let bid = env.place(&alice, OrderSide::Bid, 75_000_000, 3).await;
    let ask1 = env.place(&bob, OrderSide::Ask, 74_000_000, 1).await;
    let ask2 = env.place(&bob, OrderSide::Ask, 74_000_000, 1).await;

    // A public matcher could mark anyone's orders filled without settling anything
    assert_eq!(custom_error(env.do_match(&stranger, bid, ask1).await), unauthorized());
    env.do_match(&admin, bid, ask1).await.unwrap();
    assert_eq!(env.status(ask1).await, OrderStatus::Filled);
    assert_eq!(env.status(bid).await, OrderStatus::PartiallyFilled);

    // The gate follows an admin handover; the old admin loses it
    let new_admin = rotate_admin(&mut env.base).await;
    assert_eq!(custom_error(env.do_match(&admin, bid, ask2).await), unauthorized());
    env.do_match(&new_admin, bid, ask2).await.unwrap();
    assert_eq!(env.status(ask2).await, OrderStatus::Filled);
}

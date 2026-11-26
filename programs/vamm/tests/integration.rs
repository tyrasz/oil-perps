//! vAMM access control, in-process with perps-core loaded alongside.

#[allow(dead_code)]
#[path = "../../perps-core/tests/support/market_env.rs"]
mod support;

use solana_program_test::{processor, ProgramTest};
use support::*;

fn vamm_process(program_id: &Pubkey, accounts: &[solana_sdk::account_info::AccountInfo], data: &[u8]) -> solana_sdk::entrypoint::ProgramResult {
    // Anchor 1.2's entry wants one lifetime for the id, the accounts and the data
    let program_id: &Pubkey = Box::leak(Box::new(*program_id));
    let data: &[u8] = Box::leak(data.to_vec().into_boxed_slice());
    let accounts = Box::leak(Box::new(accounts.to_vec()));
    vamm::entry(program_id, accounts, data)
}

fn unauthorized() -> u32 {
    vamm::ErrorCode::Unauthorized.into()
}

struct Env {
    base: Base,
    vamm: Pubkey,
}

impl Env {
    async fn new() -> Env {
        let mut pt = ProgramTest::new("vamm", vamm::ID, processor!(vamm_process));
        pt.prefer_bpf(false);
        let base = start(pt).await;
        let vamm = pda(&[b"vamm", base.market.as_ref()], &vamm::ID);
        Env { base, vamm }
    }

    async fn init(&mut self, signer: &Keypair) -> Result<(), BanksClientError> {
        let accounts = vamm::accounts::InitializeVamm {
            authority: signer.pubkey(), config: config_pda(), market: self.base.market, vamm: self.vamm, system_program: system_program::ID,
        };
        let data = vamm::instruction::InitializeVamm { base_reserve: 1_000_000_000, quote_reserve: 75_000_000_000, base_spread: 10, max_spread: 100 };
        send(&mut self.base.ctx, &[ix(vamm::ID, accounts, data)], &[signer]).await
    }

    async fn open_long(&mut self, trader: &Keypair, size: u64) -> Result<(), BanksClientError> {
        let accounts = vamm::accounts::Trade { trader: trader.pubkey(), config: config_pda(), vamm: self.vamm };
        send(&mut self.base.ctx, &[ix(vamm::ID, accounts, vamm::instruction::OpenLong { size })], &[trader]).await
    }

    async fn pause(&mut self, signer: &Keypair) -> Result<(), BanksClientError> {
        let accounts = vamm::accounts::ManageVamm { authority: signer.pubkey(), config: config_pda(), vamm: self.vamm };
        send(&mut self.base.ctx, &[ix(vamm::ID, accounts, vamm::instruction::Pause {})], &[signer]).await
    }
}

#[tokio::test]
async fn nobody_but_the_admin_can_squat_a_markets_vamm() {
    let mut env = Env::new().await;
    let stranger = Keypair::new();
    fund(&mut env.base.ctx, &stranger.pubkey()).await;
    assert_eq!(custom_error(env.init(&stranger).await), unauthorized());
    let admin = env.base.admin.insecure_clone();
    env.init(&admin).await.unwrap();
}

#[tokio::test]
async fn trading_and_pausing_follow_the_live_admin() {
    let mut env = Env::new().await;
    let admin = env.base.admin.insecure_clone();
    env.init(&admin).await.unwrap();
    let stranger = Keypair::new();
    fund(&mut env.base.ctx, &stranger.pubkey()).await;

    // Trades post no collateral, so a public trader could push the price anywhere
    assert_eq!(custom_error(env.open_long(&stranger, 1_000).await), unauthorized());
    env.open_long(&admin, 1_000).await.unwrap();

    let new_admin = rotate_admin(&mut env.base).await;
    assert_eq!(custom_error(env.open_long(&admin, 1_000).await), unauthorized());
    assert_eq!(custom_error(env.pause(&admin).await), unauthorized());
    env.pause(&new_admin).await.unwrap();
    // Paused: even the admin can't trade
    assert_eq!(custom_error(env.open_long(&new_admin, 1_000).await), u32::from(vamm::ErrorCode::VammPaused));
}

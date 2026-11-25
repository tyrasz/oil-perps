//! mock-pyth end to end: natively in-process, or the SBF binary when
//! `SBF_OUT_DIR` is set (`SBF_OUT_DIR=$PWD/target/deploy-localnet`). The feed
//! is read back with pyth-sdk-solana 0.8 and perps-core's price checks, the
//! way `oracle::read_price` reads it.

use mock_pyth::{initialize_data, set_authority_data, set_price_data, PRICE_ACCOUNT_SIZE};
use pyth_sdk_solana::state::{load_price_account, PriceStatus};
use solana_program_test::{processor, BanksClientError, ProgramTest, ProgramTestContext};
use solana_sdk::{
    account::Account,
    account_info::AccountInfo,
    clock::Clock,
    instruction::{AccountMeta, Instruction, InstructionError},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction,
    transaction::{Transaction, TransactionError},
};

// perps-core's dependency-free price checks (also #[path]-included by the keeper)
#[allow(dead_code)]
#[path = "../../perps-core/src/price.rs"]
mod price;

const T0: i64 = 1_794_607_200;

async fn setup() -> ProgramTestContext {
    let pt = ProgramTest::new("mock_pyth", mock_pyth::ID, processor!(mock_pyth::process_instruction));
    let mut ctx = pt.start_with_context().await;
    set_time(&mut ctx, T0).await;
    ctx
}

async fn set_time(ctx: &mut ProgramTestContext, ts: i64) {
    let mut clock: Clock = ctx.banks_client.get_sysvar().await.unwrap();
    clock.unix_timestamp = ts;
    ctx.set_sysvar(&clock);
}

async fn send(ctx: &mut ProgramTestContext, ixs: &[Instruction], signers: &[&Keypair]) -> Result<(), BanksClientError> {
    let blockhash = ctx.get_new_latest_blockhash().await.unwrap();
    let mut all: Vec<&Keypair> = vec![&ctx.payer];
    all.extend_from_slice(signers);
    let tx = Transaction::new_signed_with_payer(ixs, Some(&ctx.payer.pubkey()), &all, blockhash);
    ctx.banks_client.process_transaction(tx).await
}

fn ix(feed: &Pubkey, feed_signs: bool, authority: &Pubkey, data: Vec<u8>) -> Instruction {
    let feed_meta = if feed_signs { AccountMeta::new(*feed, true) } else { AccountMeta::new(*feed, false) };
    Instruction { program_id: mock_pyth::ID, accounts: vec![feed_meta, AccountMeta::new_readonly(*authority, true)], data }
}

async fn create_ix(ctx: &mut ProgramTestContext, feed: &Keypair, space: usize) -> Instruction {
    let rent = ctx.banks_client.get_rent().await.unwrap();
    system_instruction::create_account(&ctx.payer.pubkey(), &feed.pubkey(), rent.minimum_balance(space), space as u64, &mock_pyth::ID)
}

/// A new feed at $75.00 ± $0.05 (expo -8), authority `authority`.
async fn new_feed(ctx: &mut ProgramTestContext, authority: &Keypair) -> Keypair {
    let feed = Keypair::new();
    let create = create_ix(ctx, &feed, PRICE_ACCOUNT_SIZE).await;
    let init = ix(&feed.pubkey(), true, &authority.pubkey(), initialize_data(7_500_000_000, 5_000_000, -8));
    send(ctx, &[create, init], &[&feed, authority]).await.unwrap();
    feed
}

async fn account(ctx: &mut ProgramTestContext, key: &Pubkey) -> Account {
    ctx.banks_client.get_account(*key).await.unwrap().unwrap()
}

/// What perps-core's `read_price` sees: pyth-sdk's price no older than 60 s
/// at `now`, then perps-core's confidence and decimal checks.
fn read_price(key: &Pubkey, acct: &mut Account, now: i64) -> Option<u64> {
    let mut lamports = acct.lamports;
    let info = AccountInfo::new(key, false, false, &mut lamports, &mut acct.data, &acct.owner, false, 0);
    let feed = pyth_sdk_solana::load_price_feed_from_account_info(&info).ok()?;
    let p = feed.get_price_no_older_than(now, price::MAX_PRICE_AGE_SECS)?;
    price::validate_price(p.price, p.conf, p.expo, price::DEFAULT_MAX_CONFIDENCE_BPS, 0).ok()
}

fn custom(result: Result<(), BanksClientError>) -> InstructionError {
    match result.expect_err("should fail").unwrap() {
        TransactionError::InstructionError(_, e) => e,
        other => panic!("unexpected {:?}", other),
    }
}

#[tokio::test]
async fn feed_reads_as_pyth_with_on_chain_publish_time() {
    let mut ctx = setup().await;
    let authority = Keypair::new();
    let feed = new_feed(&mut ctx, &authority).await;
    let mut acct = account(&mut ctx, &feed.pubkey()).await;
    assert_eq!(acct.owner, mock_pyth::ID);
    assert_eq!(acct.data.len(), 3_312);
    {
        let pa = load_price_account(&acct.data).unwrap();
        assert_eq!((pa.expo, pa.num, pa.timestamp, pa.prev_timestamp), (-8, 1, T0, T0));
        assert_eq!((pa.agg.price, pa.agg.conf, pa.agg.status), (7_500_000_000, 5_000_000, PriceStatus::Trading));
        assert_eq!(pa.comp[0].publisher.to_bytes(), authority.pubkey().to_bytes());
        assert_eq!((pa.prev_price, pa.ema_price.val), (7_500_000_000, 7_500_000_000));
    }
    // Fresh within 60 s of the publish time, stale after
    assert_eq!(read_price(&feed.pubkey(), &mut acct, T0), Some(75_000_000));
    assert_eq!(read_price(&feed.pubkey(), &mut acct, T0 + 60), Some(75_000_000));
    assert_eq!(read_price(&feed.pubkey(), &mut acct, T0 + 61), None);

    // An update is stamped with the clock at that time
    set_time(&mut ctx, T0 + 500).await;
    let set = ix(&feed.pubkey(), false, &authority.pubkey(), set_price_data(8_012_345_678, 4_000_000, -8, 1));
    send(&mut ctx, &[set], &[&authority]).await.unwrap();
    let mut acct = account(&mut ctx, &feed.pubkey()).await;
    {
        let pa = load_price_account(&acct.data).unwrap();
        assert_eq!((pa.timestamp, pa.agg.price, pa.agg.conf), (T0 + 500, 8_012_345_678, 4_000_000));
        assert_eq!((pa.prev_price, pa.prev_timestamp), (7_500_000_000, T0));
        assert!(pa.last_slot >= pa.prev_slot);
    }
    assert_eq!(read_price(&feed.pubkey(), &mut acct, T0 + 520), Some(80_123_456));
    assert_eq!(read_price(&feed.pubkey(), &mut acct, T0 + 561), None);
    // A wide band fails perps-core's 1% confidence check
    let wide = ix(&feed.pubkey(), false, &authority.pubkey(), set_price_data(7_500_000_000, 150_000_000, -8, 1));
    send(&mut ctx, &[wide], &[&authority]).await.unwrap();
    let mut acct = account(&mut ctx, &feed.pubkey()).await;
    assert_eq!(read_price(&feed.pubkey(), &mut acct, T0 + 500), None);
}

/// Not Trading: readers get the last Trading price at its publish time,
/// which then goes stale like a halted Pyth feed.
#[tokio::test]
async fn halted_feed_serves_the_last_trading_price() {
    let mut ctx = setup().await;
    let authority = Keypair::new();
    let feed = new_feed(&mut ctx, &authority).await;
    set_time(&mut ctx, T0 + 30).await;
    let halt = ix(&feed.pubkey(), false, &authority.pubkey(), set_price_data(9_000_000_000, 5_000_000, -8, 2));
    send(&mut ctx, &[halt], &[&authority]).await.unwrap();
    let mut acct = account(&mut ctx, &feed.pubkey()).await;
    assert_eq!(read_price(&feed.pubkey(), &mut acct, T0 + 30), Some(75_000_000));
    assert_eq!(read_price(&feed.pubkey(), &mut acct, T0 + 61), None);
    // Halted again: prev_* still holds the last Trading price
    set_time(&mut ctx, T0 + 40).await;
    let halt = ix(&feed.pubkey(), false, &authority.pubkey(), set_price_data(9_100_000_000, 5_000_000, -8, 2));
    send(&mut ctx, &[halt], &[&authority]).await.unwrap();
    let mut acct = account(&mut ctx, &feed.pubkey()).await;
    assert_eq!(read_price(&feed.pubkey(), &mut acct, T0 + 40), Some(75_000_000));
    // Trading resumes at the new price and time
    set_time(&mut ctx, T0 + 1_000).await;
    let resume = ix(&feed.pubkey(), false, &authority.pubkey(), set_price_data(6_600_000_000, 5_000_000, -8, 1));
    send(&mut ctx, &[resume], &[&authority]).await.unwrap();
    let mut acct = account(&mut ctx, &feed.pubkey()).await;
    assert_eq!(read_price(&feed.pubkey(), &mut acct, T0 + 1_000), Some(66_000_000));
}

#[tokio::test]
async fn only_the_authority_writes() {
    let mut ctx = setup().await;
    let authority = Keypair::new();
    let stranger = Keypair::new();
    let feed = new_feed(&mut ctx, &authority).await;
    let by_stranger = ix(&feed.pubkey(), false, &stranger.pubkey(), set_price_data(1, 0, -8, 1));
    assert_eq!(custom(send(&mut ctx, &[by_stranger], &[&stranger]).await), InstructionError::IllegalOwner);
    // Re-initializing is refused, even by the authority
    let again = ix(&feed.pubkey(), true, &authority.pubkey(), initialize_data(1, 0, -8));
    assert_eq!(custom(send(&mut ctx, &[again], &[&feed, &authority]).await), InstructionError::AccountAlreadyInitialized);
    // Unknown status, bad data
    let bad_status = ix(&feed.pubkey(), false, &authority.pubkey(), set_price_data(1, 0, -8, 4));
    assert_eq!(custom(send(&mut ctx, &[bad_status], &[&authority]).await), InstructionError::InvalidInstructionData);
    let short = ix(&feed.pubkey(), false, &authority.pubkey(), vec![1, 2, 3]);
    assert_eq!(custom(send(&mut ctx, &[short], &[&authority]).await), InstructionError::InvalidInstructionData);
    // Hand over the authority: the old one is out
    let new_authority = Keypair::new();
    let hand_over = ix(&feed.pubkey(), false, &authority.pubkey(), set_authority_data(&new_authority.pubkey()));
    send(&mut ctx, &[hand_over], &[&authority]).await.unwrap();
    let old = ix(&feed.pubkey(), false, &authority.pubkey(), set_price_data(7_000_000_000, 0, -8, 1));
    assert_eq!(custom(send(&mut ctx, &[old], &[&authority]).await), InstructionError::IllegalOwner);
    let new = ix(&feed.pubkey(), false, &new_authority.pubkey(), set_price_data(7_000_000_000, 0, -8, 1));
    send(&mut ctx, &[new], &[&new_authority]).await.unwrap();
    let acct = account(&mut ctx, &feed.pubkey()).await;
    assert_eq!(load_price_account(&acct.data).unwrap().agg.price, 7_000_000_000);
}

#[tokio::test]
async fn initialize_needs_a_fresh_signed_account_of_the_right_size() {
    let mut ctx = setup().await;
    let authority = Keypair::new();
    // Without the feed's signature
    let feed = Keypair::new();
    let create = create_ix(&mut ctx, &feed, PRICE_ACCOUNT_SIZE).await;
    send(&mut ctx, &[create], &[&feed]).await.unwrap();
    let unsigned = ix(&feed.pubkey(), false, &authority.pubkey(), initialize_data(1, 0, -8));
    assert_eq!(custom(send(&mut ctx, &[unsigned], &[&authority]).await), InstructionError::MissingRequiredSignature);
    // SetPrice before Initialize
    let early = ix(&feed.pubkey(), false, &authority.pubkey(), set_price_data(1, 0, -8, 1));
    assert_eq!(custom(send(&mut ctx, &[early], &[&authority]).await), InstructionError::UninitializedAccount);
    // Wrong size
    let small = Keypair::new();
    let create = create_ix(&mut ctx, &small, 3_311).await;
    let init = ix(&small.pubkey(), true, &authority.pubkey(), initialize_data(1, 0, -8));
    assert_eq!(custom(send(&mut ctx, &[create, init], &[&small, &authority]).await), InstructionError::InvalidAccountData);
    // An account this program does not own (the payer's own system account)
    let payer = ctx.payer.pubkey();
    let foreign = ix(&payer, true, &authority.pubkey(), initialize_data(1, 0, -8));
    assert_eq!(custom(send(&mut ctx, &[foreign], &[&authority]).await), InstructionError::IncorrectProgramId);
}

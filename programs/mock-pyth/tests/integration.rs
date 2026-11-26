//! mock-pyth end to end: natively in-process, or the SBF binary when
//! `SBF_OUT_DIR` is set (`SBF_OUT_DIR=$PWD/target/deploy-localnet`). The feed
//! is read back with perps-core's `PriceUpdateV2` reader (`pyth_receiver.rs`,
//! pinned to pyth-solana-receiver-sdk 2.0.0 by perps-core's parity test) and
//! price checks, the way `oracle::read_price` reads it.

use mock_pyth::pyth_receiver::{self, ParseRejection, PriceUpdateV2View};
use mock_pyth::{
    initialize_data, read_update, set_authority_data, set_feed_id_data, set_price_data, set_verification_data, PRICE_ACCOUNT_SIZE,
};
use solana_program_test::{processor, BanksClientError, ProgramTest, ProgramTestContext};
use solana_sdk::{
    account::Account,
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
const FEED_ID: [u8; 32] = [0x92; 32];

async fn setup() -> ProgramTestContext {
    let pt = ProgramTest::new("mock_pyth", mock_pyth::ID, processor!(mock_pyth::process_instruction));
    let mut ctx = pt.start_with_context().await;
    // Lets the native (non-SBF) program reach program-test's clock stub
    solana_sdk::native_stubs::install();
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

/// A new feed at $75.00 ± $0.05 (expo -8) for `FEED_ID`, authority `authority`.
async fn new_feed(ctx: &mut ProgramTestContext, authority: &Keypair) -> Keypair {
    let feed = Keypair::new();
    let create = create_ix(ctx, &feed, PRICE_ACCOUNT_SIZE).await;
    let init = ix(&feed.pubkey(), true, &authority.pubkey(), initialize_data(&FEED_ID, 7_500_000_000, 5_000_000, -8));
    send(ctx, &[create, init], &[&feed, authority]).await.unwrap();
    feed
}

async fn account(ctx: &mut ProgramTestContext, key: &Pubkey) -> Account {
    ctx.banks_client.get_account(*key).await.unwrap().unwrap()
}

async fn update(ctx: &mut ProgramTestContext, key: &Pubkey) -> PriceUpdateV2View {
    pyth_receiver::parse_price_update_v2(&account(ctx, key).await.data).unwrap()
}

/// What perps-core's `read_price` sees (after its key and owner checks): a
/// Full update for `FEED_ID` published within 60 s of `now`, then its
/// confidence and decimal checks.
fn read_price(acct: &Account, now: i64) -> Option<u64> {
    let v = pyth_receiver::parse_price_update_v2(&acct.data).ok()?;
    if v.feed_id != FEED_ID || !pyth_receiver::is_fresh(v.publish_time, now, price::MAX_PRICE_AGE_SECS) {
        return None;
    }
    price::validate_price(v.price, v.conf, v.exponent, price::DEFAULT_MAX_CONFIDENCE_BPS, 0).ok()
}

fn custom(result: Result<(), BanksClientError>) -> InstructionError {
    match result.expect_err("should fail").unwrap() {
        TransactionError::InstructionError(_, e) => e,
        other => panic!("unexpected {:?}", other),
    }
}

#[tokio::test]
async fn feed_reads_as_a_price_update_with_on_chain_publish_time() {
    let mut ctx = setup().await;
    let authority = Keypair::new();
    let feed = new_feed(&mut ctx, &authority).await;
    let acct = account(&mut ctx, &feed.pubkey()).await;
    assert_eq!(acct.owner, mock_pyth::ID);
    assert_eq!(acct.data.len(), 134);
    let slot0 = {
        let v = pyth_receiver::parse_price_update_v2(&acct.data).unwrap();
        assert_eq!(v.write_authority, authority.pubkey().to_bytes());
        assert_eq!(v.feed_id, FEED_ID);
        assert_eq!((v.price, v.conf, v.exponent), (7_500_000_000, 5_000_000, -8));
        assert_eq!((v.publish_time, v.prev_publish_time), (T0, T0));
        assert_eq!((v.ema_price, v.ema_conf), (7_500_000_000, 5_000_000));
        assert_eq!(acct.data[133], 0);
        v.posted_slot
    };
    // Fresh within 60 s of the publish time, either way; stale after
    assert_eq!(read_price(&acct, T0), Some(75_000_000));
    assert_eq!(read_price(&acct, T0 + 60), Some(75_000_000));
    assert_eq!(read_price(&acct, T0 - 60), Some(75_000_000));
    assert_eq!(read_price(&acct, T0 + 61), None);

    // An update is stamped with the clock at that time
    set_time(&mut ctx, T0 + 500).await;
    let set = ix(&feed.pubkey(), false, &authority.pubkey(), set_price_data(8_012_345_678, 4_000_000, -8, 1));
    send(&mut ctx, &[set], &[&authority]).await.unwrap();
    let acct = account(&mut ctx, &feed.pubkey()).await;
    let v = pyth_receiver::parse_price_update_v2(&acct.data).unwrap();
    assert_eq!((v.publish_time, v.prev_publish_time, v.price, v.conf), (T0 + 500, T0, 8_012_345_678, 4_000_000));
    assert!(v.posted_slot >= slot0);
    assert_eq!(read_price(&acct, T0 + 520), Some(80_123_456));
    assert_eq!(read_price(&acct, T0 + 561), None);
    // A wide band fails perps-core's 1% confidence check
    let wide = ix(&feed.pubkey(), false, &authority.pubkey(), set_price_data(7_500_000_000, 150_000_000, -8, 1));
    send(&mut ctx, &[wide], &[&authority]).await.unwrap();
    let acct = account(&mut ctx, &feed.pubkey()).await;
    assert_eq!(read_price(&acct, T0 + 500), None);
}

/// Not Trading: nothing is written, so the last update stays and goes stale
/// like a Pyth feed whose market is closed.
#[tokio::test]
async fn halted_feed_freezes_and_goes_stale() {
    let mut ctx = setup().await;
    let authority = Keypair::new();
    let feed = new_feed(&mut ctx, &authority).await;
    let before = account(&mut ctx, &feed.pubkey()).await.data;
    set_time(&mut ctx, T0 + 30).await;
    for status in [0u32, 2, 3] {
        let halt = ix(&feed.pubkey(), false, &authority.pubkey(), set_price_data(9_000_000_000 + status as i64, 5_000_000, -8, status));
        send(&mut ctx, &[halt], &[&authority]).await.unwrap();
        assert_eq!(account(&mut ctx, &feed.pubkey()).await.data, before, "status {}", status);
    }
    let acct = account(&mut ctx, &feed.pubkey()).await;
    assert_eq!(read_price(&acct, T0 + 30), Some(75_000_000));
    assert_eq!(read_price(&acct, T0 + 61), None);
    // Trading resumes at the new price and time
    set_time(&mut ctx, T0 + 1_000).await;
    let resume = ix(&feed.pubkey(), false, &authority.pubkey(), set_price_data(6_600_000_000, 5_000_000, -8, 1));
    send(&mut ctx, &[resume], &[&authority]).await.unwrap();
    let acct = account(&mut ctx, &feed.pubkey()).await;
    assert_eq!(read_price(&acct, T0 + 1_000), Some(66_000_000));
    let v = pyth_receiver::parse_price_update_v2(&acct.data).unwrap();
    assert_eq!((v.publish_time, v.prev_publish_time), (T0 + 1_000, T0));
}

#[tokio::test]
async fn verification_level_and_feed_id_can_be_set_for_negative_tests() {
    let mut ctx = setup().await;
    let authority = Keypair::new();
    let feed = new_feed(&mut ctx, &authority).await;
    let full = account(&mut ctx, &feed.pubkey()).await.data;

    let partial = ix(&feed.pubkey(), false, &authority.pubkey(), set_verification_data(0, 7));
    send(&mut ctx, &[partial], &[&authority]).await.unwrap();
    let acct = account(&mut ctx, &feed.pubkey()).await;
    assert_eq!(pyth_receiver::parse_price_update_v2(&acct.data), Err(ParseRejection::NotFullyVerified));
    let (v, level, n) = read_update(&acct.data).unwrap();
    assert_eq!((level, n), (0, 7));
    assert_eq!(acct.data, v.to_partial_bytes(7));
    // Prices still move in the Partial layout
    set_time(&mut ctx, T0 + 10).await;
    let set = ix(&feed.pubkey(), false, &authority.pubkey(), set_price_data(7_600_000_000, 5_000_000, -8, 1));
    send(&mut ctx, &[set], &[&authority]).await.unwrap();
    let (v, level, _) = read_update(&account(&mut ctx, &feed.pubkey()).await.data).unwrap();
    assert_eq!((level, v.price, v.publish_time), (0, 7_600_000_000, T0 + 10));
    // Back to Full: the Full layout again
    let back = ix(&feed.pubkey(), false, &authority.pubkey(), set_verification_data(1, 9));
    send(&mut ctx, &[back], &[&authority]).await.unwrap();
    let v = update(&mut ctx, &feed.pubkey()).await;
    assert_eq!((v.price, v.publish_time), (7_600_000_000, T0 + 10));
    assert_eq!(full.len(), account(&mut ctx, &feed.pubkey()).await.data.len());
    // An unknown level is refused
    let bad = ix(&feed.pubkey(), false, &authority.pubkey(), set_verification_data(2, 0));
    assert_eq!(custom(send(&mut ctx, &[bad], &[&authority]).await), InstructionError::InvalidInstructionData);

    // Another feed id
    let other = [0x11u8; 32];
    let set_id = ix(&feed.pubkey(), false, &authority.pubkey(), set_feed_id_data(&other));
    send(&mut ctx, &[set_id], &[&authority]).await.unwrap();
    let acct = account(&mut ctx, &feed.pubkey()).await;
    assert_eq!(pyth_receiver::parse_price_update_v2(&acct.data).unwrap().feed_id, other);
    assert_eq!(read_price(&acct, T0 + 10), None);
}

#[tokio::test]
async fn only_the_authority_writes() {
    let mut ctx = setup().await;
    let authority = Keypair::new();
    let stranger = Keypair::new();
    let feed = new_feed(&mut ctx, &authority).await;
    for data in [set_price_data(1, 0, -8, 1), set_verification_data(0, 1), set_feed_id_data(&[1; 32]), set_authority_data(&stranger.pubkey())] {
        let by_stranger = ix(&feed.pubkey(), false, &stranger.pubkey(), data);
        assert_eq!(custom(send(&mut ctx, &[by_stranger], &[&stranger]).await), InstructionError::IllegalOwner);
    }
    // Re-initializing is refused, even by the authority
    let again = ix(&feed.pubkey(), true, &authority.pubkey(), initialize_data(&FEED_ID, 1, 0, -8));
    assert_eq!(custom(send(&mut ctx, &[again], &[&feed, &authority]).await), InstructionError::AccountAlreadyInitialized);
    // Unknown status, bad data
    let bad_status = ix(&feed.pubkey(), false, &authority.pubkey(), set_price_data(1, 0, -8, 4));
    assert_eq!(custom(send(&mut ctx, &[bad_status], &[&authority]).await), InstructionError::InvalidInstructionData);
    let short = ix(&feed.pubkey(), false, &authority.pubkey(), vec![1, 2, 3]);
    assert_eq!(custom(send(&mut ctx, &[short], &[&authority]).await), InstructionError::InvalidInstructionData);
    let unknown = ix(&feed.pubkey(), false, &authority.pubkey(), vec![9]);
    assert_eq!(custom(send(&mut ctx, &[unknown], &[&authority]).await), InstructionError::InvalidInstructionData);
    // Hand over the authority: the old one is out
    let new_authority = Keypair::new();
    let hand_over = ix(&feed.pubkey(), false, &authority.pubkey(), set_authority_data(&new_authority.pubkey()));
    send(&mut ctx, &[hand_over], &[&authority]).await.unwrap();
    assert_eq!(update(&mut ctx, &feed.pubkey()).await.write_authority, new_authority.pubkey().to_bytes());
    let old = ix(&feed.pubkey(), false, &authority.pubkey(), set_price_data(7_000_000_000, 0, -8, 1));
    assert_eq!(custom(send(&mut ctx, &[old], &[&authority]).await), InstructionError::IllegalOwner);
    let new = ix(&feed.pubkey(), false, &new_authority.pubkey(), set_price_data(7_000_000_000, 0, -8, 1));
    send(&mut ctx, &[new], &[&new_authority]).await.unwrap();
    assert_eq!(update(&mut ctx, &feed.pubkey()).await.price, 7_000_000_000);
}

#[tokio::test]
async fn initialize_needs_a_fresh_signed_account_of_the_right_size() {
    let mut ctx = setup().await;
    let authority = Keypair::new();
    // Without the feed's signature
    let feed = Keypair::new();
    let create = create_ix(&mut ctx, &feed, PRICE_ACCOUNT_SIZE).await;
    send(&mut ctx, &[create], &[&feed]).await.unwrap();
    let unsigned = ix(&feed.pubkey(), false, &authority.pubkey(), initialize_data(&FEED_ID, 1, 0, -8));
    assert_eq!(custom(send(&mut ctx, &[unsigned], &[&authority]).await), InstructionError::MissingRequiredSignature);
    // SetPrice before Initialize
    let early = ix(&feed.pubkey(), false, &authority.pubkey(), set_price_data(1, 0, -8, 1));
    assert_eq!(custom(send(&mut ctx, &[early], &[&authority]).await), InstructionError::UninitializedAccount);
    // Initialize data of the wrong length (the pre-D47 21-byte form)
    let mut old_form = vec![0u8];
    old_form.extend_from_slice(&[0u8; 20]);
    let old = ix(&feed.pubkey(), true, &authority.pubkey(), old_form);
    assert_eq!(custom(send(&mut ctx, &[old], &[&feed, &authority]).await), InstructionError::InvalidInstructionData);
    // Wrong size (the legacy 3_312-byte account, or one byte short)
    for space in [3_312usize, 133] {
        let small = Keypair::new();
        let create = create_ix(&mut ctx, &small, space).await;
        let init = ix(&small.pubkey(), true, &authority.pubkey(), initialize_data(&FEED_ID, 1, 0, -8));
        assert_eq!(custom(send(&mut ctx, &[create, init], &[&small, &authority]).await), InstructionError::InvalidAccountData);
    }
    // An account this program does not own (the payer's own system account)
    let payer = ctx.payer.pubkey();
    let foreign = ix(&payer, true, &authority.pubkey(), initialize_data(&FEED_ID, 1, 0, -8));
    assert_eq!(custom(send(&mut ctx, &[foreign], &[&authority]).await), InstructionError::IncorrectProgramId);
}

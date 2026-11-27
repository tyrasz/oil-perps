// Pyth's two Solana program pairs and the push oracle's feed-account
// address (docs/lp-pool.md D52). A perps-core module; prop-amm `include!`s
// it. Keep `//` comments: `include!` rejects `//!`.
//
// The Pyth Receiver lets anyone post a verified `PriceUpdateV2` into an
// account of their own (`post_update` / `post_update_atomic` with
// themselves as write authority), and overwrite it later with any verified
// update, older ones included, or close it (`reclaim_rent`). Only the push
// oracle's feed accounts move forward only: the push oracle writes the PDA
// `[shard_id as u16 LE, feed_id]` under its own program id, with that PDA
// as write authority, and only with a newer publish time. So where a
// market's (or vault's) oracle program is one of Pyth's two Receivers, the
// account pinned for a feed id must be that PDA for shard 0; for any other
// owner (the localnet mock, the tests' fake program) there is no push
// oracle to derive from and the account is not constrained here.

use anchor_lang::prelude::*;

/// Pyth Receiver, pro-compatible build (pyth-solana-receiver-sdk 2.0.0
/// `pro-compatible`), and the push oracle that writes its feed accounts.
pub const PYTH_RECEIVER_PRO: Pubkey = pubkey!("rec2HHDDnjLfj4kE7VyEtFA1HPGQLK33259532cRyHp");
pub const PYTH_PUSH_ORACLE_PRO: Pubkey = pubkey!("pyt2F414BA6dPttK6RddPZUdHfapoBN24GL5wbrPCou");
/// Pyth Receiver, default build, and its push oracle.
pub const PYTH_RECEIVER_LEGACY: Pubkey = pubkey!("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");
pub const PYTH_PUSH_ORACLE_LEGACY: Pubkey = pubkey!("pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT");
/// The push-oracle shard the protocol pins (the one Pyth's docs and the
/// scripts' `pushFeedAddress` use).
pub const PUSH_FEED_SHARD: u16 = 0;

/// The push oracle that writes `receiver`'s feed accounts, for Pyth's two
/// Receivers; `None` for any other program.
pub fn push_oracle_for(receiver: &Pubkey) -> Option<Pubkey> {
    if *receiver == PYTH_RECEIVER_PRO {
        Some(PYTH_PUSH_ORACLE_PRO)
    } else if *receiver == PYTH_RECEIVER_LEGACY {
        Some(PYTH_PUSH_ORACLE_LEGACY)
    } else {
        None
    }
}

/// The push oracle's shard-0 feed account for `feed_id`.
pub fn push_feed_address(push_oracle: &Pubkey, feed_id: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[&PUSH_FEED_SHARD.to_le_bytes(), feed_id], push_oracle).0
}

/// Whether `feed` may be pinned for `feed_id` under `oracle_program`: the
/// push oracle's shard-0 PDA when `oracle_program` is a Pyth Receiver, any
/// account otherwise.
pub fn is_pinnable_feed(feed: &Pubkey, oracle_program: &Pubkey, feed_id: &[u8; 32]) -> bool {
    match push_oracle_for(oracle_program) {
        Some(push) => *feed == push_feed_address(&push, feed_id),
        None => true,
    }
}

#[cfg(test)]
mod pyth_push_tests {
    use super::*;

    fn hex32(s: &str) -> [u8; 32] {
        let mut b = [0u8; 32];
        for (i, out) in b.iter_mut().enumerate() {
            *out = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
        }
        b
    }

    const OIL: &str = "925ca92ff005ae943c158e3563f59698ce7e75c5a8c8dd43303a0a154887b3e6";

    /// Pyth's documented SOL/USD shard-0 account, and accounts that exist
    /// on mainnet at these addresses, owned by the matching Receiver (D53):
    /// SOL/USD and XAU/USD under the pro pair, and USOILSPOT (the former OIL
    /// feed id) under the legacy one.
    #[test]
    fn derivation_matches_pyths_published_address() {
        let sol = hex32("ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d");
        assert_eq!(push_feed_address(&PYTH_PUSH_ORACLE_LEGACY, &sol), pubkey!("7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE"));
        assert_eq!(push_feed_address(&PYTH_PUSH_ORACLE_PRO, &sol), pubkey!("7AviUf9nL62mcxNbQGKm4nKDQnPjswo6c5MX4D57HmyE"));
        let xau = hex32("765d2ba906dbc32ca17cc11f5310a89e9ee1f6420508c63861f2f8ba4ee34bb2");
        assert_eq!(push_feed_address(&PYTH_PUSH_ORACLE_PRO, &xau), pubkey!("csXV1aFF7uAfTHYWx6dDj718hnf7dUDU8v54dubib17"));
        assert_eq!(push_feed_address(&PYTH_PUSH_ORACLE_PRO, &hex32(OIL)), pubkey!("65sQHMgQBPWnV9nPGL5PKsBmAs3B24cJcFzFh9GwPip7"));
        assert_eq!(push_feed_address(&PYTH_PUSH_ORACLE_LEGACY, &hex32(OIL)), pubkey!("4LPPjSGx5s3fvANM78zVhVFhEUQSJGHc1PUFKRjR76sX"));
    }

    #[test]
    fn only_pyth_receivers_constrain_the_account() {
        let oil = hex32(OIL);
        let pro = push_feed_address(&PYTH_PUSH_ORACLE_PRO, &oil);
        let legacy = push_feed_address(&PYTH_PUSH_ORACLE_LEGACY, &oil);
        assert!(is_pinnable_feed(&pro, &PYTH_RECEIVER_PRO, &oil));
        assert!(is_pinnable_feed(&legacy, &PYTH_RECEIVER_LEGACY, &oil));
        // The other pair's account, another feed id's, a keypair account
        assert!(!is_pinnable_feed(&legacy, &PYTH_RECEIVER_PRO, &oil));
        assert!(!is_pinnable_feed(&pro, &PYTH_RECEIVER_LEGACY, &oil));
        assert!(!is_pinnable_feed(&pro, &PYTH_RECEIVER_PRO, &[7; 32]));
        assert!(!is_pinnable_feed(&Pubkey::new_unique(), &PYTH_RECEIVER_PRO, &oil));
        // Any other owner: not constrained
        assert!(is_pinnable_feed(&Pubkey::new_unique(), &Pubkey::new_unique(), &oil));
        assert_eq!(push_oracle_for(&PYTH_PUSH_ORACLE_PRO), None);
    }
}

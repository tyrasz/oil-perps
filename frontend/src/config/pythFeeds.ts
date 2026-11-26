// Pyth on Solana: the program ids and the Pyth Core feed id of every
// commodity (docs/lp-pool.md D47, D51). Dependency-free so the deploy
// scripts (scripts/pyth-feeds.ts) and the localnet harness share it with the
// frontend: one table, no second copy to drift.
//
// A market pins one account: the push oracle's sponsored shard-0 feed
// account for its feed id, PDA([shard_id as u16 LE, feed_id], push oracle),
// a PriceUpdateV2 owned by the Pyth Receiver (ProtocolConfig.oracle_program).
// Pyth sponsors no commodity feed on Solana: the protocol keeps these
// accounts fresh itself (Pyth's price pusher), and the first push creates
// the account, so a market can only be created after it. Whether any of the
// accounts exists on a cluster was not checked here.

/**
 * The two Pyth program pairs. `pro` ("pro-compatible", the upgraded Core
 * programs Pyth's upgrade guide points to) is the recommendation, `legacy`
 * the original ones; the per-feed account addresses differ between them.
 * Which pair mainnet uses must be checked on chain before launch (owner
 * decision, docs/lp-pool.md §18).
 */
export const PYTH_PROGRAM_SETS = {
  pro: {
    /** Pyth Solana Receiver: owner of every PriceUpdateV2 account */
    receiver: 'rec2HHDDnjLfj4kE7VyEtFA1HPGQLK33259532cRyHp',
    /** Pyth push oracle ("price feed program"): derives the feed accounts */
    pushOracle: 'pyt2F414BA6dPttK6RddPZUdHfapoBN24GL5wbrPCou',
    wormhole: 'HDw2E7P8X1SkCyjvoGsfBGAVUutKcj874bXjHrpVYrVL',
  },
  legacy: {
    receiver: 'rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ',
    pushOracle: 'pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT',
    wormhole: 'HDwcJBJXjL9FpJ7UBsYBtaDjsBUhuLCUYoz3zr8SWWaQ',
  },
} as const;
export type PythProgramSet = keyof typeof PYTH_PROGRAM_SETS;
export const DEFAULT_PYTH_PROGRAM_SET: PythProgramSet = 'pro';

/** The push oracle's sponsored shard: the one every market's account is derived on. */
export const PYTH_PUSH_SHARD_ID = 0;

/**
 * Pyth Core (Hermes) feed ids, 32-byte hex. `null` = no verified Pyth Core
 * feed id: that market cannot launch (scripts/initialize-market.ts refuses
 * it) until one is found and checked.
 *
 * - OIL: Commodities.USOILSPOT, a non-expiring CFD tracking WTI (no roll; its
 *   hours differ from CME's, and a mismatch fails closed with
 *   StaleOraclePrice). The CME-hour WTI futures feeds are dated and would
 *   need a monthly rotation (owner decision, docs/lp-pool.md §18). Same id as
 *   backend/api/src/oracle.rs.
 * - GOLD: Metal.XAU/USD. SILVER: Metal.XAG/USD.
 * - NATGAS: only dated Henry Hub futures (deprecated monthly) and an index
 *   with a Pyth Pro id only were found. COPPER: only Pro/Terminal ids.
 *
 * Check every id against Pyth's feed list before mainnet.
 */
export const PYTH_FEED_IDS: Record<string, string | null> = {
  OIL: '925ca92ff005ae943c158e3563f59698ce7e75c5a8c8dd43303a0a154887b3e6',
  GOLD: '765d2ba906dbc32ca17cc11f5310a89e9ee1f6420508c63861f2f8ba4ee34bb2',
  SILVER: 'f2fb02c32b055c805e7238d628e5e9dadef274376114eb1f012337cabe93871e',
  NATGAS: null,
  COPPER: null,
};

/** 32 bytes from a feed id in hex (an optional 0x prefix); throws on anything else. */
export function feedIdFromHex(hex: string): Uint8Array {
  const h = hex.startsWith('0x') || hex.startsWith('0X') ? hex.slice(2) : hex;
  if (!/^[0-9a-fA-F]{64}$/.test(h)) throw new Error(`not a 32-byte hex feed id: ${hex}`);
  const out = new Uint8Array(32);
  for (let i = 0; i < 32; i++) out[i] = parseInt(h.slice(2 * i, 2 * i + 2), 16);
  return out;
}

/** Lowercase hex, no prefix. */
export function feedIdToHex(id: Uint8Array | number[]): string {
  return Array.from(id, (b) => b.toString(16).padStart(2, '0')).join('');
}

/**
 * Seeds of the push oracle's feed account for `feedId` on `shardId`:
 * `[shard_id.to_le_bytes() (u16), feed_id]`. Derive the address with
 * `PublicKey.findProgramAddressSync(pushFeedSeeds(id), pushOracle)`.
 */
export function pushFeedSeeds(feedId: Uint8Array, shardId = PYTH_PUSH_SHARD_ID): Uint8Array[] {
  if (feedId.length !== 32) throw new Error('a feed id is 32 bytes');
  if (!Number.isInteger(shardId) || shardId < 0 || shardId > 0xffff) throw new Error('a shard id is a u16');
  return [new Uint8Array([shardId & 0xff, shardId >> 8]), feedId];
}

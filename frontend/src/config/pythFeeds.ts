// Pyth on Solana: the program ids and the Pyth Core feed id of every
// commodity (docs/lp-pool.md D47, D51). Dependency-free so the deploy
// scripts (scripts/pyth-feeds.ts) and the localnet harness share it with the
// frontend: one table, no second copy to drift.
//
// A market pins one account: the push oracle's sponsored shard-0 feed
// account for its feed id, PDA([shard_id as u16 LE, feed_id], push oracle),
// a PriceUpdateV2 owned by the Pyth Receiver (ProtocolConfig.oracle_program).
// Checked on mainnet on 2026-11-27 (docs/lp-pool.md D53): of these feeds Pyth
// sponsors only GOLD (XAU/USD, fresh on both program pairs); the OIL, NATGAS
// and COPPER accounts do not exist on either pair and SILVER's exists only
// under the legacy pair, frozen since 2026-07-03. The protocol keeps those
// accounts fresh itself (Pyth's price pusher, with a Pyth API key), and the
// first push creates the account, so a market can only be created after it.

/**
 * The two Pyth program pairs, both live on mainnet (all six programs
 * deployed; checked 2026-11-27, D53). `pro` is Pyth's "Pyth Core
 * Compatible" pair, the one its Solana guide uses; `legacy` the original
 * "Pyth Core" pair, upgraded in place on 2026-08-26. Pyth's docs say both
 * work and that sponsored updates reach the feed accounts of both; the
 * per-feed account addresses differ between them. Which one to pin is the
 * owner's choice (docs/lp-pool.md §18 #18).
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
 * Pyth Core (Hermes) feed ids, 32-byte hex, each checked against Pyth's own
 * catalog (Hermes `/v2/price_feeds`, 2026-11-27; docs/lp-pool.md D53). `null`
 * = no verified Pyth Core feed id: that market cannot launch
 * (scripts/initialize-market.ts refuses it).
 *
 * Every feed here trades on Pyth's CME Globex schedule (America/New_York,
 * Sun 18:00 to Fri 17:00 with a 17:00–18:00 daily break and CME holiday
 * hours), the hours perps-core's SCHEDULE_CME_GLOBEX models, and none
 * expires, so there is no monthly roll:
 * - OIL: Commodities.Index.WTI1M/USD, "constant 1M WTI 23/5", a rolling
 *   constant-maturity one-month WTI index (not spot, not a dated contract).
 *   It replaces Commodities.USOILSPOT (925ca92f…b3e6), which Pyth dropped at
 *   its 2026-08-26 Core upgrade: gone from Hermes, and its legacy mainnet
 *   account was last written that day.
 * - GOLD: Metal.XAU/USD. SILVER: Metal.XAG/USD (spot).
 * - NATGAS: Commodities.Index.HHGAS1M/USD, constant 1M Henry Hub, 23/5.
 * - COPPER: Commodities.Index.COPPER/USD, "copper 23/5".
 *
 * The OIL, NATGAS and COPPER choices are the owner's to confirm (§18 #17).
 * The 24/7 indices (PYTHOIL, NATGAS, CU) and the London-hours ones (RWTI,
 * LWTI1M, RNATGAS, HLGAS1M, Metal.CC3M) do not match the CME schedule.
 */
export const PYTH_FEED_IDS: Record<string, string | null> = {
  OIL: 'f2464139c45f26097e0173b03207c4a490dbd8bf1b72dfbf064eeb832c7c2933',
  GOLD: '765d2ba906dbc32ca17cc11f5310a89e9ee1f6420508c63861f2f8ba4ee34bb2',
  SILVER: 'f2fb02c32b055c805e7238d628e5e9dadef274376114eb1f012337cabe93871e',
  NATGAS: '3d8ec049064b20008f62b36ac247a751a1c57d8f082132e64d42cd74668db7c1',
  COPPER: '4073f4c331bf7920a3887536093b57c78ea535e88ac09c761e5b949221c33414',
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

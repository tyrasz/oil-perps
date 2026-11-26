// The oracle account a market points at (docs/lp-pool.md D47): a Pyth Solana
// Receiver `PriceUpdateV2` in pyth-solana-receiver-sdk 2.0.0's layout, read
// the way perps-core's `pyth_receiver.rs` / `oracle::read_price` read it. On
// localnet it is the TEST ONLY mock-pyth feed, which writes the same layout
// (docs/localnet.md). Dependency-free (no Buffer, no web3.js) so the TS
// suite, the deploy scripts and the localnet harness use it too.
//
//   [0..8)     discriminator sha256("account:PriceUpdateV2")[..8]
//   [8..40)    write_authority
//   [40]       verification level: 0 Partial { num_signatures: u8 }, 1 Full
//   Full (the only level perps-core accepts; 133 bytes, accounts are 134):
//   [41..73) feed_id, [73] price i64, [81] conf u64, [89] exponent i32,
//   [93] publish_time i64, [101] prev_publish_time, [109] ema_price,
//   [117] ema_conf u64, [125] posted_slot u64
//   Partial: num_signatures at 41, every later field one byte further.

/** `Market.pyth_price_feed` (8-byte discriminator included; pinned by perps-core's sdk_layout_is_pinned). */
export const MARKET_PYTH_PRICE_FEED_OFFSET = 104;
/** `Market.long_open_interest` and `short_open_interest` (u64, 6-dp contracts), after the six u32 ratios and fees. */
export const MARKET_LONG_OI_OFFSET = 168;
export const MARKET_SHORT_OI_OFFSET = 176;
/** `Market.pyth_feed_id` ([u8; 32], D47). */
export const MARKET_PYTH_FEED_ID_OFFSET = 295;
/** `Market.oracle_program`: the owner every read of the market's feed checks (D47). */
export const MARKET_ORACLE_PROGRAM_OFFSET = 327;

/** Long and short open interest in contracts, from a Market account's data; null if too short. */
export function decodeMarketOpenInterest(data: Uint8Array): { long: number; short: number } | null {
  if (data.length < MARKET_SHORT_OI_OFFSET + 8) return null;
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  return {
    long: Number(view.getBigUint64(MARKET_LONG_OI_OFFSET, true)) / 1e6,
    short: Number(view.getBigUint64(MARKET_SHORT_OI_OFFSET, true)) / 1e6,
  };
}

/** The market's Pyth feed id (32 bytes), or null for a Market account too short to have one. */
export function decodeMarketFeedId(data: Uint8Array): Uint8Array | null {
  if (data.length < MARKET_PYTH_FEED_ID_OFFSET + 32) return null;
  return data.slice(MARKET_PYTH_FEED_ID_OFFSET, MARKET_PYTH_FEED_ID_OFFSET + 32);
}

/** `PriceUpdateV2::LEN`: the size the Receiver (and mock-pyth) allocates. */
export const PRICE_UPDATE_V2_LEN = 134;
/** A Full update's serialized length: the shortest account perps-core accepts. */
export const PRICE_UPDATE_V2_FULL_LEN = 133;
/** sha256("account:PriceUpdateV2")[..8] = 22f123639d7ef4cd */
export const PRICE_UPDATE_V2_DISCRIMINATOR = Uint8Array.from([34, 241, 35, 99, 157, 126, 244, 205]);
export const VERIFICATION_PARTIAL = 0;
export const VERIFICATION_FULL = 1;
/** Byte offsets of a Full update (discriminator included). */
export const PRICE_UPDATE_V2_OFFSETS = {
  writeAuthority: 8,
  verificationLevel: 40,
  feedId: 41,
  price: 73,
  conf: 81,
  exponent: 89,
  publishTime: 93,
  prevPublishTime: 101,
  emaPrice: 109,
  emaConf: 117,
  postedSlot: 125,
  /** Partial updates store num_signatures here and shift every later field by one byte. */
  partialNumSignatures: 41,
} as const;
/** perps-core's `MAX_PRICE_AGE_SECS`: `read_price` accepts a price at most this old. */
export const MAX_PRICE_AGE_SECS = 60;
/** `pyth_receiver::MAX_FUTURE_PUBLISH_SECS`: how far a publish time may be ahead of the clock, whatever the age limit (docs/lp-pool.md D52). */
export const MAX_FUTURE_PUBLISH_SECS = 60;

/** A Full update's fields (integers as on chain). */
export interface PriceUpdateV2 {
  writeAuthority: Uint8Array;
  feedId: Uint8Array;
  price: bigint;
  conf: bigint;
  exponent: number;
  publishTime: bigint;
  prevPublishTime: bigint;
  emaPrice: bigint;
  emaConf: bigint;
  postedSlot: bigint;
}

/**
 * Why an account is not a usable update, in the order perps-core checks
 * (`pyth_receiver::ParseRejection`): perps-core refuses `notFullyVerified`
 * with OracleNotFullyVerified and the others with InvalidOracleAccount.
 */
export type ParseRejection = 'tooShort' | 'wrongDiscriminator' | 'notFullyVerified' | 'badVerificationLevel';

export type ParsedPriceUpdate = { ok: true; update: PriceUpdateV2 } | { ok: false; reason: ParseRejection };

/** `pyth_receiver::parse_price_update_v2`: length ≥ 133, then the discriminator, then a Full level. */
export function parsePriceUpdateV2(data: Uint8Array): ParsedPriceUpdate {
  if (data.length < PRICE_UPDATE_V2_FULL_LEN) return { ok: false, reason: 'tooShort' };
  for (let i = 0; i < 8; i++) {
    if (data[i] !== PRICE_UPDATE_V2_DISCRIMINATOR[i]) return { ok: false, reason: 'wrongDiscriminator' };
  }
  const level = data[PRICE_UPDATE_V2_OFFSETS.verificationLevel];
  if (level === VERIFICATION_PARTIAL) return { ok: false, reason: 'notFullyVerified' };
  if (level !== VERIFICATION_FULL) return { ok: false, reason: 'badVerificationLevel' };
  const o = PRICE_UPDATE_V2_OFFSETS;
  const v = new DataView(data.buffer, data.byteOffset, data.byteLength);
  return {
    ok: true,
    update: {
      writeAuthority: data.slice(o.writeAuthority, o.writeAuthority + 32),
      feedId: data.slice(o.feedId, o.feedId + 32),
      price: v.getBigInt64(o.price, true),
      conf: v.getBigUint64(o.conf, true),
      exponent: v.getInt32(o.exponent, true),
      publishTime: v.getBigInt64(o.publishTime, true),
      prevPublishTime: v.getBigInt64(o.prevPublishTime, true),
      emaPrice: v.getBigInt64(o.emaPrice, true),
      emaConf: v.getBigUint64(o.emaConf, true),
      postedSlot: v.getBigUint64(o.postedSlot, true),
    },
  };
}

/** A 134-byte Full update (the Receiver's and mock-pyth's account size), byte 133 zero. */
export function writePriceUpdateV2(u: PriceUpdateV2): Uint8Array {
  if (u.writeAuthority.length !== 32 || u.feedId.length !== 32) throw new Error('write authority and feed id are 32 bytes');
  const o = PRICE_UPDATE_V2_OFFSETS;
  const d = new Uint8Array(PRICE_UPDATE_V2_LEN);
  const v = new DataView(d.buffer);
  d.set(PRICE_UPDATE_V2_DISCRIMINATOR, 0);
  d.set(u.writeAuthority, o.writeAuthority);
  d[o.verificationLevel] = VERIFICATION_FULL;
  d.set(u.feedId, o.feedId);
  v.setBigInt64(o.price, u.price, true);
  v.setBigUint64(o.conf, u.conf, true);
  v.setInt32(o.exponent, u.exponent, true);
  v.setBigInt64(o.publishTime, u.publishTime, true);
  v.setBigInt64(o.prevPublishTime, u.prevPublishTime, true);
  v.setBigInt64(o.emaPrice, u.emaPrice, true);
  v.setBigUint64(o.emaConf, u.emaConf, true);
  v.setBigUint64(o.postedSlot, u.postedSlot, true);
  return d;
}

/**
 * `pyth_receiver::is_fresh`: at most `maxAgeSecs` old and at most
 * `MAX_FUTURE_PUBLISH_SECS` ahead of the clock (D52).
 */
export function isFresh(publishTime: number, now: number, maxAgeSecs = MAX_PRICE_AGE_SECS): boolean {
  return publishTime >= now ? publishTime - now <= MAX_FUTURE_PUBLISH_SECS : now - publishTime <= maxAgeSecs;
}

/** Whether two 32-byte values (feed ids, keys) are equal. */
export function sameFeedId(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === 32 && b.length === 32 && a.every((x, i) => x === b[i]);
}

export interface OraclePrice {
  /** USD per contract. */
  price: number;
  conf: number;
  publishTime: number;
  /** Lowercase hex. */
  feedId: string;
  /**
   * Fresh by publish time at `now` (at most 60 s old, at most 60 s ahead): what
   * `read_price` needs. PriceUpdateV2 has no status: outside market hours or
   * in an outage no newer update exists, so the price stops and goes stale.
   */
  trading: boolean;
}

/**
 * The price a Full update carries, scaled by its exponent. Null unless the
 * discriminator matches and the level is Full (what perps-core accepts);
 * `trading` = fresh at `now` (unix seconds, default the browser clock).
 */
export function decodeOraclePrice(data: Uint8Array, now: number = Math.floor(Date.now() / 1000)): OraclePrice | null {
  const parsed = parsePriceUpdateV2(data);
  if (!parsed.ok) return null;
  const u = parsed.update;
  const scale = 10 ** u.exponent;
  const publishTime = Number(u.publishTime);
  return {
    price: Number(u.price) * scale,
    conf: Number(u.conf) * scale,
    publishTime,
    feedId: Array.from(u.feedId, (b) => b.toString(16).padStart(2, '0')).join(''),
    trading: isFresh(publishTime, now),
  };
}

/**
 * The market's oracle price, only if perps-core would read this feed for it
 * (D47): a Full update owned by the market's `oracle_program`, for the
 * market's `pyth_feed_id`. Freshness is left to `trading`. Null otherwise,
 * or for a Market account from before D47.
 */
export function marketOraclePrice(
  marketData: Uint8Array,
  feedData: Uint8Array,
  feedOwner: Uint8Array,
  now: number = Math.floor(Date.now() / 1000)
): OraclePrice | null {
  if (marketData.length < MARKET_ORACLE_PROGRAM_OFFSET + 32) return null;
  const oracleProgram = marketData.subarray(MARKET_ORACLE_PROGRAM_OFFSET, MARKET_ORACLE_PROGRAM_OFFSET + 32);
  if (!sameFeedId(oracleProgram, feedOwner)) return null;
  const parsed = parsePriceUpdateV2(feedData);
  const feedId = decodeMarketFeedId(marketData);
  if (!parsed.ok || !feedId || !sameFeedId(parsed.update.feedId, feedId)) return null;
  return decodeOraclePrice(feedData, now);
}

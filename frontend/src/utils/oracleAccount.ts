// The oracle account a market points at, read over RPC (ORACLE_SOURCE
// 'chain'): a pyth-sdk-solana 0.8 PriceAccount, as perps-core's read_price
// sees it. On localnet it is the TEST ONLY mock-pyth feed (docs/localnet.md).

/** `Market.pyth_price_feed` (8-byte discriminator included; pinned by perps-core's sdk_layout_is_pinned). */
export const MARKET_PYTH_PRICE_FEED_OFFSET = 104;
/** `Market.long_open_interest` and `short_open_interest` (u64, 6-dp contracts), after the six u32 ratios and fees. */
export const MARKET_LONG_OI_OFFSET = 168;
export const MARKET_SHORT_OI_OFFSET = 176;

/** Long and short open interest in contracts, from a Market account's data; null if too short. */
export function decodeMarketOpenInterest(data: Uint8Array): { long: number; short: number } | null {
  if (data.length < MARKET_SHORT_OI_OFFSET + 8) return null;
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  return {
    long: Number(view.getBigUint64(MARKET_LONG_OI_OFFSET, true)) / 1e6,
    short: Number(view.getBigUint64(MARKET_SHORT_OI_OFFSET, true)) / 1e6,
  };
}

const MAGIC = 0xa1b2c3d4;
const STATUS_TRADING = 1;

export interface OraclePrice {
  /** USD per contract. */
  price: number;
  conf: number;
  publishTime: number;
  trading: boolean;
}

/** The aggregate while Trading, else the last Trading price (`prev_*`), as read_price does. Null if not a price account. */
export function decodeOraclePrice(data: Uint8Array): OraclePrice | null {
  if (data.length < 240) return null;
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  if (view.getUint32(0, true) !== MAGIC) return null;
  const expo = view.getInt32(20, true);
  const trading = view.getUint32(224, true) === STATUS_TRADING;
  const price = Number(view.getBigInt64(trading ? 208 : 184, true));
  const conf = Number(view.getBigUint64(trading ? 216 : 192, true));
  const publishTime = Number(view.getBigInt64(trading ? 96 : 200, true));
  const scale = 10 ** expo;
  return { price: price * scale, conf: conf * scale, publishTime, trading };
}

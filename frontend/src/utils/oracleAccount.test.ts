import { describe, expect, it } from 'vitest';
import { decodeMarketOpenInterest, decodeOraclePrice } from './oracleAccount';

describe('decodeMarketOpenInterest', () => {
  it('reads long and short open interest where the keeper and the program put them', () => {
    // Market layout: discriminator, 4 keys, commodity[8], 6 × u32, then long/short OI
    const d = new Uint8Array(298);
    const v = new DataView(d.buffer);
    v.setBigUint64(8 + 4 * 32 + 8 + 6 * 4, 1_523_400_000n, true);
    v.setBigUint64(8 + 4 * 32 + 8 + 6 * 4 + 8, 250_000_000n, true);
    expect(decodeMarketOpenInterest(d)).toEqual({ long: 1_523.4, short: 250 });
    expect(decodeMarketOpenInterest(new Uint8Array(180))).toBeNull();
  });
});

// The layout scripts/test-ts.sh and programs/mock-pyth write
function priceAccount(price: bigint, prev: bigint, status: number, ts: bigint): Uint8Array {
  const d = new Uint8Array(3312);
  const v = new DataView(d.buffer);
  v.setUint32(0, 0xa1b2c3d4, true);
  v.setUint32(4, 2, true);
  v.setUint32(8, 3, true);
  v.setInt32(20, -8, true);
  v.setBigInt64(96, ts, true);
  v.setBigInt64(184, prev, true);
  v.setBigUint64(192, 4_000_000n, true);
  v.setBigInt64(200, ts - 10n, true);
  v.setBigInt64(208, price, true);
  v.setBigUint64(216, 5_000_000n, true);
  v.setUint32(224, status, true);
  return d;
}

describe('decodeOraclePrice', () => {
  it('reads the aggregate while trading', () => {
    const p = decodeOraclePrice(priceAccount(7_512_345_678n, 7_000_000_000n, 1, 1_800_000_000n))!;
    expect(p.price).toBeCloseTo(75.12345678, 8);
    expect(p.conf).toBeCloseTo(0.05, 8);
    expect(p.publishTime).toBe(1_800_000_000);
    expect(p.trading).toBe(true);
  });

  it('falls back to the last trading price when halted', () => {
    const p = decodeOraclePrice(priceAccount(7_512_345_678n, 7_000_000_000n, 2, 1_800_000_000n))!;
    expect(p.price).toBeCloseTo(70, 8);
    expect(p.publishTime).toBe(1_799_999_990);
    expect(p.trading).toBe(false);
  });

  it('rejects anything that is not a price account', () => {
    expect(decodeOraclePrice(new Uint8Array(100))).toBeNull();
    expect(decodeOraclePrice(new Uint8Array(3312))).toBeNull();
  });
});

// @vitest-environment node
// node:fs and new URL(import.meta.url) need the node environment
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import {
  MAX_FUTURE_PUBLISH_SECS,
  MAX_PRICE_AGE_SECS,
  PRICE_UPDATE_V2_DISCRIMINATOR,
  PRICE_UPDATE_V2_LEN,
  decodeMarketFeedId,
  decodeMarketOpenInterest,
  decodeOraclePrice,
  isFresh,
  marketOraclePrice,
  parsePriceUpdateV2,
  writePriceUpdateV2,
  type PriceUpdateV2,
} from './oracleAccount';
import { feedIdFromHex } from '../config/pythFeeds';

// pyth-solana-receiver-sdk 2.0.0's own bytes for one update (Full, and Partial
// with 13 signatures), written by perps-core's parity test
// (tests/pyth_receiver_parity.rs, fixture_view()).
/** Commodities.USOILSPOT, the SDK fixtures' feed id (no longer the OIL feed, D53). */
const USOILSPOT = '925ca92ff005ae943c158e3563f59698ce7e75c5a8c8dd43303a0a154887b3e6';

const FIXTURES: Record<string, string> = {
  'price_update_v2_full.bin': fileURLToPath(new URL('../../../programs/perps-core/tests/fixtures/price_update_v2_full.bin', import.meta.url)),
  'price_update_v2_partial.bin': fileURLToPath(new URL('../../../programs/perps-core/tests/fixtures/price_update_v2_partial.bin', import.meta.url)),
};
const fixture = (name: string) => new Uint8Array(readFileSync(FIXTURES[name]));
const FIXTURE: PriceUpdateV2 = {
  writeAuthority: new Uint8Array(32).fill(0x11),
  // The SDK fixtures' feed id: USOILSPOT, the former OIL feed (D53)
  feedId: feedIdFromHex(USOILSPOT),
  price: 7_512_345_678n,
  conf: 4_200_000n,
  exponent: -8,
  publishTime: 1_794_416_400n,
  prevPublishTime: 1_794_416_399n,
  emaPrice: 7_500_000_000n,
  emaConf: 5_000_000n,
  postedSlot: 123_456_789n,
};

describe('decodeMarketOpenInterest', () => {
  it('reads long and short open interest where the keeper and the program put them', () => {
    // Market layout: discriminator, 4 keys, commodity[8], 6 × u32, then long/short OI
    const d = new Uint8Array(434);
    const v = new DataView(d.buffer);
    v.setBigUint64(8 + 4 * 32 + 8 + 6 * 4, 1_523_400_000n, true);
    v.setBigUint64(8 + 4 * 32 + 8 + 6 * 4 + 8, 250_000_000n, true);
    expect(decodeMarketOpenInterest(d)).toEqual({ long: 1_523.4, short: 250 });
    expect(decodeMarketOpenInterest(new Uint8Array(180))).toBeNull();
  });

  it('reads the pinned feed id at 295 (D47), null on a pre-D47 account', () => {
    const d = new Uint8Array(434);
    d.set(FIXTURE.feedId, 295);
    expect(Array.from(decodeMarketFeedId(d)!)).toEqual(Array.from(FIXTURE.feedId));
    expect(decodeMarketFeedId(new Uint8Array(298))).toBeNull();
  });
});

describe('parsePriceUpdateV2 against the SDK fixtures', () => {
  it('reads every field of the SDK-serialized Full update', () => {
    const full = fixture('price_update_v2_full.bin');
    expect(full.length).toBe(PRICE_UPDATE_V2_LEN);
    const p = parsePriceUpdateV2(full);
    expect(p).toEqual({ ok: true, update: FIXTURE });
  });

  it('writes the SDK bytes exactly', () => {
    expect(Array.from(writePriceUpdateV2(FIXTURE))).toEqual(Array.from(fixture('price_update_v2_full.bin')));
  });

  it('refuses the SDK-serialized Partial update', () => {
    const partial = fixture('price_update_v2_partial.bin');
    expect(partial[40]).toBe(0);
    expect(partial[41]).toBe(13);
    expect(parsePriceUpdateV2(partial)).toEqual({ ok: false, reason: 'notFullyVerified' });
    expect(decodeOraclePrice(partial)).toBeNull();
  });
});

// Real Solana mainnet push-feed accounts, saved byte for byte by the Rust
// tests' fixtures (tests/oracle/mainnet.rs, mainnet_price_updates.json; D53)
const mainnet = (name: string) =>
  new Uint8Array(readFileSync(fileURLToPath(new URL(`../../../programs/perps-core/tests/fixtures/${name}`, import.meta.url))));

describe('parsePriceUpdateV2 against real mainnet accounts', () => {
  it('reads the sponsored XAU/USD account of the pro-compatible push oracle', () => {
    const r = parsePriceUpdateV2(mainnet('mainnet_xau_usd_pro.bin'));
    if ('reason' in r) throw new Error(r.reason);
    expect(Buffer.from(r.update.feedId).toString('hex')).toBe('765d2ba906dbc32ca17cc11f5310a89e9ee1f6420508c63861f2f8ba4ee34bb2');
    expect([r.update.price, r.update.conf, r.update.exponent, r.update.publishTime]).toEqual([3_623_512n, 263n, -3, 1_795_746_734n]);
    expect(decodeOraclePrice(mainnet('mainnet_xau_usd_pro.bin'), 1_795_746_782)!.price).toBeCloseTo(3623.512, 9);
  });

  it('reads the frozen USOILSPOT account as stale', () => {
    const p = decodeOraclePrice(mainnet('mainnet_usoilspot_legacy.bin'), 1_795_746_782)!;
    expect(p.feedId).toBe(USOILSPOT);
    expect(p.publishTime).toBe(1_787_760_039); // 2026-08-26 16:00:39 UTC
    expect(p.trading).toBe(false);
  });
});

describe('parsePriceUpdateV2 rejections (pyth_receiver.rs order)', () => {
  const full = () => writePriceUpdateV2(FIXTURE);

  it('accepts 133 bytes (the serialized length), refuses 132', () => {
    expect(parsePriceUpdateV2(full().slice(0, 133)).ok).toBe(true);
    expect(parsePriceUpdateV2(full().slice(0, 132))).toEqual({ ok: false, reason: 'tooShort' });
    expect(parsePriceUpdateV2(new Uint8Array(0))).toEqual({ ok: false, reason: 'tooShort' });
  });

  it('checks the discriminator before the level', () => {
    const d = full();
    d[7] ^= 1;
    d[40] = 0;
    expect(parsePriceUpdateV2(d)).toEqual({ ok: false, reason: 'wrongDiscriminator' });
  });

  it('refuses an unknown verification level', () => {
    const d = full();
    d[40] = 2;
    expect(parsePriceUpdateV2(d)).toEqual({ ok: false, reason: 'badVerificationLevel' });
  });

  it('refuses a legacy Pyth v2 price account (magic 0xa1b2c3d4, 3_312 bytes)', () => {
    const legacy = new Uint8Array(3_312);
    new DataView(legacy.buffer).setUint32(0, 0xa1b2c3d4, true);
    expect(parsePriceUpdateV2(legacy)).toEqual({ ok: false, reason: 'wrongDiscriminator' });
    expect(decodeOraclePrice(legacy)).toBeNull();
  });

  it('has the Anchor discriminator sha256("account:PriceUpdateV2")[..8]', async () => {
    const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode('account:PriceUpdateV2')));
    expect(Array.from(digest.slice(0, 8))).toEqual(Array.from(PRICE_UPDATE_V2_DISCRIMINATOR));
  });
});

describe('decodeOraclePrice', () => {
  it('scales by the exponent and reports the feed id', () => {
    const p = decodeOraclePrice(writePriceUpdateV2(FIXTURE), 1_794_416_400)!;
    expect(p.price).toBeCloseTo(75.12345678, 8);
    expect(p.conf).toBeCloseTo(0.042, 8);
    expect(p.publishTime).toBe(1_794_416_400);
    expect(p.feedId).toBe(USOILSPOT);
    expect(p.trading).toBe(true);
  });

  it('is trading only while fresh by publish time, both ways (|publish − now| ≤ 60)', () => {
    const d = writePriceUpdateV2(FIXTURE);
    const t = 1_794_416_400;
    for (const [now, fresh] of [
      [t + 60, true], [t - 60, true], [t + 61, false], [t - 61, false],
    ] as const) {
      expect(decodeOraclePrice(d, now)!.trading, String(now - t)).toBe(fresh);
      expect(isFresh(t, now)).toBe(fresh);
    }
    expect(MAX_PRICE_AGE_SECS).toBe(60);
    // A shorter age limit (a venue's) does not shorten the future side (D52)
    expect(isFresh(t, t - 1, 0)).toBe(true);
    expect(isFresh(t, t - 60, 1)).toBe(true);
    expect(isFresh(t, t - 61, 1)).toBe(false);
    expect(isFresh(t, t + 2, 1)).toBe(false);
    expect(MAX_FUTURE_PUBLISH_SECS).toBe(60);
  });

  it('reads a positive exponent and a 12-dp one', () => {
    expect(decodeOraclePrice(writePriceUpdateV2({ ...FIXTURE, price: 75n, exponent: 0 }))!.price).toBe(75);
    expect(decodeOraclePrice(writePriceUpdateV2({ ...FIXTURE, price: 7n, exponent: 1 }))!.price).toBe(70);
    expect(decodeOraclePrice(writePriceUpdateV2({ ...FIXTURE, price: 75_000_000_000_000n, exponent: -12 }))!.price).toBeCloseTo(75, 9);
  });
});

describe('marketOraclePrice (what perps-core would read for the market)', () => {
  const owner = new Uint8Array(32).fill(0x50);
  const market = () => {
    const d = new Uint8Array(434);
    d.set(FIXTURE.feedId, 295);
    d.set(owner, 327);
    return d;
  };
  const feed = writePriceUpdateV2(FIXTURE);
  const t = Number(FIXTURE.publishTime);

  it('reads a Full update of the market feed id owned by the market oracle program', () => {
    expect(marketOraclePrice(market(), feed, owner, t)?.price).toBeCloseTo(75.12345678, 8);
  });

  it('is null for another owner, another feed id, a Partial update or a pre-D47 market', () => {
    expect(marketOraclePrice(market(), feed, new Uint8Array(32), t)).toBeNull();
    const other = market();
    other[295] ^= 1;
    expect(marketOraclePrice(other, feed, owner, t)).toBeNull();
    expect(marketOraclePrice(market(), fixture('price_update_v2_partial.bin'), owner, t)).toBeNull();
    expect(marketOraclePrice(market().slice(0, 298), feed, owner, t)).toBeNull();
  });
});

// @vitest-environment node
// web3.js address derivation fails under jsdom (mixed Buffer/Uint8Array realms).
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { PublicKey } from '@solana/web3.js';
import { describe, expect, it } from 'vitest';
import { PYTH_FEED_IDS, PYTH_PROGRAM_SETS, feedIdFromHex, feedIdToHex, pushFeedSeeds } from './pythFeeds';

const address = (hex: string, set: keyof typeof PYTH_PROGRAM_SETS) =>
  PublicKey.findProgramAddressSync(pushFeedSeeds(feedIdFromHex(hex)), new PublicKey(PYTH_PROGRAM_SETS[set].pushOracle))[0].toBase58();

describe('Pyth push-feed accounts (PDA [u16 LE shard, feed_id], push oracle)', () => {
  it("derives Pyth's documented SOL/USD shard-0 account", () => {
    // Pyth's push-feed docs list SOL/USD shard 0 on the legacy push oracle
    // (pythWSns…) at 7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE
    expect(address('ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d', 'legacy')).toBe(
      '7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE'
    );
  });

  it('derives the commodity accounts the SDK computes (migration spec A5)', () => {
    expect(address(PYTH_FEED_IDS.OIL!, 'pro')).toBe('65sQHMgQBPWnV9nPGL5PKsBmAs3B24cJcFzFh9GwPip7');
    expect(address(PYTH_FEED_IDS.OIL!, 'legacy')).toBe('4LPPjSGx5s3fvANM78zVhVFhEUQSJGHc1PUFKRjR76sX');
    expect(address(PYTH_FEED_IDS.GOLD!, 'pro')).toBe('csXV1aFF7uAfTHYWx6dDj718hnf7dUDU8v54dubib17');
    expect(address(PYTH_FEED_IDS.GOLD!, 'legacy')).toBe('2uPQGpm8X4ZkxMHxrAW1QuhXcse1AHEgPih6Xp9NuEWW');
    expect(address(PYTH_FEED_IDS.SILVER!, 'pro')).toBe('6Ls6yntvTkfHW992uxPoKjGmcaCih5xP9EYdrAnPUBSs');
    expect(address(PYTH_FEED_IDS.SILVER!, 'legacy')).toBe('H9JxsWwtDZxjSL6m7cdCVsWibj3JBMD9sxqLjadoZnot');
  });

  it('encodes the shard as two little-endian bytes', () => {
    const id = feedIdFromHex(PYTH_FEED_IDS.OIL!);
    expect(Array.from(pushFeedSeeds(id)[0])).toEqual([0, 0]);
    expect(Array.from(pushFeedSeeds(id, 0x0102)[0])).toEqual([2, 1]);
    expect(() => pushFeedSeeds(id, 65_536)).toThrow();
    expect(() => pushFeedSeeds(id.slice(1))).toThrow();
  });
});

describe('feed ids', () => {
  it('round-trips hex, with or without 0x', () => {
    const hex = PYTH_FEED_IDS.GOLD!;
    expect(feedIdToHex(feedIdFromHex(hex))).toBe(hex);
    expect(feedIdToHex(feedIdFromHex(`0x${hex.toUpperCase()}`))).toBe(hex);
  });

  it('refuses anything but 32 bytes of hex', () => {
    expect(() => feedIdFromHex('abcd')).toThrow();
    expect(() => feedIdFromHex(`${PYTH_FEED_IDS.OIL!}00`)).toThrow();
    expect(() => feedIdFromHex('zz'.repeat(32))).toThrow();
  });

  it('leaves the commodities without a verified Core feed id unset (NATGAS, COPPER)', () => {
    expect(PYTH_FEED_IDS.NATGAS).toBeNull();
    expect(PYTH_FEED_IDS.COPPER).toBeNull();
    for (const id of [PYTH_FEED_IDS.OIL, PYTH_FEED_IDS.GOLD, PYTH_FEED_IDS.SILVER]) expect(id).toMatch(/^[0-9a-f]{64}$/);
  });
});

describe('pythPriceFeed (config/program.ts)', () => {
  it('defaults to the pro-compatible shard-0 account, none without a feed id', async () => {
    const { pythPriceFeed, PYTH_PROGRAM_SET, PYTH_RECEIVER_PROGRAM_ID } = await import('./program');
    expect(PYTH_PROGRAM_SET).toBe('pro');
    expect(PYTH_RECEIVER_PROGRAM_ID.toBase58()).toBe(PYTH_PROGRAM_SETS.pro.receiver);
    expect(pythPriceFeed('OIL')?.toBase58()).toBe('65sQHMgQBPWnV9nPGL5PKsBmAs3B24cJcFzFh9GwPip7');
    expect(pythPriceFeed('NATGAS')).toBeUndefined();
    expect(pythPriceFeed('COPPER')).toBeUndefined();
  });
});

describe('backend/api copy of the feed accounts', () => {
  // backend/api/src/routes.rs hard-codes each commodity's pro shard-0
  // account (it has no Solana dependencies to derive it): it must be this
  // table's derivation, and empty where there is no feed id (D52)
  it('matches the pro shard-0 PDA of every feed id', () => {
    const src = readFileSync(fileURLToPath(new URL('../../../backend/api/src/routes.rs', import.meta.url)), 'utf8');
    const found = new Map<string, string>();
    for (const m of src.matchAll(/id: "([A-Z]+)"\.to_string\(\),[\s\S]*?pyth_feed: "([^"]*)"\.to_string\(\)/g)) {
      found.set(m[1], m[2]);
    }
    expect([...found.keys()].sort()).toEqual(Object.keys(PYTH_FEED_IDS).sort());
    for (const [commodity, id] of Object.entries(PYTH_FEED_IDS)) {
      expect(found.get(commodity), commodity).toBe(id ? address(id, 'pro') : '');
    }
  });
});

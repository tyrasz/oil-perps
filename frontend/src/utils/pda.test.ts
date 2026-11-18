// @vitest-environment node
// web3.js address derivation fails under jsdom (mixed Buffer/Uint8Array realms).
import { describe, it, expect } from 'vitest';
import { PublicKey } from '@solana/web3.js';
import { commodityForMarket, getCommodityMarket, getMarketPDA, getUserAccountPDA, isProgramAccount } from './pda';
import { COMMODITIES } from '../config/commodities';
import { PERPS_CORE_PROGRAM_ID, USDC_MINT } from '../config/program';

// Pinned in programs/perps-core (state/market.rs) and the keeper too, so all
// three derive the same addresses.
const OIL_MARKET = '6Chy7k9ArMFQamqogr5jsYVoRTbnP9TaCxvDqRV6ntD9';
const GOLD_MARKET = 'GcCG6HTriTyV36D5MqC4YL5dP28j5VZBNL9uTUUWMfso';
const OWNER = new PublicKey('11111111111111111111111111111112');
const OWNER_OIL_ACCOUNT = '4NBuxBaDE8SpGFiytjqTmkYqkyc628SAsGxrc51jkHso';

describe('market PDAs', () => {
  it('matches the addresses pinned in the program and keeper tests', () => {
    expect(getMarketPDA(USDC_MINT, 'OIL')[0].toBase58()).toBe(OIL_MARKET);
    expect(getCommodityMarket('GOLD').toBase58()).toBe(GOLD_MARKET);
    expect(getUserAccountPDA(OWNER, new PublicKey(OIL_MARKET))[0].toBase58()).toBe(OWNER_OIL_ACCOUNT);
  });

  it('gives every commodity its own market and user account', () => {
    const markets = COMMODITIES.map((c) => getCommodityMarket(c.id).toBase58());
    expect(new Set(markets).size).toBe(COMMODITIES.length);

    const accounts = markets.map((m) => getUserAccountPDA(OWNER, new PublicKey(m))[0].toBase58());
    expect(new Set(accounts).size).toBe(COMMODITIES.length);
  });

  it('maps market addresses back to commodities', () => {
    for (const c of COMMODITIES) {
      expect(commodityForMarket(getCommodityMarket(c.id))).toBe(c.id);
    }
    expect(commodityForMarket(OIL_MARKET)).toBe('OIL');
    expect(commodityForMarket(PublicKey.default)).toBeUndefined();
  });

  it('only treats initialized program accounts as existing', () => {
    const data = { length: 290 };
    expect(isProgramAccount({ owner: PERPS_CORE_PROGRAM_ID, data })).toBe(true);
    expect(isProgramAccount(null)).toBe(false);
    // Lamports sent to an unused address: system-owned and empty
    expect(isProgramAccount({ owner: PublicKey.default, data: { length: 0 } })).toBe(false);
    expect(isProgramAccount({ owner: PERPS_CORE_PROGRAM_ID, data: { length: 0 } })).toBe(false);
  });
});

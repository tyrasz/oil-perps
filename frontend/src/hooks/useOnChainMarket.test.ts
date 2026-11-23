import { describe, it, expect, beforeEach, vi } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import { useMarketStore, COMMODITIES } from '../stores/marketStore';

// A fake program whose market fetches resolve when the test says so
const pending: { address: string; resolve: (m: unknown) => void }[] = [];
const program = {
  account: {
    market: {
      fetch: (address: { toBase58: () => string }) =>
        new Promise((resolve) => pending.push({ address: address.toBase58(), resolve })),
    },
  },
};
vi.mock('./usePerpsProgram', () => ({ usePerpsProgram: () => ({ program }) }));
// web3.js address derivation doesn't work under jsdom; stand in readable keys
const getCommodityMarket = (id: string) => ({ toBase58: () => `market-${id}`, toString: () => `market-${id}` });
vi.mock('../utils/pda', () => ({ getCommodityMarket }));

const { useOnChainMarket } = await import('./useOnChainMarket');

function fakeMarket(maxLeverage: number, extra: Record<string, unknown> = {}) {
  const bn = (n: number) => ({ toNumber: () => n });
  return {
    fundingRate: bn(0), lastFundingTime: bn(0), fundingInterval: bn(3600), longOpenInterest: bn(0),
    shortOpenInterest: bn(0), maxOpenInterest: bn(0), totalPositions: bn(0), totalTrades: bn(0),
    insuranceFund: bn(0), isPaused: false, maxLeverage, initialMarginRatio: 1000, maintenanceMarginRatio: 500,
    takerFee: 5, makerFee: 2, liquidationFee: 100,
    ...extra,
  };
}

const initialState = useMarketStore.getState();
const [OIL, GOLD] = COMMODITIES;

beforeEach(() => {
  pending.length = 0;
  useMarketStore.setState(initialState, true);
});

describe('useOnChainMarket', () => {
  it("ignores a slow response for the previously selected commodity", async () => {
    const { result } = renderHook(() => useOnChainMarket());
    act(() => useMarketStore.getState().setSelectedCommodity(GOLD));

    const oilFetch = pending.find((p) => p.address === getCommodityMarket(OIL.id).toBase58())!;
    const goldFetch = pending.find((p) => p.address === getCommodityMarket(GOLD.id).toBase58())!;
    await act(async () => goldFetch.resolve(fakeMarket(15_000)));
    await act(async () => oilFetch.resolve(fakeMarket(20_000)));

    await waitFor(() => expect(result.current.marketData?.maxLeverage).toBe(15_000));
    expect(result.current.marketData?.address).toBe(getCommodityMarket(GOLD.id).toBase58());
  });

  it('counts open seconds and the carry to the next funding time', async () => {
    const { result } = renderHook(() => useOnChainMarket());
    const oilFetch = pending.find((p) => p.address === getCommodityMarket(OIL.id).toBase58())!;
    // Cranked a minute before the Friday 17:00 ET close with 600 s carried:
    // due 49 open minutes after the Sunday 18:00 ET reopen, not an hour later
    const close = 1_794_607_200;
    const bn = (n: number) => ({ toNumber: () => n });
    await act(async () =>
      oilFetch.resolve(fakeMarket(20_000, { lastFundingTime: bn(close - 60), sessionSchedule: 1, fundingCarrySecs: 600 }))
    );
    await waitFor(() => expect(result.current.marketData?.fundingCarrySecs).toBe(600));
    expect(result.current.marketData?.nextFundingTime).toBe(close + 49 * 3_600 + 2_940);
  });
});

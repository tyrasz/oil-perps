import { describe, it, expect, beforeEach, vi } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import { useMarketStore } from '../stores/marketStore';

// --- mocks -----------------------------------------------------------------
const bn = (n: number) => ({ toNumber: () => n });
const key = (s: string) => ({ toBase58: () => s, toString: () => s });

let wallet = key('alice');
vi.mock('@solana/wallet-adapter-react', () => ({ useWallet: () => ({ publicKey: wallet }) }));

// 100 OIL contracts long at $75 with $750 collateral (10%)
const oilLong = {
  publicKey: key('pos1'),
  account: {
    owner: key('alice'), market: key('market-OIL'), side: { long: {} }, size: bn(100_000_000),
    collateral: bn(750_000_000), entryPrice: bn(75_000_000), leverage: 10_000, realizedPnl: bn(0),
    lastFundingPayment: bn(0), openedAt: bn(1), lastUpdatedAt: bn(1), status: { open: {} },
    executionSource: { orderBook: {} }, bump: 0,
  },
};

const positionAll = vi.fn();
const program = {
  account: {
    position: { all: positionAll },
    market: { fetchMultiple: vi.fn(async () => [{ maintenanceMarginRatio: 500, cumulativeFunding: bn(0) }]) },
    userAccount: { fetchNullable: vi.fn(async () => null) },
  },
};
vi.mock('./usePerpsProgram', () => ({ usePerpsProgram: () => ({ program }) }));
vi.mock('../utils/pda', () => ({
  getCommodityMarket: (id: string) => key(`market-${id}`),
  getUserAccountPDA: () => [key('user-account')],
  commodityForMarket: (m: { toBase58: () => string }) => m.toBase58().replace('market-', ''),
}));

const { useOnChainPositions } = await import('./useOnChainPositions');

const initialState = useMarketStore.getState();
const setOilPrice = (price: number) =>
  useMarketStore.getState().setMarket('OIL', { commodity: 'OIL', price } as never);

beforeEach(() => {
  useMarketStore.setState(initialState, true);
  positionAll.mockReset();
  wallet = key('alice');
  vi.spyOn(console, 'error').mockImplementation(() => {});
});

describe('useOnChainPositions', () => {
  it('re-prices positions on price updates without reloading from chain', async () => {
    positionAll.mockResolvedValue([oilLong]);
    setOilPrice(75);
    const { result } = renderHook(() => useOnChainPositions());
    await waitFor(() => expect(result.current.positions).toHaveLength(1));
    expect(result.current.positions[0].marginRatio).toBeCloseTo(10);
    const loads = positionAll.mock.calls.length;

    act(() => setOilPrice(71));
    expect(result.current.positions[0].marginRatio).toBeCloseTo(4.667, 3);
    expect(result.current.positions[0].markPrice).toBe(71);
    expect(positionAll.mock.calls.length).toBe(loads);
  });

  it('keeps the last positions when a reload fails', async () => {
    positionAll.mockResolvedValueOnce([oilLong]).mockRejectedValue(new Error('rpc down'));
    const { result } = renderHook(() => useOnChainPositions());
    await waitFor(() => expect(result.current.positions).toHaveLength(1));

    await act(async () => result.current.refresh());
    expect(result.current.error).toBe('Failed to fetch positions');
    expect(result.current.positions).toHaveLength(1);
  });

  it("drops a slow response for the previous wallet", async () => {
    let resolveAlice: (v: unknown) => void = () => {};
    positionAll.mockImplementationOnce(() => new Promise((r) => { resolveAlice = r; }));
    const { result, rerender } = renderHook(() => useOnChainPositions());

    positionAll.mockResolvedValue([]); // bob has no positions
    wallet = key('bob');
    rerender();
    await waitFor(() => expect(positionAll).toHaveBeenCalledTimes(2));

    await act(async () => resolveAlice([oilLong]));
    expect(result.current.positions).toHaveLength(0);
  });
});

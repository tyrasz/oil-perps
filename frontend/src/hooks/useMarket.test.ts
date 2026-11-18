import { describe, it, expect, beforeEach, vi } from 'vitest';
import { act, renderHook } from '@testing-library/react';
import { useMarket, useMarketFeed } from './useMarket';
import { useMarketStore, COMMODITIES } from '../stores/marketStore';
import { MockWebSocket } from '../test/setup';
import type { MarketData } from '../types';

const initialState = useMarketStore.getState();
const [OIL, GOLD] = COMMODITIES;

function market(commodity: string, price: number): MarketData {
  return {
    address: `${commodity}_market`,
    commodity,
    price,
    priceChange24h: 0,
    volume24h: 0,
    openInterest: 0,
    longOpenInterest: 0,
    shortOpenInterest: 0,
    fundingRate: 0,
    maxLeverage: 20,
    isPaused: false,
  };
}

beforeEach(() => {
  useMarketStore.setState(initialState, true);
  vi.spyOn(console, 'error').mockImplementation(() => {});
});

describe('useMarket', () => {
  it('returns data for the selected commodity only', () => {
    useMarketStore.setState({
      markets: { OIL: market('OIL', 75), GOLD: market('GOLD', 2000) },
    });

    const { result } = renderHook(() => useMarket());
    expect(result.current.selectedCommodity.id).toBe('OIL');
    expect(result.current.market?.price).toBe(75);

    act(() => useMarketStore.getState().setSelectedCommodity(GOLD));
    expect(result.current.market?.price).toBe(2000);
  });

  it('returns null / empty defaults when nothing is loaded', () => {
    const { result } = renderHook(() => useMarket());
    expect(result.current.market).toBeNull();
    expect(result.current.orderBook).toBeNull();
    expect(result.current.recentTrades).toEqual([]);
  });

  it('does not open a WebSocket or poll by itself', () => {
    renderHook(() => useMarket());
    renderHook(() => useMarket());
    renderHook(() => useMarket());
    expect(MockWebSocket.instances).toHaveLength(0);
    expect(fetch).not.toHaveBeenCalled();
  });
});

describe('useMarketFeed', () => {
  it('opens exactly one WebSocket', () => {
    renderHook(() => useMarketFeed());
    expect(MockWebSocket.instances).toHaveLength(1);
  });

  it('subscribes to the selected commodity once connected', () => {
    renderHook(() => useMarketFeed());
    const ws = MockWebSocket.instances[0];
    act(() => ws.simulateOpen());

    const subs = ws.sent.map((m) => JSON.parse(m));
    expect(subs.map((s) => s.channel)).toEqual(['price', 'orderbook', 'trades']);
    expect(subs.every((s) => s.type === 'subscribe' && s.commodity === OIL.id)).toBe(true);
  });

  it('keeps the same connection when price updates arrive', () => {
    useMarketStore.setState({ markets: { OIL: market('OIL', 75) } });
    renderHook(() => useMarketFeed());
    const ws = MockWebSocket.instances[0];
    act(() => ws.simulateOpen());

    act(() => ws.simulateMessage({ type: 'price', commodity: 'OIL', price: 76_500_000 }));
    act(() => ws.simulateMessage({ type: 'price', commodity: 'OIL', price: 77_000_000 }));

    expect(useMarketStore.getState().markets.OIL.price).toBe(77);
    expect(MockWebSocket.instances).toHaveLength(1);
    expect(ws.readyState).toBe(MockWebSocket.OPEN);
  });

  it('applies price updates to the latest market state', () => {
    useMarketStore.setState({ markets: { OIL: market('OIL', 75) } });
    renderHook(() => useMarketFeed());
    const ws = MockWebSocket.instances[0];

    // Market data changes after the socket was created.
    act(() => useMarketStore.getState().setMarket('OIL', { ...market('OIL', 75), volume24h: 42 }));
    act(() => ws.simulateMessage({ type: 'price', commodity: 'OIL', price: 80_000_000 }));

    const oil = useMarketStore.getState().markets.OIL;
    expect(oil.price).toBe(80);
    expect(oil.volume24h).toBe(42);
  });

  it('records trades for the commodity in the message', () => {
    renderHook(() => useMarketFeed());
    const ws = MockWebSocket.instances[0];

    act(() =>
      ws.simulateMessage({
        type: 'trade',
        commodity: 'GOLD',
        side: 'long',
        price: 2_000_000_000,
        size: 1_500_000,
        timestamp: 1_700_000_000,
      })
    );

    const trades = useMarketStore.getState().recentTrades;
    expect(trades.GOLD).toEqual([
      { signature: '', commodity: 'GOLD', side: 'long', price: 2000, size: 1.5, timestamp: 1_700_000_000 },
    ]);
    expect(trades.OIL).toBeUndefined();
  });

  it('attributes untagged messages to the default market, not the selected one', () => {
    useMarketStore.setState({ markets: { OIL: market('OIL', 75), GOLD: market('GOLD', 2000) } });
    act(() => useMarketStore.getState().setSelectedCommodity(GOLD));
    renderHook(() => useMarketFeed());
    const ws = MockWebSocket.instances[0];

    act(() => ws.simulateMessage({ type: 'price', price: 76_000_000 }));

    expect(useMarketStore.getState().markets.OIL.price).toBe(76);
    expect(useMarketStore.getState().markets.GOLD.price).toBe(2000);
  });

  it('keeps polling prices for every commodity', async () => {
    vi.useFakeTimers();
    renderHook(() => useMarketFeed());
    const callsFor = (id: string) =>
      vi.mocked(fetch).mock.calls.filter(([url]) => String(url).endsWith(`/api/market/${id}`)).length;
    expect(callsFor('GOLD')).toBe(1);

    await act(async () => vi.advanceTimersByTime(5_000));
    expect(callsFor('GOLD')).toBe(2);
    expect(callsFor('COPPER')).toBe(2);
  });

  it('ignores malformed messages', () => {
    renderHook(() => useMarketFeed());
    const ws = MockWebSocket.instances[0];
    expect(() => act(() => ws.simulateMessage('not json'))).not.toThrow();
  });

  it('reconnects after the server drops the connection', () => {
    vi.useFakeTimers();
    renderHook(() => useMarketFeed());
    act(() => MockWebSocket.instances[0].simulateDrop());

    expect(MockWebSocket.instances).toHaveLength(1);
    act(() => vi.advanceTimersByTime(3000));
    expect(MockWebSocket.instances).toHaveLength(2);
    expect(MockWebSocket.open).toHaveLength(1);
  });

  it('does not reconnect after unmount', () => {
    vi.useFakeTimers();
    const { unmount } = renderHook(() => useMarketFeed());
    unmount();

    act(() => vi.advanceTimersByTime(10_000));
    expect(MockWebSocket.instances).toHaveLength(1);
    expect(MockWebSocket.open).toHaveLength(0);
  });

  it('subscribes to a newly selected commodity without reconnecting', () => {
    renderHook(() => useMarketFeed());
    const ws = MockWebSocket.instances[0];
    act(() => ws.simulateOpen());
    ws.sent = [];

    act(() => useMarketStore.getState().setSelectedCommodity(GOLD));

    expect(MockWebSocket.instances).toHaveLength(1);
    expect(ws.sent.map((m) => JSON.parse(m).commodity)).toEqual(['GOLD', 'GOLD', 'GOLD']);
  });

  it('keeps the last real price when a later fetch fails', async () => {
    useMarketStore.setState({ markets: { GOLD: market('GOLD', 1_950) } });
    renderHook(() => useMarketFeed()); // every fetch rejects in tests
    await vi.waitFor(() => expect(useMarketStore.getState().markets.OIL).toBeDefined());
    expect(useMarketStore.getState().markets.GOLD.price).toBe(1_950);
  });

  it("never stores the default market's legacy price under another commodity", async () => {
    vi.mocked(fetch).mockImplementation(async (url) => {
      const u = String(url);
      if (u.endsWith('/api/market/stats')) return new Response(JSON.stringify({ price: 75_000_000 }));
      if (u.endsWith('/api/market')) return new Response(JSON.stringify({}));
      return new Response('not found', { status: 404 });
    });
    useMarketStore.setState({ markets: { GOLD: market('GOLD', 1_950) } });
    renderHook(() => useMarketFeed());
    await vi.waitFor(() => expect(useMarketStore.getState().markets.OIL?.price).toBe(75));
    expect(useMarketStore.getState().markets.GOLD.price).toBe(1_950);
  });

  it('falls back to mock market data when the API is unreachable', async () => {
    renderHook(() => useMarketFeed());
    await vi.waitFor(() => {
      expect(Object.keys(useMarketStore.getState().markets).sort()).toEqual(
        COMMODITIES.map((c) => c.id).sort()
      );
    });
    expect(useMarketStore.getState().markets.OIL.price).toBe(OIL.basePrice);
  });
});

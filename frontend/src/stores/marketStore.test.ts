import { describe, it, expect, beforeEach } from 'vitest';
import { useMarketStore, COMMODITIES, DEFAULT_COMMODITY } from './marketStore';
import type { Order, Position, Trade } from '../types';

const initialState = useMarketStore.getState();
const store = () => useMarketStore.getState();

function trade(commodity: string, timestamp: number): Trade {
  return { signature: `sig${timestamp}`, commodity, side: 'long', price: 75, size: 1, timestamp };
}

beforeEach(() => {
  useMarketStore.setState(initialState, true);
});

describe('marketStore', () => {
  it('starts on the default commodity with empty data', () => {
    expect(store().selectedCommodity).toBe(DEFAULT_COMMODITY);
    expect(store().getCurrentMarket()).toBeNull();
    expect(store().getCurrentOrderBook()).toBeNull();
    expect(store().getCurrentTrades()).toEqual([]);
    expect(store().selectedLeverage).toBe(10);
    expect(store().orderType).toBe('market');
  });

  it('keeps market data separate per commodity', () => {
    const gold = COMMODITIES.find((c) => c.id === 'GOLD')!;
    store().setMarket('OIL', { commodity: 'OIL', price: 75 } as never);
    store().setMarket('GOLD', { commodity: 'GOLD', price: 2000 } as never);

    expect(store().getCurrentMarket()?.price).toBe(75);
    store().setSelectedCommodity(gold);
    expect(store().getCurrentMarket()?.price).toBe(2000);
  });

  it('keeps order books separate per commodity', () => {
    const book = { bids: [], asks: [], spread: 0.1, midPrice: 75 };
    store().setOrderBook('OIL', book);
    expect(store().getCurrentOrderBook()).toBe(book);
    expect(store().orderBooks.GOLD).toBeUndefined();
  });

  it('prepends trades newest-first and caps at 50 per commodity', () => {
    for (let t = 1; t <= 55; t++) store().addTrade('OIL', trade('OIL', t));
    store().addTrade('GOLD', trade('GOLD', 1));

    const oil = store().recentTrades.OIL;
    expect(oil).toHaveLength(50);
    expect(oil[0].timestamp).toBe(55);
    expect(oil[49].timestamp).toBe(6);
    expect(store().recentTrades.GOLD).toHaveLength(1);
  });

  it('filters positions and orders by commodity', () => {
    store().setPositions([{ commodity: 'OIL' }, { commodity: 'GOLD' }, { commodity: 'OIL' }] as Position[]);
    store().setOrders([{ commodity: 'GOLD' }] as Order[]);

    expect(store().getPositionsForCommodity('OIL')).toHaveLength(2);
    expect(store().getOrdersForCommodity('GOLD')).toHaveLength(1);
    expect(store().getOrdersForCommodity('OIL')).toHaveLength(0);
  });

  it('updates leverage and order type', () => {
    store().setSelectedLeverage(5);
    store().setOrderType('limit');
    expect(store().selectedLeverage).toBe(5);
    expect(store().orderType).toBe('limit');
  });
});

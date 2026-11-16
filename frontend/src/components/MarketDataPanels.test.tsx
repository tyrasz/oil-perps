import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen } from '@testing-library/react';
import { OrderBook } from './OrderBook';
import { RecentTrades } from './RecentTrades';
import { useMarketStore } from '../stores/marketStore';
import { MockWebSocket } from '../test/setup';

const initialState = useMarketStore.getState();

beforeEach(() => {
  useMarketStore.setState(initialState, true);
});

describe('OrderBook', () => {
  it('renders bids, asks and the spread for the selected commodity', () => {
    useMarketStore.setState({
      markets: { OIL: { price: 75 } as never },
      orderBooks: {
        OIL: {
          bids: [{ price: 74.9, size: 2, total: 2 }],
          asks: [{ price: 75.1, size: 4, total: 4 }],
          spread: 0.2,
          midPrice: 75,
        },
      },
    });

    const { container } = render(<OrderBook />);

    expect(screen.getByText('$74.90')).toBeInTheDocument();
    expect(screen.getByText('$75.10')).toBeInTheDocument();
    expect(screen.getByText(/\$0\.20/)).toHaveTextContent('(0.267%)');

    const bars = container.querySelectorAll<HTMLElement>('.orderbook-bar');
    expect([...bars].map((b) => b.style.width)).toEqual(['100%', '50%']);
  });

  it('renders an empty book without errors', () => {
    render(<OrderBook />);
    expect(screen.getByText(/\$0\.00/)).toHaveTextContent('(0.00%)');
  });

  it('does not open its own WebSocket', () => {
    render(<OrderBook />);
    expect(MockWebSocket.instances).toHaveLength(0);
  });
});

describe('RecentTrades', () => {
  it('shows a placeholder when there are no trades', () => {
    render(<RecentTrades />);
    expect(screen.getByText('No recent trades')).toBeInTheDocument();
  });

  it('shows at most 20 trades for the selected commodity', () => {
    const trades = Array.from({ length: 25 }, (_, i) => ({
      signature: `s${i}`,
      commodity: 'OIL',
      side: (i % 2 ? 'short' : 'long') as 'long' | 'short',
      price: 75 + i / 100,
      size: 1,
      timestamp: 1_700_000_000 + i,
    }));
    useMarketStore.setState({
      recentTrades: { OIL: trades, GOLD: [{ ...trades[0], commodity: 'GOLD', price: 2000 }] },
    });

    const { container } = render(<RecentTrades />);

    expect(container.querySelectorAll('.trade-row')).toHaveLength(20);
    expect(container.querySelectorAll('.trade-row.short')).toHaveLength(10);
    expect(screen.getByText('$75.00')).toBeInTheDocument();
    expect(screen.queryByText('$2000.00')).not.toBeInTheDocument();
  });
});

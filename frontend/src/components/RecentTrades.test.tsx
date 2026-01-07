import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen } from '@testing-library/react';
import { RecentTrades } from './RecentTrades';
import { useMarketStore } from '../stores/marketStore';

// Mock the useMarket hook
vi.mock('../hooks/useMarket', () => ({
  useMarket: () => ({
    market: null,
    orderBook: null,
    recentTrades: useMarketStore.getState().recentTrades,
    refreshMarket: vi.fn(),
    refreshOrderBook: vi.fn(),
  }),
}));

describe('RecentTrades', () => {
  beforeEach(() => {
    useMarketStore.setState({
      market: null,
      orderBook: null,
      recentTrades: [],
      userAccount: null,
      positions: [],
      orders: [],
      selectedLeverage: 10,
      orderType: 'market',
    });
  });

  it('should render header', () => {
    render(<RecentTrades />);

    expect(screen.getByText('Price')).toBeInTheDocument();
    expect(screen.getByText('Size')).toBeInTheDocument();
    expect(screen.getByText('Time')).toBeInTheDocument();
  });

  it('should show empty state when no trades', () => {
    render(<RecentTrades />);

    expect(screen.getByText('No recent trades')).toBeInTheDocument();
  });

  it('should display trades when available', () => {
    useMarketStore.setState({
      recentTrades: [
        {
          signature: 'sig1',
          side: 'long',
          price: 75.50,
          size: 10.5,
          timestamp: 1704067200,
        },
      ],
    });

    render(<RecentTrades />);

    expect(screen.getByText('$75.50')).toBeInTheDocument();
    expect(screen.getByText('10.5000')).toBeInTheDocument();
  });

  it('should display multiple trades', () => {
    useMarketStore.setState({
      recentTrades: [
        {
          signature: 'sig1',
          side: 'long',
          price: 75.50,
          size: 10,
          timestamp: 1704067200,
        },
        {
          signature: 'sig2',
          side: 'short',
          price: 75.45,
          size: 5,
          timestamp: 1704067201,
        },
        {
          signature: 'sig3',
          side: 'long',
          price: 75.55,
          size: 15,
          timestamp: 1704067202,
        },
      ],
    });

    render(<RecentTrades />);

    expect(screen.getByText('$75.50')).toBeInTheDocument();
    expect(screen.getByText('$75.45')).toBeInTheDocument();
    expect(screen.getByText('$75.55')).toBeInTheDocument();
  });

  it('should format trade sizes with 4 decimals', () => {
    useMarketStore.setState({
      recentTrades: [
        {
          signature: 'sig1',
          side: 'long',
          price: 75.00,
          size: 12.3456,
          timestamp: 1704067200,
        },
      ],
    });

    render(<RecentTrades />);

    expect(screen.getByText('12.3456')).toBeInTheDocument();
  });

  it('should format trade prices with 2 decimals', () => {
    useMarketStore.setState({
      recentTrades: [
        {
          signature: 'sig1',
          side: 'long',
          price: 75.123,
          size: 10,
          timestamp: 1704067200,
        },
      ],
    });

    render(<RecentTrades />);

    expect(screen.getByText('$75.12')).toBeInTheDocument();
  });

  it('should limit displayed trades to 20', () => {
    const manyTrades = Array.from({ length: 25 }, (_, i) => ({
      signature: `sig${i}`,
      side: 'long' as const,
      price: 75 + i * 0.01,
      size: 10,
      timestamp: 1704067200 + i,
    }));

    useMarketStore.setState({
      recentTrades: manyTrades,
    });

    render(<RecentTrades />);

    // Count price elements - should be max 20
    const priceElements = screen.getAllByText(/\$75\.\d+/);
    expect(priceElements.length).toBeLessThanOrEqual(20);
  });

  it('should display timestamp as localized time', () => {
    // Use a fixed timestamp
    const timestamp = 1704067200; // 2024-01-01 00:00:00 UTC

    useMarketStore.setState({
      recentTrades: [
        {
          signature: 'sig1',
          side: 'long',
          price: 75.00,
          size: 10,
          timestamp: timestamp,
        },
      ],
    });

    render(<RecentTrades />);

    // The exact format depends on locale, just check it's rendered
    const timeElement = screen.getByText(/\d{1,2}:\d{2}/);
    expect(timeElement).toBeInTheDocument();
  });

  it('should apply long/short class to trade rows', () => {
    useMarketStore.setState({
      recentTrades: [
        {
          signature: 'sig1',
          side: 'long',
          price: 75.00,
          size: 10,
          timestamp: 1704067200,
        },
        {
          signature: 'sig2',
          side: 'short',
          price: 74.95,
          size: 5,
          timestamp: 1704067201,
        },
      ],
    });

    const { container } = render(<RecentTrades />);

    const longRow = container.querySelector('.trade-row.long');
    const shortRow = container.querySelector('.trade-row.short');

    expect(longRow).toBeInTheDocument();
    expect(shortRow).toBeInTheDocument();
  });
});

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen } from '@testing-library/react';
import { OrderBook } from './OrderBook';
import { useMarketStore } from '../stores/marketStore';

// Mock the useMarket hook
vi.mock('../hooks/useMarket', () => ({
  useMarket: () => ({
    market: useMarketStore.getState().market,
    orderBook: useMarketStore.getState().orderBook,
    recentTrades: [],
    refreshMarket: vi.fn(),
    refreshOrderBook: vi.fn(),
  }),
}));

describe('OrderBook', () => {
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

  it('should render orderbook header', () => {
    render(<OrderBook />);

    expect(screen.getByText('Price (USD)')).toBeInTheDocument();
    expect(screen.getByText('Size')).toBeInTheDocument();
    expect(screen.getByText('Total')).toBeInTheDocument();
  });

  it('should display spread section', () => {
    useMarketStore.setState({
      orderBook: {
        bids: [],
        asks: [],
        spread: 0,
        midPrice: 0,
      },
    });

    render(<OrderBook />);

    expect(screen.getByText('Spread')).toBeInTheDocument();
  });

  it('should display spread value when orderBook exists', () => {
    useMarketStore.setState({
      market: {
        address: 'market123',
        price: 75.00,
        priceChange24h: 0,
        volume24h: 0,
        openInterest: 0,
        longOpenInterest: 0,
        shortOpenInterest: 0,
        fundingRate: 0,
        maxLeverage: 20,
        isPaused: false,
      },
      orderBook: {
        bids: [{ price: 74.95, size: 100, total: 100 }],
        asks: [{ price: 75.05, size: 80, total: 80 }],
        spread: 0.10,
        midPrice: 75.00,
      },
    });

    render(<OrderBook />);

    expect(screen.getByText(/\$0\.10/)).toBeInTheDocument();
  });

  it('should display bid prices', () => {
    useMarketStore.setState({
      orderBook: {
        bids: [
          { price: 74.95, size: 100, total: 100 },
          { price: 74.90, size: 150, total: 250 },
        ],
        asks: [],
        spread: 0.10,
        midPrice: 75.00,
      },
    });

    render(<OrderBook />);

    expect(screen.getByText('$74.95')).toBeInTheDocument();
    expect(screen.getByText('$74.90')).toBeInTheDocument();
  });

  it('should display ask prices', () => {
    useMarketStore.setState({
      orderBook: {
        bids: [],
        asks: [
          { price: 75.05, size: 80, total: 80 },
          { price: 75.10, size: 120, total: 200 },
        ],
        spread: 0.10,
        midPrice: 75.00,
      },
    });

    render(<OrderBook />);

    expect(screen.getByText('$75.05')).toBeInTheDocument();
    expect(screen.getByText('$75.10')).toBeInTheDocument();
  });

  it('should display sizes and totals', () => {
    useMarketStore.setState({
      orderBook: {
        bids: [{ price: 74.95, size: 100.5, total: 100.5 }],
        asks: [{ price: 75.05, size: 80.25, total: 80.25 }],
        spread: 0.10,
        midPrice: 75.00,
      },
    });

    render(<OrderBook />);

    // Size and total can have same value, use getAllByText
    expect(screen.getAllByText('100.5000').length).toBeGreaterThanOrEqual(1);
    expect(screen.getAllByText('80.2500').length).toBeGreaterThanOrEqual(1);
  });

  it('should handle empty orderBook', () => {
    useMarketStore.setState({
      orderBook: {
        bids: [],
        asks: [],
        spread: 0,
        midPrice: 0,
      },
    });

    render(<OrderBook />);

    // Should still render without crashing
    expect(screen.getByText('Spread')).toBeInTheDocument();
    // Text is split across elements, use regex
    expect(screen.getByText(/0\.00/)).toBeInTheDocument();
  });

  it('should limit displayed bids to 8', () => {
    const manyBids = Array.from({ length: 12 }, (_, i) => ({
      price: 74.95 - i * 0.05,
      size: 100,
      total: (i + 1) * 100,
    }));

    useMarketStore.setState({
      orderBook: {
        bids: manyBids,
        asks: [],
        spread: 0.10,
        midPrice: 75.00,
      },
    });

    render(<OrderBook />);

    // Only first 8 bids should be displayed
    const bidPrices = screen.getAllByText(/\$74\.\d+/);
    expect(bidPrices.length).toBeLessThanOrEqual(8);
  });
});

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen } from '@testing-library/react';
import { MarketStats } from './MarketStats';
import { useMarketStore } from '../stores/marketStore';

// Mock the useMarket hook
vi.mock('../hooks/useMarket', () => ({
  useMarket: () => ({
    market: useMarketStore.getState().market,
    orderBook: null,
    recentTrades: [],
    refreshMarket: vi.fn(),
    refreshOrderBook: vi.fn(),
  }),
}));

describe('MarketStats', () => {
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

  it('should show loading state when market is null', () => {
    render(<MarketStats />);

    expect(screen.getByText('Loading...')).toBeInTheDocument();
  });

  it('should display market data when loaded', () => {
    useMarketStore.setState({
      market: {
        address: 'market123',
        price: 75.50,
        priceChange24h: 2.5,
        volume24h: 1000000,
        openInterest: 500000,
        longOpenInterest: 300000,
        shortOpenInterest: 200000,
        fundingRate: 0.0001,
        maxLeverage: 20,
        isPaused: false,
      },
    });

    render(<MarketStats />);

    expect(screen.getByText('OIL-PERP')).toBeInTheDocument();
    expect(screen.getByText('$75.50')).toBeInTheDocument();
    expect(screen.getByText('+2.50%')).toBeInTheDocument();
  });

  it('should display 24h volume', () => {
    useMarketStore.setState({
      market: {
        address: 'market123',
        price: 75.50,
        priceChange24h: 2.5,
        volume24h: 2500000,
        openInterest: 500000,
        longOpenInterest: 300000,
        shortOpenInterest: 200000,
        fundingRate: 0.0001,
        maxLeverage: 20,
        isPaused: false,
      },
    });

    render(<MarketStats />);

    expect(screen.getByText('24h Volume')).toBeInTheDocument();
    expect(screen.getByText('$2.50M')).toBeInTheDocument();
  });

  it('should display open interest', () => {
    useMarketStore.setState({
      market: {
        address: 'market123',
        price: 75.50,
        priceChange24h: 2.5,
        volume24h: 1000000,
        openInterest: 1500000,
        longOpenInterest: 300000,
        shortOpenInterest: 200000,
        fundingRate: 0.0001,
        maxLeverage: 20,
        isPaused: false,
      },
    });

    render(<MarketStats />);

    expect(screen.getByText('Open Interest')).toBeInTheDocument();
    expect(screen.getByText('$1.50M')).toBeInTheDocument();
  });

  it('should display funding rate', () => {
    useMarketStore.setState({
      market: {
        address: 'market123',
        price: 75.50,
        priceChange24h: 2.5,
        volume24h: 1000000,
        openInterest: 500000,
        longOpenInterest: 300000,
        shortOpenInterest: 200000,
        fundingRate: 0.0005,
        maxLeverage: 20,
        isPaused: false,
      },
    });

    render(<MarketStats />);

    expect(screen.getByText('Funding Rate')).toBeInTheDocument();
    expect(screen.getByText('+0.0500%')).toBeInTheDocument();
  });

  it('should display negative price change correctly', () => {
    useMarketStore.setState({
      market: {
        address: 'market123',
        price: 72.00,
        priceChange24h: -3.5,
        volume24h: 1000000,
        openInterest: 500000,
        longOpenInterest: 300000,
        shortOpenInterest: 200000,
        fundingRate: -0.0002,
        maxLeverage: 20,
        isPaused: false,
      },
    });

    render(<MarketStats />);

    expect(screen.getByText('-3.50%')).toBeInTheDocument();
    expect(screen.getByText('-0.0200%')).toBeInTheDocument();
  });

  it('should display long/short ratio', () => {
    useMarketStore.setState({
      market: {
        address: 'market123',
        price: 75.50,
        priceChange24h: 2.5,
        volume24h: 1000000,
        openInterest: 500000,
        longOpenInterest: 600000,
        shortOpenInterest: 400000,
        fundingRate: 0.0001,
        maxLeverage: 20,
        isPaused: false,
      },
    });

    render(<MarketStats />);

    expect(screen.getByText('Long/Short')).toBeInTheDocument();
    // Text is split across elements, use regex
    expect(screen.getByText(/60\.0/)).toBeInTheDocument();
    expect(screen.getByText(/40\.0/)).toBeInTheDocument();
  });
});

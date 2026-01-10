import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { PositionTable } from './PositionTable';
import { useMarketStore } from '../stores/marketStore';

// Mock the usePositions hook
vi.mock('../hooks/usePositions', () => ({
  usePositions: () => ({
    positions: useMarketStore.getState().positions,
    orders: [],
    userAccount: null,
    refreshPositions: vi.fn(),
    refreshOrders: vi.fn(),
    refreshAccount: vi.fn(),
  }),
}));

describe('PositionTable', () => {
  beforeEach(() => {
    useMarketStore.setState({
      market: {
        address: 'market123',
        price: 76.00,
        priceChange24h: 0,
        volume24h: 0,
        openInterest: 0,
        longOpenInterest: 0,
        shortOpenInterest: 0,
        fundingRate: 0,
        maxLeverage: 20,
        isPaused: false,
      },
      orderBook: null,
      recentTrades: [],
      userAccount: null,
      positions: [],
      orders: [],
      selectedLeverage: 10,
      orderType: 'market',
    });
  });

  it('should show empty state when no positions', () => {
    render(<PositionTable />);

    expect(screen.getByText('No open positions')).toBeInTheDocument();
  });

  it('should render table headers when positions exist', () => {
    useMarketStore.setState({
      positions: [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'long',
          size: 100,
          collateral: 1000,
          entryPrice: 75.00,
          leverage: 10,
          unrealizedPnl: 100,
          marginRatio: 15,
          openedAt: Date.now(),
        },
      ],
    });

    render(<PositionTable />);

    expect(screen.getByText('Side')).toBeInTheDocument();
    expect(screen.getByText('Size')).toBeInTheDocument();
    expect(screen.getByText('Entry Price')).toBeInTheDocument();
    expect(screen.getByText('Mark Price')).toBeInTheDocument();
    expect(screen.getByText('Leverage')).toBeInTheDocument();
    expect(screen.getByText('PnL')).toBeInTheDocument();
    expect(screen.getByText('Margin Ratio')).toBeInTheDocument();
    expect(screen.getByText('Actions')).toBeInTheDocument();
  });

  it('should display long position correctly', () => {
    useMarketStore.setState({
      positions: [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'long',
          size: 100,
          collateral: 1000,
          entryPrice: 75.00,
          leverage: 10,
          unrealizedPnl: 100,
          marginRatio: 15,
          openedAt: Date.now(),
        },
      ],
    });

    render(<PositionTable />);

    expect(screen.getByText('LONG')).toBeInTheDocument();
    expect(screen.getByText('100.0000')).toBeInTheDocument();
    expect(screen.getByText('$75.00')).toBeInTheDocument();
    expect(screen.getByText('10x')).toBeInTheDocument();
    expect(screen.getByText('$100.00')).toBeInTheDocument();
    expect(screen.getByText('15.00%')).toBeInTheDocument();
  });

  it('should display short position correctly', () => {
    useMarketStore.setState({
      positions: [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'short',
          size: 50,
          collateral: 500,
          entryPrice: 77.00,
          leverage: 5,
          unrealizedPnl: 50,
          marginRatio: 12,
          openedAt: Date.now(),
        },
      ],
    });

    render(<PositionTable />);

    expect(screen.getByText('SHORT')).toBeInTheDocument();
    expect(screen.getByText('50.0000')).toBeInTheDocument();
    expect(screen.getByText('$77.00')).toBeInTheDocument();
    expect(screen.getByText('5x')).toBeInTheDocument();
  });

  it('should display negative PnL correctly', () => {
    useMarketStore.setState({
      positions: [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'long',
          size: 100,
          collateral: 1000,
          entryPrice: 78.00,
          leverage: 10,
          unrealizedPnl: -200,
          marginRatio: 10,
          openedAt: Date.now(),
        },
      ],
    });

    render(<PositionTable />);

    expect(screen.getByText('$-200.00')).toBeInTheDocument();
  });

  it('should display PnL percentage', () => {
    useMarketStore.setState({
      positions: [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'long',
          size: 100,
          collateral: 1000,
          entryPrice: 75.00,
          leverage: 10,
          unrealizedPnl: 100,
          marginRatio: 15,
          openedAt: Date.now(),
        },
      ],
    });

    render(<PositionTable />);

    expect(screen.getByText('(10.00%)')).toBeInTheDocument();
  });

  it('should display close button', () => {
    useMarketStore.setState({
      positions: [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'long',
          size: 100,
          collateral: 1000,
          entryPrice: 75.00,
          leverage: 10,
          unrealizedPnl: 100,
          marginRatio: 15,
          openedAt: Date.now(),
        },
      ],
    });

    render(<PositionTable />);

    expect(screen.getByText('Close')).toBeInTheDocument();
  });

  it('should handle close button click', () => {
    const consoleSpy = vi.spyOn(console, 'log').mockImplementation(() => {});

    useMarketStore.setState({
      positions: [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'long',
          size: 100,
          collateral: 1000,
          entryPrice: 75.00,
          leverage: 10,
          unrealizedPnl: 100,
          marginRatio: 15,
          openedAt: Date.now(),
        },
      ],
    });

    render(<PositionTable />);

    fireEvent.click(screen.getByText('Close'));

    expect(consoleSpy).toHaveBeenCalledWith('Closing position:', 'pos1');
    consoleSpy.mockRestore();
  });

  it('should display multiple positions', () => {
    useMarketStore.setState({
      positions: [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'long',
          size: 100,
          collateral: 1000,
          entryPrice: 75.00,
          leverage: 10,
          unrealizedPnl: 100,
          marginRatio: 15,
          openedAt: Date.now(),
        },
        {
          address: 'pos2',
          owner: 'user123',
          side: 'short',
          size: 50,
          collateral: 500,
          entryPrice: 76.00,
          leverage: 5,
          unrealizedPnl: -20,
          marginRatio: 12,
          openedAt: Date.now(),
        },
      ],
    });

    render(<PositionTable />);

    expect(screen.getByText('LONG')).toBeInTheDocument();
    expect(screen.getByText('SHORT')).toBeInTheDocument();
  });

  it('should display mark price from market', () => {
    useMarketStore.setState({
      market: {
        address: 'market123',
        price: 76.50,
        priceChange24h: 0,
        volume24h: 0,
        openInterest: 0,
        longOpenInterest: 0,
        shortOpenInterest: 0,
        fundingRate: 0,
        maxLeverage: 20,
        isPaused: false,
      },
      positions: [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'long',
          size: 100,
          collateral: 1000,
          entryPrice: 75.00,
          leverage: 10,
          unrealizedPnl: 100,
          marginRatio: 15,
          openedAt: Date.now(),
        },
      ],
    });

    render(<PositionTable />);

    expect(screen.getByText('$76.50')).toBeInTheDocument();
  });

  it('should show danger class for margin ratio below 10%', () => {
    useMarketStore.setState({
      positions: [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'long',
          size: 100,
          collateral: 1000,
          entryPrice: 75.00,
          leverage: 10,
          unrealizedPnl: -800,
          marginRatio: 5,
          openedAt: Date.now(),
        },
      ],
    });

    const { container } = render(<PositionTable />);

    const marginCell = container.querySelector('td.danger');
    expect(marginCell).toBeInTheDocument();
    expect(marginCell?.textContent).toBe('5.00%');
  });

  it('should show warning class for margin ratio between 10% and 20%', () => {
    useMarketStore.setState({
      positions: [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'long',
          size: 100,
          collateral: 1000,
          entryPrice: 75.00,
          leverage: 10,
          unrealizedPnl: -500,
          marginRatio: 15,
          openedAt: Date.now(),
        },
      ],
    });

    const { container } = render(<PositionTable />);

    const marginCell = container.querySelector('td.warning');
    expect(marginCell).toBeInTheDocument();
    expect(marginCell?.textContent).toBe('15.00%');
  });

  it('should show no warning class for margin ratio above 20%', () => {
    useMarketStore.setState({
      positions: [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'long',
          size: 100,
          collateral: 1000,
          entryPrice: 75.00,
          leverage: 10,
          unrealizedPnl: 100,
          marginRatio: 25,
          openedAt: Date.now(),
        },
      ],
    });

    const { container } = render(<PositionTable />);

    expect(container.querySelector('td.danger')).not.toBeInTheDocument();
    expect(container.querySelector('td.warning')).not.toBeInTheDocument();
    expect(screen.getByText('25.00%')).toBeInTheDocument();
  });

  it('should show positive class for positive PnL', () => {
    useMarketStore.setState({
      positions: [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'long',
          size: 100,
          collateral: 1000,
          entryPrice: 75.00,
          leverage: 10,
          unrealizedPnl: 100,
          marginRatio: 15,
          openedAt: Date.now(),
        },
      ],
    });

    const { container } = render(<PositionTable />);

    const pnlCell = container.querySelector('td.positive');
    expect(pnlCell).toBeInTheDocument();
  });

  it('should show negative class for negative PnL', () => {
    useMarketStore.setState({
      positions: [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'long',
          size: 100,
          collateral: 1000,
          entryPrice: 75.00,
          leverage: 10,
          unrealizedPnl: -100,
          marginRatio: 15,
          openedAt: Date.now(),
        },
      ],
    });

    const { container } = render(<PositionTable />);

    const pnlCell = container.querySelector('td.negative');
    expect(pnlCell).toBeInTheDocument();
  });

  it('should display dash for mark price when market is null', () => {
    useMarketStore.setState({
      market: null,
      positions: [
        {
          address: 'pos1',
          owner: 'user123',
          side: 'long',
          size: 100,
          collateral: 1000,
          entryPrice: 75.00,
          leverage: 10,
          unrealizedPnl: 100,
          marginRatio: 15,
          openedAt: Date.now(),
        },
      ],
    });

    render(<PositionTable />);

    // The component renders "$-" when market is null
    expect(screen.getByText(/\$-/)).toBeInTheDocument();
  });
});

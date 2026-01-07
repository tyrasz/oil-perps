import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { TradingChart } from './TradingChart';

// Mock the lightweight-charts library
const mockAddCandlestickSeries = vi.fn().mockReturnValue({
  setData: vi.fn(),
});

const mockApplyOptions = vi.fn();
const mockRemove = vi.fn();

const mockChart = {
  addCandlestickSeries: mockAddCandlestickSeries,
  applyOptions: mockApplyOptions,
  remove: mockRemove,
};

vi.mock('lightweight-charts', () => ({
  createChart: vi.fn(() => mockChart),
}));

describe('TradingChart', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  afterEach(() => {
    cleanup();
  });

  it('should render chart container', () => {
    render(<TradingChart />);

    expect(screen.getByText('OIL-PERP')).toBeInTheDocument();
    expect(screen.getByText('1H')).toBeInTheDocument();
  });

  it('should render with default symbol', () => {
    render(<TradingChart />);

    expect(screen.getByText('OIL-PERP')).toBeInTheDocument();
  });

  it('should render with custom symbol', () => {
    render(<TradingChart symbol="BTC-PERP" />);

    expect(screen.getByText('BTC-PERP')).toBeInTheDocument();
  });

  it('should create chart on mount', async () => {
    const { createChart } = await import('lightweight-charts');

    render(<TradingChart />);

    expect(createChart).toHaveBeenCalled();
  });

  it('should add candlestick series', async () => {
    render(<TradingChart />);

    expect(mockAddCandlestickSeries).toHaveBeenCalledWith({
      upColor: '#22c55e',
      downColor: '#ef4444',
      borderUpColor: '#22c55e',
      borderDownColor: '#ef4444',
      wickUpColor: '#22c55e',
      wickDownColor: '#ef4444',
    });
  });

  it('should set data on candlestick series', () => {
    render(<TradingChart />);

    const candleSeries = mockAddCandlestickSeries.mock.results[0].value;
    expect(candleSeries.setData).toHaveBeenCalled();

    // Verify data structure
    const setDataCall = candleSeries.setData.mock.calls[0][0];
    expect(Array.isArray(setDataCall)).toBe(true);
    expect(setDataCall.length).toBe(101); // 101 data points generated

    // Verify data point structure
    const firstPoint = setDataCall[0];
    expect(firstPoint).toHaveProperty('time');
    expect(firstPoint).toHaveProperty('open');
    expect(firstPoint).toHaveProperty('high');
    expect(firstPoint).toHaveProperty('low');
    expect(firstPoint).toHaveProperty('close');
  });

  it('should cleanup chart on unmount', () => {
    const { unmount } = render(<TradingChart />);

    unmount();

    expect(mockRemove).toHaveBeenCalled();
  });

  it('should handle window resize', () => {
    render(<TradingChart />);

    // Trigger resize event
    window.dispatchEvent(new Event('resize'));

    expect(mockApplyOptions).toHaveBeenCalled();
  });

  it('should remove resize listener on unmount', () => {
    const removeEventListenerSpy = vi.spyOn(window, 'removeEventListener');

    const { unmount } = render(<TradingChart />);

    unmount();

    expect(removeEventListenerSpy).toHaveBeenCalledWith('resize', expect.any(Function));

    removeEventListenerSpy.mockRestore();
  });

  it('should render chart div element', () => {
    const { container } = render(<TradingChart />);

    const chartDiv = container.querySelector('.chart');
    expect(chartDiv).toBeInTheDocument();
  });

  it('should render chart header', () => {
    const { container } = render(<TradingChart />);

    const header = container.querySelector('.chart-header');
    expect(header).toBeInTheDocument();
  });

  it('should display interval', () => {
    render(<TradingChart />);

    expect(screen.getByText('1H')).toBeInTheDocument();
  });
});

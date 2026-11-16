import { useEffect, useRef, useState, useCallback } from 'react';
import { createChart, CandlestickSeries, LineSeries, HistogramSeries } from 'lightweight-charts';
import type { IChartApi, ISeriesApi, CandlestickData, Time, LineData, HistogramData } from 'lightweight-charts';
import { useMarketStore } from '../stores/marketStore';

type Timeframe = '1m' | '5m' | '15m' | '1H' | '4H' | '1D';

interface ChartIndicators {
  sma20: boolean;
  sma50: boolean;
  volume: boolean;
}

export function TradingChart() {
  const { selectedCommodity, getCurrentMarket } = useMarketStore();
  const market = getCurrentMarket();
  const chartContainerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<IChartApi | null>(null);
  const candleSeriesRef = useRef<ISeriesApi<'Candlestick'> | null>(null);
  const sma20Ref = useRef<ISeriesApi<'Line'> | null>(null);
  const sma50Ref = useRef<ISeriesApi<'Line'> | null>(null);
  const volumeRef = useRef<ISeriesApi<'Histogram'> | null>(null);

  const [timeframe, setTimeframe] = useState<Timeframe>('1H');
  const [indicators, setIndicators] = useState<ChartIndicators>({
    sma20: true,
    sma50: false,
    volume: true,
  });
  const [lastCandle, setLastCandle] = useState<CandlestickData<Time> | null>(null);

  // Calculate SMA
  const calculateSMA = useCallback((data: CandlestickData<Time>[], period: number): LineData<Time>[] => {
    const result: LineData<Time>[] = [];
    for (let i = period - 1; i < data.length; i++) {
      const sum = data.slice(i - period + 1, i + 1).reduce((acc, d) => acc + d.close, 0);
      result.push({
        time: data[i].time,
        value: sum / period,
      });
    }
    return result;
  }, []);

  // Generate mock candlestick data
  const generateMockData = useCallback((tf: Timeframe): CandlestickData<Time>[] => {
    const data: CandlestickData<Time>[] = [];
    let basePrice = selectedCommodity.basePrice;
    const volatility = basePrice * 0.015;
    const now = Math.floor(Date.now() / 1000);

    const intervals: Record<Timeframe, number> = {
      '1m': 60,
      '5m': 300,
      '15m': 900,
      '1H': 3600,
      '4H': 14400,
      '1D': 86400,
    };

    const interval = intervals[tf];
    const numCandles = tf === '1D' ? 90 : tf === '4H' ? 120 : 200;

    for (let i = numCandles; i >= 0; i--) {
      const time = (now - i * interval) as Time;
      const trend = Math.sin(i / 20) * volatility * 0.5;
      const open = basePrice + trend + (Math.random() - 0.5) * volatility;
      const close = open + (Math.random() - 0.5) * volatility * 1.5;
      const high = Math.max(open, close) + Math.random() * volatility * 0.3;
      const low = Math.min(open, close) - Math.random() * volatility * 0.3;

      data.push({ time, open, high, low, close });
      basePrice = close;
    }

    return data;
  }, [selectedCommodity]);

  // Generate volume data
  const generateVolumeData = useCallback((candleData: CandlestickData<Time>[]): HistogramData<Time>[] => {
    return candleData.map((candle) => ({
      time: candle.time,
      value: Math.random() * 1000000 + 500000,
      color: candle.close >= candle.open ? 'rgba(34, 197, 94, 0.5)' : 'rgba(239, 68, 68, 0.5)',
    }));
  }, []);

  // Initialize chart
  useEffect(() => {
    if (!chartContainerRef.current) return;

    const chart = createChart(chartContainerRef.current, {
      layout: {
        background: { color: '#0d1117' },
        textColor: '#c9d1d9',
      },
      grid: {
        vertLines: { color: '#21262d' },
        horzLines: { color: '#21262d' },
      },
      crosshair: {
        mode: 1,
        vertLine: {
          color: '#58a6ff',
          labelBackgroundColor: '#58a6ff',
        },
        horzLine: {
          color: '#58a6ff',
          labelBackgroundColor: '#58a6ff',
        },
      },
      rightPriceScale: {
        borderColor: '#21262d',
        scaleMargins: {
          top: 0.1,
          bottom: 0.2,
        },
      },
      timeScale: {
        borderColor: '#21262d',
        timeVisible: true,
        secondsVisible: false,
      },
      width: chartContainerRef.current.clientWidth,
      height: 450,
    });

    // Volume series (add first so it's behind candles)
    const volumeSeries = chart.addSeries(HistogramSeries, {
      priceFormat: { type: 'volume' },
      priceScaleId: 'volume',
    });
    chart.priceScale('volume').applyOptions({
      scaleMargins: {
        top: 0.85,
        bottom: 0,
      },
    });

    // Candlestick series
    const candleSeries = chart.addSeries(CandlestickSeries, {
      upColor: '#22c55e',
      downColor: '#ef4444',
      borderUpColor: '#22c55e',
      borderDownColor: '#ef4444',
      wickUpColor: '#22c55e',
      wickDownColor: '#ef4444',
    });

    // SMA 20
    const sma20Series = chart.addSeries(LineSeries, {
      color: '#f59e0b',
      lineWidth: 1,
      priceLineVisible: false,
      lastValueVisible: false,
    });

    // SMA 50
    const sma50Series = chart.addSeries(LineSeries, {
      color: '#8b5cf6',
      lineWidth: 1,
      priceLineVisible: false,
      lastValueVisible: false,
    });

    // Generate and set data
    const candleData = generateMockData(timeframe);
    const volumeData = generateVolumeData(candleData);
    const sma20Data = calculateSMA(candleData, 20);
    const sma50Data = calculateSMA(candleData, 50);

    candleSeries.setData(candleData);
    volumeSeries.setData(volumeData);
    sma20Series.setData(sma20Data);
    sma50Series.setData(sma50Data);

    setLastCandle(candleData[candleData.length - 1]);

    chartRef.current = chart;
    candleSeriesRef.current = candleSeries;
    volumeRef.current = volumeSeries;
    sma20Ref.current = sma20Series;
    sma50Ref.current = sma50Series;

    // Handle resize
    const handleResize = () => {
      if (chartContainerRef.current) {
        chart.applyOptions({ width: chartContainerRef.current.clientWidth });
      }
    };

    window.addEventListener('resize', handleResize);

    return () => {
      window.removeEventListener('resize', handleResize);
      chart.remove();
    };
  }, [selectedCommodity, timeframe, generateMockData, generateVolumeData, calculateSMA]);

  // Update indicator visibility
  useEffect(() => {
    if (sma20Ref.current) {
      sma20Ref.current.applyOptions({ visible: indicators.sma20 });
    }
    if (sma50Ref.current) {
      sma50Ref.current.applyOptions({ visible: indicators.sma50 });
    }
    if (volumeRef.current) {
      volumeRef.current.applyOptions({ visible: indicators.volume });
    }
  }, [indicators]);

  // Simulate real-time updates
  useEffect(() => {
    if (!candleSeriesRef.current || !lastCandle) return;

    const updateInterval = setInterval(() => {
      const currentPrice = market?.price || lastCandle.close;
      const variation = currentPrice * 0.001 * (Math.random() - 0.5);
      const newClose = currentPrice + variation;

      const updatedCandle: CandlestickData<Time> = {
        ...lastCandle,
        close: newClose,
        high: Math.max(lastCandle.high, newClose),
        low: Math.min(lastCandle.low, newClose),
      };

      candleSeriesRef.current?.update(updatedCandle);
    }, 2000);

    return () => clearInterval(updateInterval);
  }, [lastCandle, market]);

  const toggleIndicator = (key: keyof ChartIndicators) => {
    setIndicators((prev) => ({ ...prev, [key]: !prev[key] }));
  };

  const priceChange = lastCandle ? lastCandle.close - lastCandle.open : 0;
  const priceChangePercent = lastCandle ? (priceChange / lastCandle.open) * 100 : 0;

  return (
    <div className="chart-container">
      <div className="chart-header">
        <div className="chart-title">
          <span className="commodity-icon">{selectedCommodity.icon}</span>
          <span className="symbol">{selectedCommodity.symbol}</span>
          <span className={`price-change ${priceChange >= 0 ? 'positive' : 'negative'}`}>
            {priceChange >= 0 ? '+' : ''}{priceChangePercent.toFixed(2)}%
          </span>
        </div>

        <div className="chart-controls">
          <div className="timeframe-selector">
            {(['1m', '5m', '15m', '1H', '4H', '1D'] as Timeframe[]).map((tf) => (
              <button
                key={tf}
                className={`tf-btn ${timeframe === tf ? 'active' : ''}`}
                onClick={() => setTimeframe(tf)}
              >
                {tf}
              </button>
            ))}
          </div>

          <div className="indicator-toggles">
            <button
              className={`indicator-btn ${indicators.sma20 ? 'active' : ''}`}
              onClick={() => toggleIndicator('sma20')}
              title="SMA 20"
            >
              <span className="indicator-dot" style={{ backgroundColor: '#f59e0b' }} />
              MA20
            </button>
            <button
              className={`indicator-btn ${indicators.sma50 ? 'active' : ''}`}
              onClick={() => toggleIndicator('sma50')}
              title="SMA 50"
            >
              <span className="indicator-dot" style={{ backgroundColor: '#8b5cf6' }} />
              MA50
            </button>
            <button
              className={`indicator-btn ${indicators.volume ? 'active' : ''}`}
              onClick={() => toggleIndicator('volume')}
              title="Volume"
            >
              Vol
            </button>
          </div>
        </div>
      </div>

      <div ref={chartContainerRef} className="chart" />

      <div className="chart-legend">
        <div className="legend-item">
          <span className="legend-label">O</span>
          <span className="legend-value">{lastCandle?.open.toFixed(2) || '-'}</span>
        </div>
        <div className="legend-item">
          <span className="legend-label">H</span>
          <span className="legend-value">{lastCandle?.high.toFixed(2) || '-'}</span>
        </div>
        <div className="legend-item">
          <span className="legend-label">L</span>
          <span className="legend-value">{lastCandle?.low.toFixed(2) || '-'}</span>
        </div>
        <div className="legend-item">
          <span className="legend-label">C</span>
          <span className="legend-value">{lastCandle?.close.toFixed(2) || '-'}</span>
        </div>
      </div>
    </div>
  );
}

import { useEffect, useRef } from 'react';
import { createChart, CandlestickSeries } from 'lightweight-charts';
import type { IChartApi, ISeriesApi, CandlestickData, Time } from 'lightweight-charts';
import { useMarketStore } from '../stores/marketStore';

export function TradingChart() {
  const { selectedCommodity } = useMarketStore();
  const chartContainerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<IChartApi | null>(null);
  const candleSeriesRef = useRef<ISeriesApi<'Candlestick'> | null>(null);

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
      },
      rightPriceScale: {
        borderColor: '#21262d',
      },
      timeScale: {
        borderColor: '#21262d',
        timeVisible: true,
      },
      width: chartContainerRef.current.clientWidth,
      height: 400,
    });

    const candleSeries = chart.addSeries(CandlestickSeries, {
      upColor: '#22c55e',
      downColor: '#ef4444',
      borderUpColor: '#22c55e',
      borderDownColor: '#ef4444',
      wickUpColor: '#22c55e',
      wickDownColor: '#ef4444',
    });

    // Generate mock data for demo based on commodity
    const generateMockData = (): CandlestickData<Time>[] => {
      const data: CandlestickData<Time>[] = [];
      let basePrice = selectedCommodity.basePrice;
      const volatility = basePrice * 0.02; // 2% volatility
      const now = Math.floor(Date.now() / 1000);

      for (let i = 100; i >= 0; i--) {
        const time = (now - i * 3600) as Time;
        const open = basePrice + (Math.random() - 0.5) * volatility;
        const close = open + (Math.random() - 0.5) * volatility * 1.5;
        const high = Math.max(open, close) + Math.random() * volatility * 0.5;
        const low = Math.min(open, close) - Math.random() * volatility * 0.5;

        data.push({ time, open, high, low, close });
        basePrice = close;
      }

      return data;
    };

    candleSeries.setData(generateMockData());

    chartRef.current = chart;
    candleSeriesRef.current = candleSeries;

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
  }, [selectedCommodity]);

  return (
    <div className="chart-container">
      <div className="chart-header">
        <span className="symbol">
          <span className="commodity-icon">{selectedCommodity.icon}</span>
          {selectedCommodity.symbol}
        </span>
        <span className="interval">1H</span>
      </div>
      <div ref={chartContainerRef} className="chart" />
    </div>
  );
}

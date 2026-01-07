import { useEffect, useCallback } from 'react';
import { useConnection, useWallet } from '@solana/wallet-adapter-react';
import { useMarketStore } from '../stores/marketStore';
import type { MarketData, OrderBook, Trade } from '../types';

const API_URL = import.meta.env.VITE_API_URL || 'http://localhost:3001';
const WS_URL = import.meta.env.VITE_WS_URL || 'ws://localhost:3001/ws';

export function useMarket() {
  const { connection } = useConnection();
  const { publicKey } = useWallet();
  const {
    market,
    orderBook,
    recentTrades,
    setMarket,
    setOrderBook,
    addTrade,
  } = useMarketStore();

  // Fetch market data
  const fetchMarket = useCallback(async () => {
    try {
      const [marketRes, statsRes] = await Promise.all([
        fetch(`${API_URL}/api/market`),
        fetch(`${API_URL}/api/market/stats`),
      ]);

      const marketData = await marketRes.json();
      const stats = await statsRes.json();

      setMarket({
        address: marketData.address,
        price: stats.price / 1_000_000,
        priceChange24h: stats.price_change_24h,
        volume24h: stats.volume_24h / 1_000_000,
        openInterest: stats.open_interest / 1_000_000,
        longOpenInterest: marketData.long_open_interest / 1_000_000,
        shortOpenInterest: marketData.short_open_interest / 1_000_000,
        fundingRate: marketData.funding_rate / 1_000_000,
        maxLeverage: marketData.max_leverage / 1000,
        isPaused: marketData.is_paused,
      });
    } catch (error) {
      console.error('Failed to fetch market data:', error);
    }
  }, [setMarket]);

  // Fetch order book
  const fetchOrderBook = useCallback(async () => {
    try {
      const res = await fetch(`${API_URL}/api/orderbook`);
      const data = await res.json();

      const bids = data.bids.map(([price, size]: [number, number], i: number, arr: [number, number][]) => ({
        price: price / 1_000_000,
        size: size / 1_000_000,
        total: arr.slice(0, i + 1).reduce((sum, [, s]) => sum + s / 1_000_000, 0),
      }));

      const asks = data.asks.map(([price, size]: [number, number], i: number, arr: [number, number][]) => ({
        price: price / 1_000_000,
        size: size / 1_000_000,
        total: arr.slice(0, i + 1).reduce((sum, [, s]) => sum + s / 1_000_000, 0),
      }));

      const bestBid = bids[0]?.price || 0;
      const bestAsk = asks[0]?.price || 0;

      setOrderBook({
        bids,
        asks,
        spread: bestAsk - bestBid,
        midPrice: (bestBid + bestAsk) / 2,
      });
    } catch (error) {
      console.error('Failed to fetch order book:', error);
    }
  }, [setOrderBook]);

  // WebSocket connection for real-time updates
  useEffect(() => {
    let ws: WebSocket | null = null;

    const connect = () => {
      ws = new WebSocket(WS_URL);

      ws.onopen = () => {
        console.log('WebSocket connected');
        ws?.send(JSON.stringify({ type: 'subscribe', channel: 'price' }));
        ws?.send(JSON.stringify({ type: 'subscribe', channel: 'orderbook' }));
        ws?.send(JSON.stringify({ type: 'subscribe', channel: 'trades' }));
      };

      ws.onmessage = (event) => {
        const data = JSON.parse(event.data);

        switch (data.type) {
          case 'price':
            if (market) {
              setMarket({ ...market, price: data.price / 1_000_000 });
            }
            break;

          case 'trade':
            addTrade({
              signature: '',
              side: data.side,
              price: data.price / 1_000_000,
              size: data.size / 1_000_000,
              timestamp: data.timestamp,
            });
            break;
        }
      };

      ws.onclose = () => {
        console.log('WebSocket disconnected, reconnecting...');
        setTimeout(connect, 3000);
      };

      ws.onerror = (error) => {
        console.error('WebSocket error:', error);
      };
    };

    connect();

    return () => {
      ws?.close();
    };
  }, [market, setMarket, addTrade]);

  // Fetch data on mount
  useEffect(() => {
    fetchMarket();
    fetchOrderBook();

    const interval = setInterval(() => {
      fetchMarket();
      fetchOrderBook();
    }, 5000);

    return () => clearInterval(interval);
  }, [fetchMarket, fetchOrderBook]);

  return {
    market,
    orderBook,
    recentTrades,
    refreshMarket: fetchMarket,
    refreshOrderBook: fetchOrderBook,
  };
}

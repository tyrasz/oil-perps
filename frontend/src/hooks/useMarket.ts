import { useEffect, useCallback, useRef } from 'react';
import { useMarketStore, COMMODITIES } from '../stores/marketStore';
import type { CommodityConfig } from '../config/commodities';

const API_URL = import.meta.env.VITE_API_URL || 'http://localhost:3003';
const WS_URL = import.meta.env.VITE_WS_URL || 'ws://localhost:3003/ws';

export function useMarket() {
  const {
    selectedCommodity,
    markets,
    setMarket,
    setOrderBook,
    addTrade,
    getCurrentMarket,
    getCurrentOrderBook,
    getCurrentTrades,
  } = useMarketStore();

  const wsRef = useRef<WebSocket | null>(null);
  const subscribedCommoditiesRef = useRef<Set<string>>(new Set());

  // Fetch market data for a specific commodity
  const fetchMarket = useCallback(async (commodity: CommodityConfig) => {
    try {
      const [marketRes, statsRes] = await Promise.all([
        fetch(`${API_URL}/api/market/${commodity.id}`),
        fetch(`${API_URL}/api/market/${commodity.id}/stats`),
      ]);

      // Fallback to old endpoints if new ones not available
      const marketData = marketRes.ok
        ? await marketRes.json()
        : await fetch(`${API_URL}/api/market`).then(r => r.json());

      const stats = statsRes.ok
        ? await statsRes.json()
        : await fetch(`${API_URL}/api/market/stats`).then(r => r.json());

      setMarket(commodity.id, {
        address: marketData.address || `${commodity.id}_market`,
        commodity: commodity.id,
        price: (stats.price || commodity.basePrice * 1_000_000) / 1_000_000,
        priceChange24h: stats.price_change_24h || 0,
        volume24h: (stats.volume_24h || 0) / 1_000_000,
        openInterest: (stats.open_interest || 0) / 1_000_000,
        longOpenInterest: (marketData.long_open_interest || 0) / 1_000_000,
        shortOpenInterest: (marketData.short_open_interest || 0) / 1_000_000,
        fundingRate: (marketData.funding_rate || 0) / 1_000_000,
        maxLeverage: (marketData.max_leverage || commodity.maxLeverage * 1000) / 1000,
        isPaused: marketData.is_paused || false,
      });
    } catch (error) {
      console.error(`Failed to fetch market data for ${commodity.id}:`, error);
      // Set mock data as fallback
      setMarket(commodity.id, {
        address: `${commodity.id}_market`,
        commodity: commodity.id,
        price: commodity.basePrice,
        priceChange24h: (Math.random() - 0.5) * 4,
        volume24h: Math.random() * 10,
        openInterest: Math.random() * 5,
        longOpenInterest: Math.random() * 2.5,
        shortOpenInterest: Math.random() * 2.5,
        fundingRate: (Math.random() - 0.5) * 0.001,
        maxLeverage: commodity.maxLeverage,
        isPaused: false,
      });
    }
  }, [setMarket]);

  // Fetch order book for a specific commodity
  const fetchOrderBook = useCallback(async (commodity: CommodityConfig) => {
    try {
      const res = await fetch(`${API_URL}/api/orderbook/${commodity.id}`);

      // Fallback to old endpoint
      const data = res.ok
        ? await res.json()
        : await fetch(`${API_URL}/api/orderbook`).then(r => r.json());

      const bids = (data.bids || []).map(([price, size]: [number, number], i: number, arr: [number, number][]) => ({
        price: price / 1_000_000,
        size: size / 1_000_000,
        total: arr.slice(0, i + 1).reduce((sum, [, s]) => sum + s / 1_000_000, 0),
      }));

      const asks = (data.asks || []).map(([price, size]: [number, number], i: number, arr: [number, number][]) => ({
        price: price / 1_000_000,
        size: size / 1_000_000,
        total: arr.slice(0, i + 1).reduce((sum, [, s]) => sum + s / 1_000_000, 0),
      }));

      const bestBid = bids[0]?.price || 0;
      const bestAsk = asks[0]?.price || 0;

      setOrderBook(commodity.id, {
        bids,
        asks,
        spread: bestAsk - bestBid,
        midPrice: (bestBid + bestAsk) / 2,
      });
    } catch (error) {
      console.error(`Failed to fetch order book for ${commodity.id}:`, error);
    }
  }, [setOrderBook]);

  // Subscribe to WebSocket updates for a commodity
  const subscribeToCommodity = useCallback((commodity: CommodityConfig) => {
    if (!wsRef.current || wsRef.current.readyState !== WebSocket.OPEN) return;
    if (subscribedCommoditiesRef.current.has(commodity.id)) return;

    wsRef.current.send(JSON.stringify({
      type: 'subscribe',
      channel: 'price',
      commodity: commodity.id,
    }));
    wsRef.current.send(JSON.stringify({
      type: 'subscribe',
      channel: 'orderbook',
      commodity: commodity.id,
    }));
    wsRef.current.send(JSON.stringify({
      type: 'subscribe',
      channel: 'trades',
      commodity: commodity.id,
    }));

    subscribedCommoditiesRef.current.add(commodity.id);
  }, []);

  // WebSocket connection for real-time updates
  useEffect(() => {
    let reconnectTimeout: NodeJS.Timeout;

    const connect = () => {
      wsRef.current = new WebSocket(WS_URL);

      wsRef.current.onopen = () => {
        console.log('WebSocket connected');
        subscribedCommoditiesRef.current.clear();
        // Subscribe to current commodity
        subscribeToCommodity(selectedCommodity);
      };

      wsRef.current.onmessage = (event) => {
        try {
          const data = JSON.parse(event.data);
          const commodityId = data.commodity || selectedCommodity.id;

          switch (data.type) {
            case 'price': {
              const currentMarket = markets[commodityId];
              if (currentMarket) {
                setMarket(commodityId, {
                  ...currentMarket,
                  price: data.price / 1_000_000,
                });
              }
              break;
            }

            case 'trade':
              addTrade(commodityId, {
                signature: data.signature || '',
                commodity: commodityId,
                side: data.side,
                price: data.price / 1_000_000,
                size: data.size / 1_000_000,
                timestamp: data.timestamp,
              });
              break;
          }
        } catch (error) {
          console.error('Failed to parse WebSocket message:', error);
        }
      };

      wsRef.current.onclose = () => {
        console.log('WebSocket disconnected, reconnecting...');
        reconnectTimeout = setTimeout(connect, 3000);
      };

      wsRef.current.onerror = (error) => {
        console.error('WebSocket error:', error);
      };
    };

    connect();

    return () => {
      clearTimeout(reconnectTimeout);
      wsRef.current?.close();
    };
  }, [selectedCommodity, markets, setMarket, addTrade, subscribeToCommodity]);

  // Fetch data for selected commodity when it changes
  useEffect(() => {
    fetchMarket(selectedCommodity);
    fetchOrderBook(selectedCommodity);
    subscribeToCommodity(selectedCommodity);

    const interval = setInterval(() => {
      fetchMarket(selectedCommodity);
      fetchOrderBook(selectedCommodity);
    }, 5000);

    return () => clearInterval(interval);
  }, [selectedCommodity, fetchMarket, fetchOrderBook, subscribeToCommodity]);

  // Also fetch data for all commodities on initial mount
  useEffect(() => {
    COMMODITIES.forEach(commodity => {
      fetchMarket(commodity);
    });
  }, [fetchMarket]);

  return {
    // Current commodity
    selectedCommodity,
    market: getCurrentMarket(),
    orderBook: getCurrentOrderBook(),
    recentTrades: getCurrentTrades(),

    // All markets (for mini-charts, etc.)
    markets,

    // Refresh functions
    refreshMarket: () => fetchMarket(selectedCommodity),
    refreshOrderBook: () => fetchOrderBook(selectedCommodity),

    // Available commodities
    commodities: COMMODITIES,
  };
}

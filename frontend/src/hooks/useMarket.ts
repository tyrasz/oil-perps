import { useEffect, useCallback, useRef } from 'react';
import { useConnection } from '@solana/wallet-adapter-react';
import { PublicKey, type Connection } from '@solana/web3.js';
import { useMarketStore, COMMODITIES, DEFAULT_COMMODITY } from '../stores/marketStore';
import { ORACLE_SOURCE } from '../config/program';
import { getCommodityMarket } from '../utils/pda';
import { decodeMarketOpenInterest, decodeOraclePrice, MARKET_PYTH_PRICE_FEED_OFFSET } from '../utils/oracleAccount';
import type { CommodityConfig } from '../config/commodities';
import type { Trade } from '../types';

const API_URL = import.meta.env.VITE_API_URL || 'http://localhost:3003';
const WS_URL = import.meta.env.VITE_WS_URL || 'ws://localhost:3003/ws';
const EMPTY_TRADES: Trade[] = [];

async function fetchJson(url: string) {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`HTTP ${res.status} for ${url}`);
  return res.json();
}

/**
 * Owns the market data feed: REST polling plus one WebSocket connection.
 * Mount it exactly once (in App); components read the data via useMarket().
 */
/**
 * One commodity's price and open interest straight from the chain: the
 * market account and its oracle (ORACLE_SOURCE 'chain', the localnet
 * default). Null if the market does not exist on this cluster.
 */
async function fetchChainMarket(connection: Connection, commodity: CommodityConfig, firstPrice: Map<string, number>) {
  const address = getCommodityMarket(commodity.id);
  const info = await connection.getAccountInfo(address);
  if (!info || info.data.length < MARKET_PYTH_PRICE_FEED_OFFSET + 32) return null;
  const feed = new PublicKey(info.data.subarray(MARKET_PYTH_PRICE_FEED_OFFSET, MARKET_PYTH_PRICE_FEED_OFFSET + 32));
  const feedInfo = await connection.getAccountInfo(feed);
  const oracle = feedInfo ? decodeOraclePrice(feedInfo.data) : null;
  if (!oracle) return null;
  const first = firstPrice.get(commodity.id) ?? oracle.price;
  firstPrice.set(commodity.id, first);
  // Open interest from the market account (contracts), in USD at the oracle
  const oi = decodeMarketOpenInterest(info.data) ?? { long: 0, short: 0 };
  return {
    address: address.toBase58(),
    commodity: commodity.id,
    price: oracle.price,
    // Since this page started watching (no 24 h history without the API)
    priceChange24h: first > 0 ? ((oracle.price - first) / first) * 100 : 0,
    // No volume history on chain without the API: shown as unavailable
    volume24h: Number.NaN,
    openInterest: (oi.long + oi.short) * oracle.price,
    longOpenInterest: oi.long * oracle.price,
    shortOpenInterest: oi.short * oracle.price,
    fundingRate: 0,
    maxLeverage: commodity.maxLeverage,
    isPaused: false,
  };
}

export function useMarketFeed() {
  const { connection } = useConnection();
  const firstPrice = useRef(new Map<string, number>());
  const selectedCommodity = useMarketStore((s) => s.selectedCommodity);
  const setMarket = useMarketStore((s) => s.setMarket);
  const setOrderBook = useMarketStore((s) => s.setOrderBook);
  const addTrade = useMarketStore((s) => s.addTrade);

  const wsRef = useRef<WebSocket | null>(null);
  const subscribedCommoditiesRef = useRef<Set<string>>(new Set());

  // Fetch market data for a specific commodity
  const fetchMarket = useCallback(async (commodity: CommodityConfig) => {
    if (ORACLE_SOURCE === 'chain') {
      try {
        const m = await fetchChainMarket(connection, commodity, firstPrice.current);
        if (m) setMarket(commodity.id, m);
      } catch (error) {
        console.error(`Failed to read ${commodity.id} from the chain:`, error);
      }
      return;
    }
    try {
      const [marketRes, statsRes] = await Promise.all([
        fetch(`${API_URL}/api/market/${commodity.id}`),
        fetch(`${API_URL}/api/market/${commodity.id}/stats`),
      ]);

      // The legacy single-market endpoints serve the original (default) market
      // only; using them for another commodity would store its price under
      // the wrong market.
      const legacy = commodity.id === DEFAULT_COMMODITY.id;
      const marketData = marketRes.ok
        ? await marketRes.json()
        : legacy ? await fetchJson(`${API_URL}/api/market`) : null;
      const stats = statsRes.ok
        ? await statsRes.json()
        : legacy ? await fetchJson(`${API_URL}/api/market/stats`) : null;
      if (!marketData || !stats?.price) {
        throw new Error(`no price data for ${commodity.id}`);
      }

      setMarket(commodity.id, {
        address: marketData.address || `${commodity.id}_market`,
        commodity: commodity.id,
        price: stats.price / 1_000_000,
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
      // Keep the last real data: replacing a live price with a demo value
      // could fire TP/SL orders or misstate margin. Demo data is only for a
      // commodity that has never loaded (e.g. no backend running).
      if (useMarketStore.getState().markets[commodity.id]) return;
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
  }, [setMarket, connection]);

  // Fetch order book for a specific commodity
  const fetchOrderBook = useCallback(async (commodity: CommodityConfig) => {
    // No backend book on the chain source (venue quotes live on mm-registry)
    if (ORACLE_SOURCE === 'chain') return;
    try {
      const res = await fetch(`${API_URL}/api/orderbook/${commodity.id}`);

      // The legacy endpoint serves the default (OIL) book only; never show it
      // under another commodity
      let data: { bids?: [number, number][]; asks?: [number, number][] };
      if (res.ok) {
        data = await res.json();
      } else if (commodity.id === DEFAULT_COMMODITY.id) {
        data = await fetch(`${API_URL}/api/orderbook`).then(r => r.json());
      } else {
        data = { bids: [], asks: [] };
      }

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

  // WebSocket connection for real-time updates. Opened once; handlers read the
  // latest store state so price updates don't tear down the connection.
  useEffect(() => {
    // The chain source has no backend to stream from
    if (ORACLE_SOURCE === 'chain') return;
    let reconnectTimeout: ReturnType<typeof setTimeout> | undefined;
    let disposed = false;

    const connect = () => {
      const ws = new WebSocket(WS_URL);
      wsRef.current = ws;

      ws.onopen = () => {
        subscribedCommoditiesRef.current.clear();
        subscribeToCommodity(useMarketStore.getState().selectedCommodity);
      };

      ws.onmessage = (event) => {
        try {
          const data = JSON.parse(event.data);
          const state = useMarketStore.getState();
          // The backend's feed doesn't tag messages with a commodity; untagged
          // messages are the original (default) market's, not the selected one.
          const commodityId = data.commodity || DEFAULT_COMMODITY.id;

          switch (data.type) {
            case 'price': {
              const currentMarket = state.markets[commodityId];
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

      ws.onclose = () => {
        if (disposed) return;
        reconnectTimeout = setTimeout(connect, 3000);
      };

      ws.onerror = (error) => {
        console.error('WebSocket error:', error);
      };
    };

    connect();

    return () => {
      disposed = true;
      clearTimeout(reconnectTimeout);
      wsRef.current?.close();
      wsRef.current = null;
    };
  }, [setMarket, addTrade, subscribeToCommodity]);

  // Poll every commodity's market data (open positions can be in any of them)
  // and the selected commodity's order book.
  useEffect(() => {
    const refresh = () => {
      COMMODITIES.forEach(commodity => fetchMarket(commodity));
      fetchOrderBook(selectedCommodity);
    };
    refresh();
    subscribeToCommodity(selectedCommodity);

    const interval = setInterval(refresh, ORACLE_SOURCE === 'chain' ? 2000 : 5000);
    return () => clearInterval(interval);
  }, [selectedCommodity, fetchMarket, fetchOrderBook, subscribeToCommodity]);

}

/** Read-only view of market data for the selected commodity. */
export function useMarket() {
  const selectedCommodity = useMarketStore((s) => s.selectedCommodity);
  const markets = useMarketStore((s) => s.markets);
  const market = useMarketStore((s) => s.markets[s.selectedCommodity.id] ?? null);
  const orderBook = useMarketStore((s) => s.orderBooks[s.selectedCommodity.id] ?? null);
  const recentTrades = useMarketStore((s) => s.recentTrades[s.selectedCommodity.id] ?? EMPTY_TRADES);

  return {
    selectedCommodity,
    market,
    orderBook,
    recentTrades,
    markets,
    commodities: COMMODITIES,
  };
}

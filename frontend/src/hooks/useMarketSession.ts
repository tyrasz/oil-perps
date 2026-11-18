import { useEffect, useState } from 'react';
import { marketSession, type MarketSession, type MarketSessionParams } from '../utils/session';

const nowSeconds = () => Math.floor(Date.now() / 1000);

/** Trading-session state for an on-chain market, refreshed every 30 seconds. */
export function useMarketSession(market: MarketSessionParams | null | undefined): MarketSession {
  const [now, setNow] = useState(nowSeconds);

  useEffect(() => {
    const id = setInterval(() => setNow(nowSeconds()), 30_000);
    return () => clearInterval(id);
  }, []);

  return marketSession(market, now);
}

import { useEffect, useCallback } from 'react';
import { useWallet } from '@solana/wallet-adapter-react';
import { useMarketStore } from '../stores/marketStore';
import { useOnChainPositions } from './useOnChainPositions';

const API_URL = import.meta.env.VITE_API_URL || 'http://localhost:3003';

export function usePositions() {
  const { publicKey } = useWallet();
  const { orders, setPositions, setOrders, setUserAccount } = useMarketStore();

  // Use on-chain positions as primary data source
  const {
    positions: onChainPositions,
    userAccount: onChainUserAccount,
    isLoading,
    refresh: refreshOnChain,
    refreshPositions: refreshOnChainPositions,
  } = useOnChainPositions();

  // Sync on-chain positions to store
  useEffect(() => {
    setPositions(onChainPositions);
  }, [onChainPositions, setPositions]);

  // Sync on-chain user account to store
  useEffect(() => {
    if (onChainUserAccount) {
      setUserAccount(onChainUserAccount);
    }
  }, [onChainUserAccount, setUserAccount]);

  // Fetch orders from API (no on-chain order book yet)
  const fetchOrders = useCallback(async () => {
    if (!publicKey) return;

    try {
      const res = await fetch(`${API_URL}/api/orders/${publicKey.toString()}`);
      const data = await res.json();

      setOrders(data.map((o: any) => ({
        address: o.address,
        owner: o.owner,
        commodity: o.commodity || 'OIL',
        side: o.side.toLowerCase(),
        orderType: o.order_type.toLowerCase(),
        price: o.price / 1_000_000,
        size: o.size / 1_000_000,
        filledSize: o.filled_size / 1_000_000,
        status: o.status.toLowerCase(),
        createdAt: o.created_at,
      })));
    } catch (error) {
      console.error('Failed to fetch orders:', error);
    }
  }, [publicKey, setOrders]);

  useEffect(() => {
    if (publicKey) {
      fetchOrders();

      const interval = setInterval(() => {
        fetchOrders();
      }, 5000);

      return () => clearInterval(interval);
    }
  }, [publicKey, fetchOrders]);

  return {
    positions: onChainPositions,
    orders,
    userAccount: onChainUserAccount,
    isLoading,
    refreshPositions: refreshOnChainPositions,
    refreshOrders: fetchOrders,
    refreshAccount: refreshOnChain,
  };
}

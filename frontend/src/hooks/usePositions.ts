import { useEffect, useCallback } from 'react';
import { useWallet } from '@solana/wallet-adapter-react';
import { useMarketStore } from '../stores/marketStore';

const API_URL = import.meta.env.VITE_API_URL || 'http://localhost:3001';

export function usePositions() {
  const { publicKey } = useWallet();
  const { positions, orders, userAccount, setPositions, setOrders, setUserAccount } = useMarketStore();

  const fetchPositions = useCallback(async () => {
    if (!publicKey) return;

    try {
      const res = await fetch(`${API_URL}/api/positions/${publicKey.toString()}`);
      const data = await res.json();

      setPositions(data.map((p: any) => ({
        address: p.address,
        owner: p.owner,
        side: p.side === 'Long' ? 'long' : 'short',
        size: p.size / 1_000_000,
        collateral: p.collateral / 1_000_000,
        entryPrice: p.entry_price / 1_000_000,
        leverage: p.leverage / 1000,
        unrealizedPnl: p.unrealized_pnl / 1_000_000,
        marginRatio: p.margin_ratio / 100,
        openedAt: p.opened_at,
      })));
    } catch (error) {
      console.error('Failed to fetch positions:', error);
    }
  }, [publicKey, setPositions]);

  const fetchOrders = useCallback(async () => {
    if (!publicKey) return;

    try {
      const res = await fetch(`${API_URL}/api/orders/${publicKey.toString()}`);
      const data = await res.json();

      setOrders(data.map((o: any) => ({
        address: o.address,
        owner: o.owner,
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

  const fetchAccount = useCallback(async () => {
    if (!publicKey) return;

    try {
      const res = await fetch(`${API_URL}/api/account/${publicKey.toString()}`);
      const data = await res.json();

      setUserAccount({
        address: data.address,
        collateralBalance: data.collateral_balance / 1_000_000,
        totalPositions: data.total_positions,
        realizedPnl: data.realized_pnl / 1_000_000,
      });
    } catch (error) {
      console.error('Failed to fetch account:', error);
    }
  }, [publicKey, setUserAccount]);

  useEffect(() => {
    if (publicKey) {
      fetchPositions();
      fetchOrders();
      fetchAccount();

      const interval = setInterval(() => {
        fetchPositions();
        fetchOrders();
      }, 5000);

      return () => clearInterval(interval);
    }
  }, [publicKey, fetchPositions, fetchOrders, fetchAccount]);

  return {
    positions,
    orders,
    userAccount,
    refreshPositions: fetchPositions,
    refreshOrders: fetchOrders,
    refreshAccount: fetchAccount,
  };
}

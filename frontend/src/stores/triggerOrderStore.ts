import { create } from 'zustand';
import { persist } from 'zustand/middleware';
import type { TriggerOrder, PriceAlert } from '../types';

interface TriggerOrderState {
  triggerOrders: TriggerOrder[];
  priceAlerts: PriceAlert[];

  // Trigger order actions
  addTriggerOrder: (order: Omit<TriggerOrder, 'id' | 'createdAt' | 'status'>) => string;
  removeTriggerOrder: (id: string) => void;
  updateTriggerOrderStatus: (id: string, status: TriggerOrder['status']) => void;
  getTriggerOrdersForPosition: (positionAddress: string) => TriggerOrder[];
  getActiveTriggerOrders: () => TriggerOrder[];

  // Price alert actions
  addPriceAlert: (alert: Omit<PriceAlert, 'id' | 'createdAt' | 'triggered' | 'notified'>) => string;
  removePriceAlert: (id: string) => void;
  markAlertTriggered: (id: string) => void;
  markAlertNotified: (id: string) => void;
  getActiveAlerts: () => PriceAlert[];
  getAlertsForCommodity: (commodity: string) => PriceAlert[];
}

function generateId(): string {
  return `${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;
}

export const useTriggerOrderStore = create<TriggerOrderState>()(
  persist(
    (set, get) => ({
      triggerOrders: [],
      priceAlerts: [],

      addTriggerOrder: (order) => {
        const id = generateId();
        const newOrder: TriggerOrder = {
          ...order,
          id,
          createdAt: Date.now(),
          status: 'active',
        };
        set((state) => ({
          triggerOrders: [...state.triggerOrders, newOrder],
        }));
        return id;
      },

      removeTriggerOrder: (id) => {
        set((state) => ({
          triggerOrders: state.triggerOrders.filter((o) => o.id !== id),
        }));
      },

      updateTriggerOrderStatus: (id, status) => {
        set((state) => ({
          triggerOrders: state.triggerOrders.map((o) =>
            o.id === id ? { ...o, status } : o
          ),
        }));
      },

      getTriggerOrdersForPosition: (positionAddress) => {
        return get().triggerOrders.filter(
          (o) => o.positionAddress === positionAddress
        );
      },

      getActiveTriggerOrders: () => {
        return get().triggerOrders.filter((o) => o.status === 'active');
      },

      addPriceAlert: (alert) => {
        const id = generateId();
        const newAlert: PriceAlert = {
          ...alert,
          id,
          createdAt: Date.now(),
          triggered: false,
          notified: false,
        };
        set((state) => ({
          priceAlerts: [...state.priceAlerts, newAlert],
        }));
        return id;
      },

      removePriceAlert: (id) => {
        set((state) => ({
          priceAlerts: state.priceAlerts.filter((a) => a.id !== id),
        }));
      },

      markAlertTriggered: (id) => {
        set((state) => ({
          priceAlerts: state.priceAlerts.map((a) =>
            a.id === id ? { ...a, triggered: true } : a
          ),
        }));
      },

      markAlertNotified: (id) => {
        set((state) => ({
          priceAlerts: state.priceAlerts.map((a) =>
            a.id === id ? { ...a, notified: true } : a
          ),
        }));
      },

      getActiveAlerts: () => {
        return get().priceAlerts.filter((a) => !a.triggered);
      },

      getAlertsForCommodity: (commodity) => {
        return get().priceAlerts.filter((a) => a.commodity === commodity);
      },
    }),
    {
      name: 'trigger-orders-storage',
      partialize: (state) => ({
        triggerOrders: state.triggerOrders.filter((o) => o.status === 'active'),
        priceAlerts: state.priceAlerts.filter((a) => !a.triggered),
      }),
    }
  )
);

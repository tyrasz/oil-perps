import { useEffect, useRef, useCallback } from 'react';
import { useTriggerOrderStore } from '../stores/triggerOrderStore';
import { useMarketStore } from '../stores/marketStore';
import { useTrading } from './useTrading';
import { usePositions } from './usePositions';
import type { TriggerOrder, Position } from '../types';

const CHECK_INTERVAL_MS = 1000; // Check every second

export function useTriggerOrderMonitor() {
  const { getActiveTriggerOrders, updateTriggerOrderStatus, getActiveAlerts, markAlertTriggered } = useTriggerOrderStore();
  const { markets } = useMarketStore();
  const { closePosition } = useTrading();
  const { positions } = usePositions();
  const processingRef = useRef<Set<string>>(new Set());

  // Check if a trigger order should be executed
  const checkTriggerOrder = useCallback(
    (order: TriggerOrder, position: Position | undefined, currentPrice: number): boolean => {
      if (!position || order.status !== 'active') return false;

      const { type, triggerPrice } = order;
      const { side } = position;

      if (type === 'take_profit') {
        // TP triggers when price moves in profitable direction
        if (side === 'long' && currentPrice >= triggerPrice) return true;
        if (side === 'short' && currentPrice <= triggerPrice) return true;
      } else if (type === 'stop_loss') {
        // SL triggers when price moves in losing direction
        if (side === 'long' && currentPrice <= triggerPrice) return true;
        if (side === 'short' && currentPrice >= triggerPrice) return true;
      }

      return false;
    },
    []
  );

  // Execute trigger order
  const executeTriggerOrder = useCallback(
    async (order: TriggerOrder, position: Position) => {
      if (processingRef.current.has(order.id)) return;

      processingRef.current.add(order.id);
      console.log(`Executing ${order.type} for position ${position.address}`);

      try {
        // Mark as triggered before attempting close
        updateTriggerOrderStatus(order.id, 'triggered');

        // Close the position (full close for now, partial close can be added later)
        await closePosition(position.address);

        console.log(`${order.type} executed successfully`);

        // Show notification
        if ('Notification' in window && Notification.permission === 'granted') {
          new Notification(`${order.type === 'take_profit' ? 'Take Profit' : 'Stop Loss'} Triggered`, {
            body: `${position.side.toUpperCase()} position closed at trigger price`,
            icon: order.type === 'take_profit' ? '🎯' : '🛑',
          });
        }
      } catch (error) {
        console.error(`Failed to execute ${order.type}:`, error);
        // Revert status on failure
        updateTriggerOrderStatus(order.id, 'active');
      } finally {
        processingRef.current.delete(order.id);
      }
    },
    [closePosition, updateTriggerOrderStatus]
  );

  // Monitor trigger orders
  useEffect(() => {
    const interval = setInterval(() => {
      const activeOrders = getActiveTriggerOrders();

      for (const order of activeOrders) {
        const position = positions.find((p) => p.address === order.positionAddress);
        if (!position) {
          // Position no longer exists, cancel the trigger order
          updateTriggerOrderStatus(order.id, 'cancelled');
          continue;
        }

        const marketPrice = markets[position.commodity]?.price;
        if (!marketPrice) continue;

        if (checkTriggerOrder(order, position, marketPrice)) {
          executeTriggerOrder(order, position);
        }
      }
    }, CHECK_INTERVAL_MS);

    return () => clearInterval(interval);
  }, [getActiveTriggerOrders, positions, markets, checkTriggerOrder, executeTriggerOrder, updateTriggerOrderStatus]);

  // Monitor price alerts
  useEffect(() => {
    const interval = setInterval(() => {
      const activeAlerts = getActiveAlerts();

      for (const alert of activeAlerts) {
        const marketPrice = markets[alert.commodity]?.price;
        if (!marketPrice) continue;

        const triggered =
          (alert.condition === 'above' && marketPrice >= alert.targetPrice) ||
          (alert.condition === 'below' && marketPrice <= alert.targetPrice);

        if (triggered) {
          markAlertTriggered(alert.id);

          // Show notification
          if ('Notification' in window && Notification.permission === 'granted') {
            new Notification('Price Alert', {
              body: `${alert.commodity} is ${alert.condition} $${alert.targetPrice.toFixed(2)}`,
              icon: '📊',
            });
          }
        }
      }
    }, CHECK_INTERVAL_MS);

    return () => clearInterval(interval);
  }, [getActiveAlerts, markets, markAlertTriggered]);

  // Request notification permission
  useEffect(() => {
    if ('Notification' in window && Notification.permission === 'default') {
      Notification.requestPermission();
    }
  }, []);
}

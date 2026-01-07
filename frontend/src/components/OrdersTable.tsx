import { usePositions } from '../hooks/usePositions';
import type { Order } from '../types';

export function OrdersTable() {
  const { orders } = usePositions();

  const handleCancelOrder = async (order: Order) => {
    // TODO: Implement cancel order transaction
    console.log('Cancelling order:', order.address);
  };

  if (orders.length === 0) {
    return (
      <div className="orders-table empty">
        <p>No open orders</p>
      </div>
    );
  }

  return (
    <div className="orders-table">
      <table>
        <thead>
          <tr>
            <th>Type</th>
            <th>Side</th>
            <th>Price</th>
            <th>Size</th>
            <th>Filled</th>
            <th>Status</th>
            <th>Created</th>
            <th>Actions</th>
          </tr>
        </thead>
        <tbody>
          {orders.map((order) => (
            <tr key={order.address}>
              <td className="order-type">{order.orderType.toUpperCase()}</td>
              <td className={order.side === 'bid' ? 'long' : 'short'}>
                {order.side === 'bid' ? 'LONG' : 'SHORT'}
              </td>
              <td>${order.price.toFixed(2)}</td>
              <td>{order.size.toFixed(4)}</td>
              <td>
                {order.filledSize.toFixed(4)}
                <span className="fill-percent">
                  ({((order.filledSize / order.size) * 100).toFixed(1)}%)
                </span>
              </td>
              <td className={`status ${order.status}`}>{order.status}</td>
              <td>{new Date(order.createdAt * 1000).toLocaleString()}</td>
              <td>
                <button
                  className="cancel-btn"
                  onClick={() => handleCancelOrder(order)}
                  disabled={order.status === 'filled' || order.status === 'cancelled'}
                >
                  Cancel
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

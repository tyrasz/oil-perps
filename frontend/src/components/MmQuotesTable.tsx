import { useState } from 'react';
import { useMarketMaker } from '../hooks/useMarketMaker';
import type { QuoteData } from '../hooks/useMarketMaker';

export function MmQuotesTable() {
  const { quotes, cancelQuote, updateQuote, isLoading } = useMarketMaker();
  const [editingQuote, setEditingQuote] = useState<string | null>(null);
  const [editForm, setEditForm] = useState<{
    bidPrice: string;
    bidSize: string;
    askPrice: string;
    askSize: string;
  }>({
    bidPrice: '',
    bidSize: '',
    askPrice: '',
    askSize: '',
  });

  const handleEdit = (quote: QuoteData) => {
    setEditingQuote(quote.id);
    setEditForm({
      bidPrice: quote.bidPrice.toString(),
      bidSize: quote.bidSize.toString(),
      askPrice: quote.askPrice.toString(),
      askSize: quote.askSize.toString(),
    });
  };

  const handleCancelEdit = () => {
    setEditingQuote(null);
    setEditForm({ bidPrice: '', bidSize: '', askPrice: '', askSize: '' });
  };

  const handleSaveEdit = async (quoteId: string) => {
    try {
      await updateQuote({
        quoteId,
        bidPrice: parseFloat(editForm.bidPrice),
        bidSize: parseFloat(editForm.bidSize),
        askPrice: parseFloat(editForm.askPrice),
        askSize: parseFloat(editForm.askSize),
      });
      setEditingQuote(null);
    } catch (err) {
      console.error('Failed to update quote:', err);
    }
  };

  const handleCancel = async (quoteId: string) => {
    if (!confirm('Are you sure you want to cancel this quote?')) return;

    try {
      await cancelQuote(quoteId);
    } catch (err) {
      console.error('Failed to cancel quote:', err);
    }
  };

  const formatTimeRemaining = (expiresAt: number) => {
    const remaining = expiresAt - Date.now() / 1000;
    if (remaining <= 0) return 'Expired';

    const hours = Math.floor(remaining / 3600);
    const minutes = Math.floor((remaining % 3600) / 60);

    if (hours > 0) return `${hours}h ${minutes}m`;
    return `${minutes}m`;
  };

  const calculateSpread = (bidPrice: number, askPrice: number) => {
    return ((askPrice - bidPrice) / bidPrice) * 10000;
  };

  const calculateFillPercent = (remaining: number, total: number) => {
    if (total === 0) return 0;
    return ((total - remaining) / total) * 100;
  };

  if (quotes.length === 0) {
    return (
      <div className="quotes-table empty">
        <p>No active quotes</p>
        <p className="hint">Post a quote to start market making</p>
      </div>
    );
  }

  return (
    <div className="quotes-table">
      <table>
        <thead>
          <tr>
            <th>Bid</th>
            <th>Ask</th>
            <th>Spread</th>
            <th>Filled</th>
            <th>Collateral</th>
            <th>Expires</th>
            <th>Actions</th>
          </tr>
        </thead>
        <tbody>
          {quotes.map((quote) => (
            <tr key={quote.id} className={editingQuote === quote.id ? 'editing' : ''}>
              {editingQuote === quote.id ? (
                <>
                  <td>
                    <div className="edit-cell">
                      <input
                        type="number"
                        value={editForm.bidPrice}
                        onChange={(e) => setEditForm({ ...editForm, bidPrice: e.target.value })}
                        step="0.01"
                        className="edit-input"
                      />
                      <input
                        type="number"
                        value={editForm.bidSize}
                        onChange={(e) => setEditForm({ ...editForm, bidSize: e.target.value })}
                        className="edit-input size"
                      />
                    </div>
                  </td>
                  <td>
                    <div className="edit-cell">
                      <input
                        type="number"
                        value={editForm.askPrice}
                        onChange={(e) => setEditForm({ ...editForm, askPrice: e.target.value })}
                        step="0.01"
                        className="edit-input"
                      />
                      <input
                        type="number"
                        value={editForm.askSize}
                        onChange={(e) => setEditForm({ ...editForm, askSize: e.target.value })}
                        className="edit-input size"
                      />
                    </div>
                  </td>
                  <td>
                    {calculateSpread(parseFloat(editForm.bidPrice) || 0, parseFloat(editForm.askPrice) || 0).toFixed(0)} bps
                  </td>
                  <td>-</td>
                  <td>-</td>
                  <td>-</td>
                  <td className="actions">
                    <button
                      className="save-btn"
                      onClick={() => handleSaveEdit(quote.id)}
                      disabled={isLoading}
                    >
                      Save
                    </button>
                    <button
                      className="cancel-edit-btn"
                      onClick={handleCancelEdit}
                    >
                      Cancel
                    </button>
                  </td>
                </>
              ) : (
                <>
                  <td className="bid-cell">
                    <div className="price-size">
                      <span className="price">${quote.bidPrice.toFixed(2)}</span>
                      <span className="size">{quote.bidRemaining}/{quote.bidSize}</span>
                    </div>
                    <div className="fill-bar">
                      <div
                        className="fill-progress bid"
                        style={{ width: `${calculateFillPercent(quote.bidRemaining, quote.bidSize)}%` }}
                      />
                    </div>
                  </td>
                  <td className="ask-cell">
                    <div className="price-size">
                      <span className="price">${quote.askPrice.toFixed(2)}</span>
                      <span className="size">{quote.askRemaining}/{quote.askSize}</span>
                    </div>
                    <div className="fill-bar">
                      <div
                        className="fill-progress ask"
                        style={{ width: `${calculateFillPercent(quote.askRemaining, quote.askSize)}%` }}
                      />
                    </div>
                  </td>
                  <td className="spread-cell">
                    {calculateSpread(quote.bidPrice, quote.askPrice).toFixed(0)} bps
                  </td>
                  <td className="filled-cell">
                    <div className="filled-info">
                      <span className="bid-filled">
                        B: {((quote.bidSize - quote.bidRemaining) || 0).toFixed(0)}
                      </span>
                      <span className="ask-filled">
                        A: {((quote.askSize - quote.askRemaining) || 0).toFixed(0)}
                      </span>
                    </div>
                  </td>
                  <td className="collateral-cell">
                    ${quote.collateralLocked.toLocaleString()}
                  </td>
                  <td className="expires-cell">
                    <span className={quote.expiresAt - Date.now() / 1000 < 600 ? 'expiring-soon' : ''}>
                      {formatTimeRemaining(quote.expiresAt)}
                    </span>
                  </td>
                  <td className="actions">
                    <button
                      className="edit-btn"
                      onClick={() => handleEdit(quote)}
                      disabled={isLoading}
                    >
                      Edit
                    </button>
                    <button
                      className="cancel-btn"
                      onClick={() => handleCancel(quote.id)}
                      disabled={isLoading}
                    >
                      Cancel
                    </button>
                  </td>
                </>
              )}
            </tr>
          ))}
        </tbody>
      </table>

      <div className="quotes-summary">
        <span>Total Quotes: {quotes.length}</span>
        <span>Total Collateral Locked: ${quotes.reduce((sum, q) => sum + q.collateralLocked, 0).toLocaleString()}</span>
      </div>
    </div>
  );
}

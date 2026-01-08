import { useState, useRef, useEffect } from 'react';
import { useMarketStore, COMMODITIES } from '../stores/marketStore';
import type { CommodityConfig } from '../config/commodities';

interface CommoditySelectorProps {
  showPrices?: boolean;
}

export function CommoditySelector({ showPrices = true }: CommoditySelectorProps) {
  const { selectedCommodity, setSelectedCommodity, markets } = useMarketStore();
  const [isOpen, setIsOpen] = useState(false);
  const dropdownRef = useRef<HTMLDivElement>(null);

  // Close dropdown when clicking outside
  useEffect(() => {
    const handleClickOutside = (event: MouseEvent) => {
      if (dropdownRef.current && !dropdownRef.current.contains(event.target as Node)) {
        setIsOpen(false);
      }
    };

    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, []);

  const handleSelect = (commodity: CommodityConfig) => {
    setSelectedCommodity(commodity);
    setIsOpen(false);
  };

  const getMarketData = (commodityId: string) => {
    return markets[commodityId];
  };

  const formatPrice = (price: number, decimals: number) => {
    return price.toFixed(decimals);
  };

  const formatChange = (change: number) => {
    const prefix = change >= 0 ? '+' : '';
    return `${prefix}${change.toFixed(2)}%`;
  };

  return (
    <div className="commodity-selector" ref={dropdownRef}>
      <button
        className="selector-button"
        onClick={() => setIsOpen(!isOpen)}
        aria-expanded={isOpen}
        aria-haspopup="listbox"
      >
        <span className="selected-commodity">
          <span className="commodity-icon">{selectedCommodity.icon}</span>
          <span className="commodity-symbol">{selectedCommodity.symbol}</span>
        </span>
        <span className={`dropdown-arrow ${isOpen ? 'open' : ''}`}>
          ▼
        </span>
      </button>

      {isOpen && (
        <div className="commodity-dropdown" role="listbox">
          {COMMODITIES.map((commodity) => {
            const market = getMarketData(commodity.id);
            const isSelected = commodity.id === selectedCommodity.id;

            return (
              <button
                key={commodity.id}
                className={`commodity-option ${isSelected ? 'selected' : ''}`}
                onClick={() => handleSelect(commodity)}
                role="option"
                aria-selected={isSelected}
              >
                <div className="option-left">
                  <span className="commodity-icon">{commodity.icon}</span>
                  <div className="commodity-info">
                    <span className="commodity-symbol">{commodity.symbol}</span>
                    <span className="commodity-name">{commodity.name}</span>
                  </div>
                </div>
                {showPrices && market && (
                  <div className="option-right">
                    <span className="commodity-price">
                      ${formatPrice(market.price, commodity.decimals)}
                    </span>
                    <span className={`commodity-change ${market.priceChange24h >= 0 ? 'positive' : 'negative'}`}>
                      {formatChange(market.priceChange24h)}
                    </span>
                  </div>
                )}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}

// Compact version for header/stats bar
export function CommoditySelectorCompact() {
  return <CommoditySelector showPrices={false} />;
}

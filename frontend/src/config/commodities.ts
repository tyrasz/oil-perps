// Commodity configuration for multi-commodity trading support
// Each commodity has a unique identifier, display properties, and market parameters

export interface CommodityConfig {
  id: string;                    // Unique identifier (e.g., 'OIL', 'GOLD', 'SILVER')
  symbol: string;                // Trading symbol (e.g., 'OIL-PERP')
  name: string;                  // Full name (e.g., 'Crude Oil')
  icon: string;                  // Emoji or icon identifier
  pythPriceFeed: string;         // Pyth Network price feed address
  basePrice: number;             // Reference base price for mock data
  decimals: number;              // Price decimals
  contractUnit: string;          // Unit per contract (e.g., 'barrel', 'oz')
  minTradeSize: number;          // Minimum trade size
  maxLeverage: number;           // Maximum allowed leverage
  tickSize: number;              // Minimum price increment
  color: string;                 // Brand color for UI
}

// Pyth Price Feed IDs (Devnet)
// https://pyth.network/developers/price-feed-ids
const PYTH_FEEDS = {
  OIL: 'GVXRSBjFk6e6J3NbVPXohDJetcTjaeeuykUpbQF8UoMU',      // WTI Crude Oil
  GOLD: 'sXgHcPCNsXM8KaC3CXNXQS8qprLR4dVxQyJyxNmBsLR',     // Gold
  SILVER: '77JipqJaP9LPFyEGjT2zqz5qxL6KBx3nVtbMd1PPdPr9',  // Silver
  NATGAS: 'DBE3N8uNjhKPNAR4oJT8vKwZQN5yDsRXGBCQu4k3Gfgr',  // Natural Gas
  COPPER: '4wxQsP2B7HNyH4sH3n2J1oKeW6vPExU6mBLgPswN8pqZ',  // Copper
};

export const COMMODITIES: CommodityConfig[] = [
  {
    id: 'OIL',
    symbol: 'OIL-PERP',
    name: 'Crude Oil',
    icon: '⛽',
    pythPriceFeed: PYTH_FEEDS.OIL,
    basePrice: 75,
    decimals: 2,
    contractUnit: 'barrel',
    minTradeSize: 0.01,
    maxLeverage: 20,
    tickSize: 0.01,
    color: '#f59e0b',
  },
  {
    id: 'GOLD',
    symbol: 'GOLD-PERP',
    name: 'Gold',
    icon: '🥇',
    pythPriceFeed: PYTH_FEEDS.GOLD,
    basePrice: 2000,
    decimals: 2,
    contractUnit: 'oz',
    minTradeSize: 0.001,
    maxLeverage: 20,
    tickSize: 0.10,
    color: '#fbbf24',
  },
  {
    id: 'SILVER',
    symbol: 'SILVER-PERP',
    name: 'Silver',
    icon: '🥈',
    pythPriceFeed: PYTH_FEEDS.SILVER,
    basePrice: 24,
    decimals: 3,
    contractUnit: 'oz',
    minTradeSize: 0.01,
    maxLeverage: 20,
    tickSize: 0.005,
    color: '#94a3b8',
  },
  {
    id: 'NATGAS',
    symbol: 'NATGAS-PERP',
    name: 'Natural Gas',
    icon: '🔥',
    pythPriceFeed: PYTH_FEEDS.NATGAS,
    basePrice: 2.5,
    decimals: 3,
    contractUnit: 'MMBtu',
    minTradeSize: 0.1,
    maxLeverage: 15,
    tickSize: 0.001,
    color: '#3b82f6',
  },
  {
    id: 'COPPER',
    symbol: 'COPPER-PERP',
    name: 'Copper',
    icon: '🔶',
    pythPriceFeed: PYTH_FEEDS.COPPER,
    basePrice: 4.2,
    decimals: 4,
    contractUnit: 'lb',
    minTradeSize: 0.1,
    maxLeverage: 15,
    tickSize: 0.0001,
    color: '#f97316',
  },
];

// Get commodity by ID
export function getCommodityById(id: string): CommodityConfig | undefined {
  return COMMODITIES.find(c => c.id === id);
}

// Get commodity by symbol
export function getCommodityBySymbol(symbol: string): CommodityConfig | undefined {
  return COMMODITIES.find(c => c.symbol === symbol);
}

// Default commodity (OIL for backward compatibility)
export const DEFAULT_COMMODITY = COMMODITIES[0];

// Get all commodity IDs
export const COMMODITY_IDS = COMMODITIES.map(c => c.id);

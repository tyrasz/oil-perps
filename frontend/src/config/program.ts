import { PublicKey } from '@solana/web3.js';

// Perps Core Program ID
export const PERPS_CORE_PROGRAM_ID = new PublicKey(
  'Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS'
);

// USDC Mint addresses
export const USDC_MINT_TESTNET = new PublicKey(
  '4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU' // Testnet USDC
);
export const USDC_MINT_DEVNET = new PublicKey(
  '4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU' // Devnet USDC (same as testnet for SPL)
);
export const USDC_MINT_MAINNET = new PublicKey(
  'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v'
);

// Use testnet by default. VITE_NETWORK=localnet is the live local simulation
// (docs/localnet.md): scripts/localnet/frontend-env.sh writes the mint, feed
// and RPC of the running validator into VITE_USDC_MINT, VITE_PYTH_FEED_OIL
// and VITE_RPC_URL.
export const NETWORK = import.meta.env.VITE_NETWORK || 'testnet';
const network = NETWORK;
export const USDC_MINT = import.meta.env.VITE_USDC_MINT
  ? new PublicKey(import.meta.env.VITE_USDC_MINT)
  : network === 'mainnet' ? USDC_MINT_MAINNET : USDC_MINT_TESTNET;

// Solana RPC endpoints
export const RPC_ENDPOINTS = {
  testnet: 'https://api.testnet.solana.com',
  devnet: 'https://api.devnet.solana.com',
  mainnet: 'https://api.mainnet-beta.solana.com',
  localnet: 'http://127.0.0.1:8899',
};

/**
 * Where market prices come from: 'api' (backend/api, the default) or 'chain'
 * (the market's oracle account read over RPC, the default on localnet, where
 * the TEST ONLY mock-pyth feed is the oracle).
 */
export const ORACLE_SOURCE: 'api' | 'chain' =
  (import.meta.env.VITE_ORACLE_SOURCE as 'api' | 'chain' | undefined) ?? (network === 'localnet' ? 'chain' : 'api');

export const RPC_ENDPOINT = import.meta.env.VITE_RPC_URL || RPC_ENDPOINTS[network as keyof typeof RPC_ENDPOINTS] || RPC_ENDPOINTS.testnet;

// Pyth Price Feed IDs (Testnet/Devnet - these are the same on both)
// VITE_PYTH_FEED_<ID> overrides one (localnet: the mock-pyth feed)
const feedOverride = (id: string, fallback: string) =>
  new PublicKey((import.meta.env as Record<string, string | undefined>)[`VITE_PYTH_FEED_${id}`] || fallback);
export const PYTH_PRICE_FEEDS: Record<string, PublicKey> = {
  OIL: feedOverride('OIL', 'GVXRSBjFk6e6J3NbVPXohDJetcTjaeeuykUpbQF8UoMU'),
  GOLD: feedOverride('GOLD', 'sXgHcPCNsXM8KaC3CXNXQS8qprLR4dVxQyJyxNmBsLR'),
  SILVER: feedOverride('SILVER', '77JipqJaP9LPFyEGjT2zqz5qxL6KBx3nVtbMd1PPdPr9'),
  NATGAS: feedOverride('NATGAS', 'DBE3N8uNjhKPNAR4oJT8vKwZQN5yDsRXGBCQu4k3Gfgr'),
  COPPER: feedOverride('COPPER', '4wxQsP2B7HNyH4sH3n2J1oKeW6vPExU6mBLgPswN8pqZ'),
};

// Program constants
export const PRICE_DECIMALS = 6; // All prices stored with 6 decimals
export const LEVERAGE_DECIMALS = 3; // Leverage uses 3 decimals (10x = 10000)
export const COLLATERAL_DECIMALS = 6; // USDC has 6 decimals

// House trades (docs/lp-pool.md §4.4, §4.5): the worst price a trade accepts
// is the quoted execution price moved this far against the trader. The quote
// uses the app's price feed, so this also absorbs its gap to the oracle.
export const DEFAULT_SLIPPAGE_BPS = 50;
// TP/SL closes: bound at the trigger price, max_impact_bps plus this much
// worse (§16.5), so a trigger never closes with no bound at all.
export const TRIGGER_EXTRA_SLIPPAGE_BPS = 100;
// LP deposits: minimum shares accepted, this far below the quoted shares.
export const LP_DEPOSIT_SLIPPAGE_BPS = 50;

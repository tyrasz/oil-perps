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

// Use testnet by default
const network = import.meta.env.VITE_NETWORK || 'testnet';
export const USDC_MINT = network === 'mainnet' ? USDC_MINT_MAINNET : USDC_MINT_TESTNET;

// Solana RPC endpoints
export const RPC_ENDPOINTS = {
  testnet: 'https://api.testnet.solana.com',
  devnet: 'https://api.devnet.solana.com',
  mainnet: 'https://api.mainnet-beta.solana.com',
};

export const RPC_ENDPOINT = import.meta.env.VITE_RPC_URL || RPC_ENDPOINTS[network as keyof typeof RPC_ENDPOINTS] || RPC_ENDPOINTS.testnet;

// Pyth Price Feed IDs (Testnet/Devnet - these are the same on both)
export const PYTH_PRICE_FEEDS: Record<string, PublicKey> = {
  OIL: new PublicKey('GVXRSBjFk6e6J3NbVPXohDJetcTjaeeuykUpbQF8UoMU'),
  GOLD: new PublicKey('sXgHcPCNsXM8KaC3CXNXQS8qprLR4dVxQyJyxNmBsLR'),
  SILVER: new PublicKey('77JipqJaP9LPFyEGjT2zqz5qxL6KBx3nVtbMd1PPdPr9'),
  NATGAS: new PublicKey('DBE3N8uNjhKPNAR4oJT8vKwZQN5yDsRXGBCQu4k3Gfgr'),
  COPPER: new PublicKey('4wxQsP2B7HNyH4sH3n2J1oKeW6vPExU6mBLgPswN8pqZ'),
};

// Program constants
export const PRICE_DECIMALS = 6; // All prices stored with 6 decimals
export const LEVERAGE_DECIMALS = 3; // Leverage uses 3 decimals (10x = 10000)
export const COLLATERAL_DECIMALS = 6; // USDC has 6 decimals

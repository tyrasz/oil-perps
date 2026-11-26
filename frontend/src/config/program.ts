import { PublicKey } from '@solana/web3.js';
import {
  DEFAULT_PYTH_PROGRAM_SET,
  PYTH_FEED_IDS,
  PYTH_PROGRAM_SETS,
  feedIdFromHex,
  pushFeedSeeds,
  type PythProgramSet,
} from './pythFeeds';

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

// The price feed account each market pins (docs/lp-pool.md D47, D51): the
// Pyth push oracle's shard-0 PriceUpdateV2 account for the commodity's feed
// id, PDA([0u16 LE, feed_id], push oracle). VITE_PYTH_PROGRAMS picks the
// Pyth program pair ('pro', the default, or 'legacy'; check which one the
// cluster uses). VITE_PYTH_FEED_<ID> overrides one account (localnet: the
// mock-pyth feed). A commodity without a verified feed id and no override has
// none. Transactions and the chain price source use the market's own pinned
// account (Market.pyth_price_feed, the only key perps-core accepts), so a
// feed rotation needs no frontend change; this is the configured default
// (what scripts/initialize-market.ts creates a market with).
export const PYTH_PROGRAM_SET: PythProgramSet =
  import.meta.env.VITE_PYTH_PROGRAMS === 'legacy' ? 'legacy' : DEFAULT_PYTH_PROGRAM_SET;
export const PYTH_RECEIVER_PROGRAM_ID = new PublicKey(PYTH_PROGRAM_SETS[PYTH_PROGRAM_SET].receiver);
export const PYTH_PUSH_ORACLE_PROGRAM_ID = new PublicKey(PYTH_PROGRAM_SETS[PYTH_PROGRAM_SET].pushOracle);

/** The push oracle's shard-0 feed account for a feed id (hex). */
export function pythPushFeedAddress(feedIdHex: string, pushOracle: PublicKey = PYTH_PUSH_ORACLE_PROGRAM_ID): PublicKey {
  return PublicKey.findProgramAddressSync(pushFeedSeeds(feedIdFromHex(feedIdHex)), pushOracle)[0];
}

const feedAccounts = new Map<string, PublicKey | undefined>();
/**
 * The configured feed account of a commodity: VITE_PYTH_FEED_<ID>, else the
 * shard-0 account of its feed id; undefined without either. Derived on first
 * use (not at import: web3.js cannot derive addresses under jsdom).
 */
export function pythPriceFeed(commodityId: string): PublicKey | undefined {
  if (!feedAccounts.has(commodityId)) {
    const override = (import.meta.env as Record<string, string | undefined>)[`VITE_PYTH_FEED_${commodityId}`];
    const feedId = PYTH_FEED_IDS[commodityId];
    feedAccounts.set(commodityId, override ? new PublicKey(override) : feedId ? pythPushFeedAddress(feedId) : undefined);
  }
  return feedAccounts.get(commodityId);
}

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

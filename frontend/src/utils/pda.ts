import { PublicKey } from '@solana/web3.js';
import { PERPS_CORE_PROGRAM_ID } from '../config/program';

/**
 * Derive User Account PDA
 * Seeds: ["user", owner_pubkey]
 */
export function getUserAccountPDA(owner: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from('user'), owner.toBuffer()],
    PERPS_CORE_PROGRAM_ID
  );
}

/**
 * Derive Market PDA
 * Seeds: ["market", collateral_mint_pubkey]
 */
export function getMarketPDA(collateralMint: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from('market'), collateralMint.toBuffer()],
    PERPS_CORE_PROGRAM_ID
  );
}

/**
 * Derive Vault PDA
 * Seeds: ["vault", market_pubkey]
 */
export function getVaultPDA(market: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from('vault'), market.toBuffer()],
    PERPS_CORE_PROGRAM_ID
  );
}

/**
 * Derive Vault Token Account PDA
 * Seeds: ["vault_token", market_pubkey]
 */
export function getVaultTokenAccountPDA(market: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from('vault_token'), market.toBuffer()],
    PERPS_CORE_PROGRAM_ID
  );
}

/**
 * Derive Position PDA
 * Seeds: ["position", owner_pubkey, market_pubkey, position_index_as_le_bytes]
 */
export function getPositionPDA(
  owner: PublicKey,
  market: PublicKey,
  positionIndex: number
): [PublicKey, number] {
  const indexBuffer = Buffer.alloc(8);
  indexBuffer.writeBigUInt64LE(BigInt(positionIndex));

  return PublicKey.findProgramAddressSync(
    [
      Buffer.from('position'),
      owner.toBuffer(),
      market.toBuffer(),
      indexBuffer,
    ],
    PERPS_CORE_PROGRAM_ID
  );
}

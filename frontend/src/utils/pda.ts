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

/**
 * Derive Referral Code PDA
 * Seeds: ["referral_code", code_bytes]
 */
export function getReferralCodePDA(code: string): [PublicKey, number] {
  // Convert code to 8-byte uppercase buffer
  const codeBytes = Buffer.alloc(8);
  const upperCode = code.toUpperCase();
  const srcBytes = Buffer.from(upperCode);
  srcBytes.copy(codeBytes, 0, 0, Math.min(8, srcBytes.length));

  return PublicKey.findProgramAddressSync(
    [Buffer.from('referral_code'), codeBytes],
    PERPS_CORE_PROGRAM_ID
  );
}

/**
 * Derive User Referral PDA
 * Seeds: ["user_referral", user_pubkey]
 */
export function getUserReferralPDA(user: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from('user_referral'), user.toBuffer()],
    PERPS_CORE_PROGRAM_ID
  );
}

/**
 * Convert string to 8-byte code array for on-chain
 */
export function codeToBytes(code: string): number[] {
  const bytes = new Array(8).fill(0);
  const upperCode = code.toUpperCase();
  for (let i = 0; i < Math.min(8, upperCode.length); i++) {
    bytes[i] = upperCode.charCodeAt(i);
  }
  return bytes;
}

/**
 * Convert 8-byte array to string
 */
export function bytesToCode(bytes: number[]): string {
  let result = '';
  for (const byte of bytes) {
    if (byte === 0) break;
    result += String.fromCharCode(byte);
  }
  return result;
}

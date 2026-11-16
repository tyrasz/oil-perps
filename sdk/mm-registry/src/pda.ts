import { PublicKey } from "@solana/web3.js";

export const SEEDS = {
  MM_REGISTRY: Buffer.from("mm_registry"),
  MARKET_MAKER: Buffer.from("market_maker"),
  MM_COLLATERAL: Buffer.from("mm_collateral"),
  QUOTE: Buffer.from("quote"),
} as const;

/**
 * Derive the MmRegistry PDA for a given market
 */
export function findRegistryPda(
  programId: PublicKey,
  market: PublicKey
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.MM_REGISTRY, market.toBuffer()],
    programId
  );
}

/**
 * Derive the MarketMaker PDA for a given registry and owner
 */
export function findMarketMakerPda(
  programId: PublicKey,
  registry: PublicKey,
  owner: PublicKey
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.MARKET_MAKER, registry.toBuffer(), owner.toBuffer()],
    programId
  );
}

/**
 * Derive the MM collateral token account PDA
 */
export function findMmCollateralPda(
  programId: PublicKey,
  marketMaker: PublicKey
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.MM_COLLATERAL, marketMaker.toBuffer()],
    programId
  );
}

/**
 * Derive a Quote PDA for a market maker
 */
export function findQuotePda(
  programId: PublicKey,
  marketMaker: PublicKey,
  quoteIndex: number
): [PublicKey, number] {
  const indexBuffer = Buffer.alloc(4);
  indexBuffer.writeUInt32LE(quoteIndex);

  return PublicKey.findProgramAddressSync(
    [SEEDS.QUOTE, marketMaker.toBuffer(), indexBuffer],
    programId
  );
}

/**
 * Get all PDAs for a market maker in one call
 */
export function getMarketMakerPdas(
  programId: PublicKey,
  registry: PublicKey,
  owner: PublicKey
): {
  marketMaker: PublicKey;
  marketMakerBump: number;
  collateralAccount: PublicKey;
  collateralBump: number;
} {
  const [marketMaker, marketMakerBump] = findMarketMakerPda(
    programId,
    registry,
    owner
  );
  const [collateralAccount, collateralBump] = findMmCollateralPda(
    programId,
    marketMaker
  );

  return {
    marketMaker,
    marketMakerBump,
    collateralAccount,
    collateralBump,
  };
}

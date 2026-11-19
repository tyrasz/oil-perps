import { PublicKey } from "@solana/web3.js";
import BN from "bn.js";

export const SEEDS = {
  MM_REGISTRY: Buffer.from("mm_registry"),
  MARKET_MAKER: Buffer.from("market_maker"),
  MM_COLLATERAL: Buffer.from("mm_collateral"),
  QUOTE: Buffer.from("quote"),
} as const;

/** perps-core, which fill_quote settles into */
export const PERPS_CORE_PROGRAM_ID = new PublicKey(
  "Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS"
);

export const BPF_LOADER_UPGRADEABLE_PROGRAM_ID = new PublicKey(
  "BPFLoaderUpgradeab1e11111111111111111111111"
);

export const PERPS_SEEDS = {
  CONFIG: Buffer.from("config"),
  USER: Buffer.from("user"),
  VENUE: Buffer.from("venue"),
  VENUE_AUTHORITY: Buffer.from("perps_venue"),
  VENUE_APPROVAL: Buffer.from("venue_approval"),
  VPOSITION: Buffer.from("vposition"),
  VMAKER: Buffer.from("vmaker"),
} as const;

/** Which side of a venue fill a VenueApproval lets the venue put its owner on */
export enum VenueRole {
  Taker = 0,
  Maker = 1,
}

function u64Le(value: number | bigint | BN): Buffer {
  const buffer = Buffer.alloc(8);
  buffer.writeBigUInt64LE(BigInt(value.toString()));
  return buffer;
}

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
 * Derive a Quote PDA for a market maker. The index is the market maker's
 * `quotesPosted` counter (u64, little-endian) at the time the quote is posted.
 */
export function findQuotePda(
  programId: PublicKey,
  marketMaker: PublicKey,
  quoteIndex: number | bigint | BN
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [SEEDS.QUOTE, marketMaker.toBuffer(), u64Le(quoteIndex)],
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

// ============================================================================
// perps-core PDAs used by fill_quote
// ============================================================================

export function findPerpsConfigPda(
  perpsProgramId: PublicKey = PERPS_CORE_PROGRAM_ID
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([PERPS_SEEDS.CONFIG], perpsProgramId);
}

/** A wallet's perps-core UserAccount in `market`, which every fill margins from */
export function findPerpsUserPda(
  owner: PublicKey,
  market: PublicKey,
  perpsProgramId: PublicKey = PERPS_CORE_PROGRAM_ID
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [PERPS_SEEDS.USER, owner.toBuffer(), market.toBuffer()],
    perpsProgramId
  );
}

/** perps-core's Venue record for `venueProgram` (here mm-registry) in `market` */
export function findVenuePda(
  market: PublicKey,
  venueProgram: PublicKey,
  perpsProgramId: PublicKey = PERPS_CORE_PROGRAM_ID
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [PERPS_SEEDS.VENUE, market.toBuffer(), venueProgram.toBuffer()],
    perpsProgramId
  );
}

/** The PDA the venue program signs venue_fill with: `[b"perps_venue", market]` under the venue program */
export function findVenueAuthorityPda(
  venueProgram: PublicKey,
  market: PublicKey
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [PERPS_SEEDS.VENUE_AUTHORITY, market.toBuffer()],
    venueProgram
  );
}

/** The upgradeable loader's ProgramData for `program`; perps-core pins its slot */
export function findProgramDataPda(program: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [program.toBuffer()],
    BPF_LOADER_UPGRADEABLE_PROGRAM_ID
  );
}

/** A wallet's consent for `venue` to open positions for it in `role` */
export function findVenueApprovalPda(
  owner: PublicKey,
  venue: PublicKey,
  role: VenueRole,
  perpsProgramId: PublicKey = PERPS_CORE_PROGRAM_ID
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [PERPS_SEEDS.VENUE_APPROVAL, owner.toBuffer(), venue.toBuffer(), Buffer.from([role])],
    perpsProgramId
  );
}

/** Taker position of a venue fill, seeded by a nonce the caller picks */
export function findVPositionPda(
  taker: PublicKey,
  market: PublicKey,
  positionNonce: number | bigint | BN,
  perpsProgramId: PublicKey = PERPS_CORE_PROGRAM_ID
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [PERPS_SEEDS.VPOSITION, taker.toBuffer(), market.toBuffer(), u64Le(positionNonce)],
    perpsProgramId
  );
}

/** Maker position of the venue fill whose taker position is `takerPosition` */
export function findVMakerPda(
  takerPosition: PublicKey,
  perpsProgramId: PublicKey = PERPS_CORE_PROGRAM_ID
): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [PERPS_SEEDS.VMAKER, takerPosition.toBuffer()],
    perpsProgramId
  );
}

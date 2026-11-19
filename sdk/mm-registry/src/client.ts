import {
  ComputeBudgetProgram,
  Connection,
  PublicKey,
  TransactionInstruction,
  SystemProgram,
  SYSVAR_RENT_PUBKEY,
  SYSVAR_INSTRUCTIONS_PUBKEY,
} from "@solana/web3.js";
import {
  TOKEN_PROGRAM_ID,
  getAssociatedTokenAddressSync,
  createAssociatedTokenAccountInstruction,
} from "@solana/spl-token";
import { Program, AnchorProvider, BN } from "@coral-xyz/anchor";
import {
  MmRegistry,
  MarketMaker,
  TwoSidedQuote,
  MmRegistryConfig,
  InitializeRegistryParams,
  PostQuoteParams,
  UpdateQuoteParams,
  FillQuoteParams,
  QuoteInfo,
  MarketMakerInfo,
  MmStatus,
  VenueApprovalParams,
  VenueSides,
} from "./types";
import {
  findRegistryPda,
  findMarketMakerPda,
  findMmCollateralPda,
  findQuotePda,
  getMarketMakerPdas,
  PERPS_CORE_PROGRAM_ID,
  VenueRole,
  findPerpsConfigPda,
  findPerpsUserPda,
  findVenuePda,
  findVenueAuthorityPda,
  findProgramDataPda,
  findVenueApprovalPda,
  findVPositionPda,
  findVMakerPda,
} from "./pda";
import {
  FILL_COMPUTE_UNITS,
  ONE_SHOT_APPROVAL_SECS,
  randomPositionNonce,
  worstCaseVenueMargin,
  isFillableQuote,
} from "./utils";

// Default program ID - replace with actual deployed ID
export const MM_REGISTRY_PROGRAM_ID = new PublicKey(
  "MMReg11111111111111111111111111111111111111"
);

// perps-core instructions are encoded here rather than through its IDL, so
// the SDK only needs mm-registry's. Anchor discriminators: sha256("global:<name>")[..8],
// also pinned by `sdk_layout_is_pinned`.
const APPROVE_VENUE_DISCRIMINATOR = Buffer.from([254, 60, 62, 30, 179, 64, 176, 184]);
const UPDATE_VENUE_APPROVAL_DISCRIMINATOR = Buffer.from([243, 224, 3, 21, 150, 246, 103, 7]);

// Byte offsets in perps-core accounts (8-byte discriminator included).
// Pinned by perps-core's `sdk_layout_is_pinned` test: change both together.
const MARKET_PYTH_PRICE_FEED = 104;
const MARKET_MAX_LEVERAGE = 144;
const MARKET_INITIAL_MARGIN_RATIO = 152;
const MARKET_WEEKEND_INITIAL_MARGIN_RATIO = 247;
const VENUE_MAX_PRICE_DEVIATION_BPS = 160;
const VENUE_MAX_ORACLE_AGE_SECS = 162;
const VENUE_MIN_SIGNERS = 164;
const VENUE_ENABLED = 165;

/** The perps-core Market fields a fill needs */
export interface PerpsMarketInfo {
  pythPriceFeed: PublicKey;
  maxLeverage: number;
  initialMarginBps: number;
  /** 0 = same as initialMarginBps */
  weekendInitialMarginBps: number;
}

/** perps-core's Venue record for mm-registry in a market */
export interface VenueInfo {
  address: PublicKey;
  maxPriceDeviationBps: number;
  maxOracleAgeSecs: number;
  minSigners: number;
  enabled: boolean;
}

export interface TakerFillOptions {
  size: BN;
  isBuy: boolean;
  limitPrice: BN;
  takerLeverage: number;
  allowPartial?: boolean;
  /** Defaults to a random u64 */
  positionNonce?: BN;
  /**
   * Initial margin in force at fill time. Defaults to the larger of the
   * market's weekday and weekend values, which over-approves slightly on
   * weekdays rather than failing in the pre-close window.
   */
  initialMarginBps?: number;
  /** Unix seconds; defaults to the local clock */
  now?: number;
  computeUnits?: number;
}

export interface TakerFill {
  instructions: TransactionInstruction[];
  positionNonce: BN;
  takerPosition: PublicKey;
  makerPosition: PublicKey;
}

function encodeVenueApprovalParams(params: VenueApprovalParams): Buffer {
  const data = Buffer.alloc(8 + 4 + 8 + 8 + 2 + 8 + 1);
  let o = 0;
  data.set(params.allowance.toArrayLike(Buffer, "le", 8), o); o += 8;
  data.writeUInt32LE(params.maxLeverage, o); o += 4;
  data.set(params.maxBuyPrice.toArrayLike(Buffer, "le", 8), o); o += 8;
  data.set(params.minSellPrice.toArrayLike(Buffer, "le", 8), o); o += 8;
  data.writeUInt16LE(params.maxPriceDeviationBps, o); o += 2;
  data.set(params.expiresAt.toTwos(64).toArrayLike(Buffer, "le", 8), o); o += 8;
  data.writeUInt8(params.allowedSides, o);
  return data;
}

export class MmRegistryClient {
  readonly connection: Connection;
  readonly programId: PublicKey;
  private program: Program | null = null;

  constructor(
    connection: Connection,
    programId: PublicKey = MM_REGISTRY_PROGRAM_ID
  ) {
    this.connection = connection;
    this.programId = programId;
  }

  /**
   * Set the Anchor program instance (required for instruction building)
   */
  setProgram(program: Program): void {
    this.program = program;
  }

  // ============================================================================
  // PDA Helpers
  // ============================================================================

  findRegistryPda(market: PublicKey): [PublicKey, number] {
    return findRegistryPda(this.programId, market);
  }

  findMarketMakerPda(registry: PublicKey, owner: PublicKey): [PublicKey, number] {
    return findMarketMakerPda(this.programId, registry, owner);
  }

  findMmCollateralPda(marketMaker: PublicKey): [PublicKey, number] {
    return findMmCollateralPda(this.programId, marketMaker);
  }

  findQuotePda(marketMaker: PublicKey, quoteIndex: number | bigint | BN): [PublicKey, number] {
    return findQuotePda(this.programId, marketMaker, quoteIndex);
  }

  // ============================================================================
  // Account Fetchers
  // ============================================================================

  async fetchRegistry(registryPubkey: PublicKey): Promise<MmRegistry | null> {
    if (!this.program) throw new Error("Program not set");
    try {
      const account = await this.program.account.mmRegistry.fetch(registryPubkey);
      return account as unknown as MmRegistry;
    } catch {
      return null;
    }
  }

  async fetchMarketMaker(
    marketMakerPubkey: PublicKey
  ): Promise<MarketMaker | null> {
    if (!this.program) throw new Error("Program not set");
    try {
      const account = await this.program.account.marketMaker.fetch(
        marketMakerPubkey
      );
      return account as unknown as MarketMaker;
    } catch {
      return null;
    }
  }

  async fetchQuote(quotePubkey: PublicKey): Promise<TwoSidedQuote | null> {
    if (!this.program) throw new Error("Program not set");
    try {
      const account = await this.program.account.twoSidedQuote.fetch(quotePubkey);
      return account as unknown as TwoSidedQuote;
    } catch {
      return null;
    }
  }

  async fetchRegistryByMarket(market: PublicKey): Promise<MmRegistry | null> {
    const [registryPda] = this.findRegistryPda(market);
    return this.fetchRegistry(registryPda);
  }

  async fetchMarketMakerByOwner(
    registry: PublicKey,
    owner: PublicKey
  ): Promise<MarketMaker | null> {
    const [mmPda] = this.findMarketMakerPda(registry, owner);
    return this.fetchMarketMaker(mmPda);
  }

  // ============================================================================
  // Bulk Fetchers
  // ============================================================================

  async fetchAllMarketMakers(registry: PublicKey): Promise<MarketMakerInfo[]> {
    if (!this.program) throw new Error("Program not set");

    const accounts = await this.program.account.marketMaker.all([
      {
        memcmp: {
          offset: 8 + 32, // After discriminator + owner
          bytes: registry.toBase58(),
        },
      },
    ]);

    return accounts.map((acc) => {
      const mm = acc.account as unknown as MarketMaker;
      const [collateralAccount] = this.findMmCollateralPda(acc.publicKey);

      return {
        marketMaker: mm,
        publicKey: acc.publicKey,
        collateralAccount,
        availableCollateral: mm.collateralAvailable,
        utilizationRate:
          mm.collateralDeposited.isZero()
            ? 0
            : mm.collateralLocked.toNumber() / mm.collateralDeposited.toNumber(),
      };
    });
  }

  /** The cluster's current unix time (Clock sysvar via the latest slot). */
  async chainTime(): Promise<number> {
    const slot = await this.connection.getSlot();
    const t = await this.connection.getBlockTime(slot);
    return t ?? Math.floor(Date.now() / 1000);
  }

  /** Fillable quotes only (see isFillableQuote); `now` defaults to chain time. */
  async fetchActiveQuotes(registry: PublicKey, now?: number): Promise<QuoteInfo[]> {
    // Chain time, not the local clock: fill_quote judges expiry on-chain
    if (now === undefined) now = await this.chainTime();
    if (!this.program) throw new Error("Program not set");

    const accounts = await this.program.account.twoSidedQuote.all([
      {
        memcmp: {
          offset: 8 + 32, // After discriminator + marketMaker
          bytes: registry.toBase58(),
        },
      },
    ]);

    const quoteInfos: QuoteInfo[] = [];

    for (const acc of accounts) {
      const quote = acc.account as unknown as TwoSidedQuote;
      // Only quotes fill_quote would accept: live, unexpired, migrated
      if (!isFillableQuote(quote, now)) continue;

      const mm = await this.fetchMarketMaker(quote.marketMaker);
      if (!mm) continue;

      const spread =
        quote.bidPrice.isZero()
          ? 0
          : ((quote.askPrice.toNumber() - quote.bidPrice.toNumber()) /
              quote.bidPrice.toNumber()) *
            10000;

      const midPrice = quote.bidPrice.add(quote.askPrice).divn(2);

      quoteInfos.push({
        quote,
        publicKey: acc.publicKey,
        marketMaker: mm,
        spread,
        midPrice,
      });
    }

    return quoteInfos;
  }

  async fetchMarketMakerQuotes(marketMaker: PublicKey): Promise<TwoSidedQuote[]> {
    if (!this.program) throw new Error("Program not set");

    const accounts = await this.program.account.twoSidedQuote.all([
      {
        memcmp: {
          offset: 8,
          bytes: marketMaker.toBase58(),
        },
      },
    ]);

    return accounts
      .map((acc) => acc.account as unknown as TwoSidedQuote)
      .filter((q) => q.isActive);
  }

  // ============================================================================
  // Instruction Builders
  // ============================================================================

  async buildInitializeRegistryIx(
    authority: PublicKey,
    market: PublicKey,
    collateralMint: PublicKey,
    params: InitializeRegistryParams,
    perpsCoreProgramId: PublicKey
  ): Promise<TransactionInstruction> {
    if (!this.program) throw new Error("Program not set");

    const [registry] = this.findRegistryPda(market);
    // Only the perps-core admin (its ProtocolConfig.admin) may create registries
    const [config] = PublicKey.findProgramAddressSync(
      [Buffer.from("config")],
      perpsCoreProgramId
    );

    return await this.program.methods
      .initializeRegistry(params)
      .accounts({
        authority,
        config,
        registry,
        market,
        collateralMint,
        systemProgram: SystemProgram.programId,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .instruction();
  }

  async buildRegisterMmIx(
    owner: PublicKey,
    registry: PublicKey,
    collateralMint: PublicKey,
    initialCollateral: BN
  ): Promise<TransactionInstruction[]> {
    if (!this.program) throw new Error("Program not set");

    const pdas = getMarketMakerPdas(this.programId, registry, owner);
    const ownerTokenAccount = getAssociatedTokenAddressSync(collateralMint, owner);

    const instructions: TransactionInstruction[] = [];

    // Check if ATA exists, if not add create instruction
    const ataInfo = await this.connection.getAccountInfo(ownerTokenAccount);
    if (!ataInfo) {
      instructions.push(
        createAssociatedTokenAccountInstruction(
          owner,
          ownerTokenAccount,
          owner,
          collateralMint
        )
      );
    }

    instructions.push(
      await this.program.methods
        .registerMm(initialCollateral)
        .accounts({
          owner,
          registry,
          marketMaker: pdas.marketMaker,
          mmCollateralAccount: pdas.collateralAccount,
          ownerTokenAccount,
          collateralMint,
          tokenProgram: TOKEN_PROGRAM_ID,
          systemProgram: SystemProgram.programId,
          rent: SYSVAR_RENT_PUBKEY,
        })
        .instruction()
    );

    return instructions;
  }

  async buildDepositCollateralIx(
    owner: PublicKey,
    registry: PublicKey,
    collateralMint: PublicKey,
    amount: BN
  ): Promise<TransactionInstruction> {
    if (!this.program) throw new Error("Program not set");

    const pdas = getMarketMakerPdas(this.programId, registry, owner);
    const ownerTokenAccount = getAssociatedTokenAddressSync(collateralMint, owner);

    return await this.program.methods
      .depositCollateral(amount)
      .accounts({
        owner,
        registry,
        marketMaker: pdas.marketMaker,
        mmCollateralAccount: pdas.collateralAccount,
        ownerTokenAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .instruction();
  }

  async buildWithdrawCollateralIx(
    owner: PublicKey,
    registry: PublicKey,
    collateralMint: PublicKey,
    amount: BN
  ): Promise<TransactionInstruction> {
    if (!this.program) throw new Error("Program not set");

    const pdas = getMarketMakerPdas(this.programId, registry, owner);
    const ownerTokenAccount = getAssociatedTokenAddressSync(collateralMint, owner);

    return await this.program.methods
      .withdrawCollateral(amount)
      .accounts({
        owner,
        registry,
        marketMaker: pdas.marketMaker,
        mmCollateralAccount: pdas.collateralAccount,
        ownerTokenAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .instruction();
  }

  /**
   * Post a two-sided quote. The quote PDA is seeded by the market maker's
   * `quotesPosted` counter, which is fetched unless `quoteIndex` is given.
   */
  async buildPostQuoteIx(
    owner: PublicKey,
    registry: PublicKey,
    params: PostQuoteParams,
    quoteIndex?: BN
  ): Promise<TransactionInstruction> {
    if (!this.program) throw new Error("Program not set");

    const [marketMaker] = this.findMarketMakerPda(registry, owner);
    let index = quoteIndex;
    if (index === undefined) {
      const mm = await this.fetchMarketMaker(marketMaker);
      if (!mm) throw new Error("Market maker not registered");
      index = mm.quotesPosted;
    }
    const [quote] = this.findQuotePda(marketMaker, index);

    return await this.program.methods
      .postQuote(params)
      .accounts({
        owner,
        registry,
        marketMaker,
        quote,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  async buildUpdateQuoteIx(
    owner: PublicKey,
    registry: PublicKey,
    quote: PublicKey,
    params: UpdateQuoteParams
  ): Promise<TransactionInstruction> {
    if (!this.program) throw new Error("Program not set");

    const [marketMaker] = this.findMarketMakerPda(registry, owner);

    return await this.program.methods
      .updateQuote(params)
      .accounts({
        owner,
        registry,
        marketMaker,
        quote,
      })
      .instruction();
  }

  async buildCancelQuoteIx(
    owner: PublicKey,
    registry: PublicKey,
    quote: PublicKey
  ): Promise<TransactionInstruction> {
    if (!this.program) throw new Error("Program not set");

    const [marketMaker] = this.findMarketMakerPda(registry, owner);

    return await this.program.methods
      .cancelQuote()
      .accounts({
        owner,
        registry,
        marketMaker,
        quote,
      })
      .instruction();
  }

  /**
   * fill_quote: opens the taker's perps-core position and the opposite MM
   * position. The taker signs and pays both positions' rent, and needs a
   * perps-core UserAccount with collateral and a taker-role VenueApproval for
   * the mm-registry venue (see buildTakerFillIxs, which adds the approval).
   */
  async buildFillQuoteIx(
    taker: PublicKey,
    registry: PublicKey,
    quote: PublicKey,
    params: FillQuoteParams,
    perpsProgramId: PublicKey = PERPS_CORE_PROGRAM_ID
  ): Promise<TransactionInstruction> {
    if (!this.program) throw new Error("Program not set");

    const quoteData = await this.fetchQuote(quote);
    if (!quoteData) throw new Error("Quote not found");
    // quote.marketMaker is already the MarketMaker PDA, not its owner
    const marketMaker = quoteData.marketMaker;
    const mm = await this.fetchMarketMaker(marketMaker);
    if (!mm) throw new Error("Market maker not found");
    const registryData = await this.fetchRegistry(registry);
    if (!registryData) throw new Error("Registry not found");
    const market = registryData.market;
    const marketInfo = await this.fetchPerpsMarket(market);

    const [venue] = findVenuePda(market, this.programId, perpsProgramId);
    const [takerPosition] = findVPositionPda(taker, market, params.positionNonce, perpsProgramId);

    return await this.program.methods
      .fillQuote(params)
      .accounts({
        taker,
        registry,
        marketMaker,
        quote,
        mmOwner: mm.owner,
        venueAuthority: findVenueAuthorityPda(this.programId, market)[0],
        perpsConfig: findPerpsConfigPda(perpsProgramId)[0],
        perpsVenue: venue,
        venueProgramData: findProgramDataPda(this.programId)[0],
        market,
        pythPriceFeed: marketInfo.pythPriceFeed,
        takerUser: findPerpsUserPda(taker, market, perpsProgramId)[0],
        takerApproval: findVenueApprovalPda(taker, venue, VenueRole.Taker, perpsProgramId)[0],
        takerPosition,
        mmUser: findPerpsUserPda(mm.owner, market, perpsProgramId)[0],
        mmApproval: findVenueApprovalPda(mm.owner, venue, VenueRole.Maker, perpsProgramId)[0],
        mmPosition: findVMakerPda(takerPosition, perpsProgramId)[0],
        perpsCoreProgram: perpsProgramId,
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  /**
   * perps-core approve_venue, or update_venue_approval when the approval
   * already exists (it overwrites every param, allowance included). For an
   * MM: role Maker, allowance = risk budget, e.g. maxPriceDeviationBps 30.
   * Setting allowance to 0 is the one-transaction cancel of every quote.
   */
  async buildVenueApprovalIx(
    owner: PublicKey,
    market: PublicKey,
    role: VenueRole,
    params: VenueApprovalParams,
    perpsProgramId: PublicKey = PERPS_CORE_PROGRAM_ID
  ): Promise<TransactionInstruction> {
    const [venue] = findVenuePda(market, this.programId, perpsProgramId);
    const [approval] = findVenueApprovalPda(owner, venue, role, perpsProgramId);
    const exists = (await this.connection.getAccountInfo(approval)) !== null;
    const encoded = encodeVenueApprovalParams(params);
    const data = exists
      ? Buffer.concat([UPDATE_VENUE_APPROVAL_DISCRIMINATOR, encoded])
      : Buffer.concat([APPROVE_VENUE_DISCRIMINATOR, Buffer.from([role]), encoded]);
    const keys = [
      { pubkey: owner, isSigner: true, isWritable: !exists },
      { pubkey: market, isSigner: false, isWritable: false },
      { pubkey: venue, isSigner: false, isWritable: false },
      { pubkey: approval, isSigner: false, isWritable: true },
    ];
    if (!exists) {
      keys.push({ pubkey: SystemProgram.programId, isSigner: false, isWritable: false });
    }
    // perps-core checks who is granting (top level, or one CPI deep under a
    // program other than the venue) through the instructions sysvar
    keys.push({ pubkey: SYSVAR_INSTRUCTIONS_PUBKEY, isSigner: false, isWritable: false });
    return new TransactionInstruction({ programId: perpsProgramId, keys, data });
  }

  /**
   * The taker's whole fill: compute budget, a one-shot taker approval sized
   * to the fill's worst-case margin (limit price, leverage, 120 s expiry),
   * then fill_quote. The taker must already have a perps-core UserAccount
   * with collateral; prepend initialize_user / deposit_collateral if not.
   * For a sweep, approve once for the total and add one fill_quote per quote,
   * each with its own nonce.
   */
  async buildTakerFillIxs(
    taker: PublicKey,
    registry: PublicKey,
    quote: PublicKey,
    options: TakerFillOptions,
    perpsProgramId: PublicKey = PERPS_CORE_PROGRAM_ID
  ): Promise<TakerFill> {
    const registryData = await this.fetchRegistry(registry);
    if (!registryData) throw new Error("Registry not found");
    const quoteData = await this.fetchQuote(quote);
    if (!quoteData) throw new Error("Quote not found");
    const market = registryData.market;
    const marketInfo = await this.fetchPerpsMarket(market);
    const venue = await this.fetchVenue(market, perpsProgramId);
    if (!venue.enabled) throw new Error("mm-registry venue is disabled");

    // Gate leverage on the weekday initial margin (the program enforces the
    // weekend one inside the pre-close window), but size the allowance for the
    // stricter of the two so it covers either
    const gateIm = options.initialMarginBps ?? marketInfo.initialMarginBps;
    const sizingIm =
      options.initialMarginBps ??
      Math.max(marketInfo.initialMarginBps, marketInfo.weekendInitialMarginBps);
    // A sell fills at the bid, which may be above the limit
    const price = options.isBuy ? options.limitPrice : BN.max(options.limitPrice, quoteData.bidPrice);
    const allowance = worstCaseVenueMargin(
      options.isBuy, options.size, price, options.takerLeverage, sizingIm, venue.maxPriceDeviationBps, gateIm
    );
    if (allowance === null) throw new Error("Leverage is above what the initial margin allows");
    const now = options.now ?? Math.floor(Date.now() / 1000);
    const approval = await this.buildVenueApprovalIx(taker, market, VenueRole.Taker, {
      allowance,
      maxLeverage: options.takerLeverage,
      maxBuyPrice: options.isBuy ? options.limitPrice : new BN(0),
      minSellPrice: options.isBuy ? new BN(0) : options.limitPrice,
      maxPriceDeviationBps: 0,
      expiresAt: new BN(now + ONE_SHOT_APPROVAL_SECS),
      // Only the side asked for: a buggy venue must not open the other side
      allowedSides: options.isBuy ? VenueSides.Long : VenueSides.Short,
    }, perpsProgramId);

    const positionNonce = options.positionNonce ?? randomPositionNonce();
    const fill = await this.buildFillQuoteIx(taker, registry, quote, {
      size: options.size,
      isBuy: options.isBuy,
      limitPrice: options.limitPrice,
      takerLeverage: options.takerLeverage,
      allowPartial: options.allowPartial ?? false,
      positionNonce,
    }, perpsProgramId);

    const [takerPosition] = findVPositionPda(taker, market, positionNonce, perpsProgramId);
    const [makerPosition] = findVMakerPda(takerPosition, perpsProgramId);
    return {
      instructions: [
        ComputeBudgetProgram.setComputeUnitLimit({ units: options.computeUnits ?? FILL_COMPUTE_UNITS }),
        approval,
        fill,
      ],
      positionNonce,
      takerPosition,
      makerPosition,
    };
  }

  /** Decode the perps-core Market fields fills need, without its IDL. */
  async fetchPerpsMarket(market: PublicKey): Promise<PerpsMarketInfo> {
    const info = await this.connection.getAccountInfo(market);
    if (!info) throw new Error("perps-core market not found");
    const d = info.data;
    return {
      pythPriceFeed: new PublicKey(d.subarray(MARKET_PYTH_PRICE_FEED, MARKET_PYTH_PRICE_FEED + 32)),
      maxLeverage: d.readUInt32LE(MARKET_MAX_LEVERAGE),
      initialMarginBps: d.readUInt32LE(MARKET_INITIAL_MARGIN_RATIO),
      weekendInitialMarginBps: d.readUInt32LE(MARKET_WEEKEND_INITIAL_MARGIN_RATIO),
    };
  }

  /** Decode perps-core's Venue record for this program in `market`. */
  async fetchVenue(
    market: PublicKey,
    perpsProgramId: PublicKey = PERPS_CORE_PROGRAM_ID
  ): Promise<VenueInfo> {
    const [address] = findVenuePda(market, this.programId, perpsProgramId);
    const info = await this.connection.getAccountInfo(address);
    if (!info) throw new Error("mm-registry is not a registered venue of this market");
    const d = info.data;
    return {
      address,
      maxPriceDeviationBps: d.readUInt16LE(VENUE_MAX_PRICE_DEVIATION_BPS),
      maxOracleAgeSecs: d.readUInt16LE(VENUE_MAX_ORACLE_AGE_SECS),
      minSigners: d.readUInt8(VENUE_MIN_SIGNERS),
      enabled: d.readUInt8(VENUE_ENABLED) !== 0,
    };
  }

  async buildDeregisterMmIx(
    owner: PublicKey,
    registry: PublicKey,
    collateralMint: PublicKey
  ): Promise<TransactionInstruction> {
    if (!this.program) throw new Error("Program not set");

    const pdas = getMarketMakerPdas(this.programId, registry, owner);
    const ownerTokenAccount = getAssociatedTokenAddressSync(collateralMint, owner);

    return await this.program.methods
      .deregisterMm()
      .accounts({
        owner,
        registry,
        marketMaker: pdas.marketMaker,
        mmCollateralAccount: pdas.collateralAccount,
        ownerTokenAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .instruction();
  }

  // ============================================================================
  // High-Level Operations (convenience methods)
  // ============================================================================

  async getMarketMakerStatus(
    registry: PublicKey,
    owner: PublicKey
  ): Promise<{
    isRegistered: boolean;
    status: MmStatus | null;
    collateral: BN;
    activeQuotes: number;
    pnl: BN;
  }> {
    const mm = await this.fetchMarketMakerByOwner(registry, owner);

    if (!mm) {
      return {
        isRegistered: false,
        status: null,
        collateral: new BN(0),
        activeQuotes: 0,
        pnl: new BN(0),
      };
    }

    return {
      isRegistered: true,
      status: mm.status,
      collateral: mm.collateralDeposited,
      activeQuotes: mm.activeQuotes,
      pnl: mm.realizedPnl.add(mm.unrealizedPnl),
    };
  }

  async getBestQuotes(
    registry: PublicKey,
    now?: number
  ): Promise<{ bestBid: QuoteInfo | null; bestAsk: QuoteInfo | null }> {
    const quotes = await this.fetchActiveQuotes(registry, now);

    let bestBid: QuoteInfo | null = null;
    let bestAsk: QuoteInfo | null = null;

    for (const quote of quotes) {
      // Best bid = highest bid price
      if (
        quote.quote.bidRemaining.gtn(0) &&
        (!bestBid || quote.quote.bidPrice.gt(bestBid.quote.bidPrice))
      ) {
        bestBid = quote;
      }

      // Best ask = lowest ask price
      if (
        quote.quote.askRemaining.gtn(0) &&
        (!bestAsk || quote.quote.askPrice.lt(bestAsk.quote.askPrice))
      ) {
        bestAsk = quote;
      }
    }

    return { bestBid, bestAsk };
  }

  async getAggregatedLiquidity(
    registry: PublicKey,
    levels: number = 5,
    now?: number
  ): Promise<{
    bids: Array<{ price: BN; size: BN; numQuotes: number }>;
    asks: Array<{ price: BN; size: BN; numQuotes: number }>;
  }> {
    const quotes = await this.fetchActiveQuotes(registry, now);

    // Aggregate bids by price
    const bidMap = new Map<string, { price: BN; size: BN; numQuotes: number }>();
    const askMap = new Map<string, { price: BN; size: BN; numQuotes: number }>();

    for (const q of quotes) {
      if (q.quote.bidRemaining.gtn(0)) {
        const key = q.quote.bidPrice.toString();
        const existing = bidMap.get(key);
        if (existing) {
          existing.size = existing.size.add(q.quote.bidRemaining);
          existing.numQuotes++;
        } else {
          bidMap.set(key, {
            price: q.quote.bidPrice,
            size: q.quote.bidRemaining,
            numQuotes: 1,
          });
        }
      }

      if (q.quote.askRemaining.gtn(0)) {
        const key = q.quote.askPrice.toString();
        const existing = askMap.get(key);
        if (existing) {
          existing.size = existing.size.add(q.quote.askRemaining);
          existing.numQuotes++;
        } else {
          askMap.set(key, {
            price: q.quote.askPrice,
            size: q.quote.askRemaining,
            numQuotes: 1,
          });
        }
      }
    }

    // Sort and limit
    const bids = Array.from(bidMap.values())
      .sort((a, b) => (b.price.gt(a.price) ? 1 : -1))
      .slice(0, levels);

    const asks = Array.from(askMap.values())
      .sort((a, b) => (a.price.gt(b.price) ? 1 : -1))
      .slice(0, levels);

    return { bids, asks };
  }
}

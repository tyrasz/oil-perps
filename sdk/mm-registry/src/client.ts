import {
  Connection,
  PublicKey,
  TransactionInstruction,
  SystemProgram,
  SYSVAR_RENT_PUBKEY,
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
} from "./types";
import {
  findRegistryPda,
  findMarketMakerPda,
  findMmCollateralPda,
  findQuotePda,
  getMarketMakerPdas,
} from "./pda";

// Default program ID - replace with actual deployed ID
export const MM_REGISTRY_PROGRAM_ID = new PublicKey(
  "MMReg11111111111111111111111111111111111111"
);

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

  findQuotePda(marketMaker: PublicKey, quoteIndex: number): [PublicKey, number] {
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

  async fetchActiveQuotes(registry: PublicKey): Promise<QuoteInfo[]> {
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
      if (!quote.isActive) continue;

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
    params: InitializeRegistryParams
  ): Promise<TransactionInstruction> {
    if (!this.program) throw new Error("Program not set");

    const [registry] = this.findRegistryPda(market);

    return await this.program.methods
      .initializeRegistry(params)
      .accounts({
        authority,
        market,
        collateralMint,
        registry,
        systemProgram: SystemProgram.programId,
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

  async buildPostQuoteIx(
    owner: PublicKey,
    registry: PublicKey,
    quoteIndex: number,
    params: PostQuoteParams
  ): Promise<TransactionInstruction> {
    if (!this.program) throw new Error("Program not set");

    const [marketMaker] = this.findMarketMakerPda(registry, owner);
    const [quote] = this.findQuotePda(marketMaker, quoteIndex);

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

  async buildFillQuoteIx(
    taker: PublicKey,
    registry: PublicKey,
    quote: PublicKey,
    collateralMint: PublicKey,
    params: FillQuoteParams
  ): Promise<TransactionInstruction> {
    if (!this.program) throw new Error("Program not set");

    const quoteData = await this.fetchQuote(quote);
    if (!quoteData) throw new Error("Quote not found");

    const [marketMaker] = this.findMarketMakerPda(registry, quoteData.marketMaker);
    const [mmCollateral] = this.findMmCollateralPda(marketMaker);
    const takerTokenAccount = getAssociatedTokenAddressSync(collateralMint, taker);

    return await this.program.methods
      .fillQuote(params)
      .accounts({
        taker,
        registry,
        marketMaker,
        quote,
        mmCollateralAccount: mmCollateral,
        takerTokenAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .instruction();
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
    registry: PublicKey
  ): Promise<{ bestBid: QuoteInfo | null; bestAsk: QuoteInfo | null }> {
    const quotes = await this.fetchActiveQuotes(registry);

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
    levels: number = 5
  ): Promise<{
    bids: Array<{ price: BN; size: BN; numQuotes: number }>;
    asks: Array<{ price: BN; size: BN; numQuotes: number }>;
  }> {
    const quotes = await this.fetchActiveQuotes(registry);

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

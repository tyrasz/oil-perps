import * as anchor from "@coral-xyz/anchor";
import { Program, BN } from "@coral-xyz/anchor";
import { PerpsCore } from "../target/types/perps_core";
import {
  Keypair,
  PublicKey,
  SystemProgram,
  LAMPORTS_PER_SOL,
  SYSVAR_CLOCK_PUBKEY,
  Transaction,
} from "@solana/web3.js";
import {
  createMint,
  createAssociatedTokenAccount,
  mintTo,
  getAccount,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { expect } from "chai";
import * as fs from "fs";
import {
  findLpPoolPda,
  findLpPositionPda,
  neutralPoolParams,
  productionPoolParams,
  encodePoolParams,
  decodePoolParams,
  decodeLpPool,
  decodeLpPosition,
  decodeMarketPoolFields,
  buildProposePoolParamsIx,
  buildInitializePoolIx,
  LP_POOL_LEN,
  LP_POSITION_LEN,
  LP_POOL_OFFSETS,
  PoolParams,
} from "../sdk/mm-registry/src";

// The LP pool (docs/lp-pool.md) is the house counterparty: every market gets
// initialize_pool (neutral params, §15.2) and the protocol's seed lp_deposit
// in the same transaction as initialize_market (D11, §16.6).
const LP_DEAD_SHARES = 1_000_000;
const SEED = new BN(1_000_000_000_000); // $1M of LP cash, as the Rust harness

/** Genesis Pyth feeds written by scripts/test-ts.sh (fresh ±60 s each) */
interface OracleManifest {
  price: number;
  stale: { pubkey: string; publishTime: number };
  feeds: { pubkey: string; publishTime: number }[];
}

describe("oil-perps", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.PerpsCore as Program<PerpsCore>;

  const oracles: OracleManifest | null = process.env.TEST_ORACLES
    ? JSON.parse(fs.readFileSync(process.env.TEST_ORACLES, "utf-8"))
    : null;

  // Test accounts
  let authority: Keypair;
  let user1: Keypair;
  let user2: Keypair;
  let collateralMint: PublicKey;
  let pythPriceFeed: PublicKey;
  const [configPda] = PublicKey.findProgramAddressSync([Buffer.from("config")], program.programId);

  // PDAs
  let marketPda: PublicKey;
  let vaultPda: PublicKey;
  let vaultTokenPda: PublicKey;
  let lpPoolPda: PublicKey;
  let user1AccountPda: PublicKey;
  let user2AccountPda: PublicKey;

  // Token accounts
  let authorityTokenAccount: PublicKey;
  let user1TokenAccount: PublicKey;
  let user2TokenAccount: PublicKey;

  // Market params for OIL commodity
  const marketParams = {
    commodity: "OIL",              // Commodity identifier
    maxLeverage: 20_000, // 20x
    maintenanceMarginRatio: 500, // 5%
    initialMarginRatio: 1000, // 10%
    takerFee: 5, // 0.05%
    makerFee: 2, // 0.02%
    liquidationFee: 250, // 2.5%
    maxOpenInterest: new BN(1_000_000_000_000), // 1M contracts
    fundingInterval: new BN(3600), // 1 hour
  };

  // Market params for GOLD commodity (for multi-commodity tests)
  const goldMarketParams = {
    ...marketParams,
    commodity: "GOLD",
  };

  // ---------------------------------------------------------------------------
  // Helpers
  // ---------------------------------------------------------------------------

  /** Expect `p` to fail with the Anchor error `code` (or a message containing it). */
  async function expectErr(p: Promise<unknown>, code: string): Promise<void> {
    let failed = false;
    try {
      await p;
    } catch (err: any) {
      failed = true;
      const anchorCode = err?.error?.errorCode?.code;
      if (anchorCode) {
        expect(anchorCode).to.equal(code);
      } else {
        const text = `${err}\n${(err?.logs ?? []).join("\n")}`;
        expect(text).to.include(code);
      }
    }
    if (!failed) expect.fail(`expected ${code}`);
  }

  /** The cluster's Clock::unix_timestamp, which read_price compares against. */
  async function chainTime(): Promise<number> {
    const info = await provider.connection.getAccountInfo(SYSVAR_CLOCK_PUBKEY);
    return Number(info!.data.readBigInt64LE(32));
  }

  /**
   * A genesis feed fresh now and for at least the next ~65 s: the latest one
   * published at most 55 s ahead of the chain clock (feeds are 50 s apart,
   * read_price accepts |publish_time − now| ≤ 60).
   */
  async function freshFeed(): Promise<PublicKey> {
    if (!oracles) throw new Error("TEST_ORACLES is not set: run the suite with scripts/test-ts.sh");
    const now = await chainTime();
    const candidates = oracles.feeds.filter((f) => f.publishTime <= now + 55 && f.publishTime >= now + 5);
    if (candidates.length === 0) throw new Error(`no fresh test oracle at chain time ${now}`);
    return new PublicKey(candidates[candidates.length - 1].pubkey);
  }

  function marketPdas(mint: PublicKey, commodity = "OIL") {
    const [market] = PublicKey.findProgramAddressSync(
      [Buffer.from("market"), mint.toBuffer(), Buffer.from(commodity)],
      program.programId
    );
    const [vault] = PublicKey.findProgramAddressSync([Buffer.from("vault"), market.toBuffer()], program.programId);
    const [vaultToken] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault_token"), market.toBuffer()],
      program.programId
    );
    const [lpPool] = findLpPoolPda(market, program.programId);
    return { market, vault, vaultToken, lpPool };
  }

  function lpPositionPda(lpPool: PublicKey, owner: PublicKey, lpClass = 0): PublicKey {
    return findLpPositionPda(lpPool, owner, lpClass, program.programId)[0];
  }

  /** §11: vault tokens == free + position collateral + insurance + cash + treasury (nobody transfers directly here) */
  async function expectBucketsBacked(market: PublicKey, vaultToken: PublicKey, lpPool: PublicKey): Promise<void> {
    const pool = await program.account.lpPool.fetch(lpPool);
    const m = await program.account.market.fetch(market);
    const vt = await getAccount(provider.connection, vaultToken);
    const buckets = pool.freeCollateral
      .add(pool.positionCollateral)
      .add(m.insuranceFund)
      .add(pool.cash)
      .add(pool.treasury);
    expect(vt.amount.toString()).to.equal(buckets.toString());
  }

  function initPoolIx(admin: PublicKey, market: PublicKey, lpPool: PublicKey, params: PoolParams) {
    return program.methods
      .initializePool(params, admin)
      .accounts({ admin, config: configPda, market, lpPool, systemProgram: SystemProgram.programId })
      .instruction();
  }

  function initLpPositionIx(owner: PublicKey, lpPool: PublicKey, lpClass = 0) {
    return program.methods
      .initLpPosition(lpClass)
      .accounts({
        owner,
        lpPool,
        lpPosition: lpPositionPda(lpPool, owner, lpClass),
        systemProgram: SystemProgram.programId,
      })
      .instruction();
  }

  function lpDepositIx(
    owner: PublicKey,
    m: { market: PublicKey; lpPool: PublicKey; vaultToken: PublicKey },
    ownerTokenAccount: PublicKey,
    feed: PublicKey,
    amount: BN,
    minSharesOut: BN
  ) {
    return program.methods
      .lpDeposit(amount, minSharesOut)
      .accounts({
        owner,
        market: m.market,
        lpPool: m.lpPool,
        lpPosition: lpPositionPda(m.lpPool, owner),
        vaultTokenAccount: m.vaultToken,
        ownerTokenAccount,
        pythPriceFeed: feed,
        tokenProgram: TOKEN_PROGRAM_ID,
      });
  }

  function initMarketIx(params: typeof marketParams, mint: PublicKey, feed: PublicKey) {
    const p = marketPdas(mint, params.commodity);
    return program.methods.initializeMarket(params).accounts({
      authority: authority.publicKey,
      config: configPda,
      market: p.market,
      vault: p.vault,
      vaultTokenAccount: p.vaultToken,
      collateralMint: mint,
      pythPriceFeed: feed,
      systemProgram: SystemProgram.programId,
      tokenProgram: TOKEN_PROGRAM_ID,
      rent: anchor.web3.SYSVAR_RENT_PUBKEY,
    });
  }

  /**
   * A new market on its own mint with its pool, created in one transaction
   * (D11): initialize_market, initialize_pool, then (when `seed` > 0) the
   * admin's init_lp_position and seed lp_deposit. The admin holds 2× SEED of
   * the new mint.
   */
  async function createPoolMarket(opts: { params?: PoolParams; seed?: BN; feed?: PublicKey } = {}) {
    const params = opts.params ?? neutralPoolParams();
    const seed = opts.seed ?? SEED;
    const mint = await createMint(provider.connection, authority, authority.publicKey, null, 6);
    const adminToken = await createAssociatedTokenAccount(provider.connection, authority, mint, authority.publicKey);
    // Enough for a later seed too (an unseeded pool is seeded by its tests)
    await mintTo(provider.connection, authority, mint, adminToken, authority, BigInt(SEED.muln(2).toString()));
    const feed = opts.feed ?? (await freshFeed());
    const p = marketPdas(mint);
    const post = [await initPoolIx(authority.publicKey, p.market, p.lpPool, params)];
    if (!seed.isZero()) {
      post.push(await initLpPositionIx(authority.publicKey, p.lpPool));
      post.push(
        await lpDepositIx(authority.publicKey, p, adminToken, feed, seed, seed.subn(LP_DEAD_SHARES)).instruction()
      );
    }
    await initMarketIx(marketParams, mint, feed).postInstructions(post).signers([authority]).rpc();
    return { ...p, mint, feed, adminToken };
  }

  before(async () => {
    // Generate keypairs
    authority = Keypair.generate();
    user1 = Keypair.generate();
    user2 = Keypair.generate();

    // Airdrop SOL
    const airdropAmount = 100 * LAMPORTS_PER_SOL;
    const sigs = await Promise.all([
      provider.connection.requestAirdrop(authority.publicKey, airdropAmount),
      provider.connection.requestAirdrop(user1.publicKey, 10 * LAMPORTS_PER_SOL),
      provider.connection.requestAirdrop(user2.publicKey, 10 * LAMPORTS_PER_SOL),
    ]);
    const latest = await provider.connection.getLatestBlockhash();
    await Promise.all(sigs.map((signature) => provider.connection.confirmTransaction({ signature, ...latest })));

    // Create collateral mint (USDC-like, 6 decimals)
    collateralMint = await createMint(
      provider.connection,
      authority,
      authority.publicKey,
      null,
      6
    );

    // One market per (collateral mint, commodity)
    ({ market: marketPda, vault: vaultPda, vaultToken: vaultTokenPda, lpPool: lpPoolPda } = marketPdas(collateralMint));

    // User accounts are per market
    [user1AccountPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("user"), user1.publicKey.toBuffer(), marketPda.toBuffer()],
      program.programId
    );

    [user2AccountPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("user"), user2.publicKey.toBuffer(), marketPda.toBuffer()],
      program.programId
    );

    // Create token accounts
    authorityTokenAccount = await createAssociatedTokenAccount(
      provider.connection,
      authority,
      collateralMint,
      authority.publicKey
    );
    user1TokenAccount = await createAssociatedTokenAccount(
      provider.connection,
      user1,
      collateralMint,
      user1.publicKey
    );

    user2TokenAccount = await createAssociatedTokenAccount(
      provider.connection,
      user2,
      collateralMint,
      user2.publicKey
    );

    // Mint tokens to users (10,000 USDC each) and the LP seed to the admin
    const mintAmount = 10_000_000_000; // 10,000 with 6 decimals
    await mintTo(provider.connection, authority, collateralMint, user1TokenAccount, authority, mintAmount);
    await mintTo(provider.connection, authority, collateralMint, user2TokenAccount, authority, mintAmount);
    await mintTo(provider.connection, authority, collateralMint, authorityTokenAccount, authority, BigInt(SEED.toString()));

    // Protocol roles: the provider wallet deployed the program, so it is the
    // upgrade authority and creates the config (becoming admin), then hands
    // the admin role to `authority`, which creates the markets below.
    // The oracle program is the system program: scripts/test-ts.sh loads the
    // test Pyth feeds as system-owned accounts.
    const [programData] = PublicKey.findProgramAddressSync(
      [program.programId.toBuffer()],
      new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111")
    );
    await program.methods
      .initializeConfig({ guardian: provider.wallet.publicKey, oracleProgram: SystemProgram.programId })
      .accounts({
        upgradeAuthority: provider.wallet.publicKey,
        config: configPda,
        programData,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    await program.methods
      .proposeAdmin(authority.publicKey)
      .accounts({ admin: provider.wallet.publicKey, config: configPda })
      .rpc();
    await program.methods
      .acceptAdmin()
      .accounts({ newAdmin: authority.publicKey, config: configPda })
      .signers([authority])
      .rpc();
  });

  describe("initialize_market", () => {
    it("should initialize OIL market with its seeded LP pool in one transaction", async () => {
      pythPriceFeed = await freshFeed();
      const pool = { market: marketPda, lpPool: lpPoolPda, vaultToken: vaultTokenPda };
      await initMarketIx(marketParams, collateralMint, pythPriceFeed)
        .postInstructions([
          await initPoolIx(authority.publicKey, marketPda, lpPoolPda, neutralPoolParams()),
          await initLpPositionIx(authority.publicKey, lpPoolPda),
          await lpDepositIx(
            authority.publicKey,
            pool,
            authorityTokenAccount,
            pythPriceFeed,
            SEED,
            SEED.subn(LP_DEAD_SHARES)
          ).instruction(),
        ])
        .signers([authority])
        .rpc();

      // Fetch and verify market account
      const market = await program.account.market.fetch(marketPda);

      expect(market.authority.toBase58()).to.equal(authority.publicKey.toBase58());
      expect(market.collateralMint.toBase58()).to.equal(collateralMint.toBase58());
      // Verify commodity identifier
      const commodityBytes = market.commodity as number[];
      const commodityStr = Buffer.from(commodityBytes).toString('utf-8').replace(/\0/g, '');
      expect(commodityStr).to.equal("OIL");
      expect(market.maxLeverage).to.equal(marketParams.maxLeverage);
      expect(market.maintenanceMarginRatio).to.equal(marketParams.maintenanceMarginRatio);
      expect(market.initialMarginRatio).to.equal(marketParams.initialMarginRatio);
      expect(market.takerFee).to.equal(marketParams.takerFee);
      expect(market.makerFee).to.equal(marketParams.makerFee);
      expect(market.liquidationFee).to.equal(marketParams.liquidationFee);
      expect(market.longOpenInterest.toNumber()).to.equal(0);
      expect(market.shortOpenInterest.toNumber()).to.equal(0);
      expect(market.isPaused).to.equal(false);
      expect(market.fundingCarrySecs).to.equal(0);
      expect(market.settledAt.toNumber()).to.equal(0);

      // The pool: neutral params, $1M of cash, LP_DEAD_SHARES owned by nobody
      const lp = await program.account.lpPool.fetch(lpPoolPda);
      expect(lp.market.toBase58()).to.equal(marketPda.toBase58());
      expect(lp.adlKeeper.toBase58()).to.equal(authority.publicKey.toBase58());
      expect(lp.cash.toString()).to.equal(SEED.toString());
      expect(lp.lpShares.toString()).to.equal(SEED.toString());
      expect(lp.freeCollateral.toNumber()).to.equal(0);
      expect(lp.openPositions.toNumber()).to.equal(0);
      expect(encodePoolParams(lp.params as PoolParams).equals(encodePoolParams(neutralPoolParams()))).to.equal(true);
      expect(lp.pendingParamsAt.toNumber()).to.equal(0);
      const position = await program.account.lpPosition.fetch(lpPositionPda(lpPoolPda, authority.publicKey));
      expect(position.owner.toBase58()).to.equal(authority.publicKey.toBase58());
      expect(position.shares.toString()).to.equal(SEED.subn(LP_DEAD_SHARES).toString());
      expect(position.depositedCum.toString()).to.equal(SEED.toString());
      await expectBucketsBacked(marketPda, vaultTokenPda, lpPoolPda);
    });

    it("should create a separate GOLD market on the same collateral", async () => {
      const gold = marketPdas(collateralMint, goldMarketParams.commodity);
      expect(gold.market.toBase58()).to.not.equal(marketPda.toBase58());

      // Without the pool: no trading or collateral is possible until it exists
      await initMarketIx(goldMarketParams, collateralMint, Keypair.generate().publicKey)
        .signers([authority])
        .rpc();

      const goldMarket = await program.account.market.fetch(gold.market);
      const oil = await program.account.market.fetch(marketPda);
      expect(Buffer.from(goldMarket.commodity).toString().replace(/\0+$/, "")).to.equal("GOLD");
      expect(Buffer.from(oil.commodity).toString().replace(/\0+$/, "")).to.equal("OIL");
    });

    async function expectInvalidMarket(params: typeof marketParams): Promise<void> {
      const newMint = await createMint(provider.connection, authority, authority.publicKey, null, 6);
      await expectErr(
        initMarketIx(params, newMint, Keypair.generate().publicKey).signers([authority]).rpc(),
        "InvalidMarketConfig"
      );
    }

    it("should fail with invalid max leverage (0)", async () => {
      await expectInvalidMarket({ ...marketParams, maxLeverage: 0 });
    });

    it("should fail when initial margin <= maintenance margin", async () => {
      await expectInvalidMarket({ ...marketParams, maintenanceMarginRatio: 1000, initialMarginRatio: 500 });
    });

    it("should fail with a funding interval outside 60 s ..= 1 day (§4.10)", async () => {
      await expectInvalidMarket({ ...marketParams, fundingInterval: new BN(59) });
      await expectInvalidMarket({ ...marketParams, fundingInterval: new BN(86_401) });
    });
  });

  describe("initialize_user", () => {
    it("should initialize user account for user1", async () => {
      await program.methods
        .initializeUser()
        .accounts({
          owner: user1.publicKey,
          market: marketPda,
          userAccount: user1AccountPda,
          systemProgram: SystemProgram.programId,
        })
        .signers([user1])
        .rpc();

      const userAccount = await program.account.userAccount.fetch(user1AccountPda);

      expect(userAccount.owner.toBase58()).to.equal(user1.publicKey.toBase58());
      expect(userAccount.collateralBalance.toNumber()).to.equal(0);
      expect(userAccount.totalPositions).to.equal(0);
      expect(userAccount.totalTrades.toNumber()).to.equal(0);
      expect(userAccount.realizedPnl.toNumber()).to.equal(0);
      expect(userAccount.houseEntryNotional.toNumber()).to.equal(0);
    });

    it("should initialize user account for user2", async () => {
      await program.methods
        .initializeUser()
        .accounts({
          owner: user2.publicKey,
          market: marketPda,
          userAccount: user2AccountPda,
          systemProgram: SystemProgram.programId,
        })
        .signers([user2])
        .rpc();

      const userAccount = await program.account.userAccount.fetch(user2AccountPda);

      expect(userAccount.owner.toBase58()).to.equal(user2.publicKey.toBase58());
    });

    it("should fail to reinitialize existing user account", async () => {
      await expectErr(
        program.methods
          .initializeUser()
          .accounts({
            owner: user1.publicKey,
            market: marketPda,
            userAccount: user1AccountPda,
            systemProgram: SystemProgram.programId,
          })
          .signers([user1])
          .rpc(),
        "already in use"
      );
    });
  });

  function depositIx(owner: Keypair, userAccount: PublicKey, tokenAccount: PublicKey, amount: BN) {
    return program.methods
      .depositCollateral(amount)
      .accounts({
        owner: owner.publicKey,
        userAccount,
        market: marketPda,
        vault: vaultPda,
        vaultTokenAccount: vaultTokenPda,
        userTokenAccount: tokenAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        lpPool: lpPoolPda,
      })
      .signers([owner]);
  }

  function withdrawIx(owner: Keypair, userAccount: PublicKey, tokenAccount: PublicKey, amount: BN) {
    return program.methods
      .withdrawCollateral(amount)
      .accounts({
        owner: owner.publicKey,
        userAccount,
        market: marketPda,
        vault: vaultPda,
        vaultTokenAccount: vaultTokenPda,
        userTokenAccount: tokenAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        lpPool: lpPoolPda,
      })
      .signers([owner]);
  }

  describe("deposit_collateral", () => {
    it("should deposit collateral successfully", async () => {
      const depositAmount = new BN(1_000_000_000); // 1,000 USDC

      const userAccountBefore = await program.account.userAccount.fetch(user1AccountPda);
      const tokenAccountBefore = await getAccount(provider.connection, user1TokenAccount);
      const poolBefore = await program.account.lpPool.fetch(lpPoolPda);

      await depositIx(user1, user1AccountPda, user1TokenAccount, depositAmount).rpc();

      const userAccountAfter = await program.account.userAccount.fetch(user1AccountPda);
      const tokenAccountAfter = await getAccount(provider.connection, user1TokenAccount);
      const poolAfter = await program.account.lpPool.fetch(lpPoolPda);

      expect(userAccountAfter.collateralBalance.toNumber()).to.equal(
        userAccountBefore.collateralBalance.toNumber() + depositAmount.toNumber()
      );
      expect(Number(tokenAccountAfter.amount)).to.equal(
        Number(tokenAccountBefore.amount) - depositAmount.toNumber()
      );
      // §4.1: the free-collateral bucket follows every balance
      expect(poolAfter.freeCollateral.toNumber()).to.equal(
        poolBefore.freeCollateral.toNumber() + depositAmount.toNumber()
      );
      expect(poolAfter.cash.toString()).to.equal(poolBefore.cash.toString());
      await expectBucketsBacked(marketPda, vaultTokenPda, lpPoolPda);
    });

    it("should allow multiple deposits", async () => {
      const depositAmount = new BN(500_000_000); // 500 USDC

      const userAccountBefore = await program.account.userAccount.fetch(user1AccountPda);

      await depositIx(user1, user1AccountPda, user1TokenAccount, depositAmount).rpc();

      const userAccountAfter = await program.account.userAccount.fetch(user1AccountPda);

      expect(userAccountAfter.collateralBalance.toNumber()).to.equal(
        userAccountBefore.collateralBalance.toNumber() + depositAmount.toNumber()
      );
    });

    it("should deposit for user2", async () => {
      const depositAmount = new BN(2_000_000_000); // 2,000 USDC

      await depositIx(user2, user2AccountPda, user2TokenAccount, depositAmount).rpc();

      const userAccount = await program.account.userAccount.fetch(user2AccountPda);
      expect(userAccount.collateralBalance.toNumber()).to.equal(depositAmount.toNumber());
      const pool = await program.account.lpPool.fetch(lpPoolPda);
      expect(pool.freeCollateral.toNumber()).to.equal(3_500_000_000);
      await expectBucketsBacked(marketPda, vaultTokenPda, lpPoolPda);
    });

    it("should reject another market's LP pool", async () => {
      const other = await createPoolMarket({ seed: new BN(0) });
      await expectErr(
        program.methods
          .depositCollateral(new BN(1_000_000))
          .accounts({
            owner: user1.publicKey,
            userAccount: user1AccountPda,
            market: marketPda,
            vault: vaultPda,
            vaultTokenAccount: vaultTokenPda,
            userTokenAccount: user1TokenAccount,
            tokenProgram: TOKEN_PROGRAM_ID,
            lpPool: other.lpPool,
          })
          .signers([user1])
          .rpc(),
        "ConstraintSeeds"
      );
    });
  });

  describe("withdraw_collateral", () => {
    it("should withdraw collateral successfully", async () => {
      const withdrawAmount = new BN(100_000_000); // 100 USDC

      const userAccountBefore = await program.account.userAccount.fetch(user1AccountPda);
      const tokenAccountBefore = await getAccount(provider.connection, user1TokenAccount);
      const poolBefore = await program.account.lpPool.fetch(lpPoolPda);

      await withdrawIx(user1, user1AccountPda, user1TokenAccount, withdrawAmount).rpc();

      const userAccountAfter = await program.account.userAccount.fetch(user1AccountPda);
      const tokenAccountAfter = await getAccount(provider.connection, user1TokenAccount);
      const poolAfter = await program.account.lpPool.fetch(lpPoolPda);

      expect(userAccountAfter.collateralBalance.toNumber()).to.equal(
        userAccountBefore.collateralBalance.toNumber() - withdrawAmount.toNumber()
      );
      expect(Number(tokenAccountAfter.amount)).to.equal(
        Number(tokenAccountBefore.amount) + withdrawAmount.toNumber()
      );
      expect(poolAfter.freeCollateral.toNumber()).to.equal(
        poolBefore.freeCollateral.toNumber() - withdrawAmount.toNumber()
      );
      await expectBucketsBacked(marketPda, vaultTokenPda, lpPoolPda);
    });

    it("should fail when withdrawing more than balance", async () => {
      const userAccount = await program.account.userAccount.fetch(user1AccountPda);
      const excessiveAmount = new BN(userAccount.collateralBalance.toNumber() + 1_000_000);

      await expectErr(
        withdrawIx(user1, user1AccountPda, user1TokenAccount, excessiveAmount).rpc(),
        "InsufficientCollateral"
      );
    });
  });

  describe("Position state calculations", () => {
    // These test the Position impl methods from state/position.rs
    it("should calculate notional value correctly", () => {
      // size * entry_price / 1_000_000
      // 100 contracts at $75 = 100 * 75_000_000 / 1_000_000 = 7,500
      const size = 100_000_000; // 100 contracts (6 decimals)
      const entryPrice = 75_000_000; // $75 (6 decimals)
      const expectedNotional = (size * entryPrice) / 1_000_000;
      expect(expectedNotional).to.equal(7_500_000_000); // $7,500 with 6 decimals
    });

    it("should calculate unrealized PnL for long position", () => {
      // Long: PnL = size * (current - entry) / 1_000_000
      const size = 100_000_000; // 100 contracts
      const entryPrice = 75_000_000; // $75
      const currentPrice = 80_000_000; // $80
      const priceDiff = currentPrice - entryPrice; // 5_000_000
      const expectedPnl = (size * priceDiff) / 1_000_000;
      expect(expectedPnl).to.equal(500_000_000); // +$500
    });

    it("should calculate unrealized PnL for short position", () => {
      // Short: PnL = size * (entry - current) / 1_000_000
      const size = 100_000_000; // 100 contracts
      const entryPrice = 75_000_000; // $75
      const currentPrice = 80_000_000; // $80
      const priceDiff = entryPrice - currentPrice; // -5_000_000
      const expectedPnl = (size * priceDiff) / 1_000_000;
      expect(expectedPnl).to.equal(-500_000_000); // -$500
    });

    it("should calculate margin ratio correctly", () => {
      // margin_ratio = equity / notional * 10000 (entry notional; display and the venue backstop)
      const collateral = 1_000_000_000; // $1,000
      const pnl = 100_000_000; // +$100
      const equity = collateral + pnl; // $1,100
      const notional = 10_000_000_000; // $10,000
      const marginRatio = (equity * 10000) / notional;
      expect(marginRatio).to.equal(1100); // 11%
    });

    // Mirrors Position::is_liquidatable (§6.1): equity·10_000 < mmr·N_mark,
    // N_mark = floor(size·mark, 1e6); with N_mark == 0 only when equity < 0.
    const isLiquidatable = (side: "long" | "short", size: bigint, entry: bigint, collateral: bigint, mark: bigint, funding: bigint, mmr: bigint) => {
      const pnl = side === "long" ? (size * (mark - entry)) / 1_000_000n : (size * (entry - mark)) / 1_000_000n;
      const equity = collateral + pnl + funding;
      const nMark = (size * mark) / 1_000_000n;
      if (nMark === 0n) return equity < 0n;
      return equity * 10_000n < mmr * nMark;
    };

    it("should detect liquidatable position on mark notional", () => {
      // 100 long at $100 with $500; at $97: equity $200 < 5% of $9,700
      expect(isLiquidatable("long", 100_000_000n, 100_000_000n, 500_000_000n, 97_000_000n, 0n, 500n)).to.equal(true);
      // At $99.50: equity $450 ≥ 5% of $9,950 ($497.50)? No: still liquidatable
      expect(isLiquidatable("long", 100_000_000n, 100_000_000n, 500_000_000n, 99_500_000n, 0n, 500n)).to.equal(true);
      // At $100: equity $500 = 5% of $10,000, not strictly below
      expect(isLiquidatable("long", 100_000_000n, 100_000_000n, 500_000_000n, 100_000_000n, 0n, 500n)).to.equal(false);
    });

    it("should liquidate a losing short on mark notional before entry notional says so", () => {
      // 100 short at $75 with $750: at $78.60 equity is $390, 5.2% of the
      // $7,500 entry notional but under 5% of the $7,860 mark notional
      const equity = 750_000_000 - 360_000_000;
      expect((equity * 10_000) / 7_500_000_000).to.be.greaterThan(500);
      expect(isLiquidatable("short", 100_000_000n, 75_000_000n, 750_000_000n, 78_600_000n, 0n, 500n)).to.equal(true);
      // At $78.50 ($400 vs $392.50) it is not
      expect(isLiquidatable("short", 100_000_000n, 75_000_000n, 750_000_000n, 78_500_000n, 0n, 500n)).to.equal(false);
    });

    it("should count accrued funding in equity and treat zero mark notional as solvent unless under water", () => {
      // Funding owed of $11 tips $10 of headroom over the line
      expect(isLiquidatable("long", 100_000_000n, 100_000_000n, 510_000_000n, 100_000_000n, 0n, 500n)).to.equal(false);
      expect(isLiquidatable("long", 100_000_000n, 100_000_000n, 510_000_000n, 100_000_000n, -11_000_000n, 500n)).to.equal(true);
      // floor(1 · 0.5, 1e6) = 0: only negative equity is liquidatable
      expect(isLiquidatable("long", 1n, 500_000n, 0n, 500_000n, 0n, 500n)).to.equal(false);
      expect(isLiquidatable("long", 1n, 500_000n, 0n, 500_000n, -1n, 500n)).to.equal(true);
    });
  });

  describe("Market OI capacity checks", () => {
    it("should allow increasing OI within cap", () => {
      const maxOI = 1_000_000_000_000;
      const currentOI = 500_000_000_000;
      const newSize = 100_000_000;
      const newOI = currentOI + newSize;
      expect(newOI <= maxOI).to.equal(true);
    });

    it("should reject OI exceeding cap", () => {
      const maxOI = 1_000_000_000_000;
      const currentOI = 999_999_000_000;
      const newSize = 2_000_000_000; // Would exceed cap
      const newOI = currentOI + newSize;
      expect(newOI <= maxOI).to.equal(false);
    });
  });

  describe("add_margin", () => {
    // Note: This requires an open position, which needs oracle price mocking
    // For unit test purposes, we test the logic
    it("should have correct margin addition logic", () => {
      const currentCollateral = 500_000_000; // $500
      const additionalMargin = 200_000_000; // $200
      const newCollateral = currentCollateral + additionalMargin;
      expect(newCollateral).to.equal(700_000_000); // $700
    });
  });

  describe("Fee calculations", () => {
    it("should calculate taker fee correctly", () => {
      // fee = notional * taker_fee / 10000
      const notional = 10_000_000_000; // $10,000
      const takerFee = 5; // 0.05%
      const fee = (notional * takerFee) / 10000;
      expect(fee).to.equal(5_000_000); // $5
    });

    it("should calculate maker fee correctly", () => {
      const notional = 10_000_000_000;
      const makerFee = 2; // 0.02%
      const fee = (notional * makerFee) / 10000;
      expect(fee).to.equal(2_000_000); // $2
    });

    it("should round the close fee up on opened notional (D21)", () => {
      // ceil(floor(size·opened_price, 1e6)·taker_fee, 10_000): 33.333333
      // contracts opened at $75 are $2,499.999975; 0.05% is $1.2499999875
      const openedNotional = (33_333_333n * 75_000_000n) / 1_000_000n;
      expect(openedNotional).to.equal(2_499_999_975n);
      const fee = (openedNotional * 5n + 9_999n) / 10_000n;
      expect(fee).to.equal(1_250_000n);
    });

    it("should calculate the stage-2 liquidation reward from the maintenance margin in force", () => {
      // reward = opened_notional * mmr_in_force * liquidation_fee / 10000^2 (§6.4, D8)
      // Based on the margin requirement, not the collateral left, so it is
      // still paid (from the insurance fund) when a position is underwater.
      const notional = 10_000_000_000; // $10,000
      const liquidationFee = 250; // 2.5%
      expect((notional * 500 * liquidationFee) / 100_000_000).to.equal(12_500_000); // $12.50 on weekdays (5%)
      expect((notional * 1_000 * liquidationFee) / 100_000_000).to.equal(25_000_000); // $25 with a 10% weekend MMR
    });
  });

  describe("Funding calculations", () => {
    // Mirrors programs/perps-core: each funding period adds
    // funding_per_contract = rate * that period's price / 1e6 (USDC per contract)
    // to Market.cumulative_funding; a position pays size * index_diff / 1e6.
    const size = 100_000_000; // 100 contracts
    const fundingPerContract = (rate: number, price: number) => (rate * price) / 1_000_000;
    const accrued = (isLong: boolean, indexDiff: number) => {
      const payment = (size * indexDiff) / 1_000_000;
      return isLong ? -payment : payment;
    };

    it("should calculate funding payment for long position", () => {
      // 0.01% at $75 = $0.0075 per contract; 100 contracts pay $0.75
      const index = fundingPerContract(100, 75_000_000);
      expect(index).to.equal(7_500);
      expect(accrued(true, index)).to.equal(-750_000);
    });

    it("should calculate funding payment for short position", () => {
      expect(accrued(false, fundingPerContract(100, 75_000_000))).to.equal(750_000);
    });

    it("should charge each period at its own price", () => {
      // 0.01% at $75 then 0.01% at $150: $0.75 + $1.50
      const index = fundingPerContract(100, 75_000_000) + fundingPerContract(100, 150_000_000);
      expect(accrued(true, index)).to.equal(-2_250_000);
    });
  });

  describe("Settlement calculations", () => {
    it("should calculate settlement on profitable close", () => {
      const collateral = 1_000_000_000; // $1,000
      const pnl = 500_000_000; // +$500
      const fundingPayment = -10_000_000; // -$10
      const fee = 5_000_000; // $5
      const totalPnl = pnl + fundingPayment - fee; // +$485
      const settlement = Math.max(0, collateral + totalPnl);
      expect(settlement).to.equal(1_485_000_000); // $1,485
    });

    it("should calculate settlement on loss (partial)", () => {
      const collateral = 1_000_000_000; // $1,000
      const pnl = -600_000_000; // -$600
      const fundingPayment = -10_000_000; // -$10
      const fee = 5_000_000; // $5
      const totalPnl = pnl + fundingPayment - fee; // -$615
      const settlement = Math.max(0, collateral + totalPnl);
      expect(settlement).to.equal(385_000_000); // $385
    });

    it("should handle full loss (settlement is 0)", () => {
      const collateral = 500_000_000; // $500
      const pnl = -600_000_000; // -$600
      const fundingPayment = 0;
      const fee = 0;
      const totalPnl = pnl + fundingPayment - fee;
      const settlement = Math.max(0, collateral + totalPnl);
      expect(settlement).to.equal(0);
    });
  });

  describe("Liquidation calculations", () => {
    // Mirrors programs/perps-core/src/instructions/liquidate.rs (§6.3–§6.4)
    // with the numbers of its unit tests. All amounts 6 dp.
    const C6 = 1_000_000n;
    const ceilU = (a: bigint, b: bigint) => (a + b - 1n) / b;
    const ceilS = (a: bigint, b: bigint) => {
      // -((-a).div_euclid(b)) for b > 0
      const n = -a;
      const q = n >= 0n ? n / b : -((-n + b - 1n) / b);
      return -q;
    };
    const notional = (size: bigint, mark: bigint) => (size * mark) / C6;
    const minB = (...xs: bigint[]) => xs.reduce((a, b) => (b < a ? b : a));
    const maxB = (...xs: bigint[]) => xs.reduce((a, b) => (b > a ? b : a));

    /** stage1_slice: smallest x with E − pen(x) ≥ IM·N(size − x), ≥ the minimum slice */
    const stage1Slice = (size: bigint, mark: bigint, e: bigint, im: bigint, p: bigint, minSliceBps: bigint, minRemaining: bigint) => {
      const xNeed = im > p ? maxB(ceilS(im * size * mark - e * 10_000_000_000n, mark * (im - p)), 0n) : size;
      let x = maxB(xNeed, ceilU(size * minSliceBps, 10_000n));
      x = minB(maxB(x, 1n), size);
      return notional(size - x, mark) < minRemaining ? size : x;
    };
    /** stage1_penalty: min(ceil(N(x)·p, 10_000), max(E, 0)) */
    const stage1Penalty = (x: bigint, mark: bigint, p: bigint, e: bigint) => minB(ceilU(notional(x, mark) * p, 10_000n), maxB(e, 0n));
    /** Market::liquidation_reward_target(opened_notional, mmr_in_force) */
    const rewardTarget = (openedNotional: bigint, mmr: bigint, liqFee: bigint) => (openedNotional * mmr * liqFee) / 100_000_000n;
    /** credit_insurance: fill insurance up to the target, the rest overflows to LP cash */
    const insuranceSplit = (insurance: bigint, target: bigint, y: bigint) => {
      const room = target > insurance ? target - insurance : 0n;
      const toIns = minB(y, room);
      return { toIns, toCash: y - toIns };
    };

    /** Stage 2 (§6.4) for a loss (realized ≤ 0), with the deficit waterfall of §2.6 */
    const stage2 = (s: {
      collateral: bigint; realized: bigint; openedNotional: bigint; mmr: bigint; liqFee: bigint;
      insurance: bigint; insuranceTarget: bigint; insCapPerWindow: bigint; lpCap: bigint;
    }) => {
      const target = rewardTarget(s.openedNotional, s.mmr, s.liqFee);
      let insurance = s.insurance;
      let insPaidWindow = 0n;
      const capIns = () => (s.insCapPerWindow === 0n ? 1n << 64n : s.insCapPerWindow - insPaidWindow);
      const eAfter = s.collateral + s.realized;
      let cash = 0n;
      if (eAfter >= 0n) {
        cash += -s.realized;
        const fromCol = minB(target, eAfter);
        const split = insuranceSplit(insurance, s.insuranceTarget, eAfter - fromCol);
        insurance += split.toIns;
        cash += split.toCash;
        const top = minB(target - fromCol, insurance, capIns());
        insurance -= top;
        insPaidWindow += top;
        return { keeper: fromCol + top, insurance, cash, deficit: 0n, insPaid: 0n, lpAbsorbed: 0n, pending: 0n };
      }
      const top = minB(target, insurance, capIns());
      insurance -= top;
      insPaidWindow += top;
      cash += s.collateral;
      const d = -eAfter;
      const insPaid = minB(d, insurance, capIns());
      insurance -= insPaid;
      cash += insPaid;
      const lpAbsorbed = minB(d - insPaid, s.lpCap);
      return { keeper: top, insurance, cash, deficit: d, insPaid, lpAbsorbed, pending: d - insPaid - lpAbsorbed };
    };

    it("should pay the stage-2 reward from collateral and send the residual to insurance, then cash", () => {
      // 100 long at $75 with $500, liquidated at $71.50: loss $350, E' = $150.
      // Reward 1% of 5% of $7,500 = $3.75 from collateral; $146.25 is left:
      // $10 fills insurance to its $100 target, $136.25 overflows to LP cash
      const r = stage2({
        collateral: 500n * C6, realized: -350n * C6, openedNotional: 7_500n * C6, mmr: 500n, liqFee: 100n,
        insurance: 90n * C6, insuranceTarget: 100n * C6, insCapPerWindow: 0n, lpCap: 1n << 64n,
      });
      expect(r.keeper).to.equal(3_750_000n);
      expect(r.insurance).to.equal(100n * C6);
      expect(r.cash).to.equal(350n * C6 + 136_250_000n);
      expect(r.deficit).to.equal(0n);
    });

    it("should use the maintenance margin in force and opened notional for the reward (D8, D21)", () => {
      // entry_price may be moved by settle_position_pnl; the reward is not
      expect(rewardTarget(7_500n * C6, 500n, 100n)).to.equal(3_750_000n);
      expect(rewardTarget(7_500n * C6, 1_000n, 100n)).to.equal(7_500_000n);
      // The test market's 2.5% fee on a $10,000 position at 5%
      expect(rewardTarget(10_000n * C6, 500n, 250n)).to.equal(12_500_000n);
    });

    it("should cap the insurance top-up of an underwater position per loss window", () => {
      // $75 on 100 @ $75, gapped to $74: equity −$25. The $3.75 reward gets
      // only the window's $2 of insurance, and the deficit then finds the
      // window used up: LPs absorb all $25
      const r = stage2({
        collateral: 75n * C6, realized: -100n * C6, openedNotional: 7_500n * C6, mmr: 500n, liqFee: 100n,
        insurance: 50n * C6, insuranceTarget: 1n << 64n, insCapPerWindow: 2n * C6, lpCap: 1n << 64n,
      });
      expect(r.keeper).to.equal(2n * C6);
      expect(r.deficit).to.equal(25n * C6);
      expect(r.insurance).to.equal(48n * C6);
      expect(r.insPaid).to.equal(0n);
      expect(r.lpAbsorbed).to.equal(25n * C6);
      expect(r.pending).to.equal(0n);
    });

    it("should run a deficit through insurance, the LP window cap, then pending bad debt", () => {
      // $75 on 10 @ $75, gapped 15% to $63.75: −$37.50. Reward $0.375 from
      // insurance, which then pays $9.625 of the deficit; LPs absorb $27.875
      const r = stage2({
        collateral: 75n * C6, realized: -112_500_000n, openedNotional: 750n * C6, mmr: 500n, liqFee: 100n,
        insurance: 10n * C6, insuranceTarget: 1n << 64n, insCapPerWindow: 0n, lpCap: 100n * C6,
      });
      expect(r.keeper).to.equal(375_000n);
      expect(r.deficit).to.equal(37_500_000n);
      expect(r.insurance).to.equal(0n);
      expect(r.insPaid).to.equal(9_625_000n);
      expect(r.lpAbsorbed).to.equal(27_875_000n);
      expect(r.pending).to.equal(0n);
      // A cap below the remainder leaves the rest pending (the ADL target)
      const capped = stage2({
        collateral: 75n * C6, realized: -112_500_000n, openedNotional: 750n * C6, mmr: 500n, liqFee: 100n,
        insurance: 10n * C6, insuranceTarget: 1n << 64n, insCapPerWindow: 0n, lpCap: 20n * C6,
      });
      expect(capped.lpAbsorbed).to.equal(20n * C6);
      expect(capped.pending).to.equal(7_875_000n);
    });

    it("should size a stage-1 slice to restore initial margin", () => {
      // 100 long at $75 with $750, mark $71: E = $350 (4.93% of $7,100).
      // Production params: IM 10%, penalty 2%, minimum slice 20%
      const size = 100n * C6;
      const mark = 71n * C6;
      const e = 350n * C6;
      const x = stage1Slice(size, mark, e, 1_000n, 200n, 2_000n, 0n);
      expect(x).to.equal(63_380_282n);
      const pen = stage1Penalty(x, mark, 200n, e);
      expect(pen).to.equal(90_000_001n);
      // E − pen ≥ IM · remaining mark notional, and one unit less is not enough
      expect((e - pen) * 10_000n >= 1_000n * notional(size - x, mark)).to.equal(true);
      const y = x - 1n;
      expect((e - stage1Penalty(y, mark, 200n, e)) * 10_000n >= 1_000n * notional(size - y, mark)).to.equal(false);
      // The keeper gets 20% of the penalty, insurance the rest
      expect((pen * 2_000n) / 10_000n).to.equal(18_000_000n);
    });

    it("should apply the stage-1 minimum slice, minimum remainder and penalty cap", () => {
      const size = 100n * C6;
      const mark = 75n * C6;
      const n = notional(size, mark);
      const e = (n * 480n) / 10_000n; // just below 5%
      const need = stage1Slice(size, mark, e, 1_000n, 200n, 1n, 0n);
      expect(stage1Slice(size, mark, e, 1_000n, 200n, 2_000n, 0n)).to.equal(maxB(need, 20n * C6));
      // A remainder under liq_min_remaining_notional closes the whole position
      const x = stage1Slice(size, mark, e, 1_000n, 200n, 2_000n, 0n);
      const rem = notional(size - x, mark);
      expect(stage1Slice(size, mark, e, 1_000n, 200n, 2_000n, rem)).to.equal(x);
      expect(stage1Slice(size, mark, e, 1_000n, 200n, 2_000n, rem + 1n)).to.equal(size);
      // IM ≤ penalty: no slice restores IM, so the whole position
      expect(stage1Slice(size, mark, e, 200n, 200n, 1n, 0n)).to.equal(size);
      // The penalty is capped by equity and rounds up
      expect(stage1Penalty(size, mark, 200n, n)).to.equal(150n * C6);
      expect(stage1Penalty(size, mark, 200n, 10n * C6)).to.equal(10n * C6);
      expect(stage1Penalty(size, mark, 200n, -5n)).to.equal(0n);
      expect(stage1Penalty(1n, 75n * C6, 200n, 1_000n)).to.equal(2n);
    });

    it("should credit a full stage-1 close's leftover equity to the owner's free collateral (D7)", () => {
      // The same position with a $100k minimum remainder closes in full:
      // $350 equity − 2% of $7,100 ($142) = $208 to the owner; the keeper
      // gets 20% of the penalty ($28.40), insurance $113.60; LP cash +$400
      const size = 100n * C6;
      const mark = 71n * C6;
      const e = 350n * C6;
      expect(stage1Slice(size, mark, e, 1_000n, 200n, 2_000n, 100_000n * C6)).to.equal(size);
      const pen = stage1Penalty(size, mark, 200n, e);
      expect(pen).to.equal(142n * C6);
      expect(e - pen).to.equal(208n * C6);
      expect((pen * 2_000n) / 10_000n).to.equal(28_400_000n);
      expect(pen - (pen * 2_000n) / 10_000n).to.equal(113_600_000n);
    });

    it("should choose stage 1 only at or above the backstop line", () => {
      // backstop = floor(mmr·backstop_threshold_bps / 10_000); stage 1 iff E·10_000 ≥ backstop·N
      const stage = (e: bigint, n: bigint, mmr: bigint, threshold: bigint) =>
        e * 10_000n >= ((mmr * threshold) / 10_000n) * n ? 1 : 2;
      const n = 7_100n * C6;
      expect(stage(350n * C6, n, 500n, 6_667n)).to.equal(1); // 4.93% ≥ 3.33%
      expect(stage(200n * C6, n, 500n, 6_667n)).to.equal(2); // 2.82% < 3.33%
      // Neutral params (threshold 100%): any liquidatable position is stage 2
      expect(stage(350n * C6, n, 500n, 10_000n)).to.equal(2);
    });
  });

  describe("SDK layout (sdk/mm-registry) against the deployed program", () => {
    it("should derive the LP pool and LP position PDAs", async () => {
      const [pool] = PublicKey.findProgramAddressSync([Buffer.from("lp_pool"), marketPda.toBuffer()], program.programId);
      expect(findLpPoolPda(marketPda, program.programId)[0].toBase58()).to.equal(pool.toBase58());
      const [pos] = PublicKey.findProgramAddressSync(
        [Buffer.from("lp_position"), pool.toBuffer(), authority.publicKey.toBuffer(), Buffer.from([0])],
        program.programId
      );
      expect(lpPositionPda(pool, authority.publicKey).toBase58()).to.equal(pos.toBase58());
    });

    it("should encode PoolParams as the program's Borsh layout (104 bytes, profit_close_quiet_secs last)", async () => {
      for (const p of [neutralPoolParams(), productionPoolParams()]) {
        const anchorBytes = await program.coder.types.encode("PoolParams", p);
        expect(anchorBytes.length).to.equal(104);
        expect(encodePoolParams(p).equals(anchorBytes)).to.equal(true);
        expect(encodePoolParams(decodePoolParams(anchorBytes)).equals(anchorBytes)).to.equal(true);
      }
      const p = { ...neutralPoolParams(), profitCloseQuietSecs: 0x01020304 };
      expect(encodePoolParams(p).subarray(100).equals(Buffer.from([4, 3, 2, 1]))).to.equal(true);
    });

    it("should hand-encode initialize_pool and propose_pool_params as the IDL does", async () => {
      const params = productionPoolParams();
      const keeper = Keypair.generate().publicKey;
      const init = buildInitializePoolIx(authority.publicKey, marketPda, params, keeper, program.programId);
      expect(init.data.equals(program.coder.instruction.encode("initializePool", { params, adlKeeper: keeper }))).to.equal(true);
      const viaIdl = await initPoolIx(authority.publicKey, marketPda, lpPoolPda, params);
      expect(init.keys.map((k) => [k.pubkey.toBase58(), k.isSigner, k.isWritable])).to.deep.equal(
        viaIdl.keys.map((k) => [k.pubkey.toBase58(), k.isSigner, k.isWritable])
      );
      const propose = buildProposePoolParamsIx(authority.publicKey, marketPda, params, program.programId);
      expect(propose.data.equals(program.coder.instruction.encode("proposePoolParams", { params }))).to.equal(true);
      const proposeViaIdl = await program.methods
        .proposePoolParams(params)
        .accounts({ admin: authority.publicKey, config: configPda, market: marketPda, lpPool: lpPoolPda })
        .instruction();
      expect(propose.keys.map((k) => [k.pubkey.toBase58(), k.isSigner, k.isWritable])).to.deep.equal(
        proposeViaIdl.keys.map((k) => [k.pubkey.toBase58(), k.isSigner, k.isWritable])
      );
    });

    it("should decode LpPool and LpPosition by byte offset", async () => {
      const raw = (await provider.connection.getAccountInfo(lpPoolPda))!.data;
      expect(raw.length).to.equal(LP_POOL_LEN);
      const sdk = decodeLpPool(raw);
      const idl = await program.account.lpPool.fetch(lpPoolPda);
      for (const [k, v] of Object.entries(idl)) {
        const s = (sdk as unknown as Record<string, unknown>)[k];
        if (k === "params" || k === "pendingParams") {
          expect(encodePoolParams(s as PoolParams).equals(encodePoolParams(v as PoolParams)), k).to.equal(true);
        } else {
          expect(String(s), k).to.equal(String(v));
        }
      }
      expect(Object.keys(sdk).sort()).to.deep.equal(Object.keys(idl).sort());
      expect(LP_POOL_OFFSETS.pendingParams).to.equal(440);
      expect(LP_POOL_OFFSETS.bump).to.equal(544);

      const posKey = lpPositionPda(lpPoolPda, authority.publicKey);
      const posRaw = (await provider.connection.getAccountInfo(posKey))!.data;
      expect(posRaw.length).to.equal(LP_POSITION_LEN);
      const pos = decodeLpPosition(posRaw);
      const posIdl = await program.account.lpPosition.fetch(posKey);
      for (const [k, v] of Object.entries(posIdl)) {
        expect(String((pos as unknown as Record<string, unknown>)[k]), k).to.equal(String(v));
      }
    });

    it("should find Market.settled_at at byte 287 once settled (D35)", async () => {
      const gold = marketPdas(collateralMint, "GOLD");
      const before = decodeMarketPoolFields((await provider.connection.getAccountInfo(gold.market))!.data);
      expect(before.settledAt.toNumber()).to.equal(0);
      await program.methods
        .settleMarket(new BN(2_000_000_000))
        .accounts({ authority: authority.publicKey, config: configPda, market: gold.market })
        .signers([authority])
        .rpc();
      const data = (await provider.connection.getAccountInfo(gold.market))!.data;
      const m = await program.account.market.fetch(gold.market);
      const fields = decodeMarketPoolFields(data);
      expect(m.settledAt.toNumber()).to.be.greaterThan(0);
      expect(fields.settledAt.toString()).to.equal(m.settledAt.toString());
      expect(fields.fundingCarrySecs).to.equal(m.fundingCarrySecs);
      // The hand-coded offsets the fill path uses do not move (§16.3)
      expect(new PublicKey(data.subarray(104, 136)).toBase58()).to.equal(m.pythPriceFeed.toBase58());
      expect(data.readUInt32LE(144)).to.equal(m.maxLeverage);
      expect(data.readUInt32LE(152)).to.equal(m.initialMarginRatio);
      expect(data.readUInt32LE(247)).to.equal(m.weekendInitialMarginRatio);
    });
  });

  describe("LP pool flows (§9)", () => {
    let m: Awaited<ReturnType<typeof createPoolMarket>>;
    let lpToken: PublicKey;
    const lp = Keypair.generate();
    const lpPos = () => lpPositionPda(m.lpPool, lp.publicKey);

    const deposit = (amount: BN, minSharesOut: BN, signer = lp, tokenAccount = lpToken) =>
      lpDepositIx(signer.publicKey, m, tokenAccount, m.feed, amount, minSharesOut).signers([signer]).rpc();
    const request = (shares: BN) =>
      program.methods
        .lpRequestWithdrawal(shares)
        .accounts({ owner: lp.publicKey, market: m.market, lpPool: m.lpPool, lpPosition: lpPos(), pythPriceFeed: m.feed })
        .signers([lp])
        .rpc();
    const cancel = () =>
      program.methods
        .lpCancelWithdrawal()
        .accounts({ owner: lp.publicKey, lpPool: m.lpPool, lpPosition: lpPos() })
        .signers([lp])
        .rpc();
    const withdraw = () =>
      program.methods
        .lpWithdraw()
        .accounts({
          owner: lp.publicKey,
          market: m.market,
          lpPool: m.lpPool,
          lpPosition: lpPos(),
          vault: m.vault,
          vaultTokenAccount: m.vaultToken,
          ownerTokenAccount: lpToken,
          pythPriceFeed: m.feed,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([lp])
        .rpc();
    const setMaxPoolCash = (cap: BN, admin = authority) =>
      program.methods
        .setMaxPoolCash(cap)
        .accounts({ admin: admin.publicKey, config: configPda, lpPool: m.lpPool })
        .signers([admin])
        .rpc();

    before(async () => {
      const sig = await provider.connection.requestAirdrop(lp.publicKey, 10 * LAMPORTS_PER_SOL);
      await provider.connection.confirmTransaction({ signature: sig, ...(await provider.connection.getLatestBlockhash()) });
      // Created last, so its feed is fresh for the oracle-gated steps below
      m = await createPoolMarket();
      lpToken = await createAssociatedTokenAccount(provider.connection, lp, m.mint, lp.publicKey);
      await mintTo(provider.connection, authority, m.mint, lpToken, authority, 10_000_000_000);
    });

    it("init_lp_position creates a class-0 position", async () => {
      await program.methods
        .initLpPosition(0)
        .accounts({ owner: lp.publicKey, lpPool: m.lpPool, lpPosition: lpPos(), systemProgram: SystemProgram.programId })
        .signers([lp])
        .rpc();
      const pos = await program.account.lpPosition.fetch(lpPos());
      expect(pos.owner.toBase58()).to.equal(lp.publicKey.toBase58());
      expect(pos.pool.toBase58()).to.equal(m.lpPool.toBase58());
      expect(pos.class).to.equal(0);
      expect(pos.shares.toNumber()).to.equal(0);
      expect(pos.pendingShares.toNumber()).to.equal(0);
    });

    it("init_lp_position rejects any class but 0 (InvalidLpClass)", async () => {
      await expectErr(
        program.methods
          .initLpPosition(1)
          .accounts({
            owner: lp.publicKey,
            lpPool: m.lpPool,
            lpPosition: lpPositionPda(m.lpPool, lp.publicKey, 1),
            systemProgram: SystemProgram.programId,
          })
          .signers([lp])
          .rpc(),
        "InvalidLpClass"
      );
    });

    it("lp_deposit rejects a zero amount (InvalidLpAmount)", async () => {
      await expectErr(deposit(new BN(0), new BN(0)), "InvalidLpAmount");
    });

    // §2.2 with no open positions and nothing vesting: U = 0 and slack = 1,
    // so NAV_mint = cash + 1 and NAV_redeem = cash − 1 (both against the LP)
    const navMint = (pool: { cash: BN; openPositions: BN }) => pool.cash.add(pool.openPositions.muln(2)).addn(1);
    const navRedeem = (pool: { cash: BN; openPositions: BN }) => pool.cash.sub(pool.openPositions.muln(2)).subn(1);
    const sharesFor = (amount: BN, pool: { cash: BN; lpShares: BN; openPositions: BN }) =>
      amount.mul(pool.lpShares).div(navMint(pool));
    const DEPOSIT = new BN(1_000_000_000); // $1,000

    it("lp_deposit enforces min_shares_out (SlippageExceeded)", async () => {
      const pool = await program.account.lpPool.fetch(m.lpPool);
      const expected = sharesFor(DEPOSIT, pool);
      // $1M + 1 of NAV_mint over 1e12 shares: one share under 1e9
      expect(expected.toString()).to.equal("999999999");
      await expectErr(deposit(DEPOSIT, expected.addn(1)), "SlippageExceeded");
    });

    it("lp_deposit mints floor(amount·lp_shares / NAV_mint) shares", async () => {
      const poolBefore = await program.account.lpPool.fetch(m.lpPool);
      const tokenBefore = await getAccount(provider.connection, lpToken);
      const expected = sharesFor(DEPOSIT, poolBefore);
      await deposit(DEPOSIT, expected);
      const pool = await program.account.lpPool.fetch(m.lpPool);
      const pos = await program.account.lpPosition.fetch(lpPos());
      const tokenAfter = await getAccount(provider.connection, lpToken);
      expect(pos.shares.toString()).to.equal(expected.toString());
      expect(pos.depositedCum.toString()).to.equal(DEPOSIT.toString());
      expect(pool.cash.sub(poolBefore.cash).toString()).to.equal(DEPOSIT.toString());
      expect(pool.lpShares.sub(poolBefore.lpShares).toString()).to.equal(expected.toString());
      expect((tokenBefore.amount - tokenAfter.amount).toString()).to.equal(DEPOSIT.toString());
      await expectBucketsBacked(m.market, m.vaultToken, m.lpPool);
    });

    it("set_max_pool_cash caps deposits on NAV_mint (PoolCashCapExceeded) and is admin-only", async () => {
      const pool = await program.account.lpPool.fetch(m.lpPool);
      const room = new BN(500_000_000);
      const cap = navMint(pool).add(room);
      await expectErr(setMaxPoolCash(cap, user1), "Unauthorized");
      await setMaxPoolCash(cap);
      expect((await program.account.lpPool.fetch(m.lpPool)).maxPoolCash.toString()).to.equal(cap.toString());
      // max(NAV_mint, 0) + amount ≤ max_pool_cash
      await expectErr(deposit(room.addn(1), new BN(0)), "PoolCashCapExceeded");
      await deposit(room, new BN(1));
      await setMaxPoolCash(new BN(0));
      expect((await program.account.lpPool.fetch(m.lpPool)).maxPoolCash.toNumber()).to.equal(0);
    });

    it("lp_request_withdrawal rejects zero or more than the free shares (InsufficientShares)", async () => {
      const pos = await program.account.lpPosition.fetch(lpPos());
      await expectErr(request(new BN(0)), "InsufficientShares");
      await expectErr(request(pos.shares.addn(1)), "InsufficientShares");
    });

    const REQUEST = new BN(400_000_000);

    it("lp_request_withdrawal records the value at NAV_redeem and starts the delay", async () => {
      const before = await program.account.lpPool.fetch(m.lpPool);
      await request(REQUEST);
      const pos = await program.account.lpPosition.fetch(lpPos());
      const pool = await program.account.lpPool.fetch(m.lpPool);
      expect(pos.pendingShares.toString()).to.equal(REQUEST.toString());
      // floor(shares·max(NAV_redeem, 0) / lp_shares)
      expect(pos.requestedValue.toString()).to.equal(REQUEST.mul(navRedeem(before)).div(before.lpShares).toString());
      expect(pos.requestedAt.toNumber()).to.be.greaterThan(0);
      // Neutral params: no delay and no claim window
      expect(pos.unlockAt.toNumber()).to.equal(pos.requestedAt.toNumber());
      expect(pos.claimWindowSecs).to.equal(0);
      expect(pool.pendingWithdrawShares.toString()).to.equal(REQUEST.toString());
      // Pending shares stay in lp_shares until paid
      expect(pool.lpShares.toString()).to.equal(before.lpShares.toString());
    });

    it("lp_request_withdrawal allows one request at a time (WithdrawalPending)", async () => {
      await expectErr(request(new BN(100_000_000)), "WithdrawalPending");
    });

    it("lp_cancel_withdrawal clears the request", async () => {
      const shares = (await program.account.lpPosition.fetch(lpPos())).shares;
      await cancel();
      const pos = await program.account.lpPosition.fetch(lpPos());
      const pool = await program.account.lpPool.fetch(m.lpPool);
      expect(pos.pendingShares.toNumber()).to.equal(0);
      expect(pos.requestedValue.toNumber()).to.equal(0);
      expect(pos.requestedAt.toNumber()).to.equal(0);
      expect(pos.unlockAt.toNumber()).to.equal(0);
      expect(pos.claimWindowSecs).to.equal(0);
      expect(pos.shares.toString()).to.equal(shares.toString());
      expect(pool.pendingWithdrawShares.toNumber()).to.equal(0);
    });

    it("lp_cancel_withdrawal and lp_withdraw need a request (NoWithdrawalPending)", async () => {
      await expectErr(cancel(), "NoWithdrawalPending");
      await expectErr(withdraw(), "NoWithdrawalPending");
    });

    it("lp_withdraw pays min(NAV now, requested value) once the delay has passed", async () => {
      await request(REQUEST);
      const requested = await program.account.lpPosition.fetch(lpPos());
      const before = await program.account.lpPool.fetch(m.lpPool);
      const tokenBefore = await getAccount(provider.connection, lpToken);
      await withdraw();
      const pool = await program.account.lpPool.fetch(m.lpPool);
      const pos = await program.account.lpPosition.fetch(lpPos());
      const tokenAfter = await getAccount(provider.connection, lpToken);
      const nowValue = REQUEST.mul(navRedeem(before)).div(before.lpShares);
      const value = BN.min(nowValue, requested.requestedValue);
      expect(value.toNumber()).to.be.within(399_000_000, 400_000_000);
      expect((tokenAfter.amount - tokenBefore.amount).toString()).to.equal(value.toString());
      expect(before.cash.sub(pool.cash).toString()).to.equal(value.toString());
      expect(before.lpShares.sub(pool.lpShares).toString()).to.equal(REQUEST.toString());
      expect(pool.pendingWithdrawShares.toNumber()).to.equal(0);
      expect(pos.shares.toString()).to.equal(requested.shares.sub(REQUEST).toString());
      expect(pos.pendingShares.toNumber()).to.equal(0);
      expect(pos.requestedValue.toNumber()).to.equal(0);
      expect(pos.withdrawnCum.toString()).to.equal(value.toString());
      await expectBucketsBacked(m.market, m.vaultToken, m.lpPool);
    });

    it("another owner cannot use an LP's position", async () => {
      await expectErr(
        program.methods
          .lpCancelWithdrawal()
          .accounts({ owner: user1.publicKey, lpPool: m.lpPool, lpPosition: lpPos() })
          .signers([user1])
          .rpc(),
        "ConstraintSeeds"
      );
    });

    it("donate_to_pool adds vesting cash without minting shares", async () => {
      const donate = (amount: BN) =>
        program.methods
          .donateToPool(amount)
          .accounts({
            donor: authority.publicKey,
            market: m.market,
            lpPool: m.lpPool,
            vaultTokenAccount: m.vaultToken,
            donorTokenAccount: m.adminToken,
            tokenProgram: TOKEN_PROGRAM_ID,
          })
          .signers([authority])
          .rpc();
      await expectErr(donate(new BN(0)), "InvalidLpAmount");
      const before = await program.account.lpPool.fetch(m.lpPool);
      await donate(new BN(10_000_000));
      const pool = await program.account.lpPool.fetch(m.lpPool);
      expect(pool.cash.sub(before.cash).toNumber()).to.equal(10_000_000);
      expect(pool.lpShares.toString()).to.equal(before.lpShares.toString());
      expect(pool.donatedCum.toNumber()).to.equal(10_000_000);
      expect(pool.vestAmount.toNumber()).to.equal(10_000_000);
      expect(pool.vestEnd.toNumber()).to.be.greaterThan(pool.vestStart.toNumber());
      await expectBucketsBacked(m.market, m.vaultToken, m.lpPool);
    });

    it("sweep_insurance_excess, withdraw_treasury and write_off_bad_debt have nothing to do", async () => {
      await expectErr(
        program.methods
          .sweepInsuranceExcess()
          .accounts({ signer: lp.publicKey, market: m.market, lpPool: m.lpPool })
          .signers([lp])
          .rpc(),
        "NothingToSweep"
      );
      await expectErr(
        program.methods
          .withdrawTreasury(new BN(1))
          .accounts({
            admin: authority.publicKey,
            config: configPda,
            market: m.market,
            lpPool: m.lpPool,
            vault: m.vault,
            vaultTokenAccount: m.vaultToken,
            destination: m.adminToken,
            tokenProgram: TOKEN_PROGRAM_ID,
          })
          .signers([authority])
          .rpc(),
        "InsufficientTreasury"
      );
      await expectErr(
        program.methods
          .writeOffBadDebt()
          .accounts({ signer: lp.publicKey, market: m.market, lpPool: m.lpPool })
          .signers([lp])
          .rpc(),
        "AdlNotRequired"
      );
    });

    it("pool params: proposals are timelocked once seeded, and can be cancelled", async () => {
      const cancelParams = () =>
        program.methods
          .cancelPoolParams()
          .accounts({ admin: authority.publicKey, config: configPda, lpPool: m.lpPool })
          .signers([authority])
          .rpc();
      const apply = () =>
        program.methods.applyPoolParams().accounts({ market: m.market, lpPool: m.lpPool }).rpc();
      const propose = (params: PoolParams, admin = authority) =>
        provider.sendAndConfirm(
          new Transaction().add(buildProposePoolParamsIx(admin.publicKey, m.market, params, program.programId)),
          [admin]
        );

      await expectErr(cancelParams(), "NoPendingParams");
      await expectErr(apply(), "ParamsTimelocked");
      await expectErr(propose({ ...neutralPoolParams(), kNetBps: 100_001 }), "InvalidPoolParams");
      // D43: with the weekend bound on (W > 0), k_net·G ≤ α·10_000; 10× is far over it for any G ≥ 1%
      await expectErr(propose({ ...neutralPoolParams(), weekendCapWindowSecs: 86_400 }), "InvalidPoolParams");
      await expectErr(propose(neutralPoolParams(), user1), "Unauthorized");

      const t0 = await chainTime();
      await propose({ ...neutralPoolParams(), profitCloseQuietSecs: 300 });
      const pool = await program.account.lpPool.fetch(m.lpPool);
      // T = max(1 d, delay 0 + window 0 + 5 d margin) = 5 d
      const at = pool.pendingParamsAt.toNumber();
      expect(at - t0).to.be.within(432_000 - 5, 432_000 + 30);
      expect(pool.pendingParams.profitCloseQuietSecs).to.equal(300);
      expect(pool.params.profitCloseQuietSecs).to.equal(0);
      await expectErr(apply(), "ParamsTimelocked");
      await cancelParams();
      const cleared = await program.account.lpPool.fetch(m.lpPool);
      expect(cleared.pendingParamsAt.toNumber()).to.equal(0);
      expect(encodePoolParams(cleared.pendingParams as PoolParams).equals(Buffer.alloc(104))).to.equal(true);
    });

    it("initialize_pool runs once per market", async () => {
      await expectErr(
        initPoolIx(authority.publicKey, m.market, m.lpPool, neutralPoolParams()).then((ix) =>
          provider.sendAndConfirm(new Transaction().add(ix), [authority])
        ),
        "already in use"
      );
    });
  });

  describe("LP pool bootstrap and gates", () => {
    it("initialize_pool validates the params (InvalidPoolParams)", async () => {
      const mint = await createMint(provider.connection, authority, authority.publicKey, null, 6);
      const p = marketPdas(mint);
      await expectErr(
        initMarketIx(marketParams, mint, Keypair.generate().publicKey)
          .postInstructions([await initPoolIx(authority.publicKey, p.market, p.lpPool, { ...neutralPoolParams(), profitCloseQuietSecs: 3_601 })])
          .signers([authority])
          .rpc(),
        "InvalidPoolParams"
      );
    });

    it("lp_deposit needs a fresh oracle (StaleOraclePrice)", async () => {
      await expectErr(
        createPoolMarket({ feed: new PublicKey(oracles!.stale.pubkey) }),
        "StaleOraclePrice"
      );
    });

    describe("an unseeded pool", () => {
      let m: Awaited<ReturnType<typeof createPoolMarket>>;
      before(async () => {
        m = await createPoolMarket({ seed: new BN(0) });
      });

      it("refuses donations and sweeps until it has LPs (PoolHasNoLps)", async () => {
        await expectErr(
          program.methods
            .donateToPool(new BN(1))
            .accounts({
              donor: authority.publicKey,
              market: m.market,
              lpPool: m.lpPool,
              vaultTokenAccount: m.vaultToken,
              donorTokenAccount: m.adminToken,
              tokenProgram: TOKEN_PROGRAM_ID,
            })
            .signers([authority])
            .rpc(),
          "PoolHasNoLps"
        );
        await expectErr(
          program.methods
            .sweepInsuranceExcess()
            .accounts({ signer: authority.publicKey, market: m.market, lpPool: m.lpPool })
            .signers([authority])
            .rpc(),
          "PoolHasNoLps"
        );
      });

      it("applies proposed params at once before the seed", async () => {
        const params = { ...neutralPoolParams(), withdrawalDelaySecs: 3_600 };
        await provider.sendAndConfirm(
          new Transaction().add(buildProposePoolParamsIx(authority.publicKey, m.market, params, program.programId)),
          [authority]
        );
        const pool = await program.account.lpPool.fetch(m.lpPool);
        expect(pool.params.withdrawalDelaySecs).to.equal(3_600);
        expect(pool.pendingParamsAt.toNumber()).to.equal(0);
      });

      it("needs more than LP_DEAD_SHARES for the first deposit (InvalidLpAmount)", async () => {
        await provider.sendAndConfirm(new Transaction().add(await initLpPositionIx(authority.publicKey, m.lpPool)), [authority]);
        await expectErr(
          lpDepositIx(authority.publicKey, m, m.adminToken, m.feed, new BN(LP_DEAD_SHARES), new BN(0)).signers([authority]).rpc(),
          "InvalidLpAmount"
        );
      });

      it("locks LP_DEAD_SHARES on the seed", async () => {
        await lpDepositIx(authority.publicKey, m, m.adminToken, m.feed, SEED, SEED.subn(LP_DEAD_SHARES)).signers([authority]).rpc();
        const pool = await program.account.lpPool.fetch(m.lpPool);
        const pos = await program.account.lpPosition.fetch(lpPositionPda(m.lpPool, authority.publicKey));
        expect(pool.lpShares.toString()).to.equal(SEED.toString());
        expect(pos.shares.toString()).to.equal(SEED.subn(LP_DEAD_SHARES).toString());
      });

      it("holds a request until the delay has passed (WithdrawalDelayNotElapsed)", async () => {
        const owner = authority.publicKey;
        const lpPosition = lpPositionPda(m.lpPool, owner);
        await program.methods
          .lpRequestWithdrawal(new BN(1_000_000))
          .accounts({ owner, market: m.market, lpPool: m.lpPool, lpPosition, pythPriceFeed: m.feed })
          .signers([authority])
          .rpc();
        const pos = await program.account.lpPosition.fetch(lpPosition);
        expect(pos.unlockAt.toNumber() - pos.requestedAt.toNumber()).to.equal(3_600);
        await expectErr(
          program.methods
            .lpWithdraw()
            .accounts({
              owner,
              market: m.market,
              lpPool: m.lpPool,
              lpPosition,
              vault: m.vault,
              vaultTokenAccount: m.vaultToken,
              ownerTokenAccount: m.adminToken,
              pythPriceFeed: m.feed,
              tokenProgram: TOKEN_PROGRAM_ID,
            })
            .signers([authority])
            .rpc(),
          "WithdrawalDelayNotElapsed"
        );
      });

      it("refuses deposits while the market is paused (MarketPaused)", async () => {
        await program.methods
          .pauseMarket()
          .accounts({ signer: authority.publicKey, config: configPda, market: m.market })
          .signers([authority])
          .rpc();
        await expectErr(
          lpDepositIx(authority.publicKey, m, m.adminToken, m.feed, new BN(1_000_000), new BN(0)).signers([authority]).rpc(),
          "MarketPaused"
        );
      });
    });
  });

  describe("Market state tracking", () => {
    it("should verify vault balance tracking", async () => {
      const vault = await program.account.vault.fetch(vaultPda);
      expect(vault.totalDeposits.toNumber()).to.be.greaterThan(0);
      await expectBucketsBacked(marketPda, vaultTokenPda, lpPoolPda);
    });

    it("should verify market counters", async () => {
      const market = await program.account.market.fetch(marketPda);
      expect(market.totalPositions.toNumber()).to.be.greaterThanOrEqual(0);
      expect(market.totalTrades.toNumber()).to.be.greaterThanOrEqual(0);
    });
  });

  describe("Error conditions", () => {
    it("should define all error codes", () => {
      // programs/perps-core/src/errors.rs, in order from 6000; the LP pool
      // appended its 42 after ApprovalViaCpi (§12). Existing codes never move.
      const expectedErrors = [
        "MarketPaused",
        "ExcessiveLeverage",
        "InsufficientCollateral",
        "PositionTooSmall",
        "PositionTooLarge",
        "OpenInterestCapExceeded",
        "BelowMaintenanceMargin",
        "NotLiquidatable",
        "InvalidOraclePrice",
        "StaleOraclePrice",
        "InvalidPositionSide",
        "PositionNotFound",
        "PositionAlreadyClosed",
        "Unauthorized",
        "MathOverflow",
        "InvalidMarketConfig",
        "InvalidPositionReduction",
        "InsufficientVaultBalance",
        "InvalidReferralCode",
        "ReferralCodeExists",
        "ReferralCodeNotFound",
        "SelfReferralNotAllowed",
        "ReferralAlreadyApplied",
        "UserAlreadyHasCode",
        "NoRewardsToClaim",
        "ReferralCodeInactive",
        "InvalidReferralParams",
        "MarketClosed",
        "MarketSettled",
        "OracleConfidenceTooWide",
        "OraclePriceBelowFloor",
        "InvalidOracleAccount",
        "VenueUnauthorized",
        "VenueDisabled",
        "VenueProgramChanged",
        "VenueFillsPaused",
        "InvalidVenueParams",
        "InvalidFillParams",
        "FillPriceOutOfBand",
        "FillTooLarge",
        "SelfTrade",
        "MissingSigner",
        "ApprovalExpired",
        "ApprovalAllowanceExceeded",
        "ApprovalLeverageExceeded",
        "ApprovalLimitPrice",
        "ApprovalPriceDeviation",
        "InvalidApprovalParams",
        "VenueMarginInvariant",
        "ApprovalSideNotAllowed",
        "ApprovalViaCpi",
        // LP pool (§12)
        "InvalidPoolParams",
        "MarketNotFresh",
        "PoolExposureLimit",
        "PoolWeekendExposureLimit",
        "AccountExposureLimit",
        "PoolAdlPending",
        "PoolCashInsufficient",
        "PoolWithdrawalsBlocked",
        "PoolCapWouldBreach",
        "PoolCashCapExceeded",
        "PoolInsolvent",
        "PoolHasNoLps",
        "SlippageExceeded",
        "InvalidLpAmount",
        "InvalidLpClass",
        "InsufficientShares",
        "WithdrawalPending",
        "NoWithdrawalPending",
        "WithdrawalDelayNotElapsed",
        "NothingToSweep",
        "InsufficientTreasury",
        "NothingToSettle",
        "SettleWouldBankrupt",
        "InvalidAdlMode",
        "AdlUnauthorized",
        "AdlNotRequired",
        "AdlWrongSide",
        "AdlCandidateNotProfitable",
        "AdlScoreBelowFloor",
        "AdlOvershoot",
        "BadDebtNotStale",
        "FundingNotDue",
        "ParamsTimelocked",
        "NoPendingParams",
        "BucketUnderflow",
        "VaultInvariantViolated",
        "PoolDrawdownFreeze",
        "LpFlowWindowClosed",
        "FundingStale",
        "WithdrawalExpired",
        "MarketNotSettled",
        "ProfitCloseQuiet",
      ];
      expect(expectedErrors.length).to.equal(93);
      expect(expectedErrors.indexOf("InvalidPoolParams")).to.equal(51);
      expect(expectedErrors.length - 51).to.equal(42);

      // Against the IDL the deployed program was generated with
      const idlErrors = (program.idl.errors ?? []).map((e) => [e.code, e.name]);
      expect(idlErrors).to.deep.equal(expectedErrors.map((name, i) => [6000 + i, name]));
    });
  });
});

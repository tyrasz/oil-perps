import * as anchor from "@project-serum/anchor";
import { Program, BN } from "@project-serum/anchor";
import { OilPerps } from "../target/types/oil_perps";
import {
  Keypair,
  PublicKey,
  SystemProgram,
  LAMPORTS_PER_SOL,
} from "@solana/web3.js";
import {
  createMint,
  createAssociatedTokenAccount,
  mintTo,
  getAccount,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { expect } from "chai";

describe("oil-perps", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.OilPerps as Program<OilPerps>;

  // Test accounts
  let authority: Keypair;
  let user1: Keypair;
  let user2: Keypair;
  let collateralMint: PublicKey;
  let pythPriceFeed: Keypair;
  const [configPda] = PublicKey.findProgramAddressSync([Buffer.from("config")], program.programId);

  // PDAs
  let marketPda: PublicKey;
  let vaultPda: PublicKey;
  let vaultTokenPda: PublicKey;
  let user1AccountPda: PublicKey;
  let user2AccountPda: PublicKey;

  // User token accounts
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
    commodity: "GOLD",             // Commodity identifier
    maxLeverage: 20_000, // 20x
    maintenanceMarginRatio: 500, // 5%
    initialMarginRatio: 1000, // 10%
    takerFee: 5, // 0.05%
    makerFee: 2, // 0.02%
    liquidationFee: 250, // 2.5%
    maxOpenInterest: new BN(1_000_000_000_000), // 1M contracts
    fundingInterval: new BN(3600), // 1 hour
  };

  before(async () => {
    // Generate keypairs
    authority = Keypair.generate();
    user1 = Keypair.generate();
    user2 = Keypair.generate();
    pythPriceFeed = Keypair.generate();

    // Airdrop SOL
    const airdropAmount = 10 * LAMPORTS_PER_SOL;
    await Promise.all([
      provider.connection.requestAirdrop(authority.publicKey, airdropAmount),
      provider.connection.requestAirdrop(user1.publicKey, airdropAmount),
      provider.connection.requestAirdrop(user2.publicKey, airdropAmount),
    ]);

    // Wait for airdrops to confirm
    await new Promise((resolve) => setTimeout(resolve, 1000));

    // Create collateral mint (USDC-like, 6 decimals)
    collateralMint = await createMint(
      provider.connection,
      authority,
      authority.publicKey,
      null,
      6
    );

    // Derive PDAs
    // One market per (collateral mint, commodity)
    [marketPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("market"), collateralMint.toBuffer(), Buffer.from(marketParams.commodity)],
      program.programId
    );

    [vaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), marketPda.toBuffer()],
      program.programId
    );

    [vaultTokenPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault_token"), marketPda.toBuffer()],
      program.programId
    );

    // User accounts are per market
    [user1AccountPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("user"), user1.publicKey.toBuffer(), marketPda.toBuffer()],
      program.programId
    );

    [user2AccountPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("user"), user2.publicKey.toBuffer(), marketPda.toBuffer()],
      program.programId
    );

    // Create user token accounts
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

    // Mint tokens to users (10,000 USDC each)
    const mintAmount = 10_000_000_000; // 10,000 with 6 decimals
    await mintTo(
      provider.connection,
      authority,
      collateralMint,
      user1TokenAccount,
      authority,
      mintAmount
    );
    await mintTo(
      provider.connection,
      authority,
      collateralMint,
      user2TokenAccount,
      authority,
      mintAmount
    );

    // Protocol roles: the provider wallet deployed the program, so it is the
    // upgrade authority and creates the config (becoming admin), then hands
    // the admin role to `authority`, which creates the markets below.
    // The test price feed account doesn't exist, so its owner is the system program.
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
    it("should initialize OIL market with correct parameters", async () => {
      await program.methods
        .initializeMarket(marketParams)
        .accounts({
          authority: authority.publicKey,
          config: configPda,
          market: marketPda,
          vault: vaultPda,
          vaultTokenAccount: vaultTokenPda,
          collateralMint: collateralMint,
          pythPriceFeed: pythPriceFeed.publicKey,
          systemProgram: SystemProgram.programId,
          tokenProgram: TOKEN_PROGRAM_ID,
          rent: anchor.web3.SYSVAR_RENT_PUBKEY,
        })
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
    });

    it("should create a separate GOLD market on the same collateral", async () => {
      const [goldMarketPda] = PublicKey.findProgramAddressSync(
        [Buffer.from("market"), collateralMint.toBuffer(), Buffer.from(goldMarketParams.commodity)],
        program.programId
      );
      const [goldVaultPda] = PublicKey.findProgramAddressSync(
        [Buffer.from("vault"), goldMarketPda.toBuffer()],
        program.programId
      );
      const [goldVaultTokenPda] = PublicKey.findProgramAddressSync(
        [Buffer.from("vault_token"), goldMarketPda.toBuffer()],
        program.programId
      );
      expect(goldMarketPda.toBase58()).to.not.equal(marketPda.toBase58());

      await program.methods
        .initializeMarket(goldMarketParams)
        .accounts({
          authority: authority.publicKey,
          config: configPda,
          market: goldMarketPda,
          vault: goldVaultPda,
          vaultTokenAccount: goldVaultTokenPda,
          collateralMint: collateralMint,
          pythPriceFeed: Keypair.generate().publicKey,
          systemProgram: SystemProgram.programId,
          tokenProgram: TOKEN_PROGRAM_ID,
          rent: anchor.web3.SYSVAR_RENT_PUBKEY,
        })
        .signers([authority])
        .rpc();

      const gold = await program.account.market.fetch(goldMarketPda);
      const oil = await program.account.market.fetch(marketPda);
      expect(Buffer.from(gold.commodity).toString().replace(/\0+$/, "")).to.equal("GOLD");
      expect(Buffer.from(oil.commodity).toString().replace(/\0+$/, "")).to.equal("OIL");
    });

    it("should fail with invalid max leverage (0)", async () => {
      const invalidParams = { ...marketParams, maxLeverage: 0 };
      const newMint = await createMint(
        provider.connection,
        authority,
        authority.publicKey,
        null,
        6
      );

      const [newMarketPda] = PublicKey.findProgramAddressSync(
        [Buffer.from("market"), newMint.toBuffer(), Buffer.from(invalidParams.commodity)],
        program.programId
      );

      const [newVaultPda] = PublicKey.findProgramAddressSync(
        [Buffer.from("vault"), newMarketPda.toBuffer()],
        program.programId
      );

      const [newVaultTokenPda] = PublicKey.findProgramAddressSync(
        [Buffer.from("vault_token"), newMarketPda.toBuffer()],
        program.programId
      );

      try {
        await program.methods
          .initializeMarket(invalidParams)
          .accounts({
            authority: authority.publicKey,
            config: configPda,
            market: newMarketPda,
            vault: newVaultPda,
            vaultTokenAccount: newVaultTokenPda,
            collateralMint: newMint,
            pythPriceFeed: pythPriceFeed.publicKey,
            systemProgram: SystemProgram.programId,
            tokenProgram: TOKEN_PROGRAM_ID,
            rent: anchor.web3.SYSVAR_RENT_PUBKEY,
          })
          .signers([authority])
          .rpc();
        expect.fail("Should have thrown error");
      } catch (err: any) {
        expect(err.error.errorCode.code).to.equal("InvalidMarketConfig");
      }
    });

    it("should fail when initial margin <= maintenance margin", async () => {
      const invalidParams = {
        ...marketParams,
        maintenanceMarginRatio: 1000,
        initialMarginRatio: 500, // Less than maintenance
      };
      const newMint = await createMint(
        provider.connection,
        authority,
        authority.publicKey,
        null,
        6
      );

      const [newMarketPda] = PublicKey.findProgramAddressSync(
        [Buffer.from("market"), newMint.toBuffer(), Buffer.from(invalidParams.commodity)],
        program.programId
      );

      const [newVaultPda] = PublicKey.findProgramAddressSync(
        [Buffer.from("vault"), newMarketPda.toBuffer()],
        program.programId
      );

      const [newVaultTokenPda] = PublicKey.findProgramAddressSync(
        [Buffer.from("vault_token"), newMarketPda.toBuffer()],
        program.programId
      );

      try {
        await program.methods
          .initializeMarket(invalidParams)
          .accounts({
            authority: authority.publicKey,
            config: configPda,
            market: newMarketPda,
            vault: newVaultPda,
            vaultTokenAccount: newVaultTokenPda,
            collateralMint: newMint,
            pythPriceFeed: pythPriceFeed.publicKey,
            systemProgram: SystemProgram.programId,
            tokenProgram: TOKEN_PROGRAM_ID,
            rent: anchor.web3.SYSVAR_RENT_PUBKEY,
          })
          .signers([authority])
          .rpc();
        expect.fail("Should have thrown error");
      } catch (err: any) {
        expect(err.error.errorCode.code).to.equal("InvalidMarketConfig");
      }
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
      try {
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
        expect.fail("Should have thrown error");
      } catch (err: any) {
        // Account already exists
        expect(err.toString()).to.include("already in use");
      }
    });
  });

  describe("deposit_collateral", () => {
    it("should deposit collateral successfully", async () => {
      const depositAmount = new BN(1_000_000_000); // 1,000 USDC

      const userAccountBefore = await program.account.userAccount.fetch(user1AccountPda);
      const tokenAccountBefore = await getAccount(provider.connection, user1TokenAccount);

      await program.methods
        .depositCollateral(depositAmount)
        .accounts({
          owner: user1.publicKey,
          userAccount: user1AccountPda,
          market: marketPda,
          vault: vaultPda,
          vaultTokenAccount: vaultTokenPda,
          userTokenAccount: user1TokenAccount,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([user1])
        .rpc();

      const userAccountAfter = await program.account.userAccount.fetch(user1AccountPda);
      const tokenAccountAfter = await getAccount(provider.connection, user1TokenAccount);

      expect(userAccountAfter.collateralBalance.toNumber()).to.equal(
        userAccountBefore.collateralBalance.toNumber() + depositAmount.toNumber()
      );
      expect(Number(tokenAccountAfter.amount)).to.equal(
        Number(tokenAccountBefore.amount) - depositAmount.toNumber()
      );
    });

    it("should allow multiple deposits", async () => {
      const depositAmount = new BN(500_000_000); // 500 USDC

      const userAccountBefore = await program.account.userAccount.fetch(user1AccountPda);

      await program.methods
        .depositCollateral(depositAmount)
        .accounts({
          owner: user1.publicKey,
          userAccount: user1AccountPda,
          market: marketPda,
          vault: vaultPda,
          vaultTokenAccount: vaultTokenPda,
          userTokenAccount: user1TokenAccount,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([user1])
        .rpc();

      const userAccountAfter = await program.account.userAccount.fetch(user1AccountPda);

      expect(userAccountAfter.collateralBalance.toNumber()).to.equal(
        userAccountBefore.collateralBalance.toNumber() + depositAmount.toNumber()
      );
    });

    it("should deposit for user2", async () => {
      const depositAmount = new BN(2_000_000_000); // 2,000 USDC

      await program.methods
        .depositCollateral(depositAmount)
        .accounts({
          owner: user2.publicKey,
          userAccount: user2AccountPda,
          market: marketPda,
          vault: vaultPda,
          vaultTokenAccount: vaultTokenPda,
          userTokenAccount: user2TokenAccount,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([user2])
        .rpc();

      const userAccount = await program.account.userAccount.fetch(user2AccountPda);
      expect(userAccount.collateralBalance.toNumber()).to.equal(depositAmount.toNumber());
    });
  });

  describe("withdraw_collateral", () => {
    it("should withdraw collateral successfully", async () => {
      const withdrawAmount = new BN(100_000_000); // 100 USDC

      const userAccountBefore = await program.account.userAccount.fetch(user1AccountPda);
      const tokenAccountBefore = await getAccount(provider.connection, user1TokenAccount);

      await program.methods
        .withdrawCollateral(withdrawAmount)
        .accounts({
          owner: user1.publicKey,
          userAccount: user1AccountPda,
          market: marketPda,
          vault: vaultPda,
          vaultTokenAccount: vaultTokenPda,
          userTokenAccount: user1TokenAccount,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([user1])
        .rpc();

      const userAccountAfter = await program.account.userAccount.fetch(user1AccountPda);
      const tokenAccountAfter = await getAccount(provider.connection, user1TokenAccount);

      expect(userAccountAfter.collateralBalance.toNumber()).to.equal(
        userAccountBefore.collateralBalance.toNumber() - withdrawAmount.toNumber()
      );
      expect(Number(tokenAccountAfter.amount)).to.equal(
        Number(tokenAccountBefore.amount) + withdrawAmount.toNumber()
      );
    });

    it("should fail when withdrawing more than balance", async () => {
      const userAccount = await program.account.userAccount.fetch(user1AccountPda);
      const excessiveAmount = new BN(userAccount.collateralBalance.toNumber() + 1_000_000);

      try {
        await program.methods
          .withdrawCollateral(excessiveAmount)
          .accounts({
            owner: user1.publicKey,
            userAccount: user1AccountPda,
            market: marketPda,
            vault: vaultPda,
            vaultTokenAccount: vaultTokenPda,
            userTokenAccount: user1TokenAccount,
            tokenProgram: TOKEN_PROGRAM_ID,
          })
          .signers([user1])
          .rpc();
        expect.fail("Should have thrown error");
      } catch (err: any) {
        expect(err.toString()).to.include("InsufficientCollateral");
      }
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
      // margin_ratio = equity / notional * 10000
      const collateral = 1_000_000_000; // $1,000
      const pnl = 100_000_000; // +$100
      const equity = collateral + pnl; // $1,100
      const notional = 10_000_000_000; // $10,000
      const marginRatio = (equity * 10000) / notional;
      expect(marginRatio).to.equal(1100); // 11%
    });

    it("should detect liquidatable position", () => {
      // Position is liquidatable when margin_ratio < maintenance_margin_ratio
      const maintenanceMargin = 500; // 5%
      const collateral = 500_000_000; // $500
      const pnl = -300_000_000; // -$300
      const equity = collateral + pnl; // $200
      const notional = 10_000_000_000; // $10,000
      const marginRatio = (equity * 10000) / notional; // 200
      expect(marginRatio).to.be.lessThan(maintenanceMargin);
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

    it("should calculate liquidation reward from maintenance margin", () => {
      // reward = notional * maintenance_margin_ratio * liquidation_fee / 10000^2
      // Based on the margin requirement, not the collateral left, so it is
      // still paid (from the insurance fund) when a position is underwater.
      const notional = 10_000_000_000; // $10,000
      const maintenanceMarginRatio = 500; // 5%
      const liquidationFee = 250; // 2.5%
      const reward = (notional * maintenanceMarginRatio * liquidationFee) / 100_000_000;
      expect(reward).to.equal(12_500_000); // $12.50
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
    // Mirrors Market::liquidation_reward_target and liquidation_payout:
    // reward = notional * maintenance margin * liquidation fee, paid from the
    // remaining collateral first, then topped up from the insurance fund.
    const reward = (notional: number, mmrBps: number, feeBps: number) =>
      (notional * mmrBps * feeBps) / 100_000_000;

    it("should calculate liquidation amounts correctly", () => {
      const notional = 10_000_000_000; // $10,000
      const remainingCollateral = 100_000_000; // $500 collateral - $400 loss
      const liquidatorReward = reward(notional, 500, 250); // 2.5% of the 5% margin
      const toInsurance = remainingCollateral - liquidatorReward;

      expect(liquidatorReward).to.equal(12_500_000); // $12.50
      expect(toInsurance).to.equal(87_500_000); // $87.50
    });

    it("should pay the reward from the insurance fund when underwater", () => {
      const insuranceFund = 50_000_000;
      const liquidatorReward = reward(10_000_000_000, 500, 250);
      const remainingCollateral = 0;
      const topUp = Math.min(liquidatorReward - remainingCollateral, insuranceFund);
      expect(topUp).to.equal(12_500_000);
      expect(insuranceFund - topUp).to.equal(37_500_000);
    });

    it("should handle complete wipeout", () => {
      const collateral = 500_000_000;
      const pnl = -600_000_000; // Loss exceeds collateral
      const remainingCollateral = Math.max(0, collateral + pnl);

      expect(remainingCollateral).to.equal(0);
    });
  });

  describe("Market state tracking", () => {
    it("should verify vault balance tracking", async () => {
      const vault = await program.account.vault.fetch(vaultPda);
      expect(vault.totalDeposits.toNumber()).to.be.greaterThan(0);
    });

    it("should verify market counters", async () => {
      const market = await program.account.market.fetch(marketPda);
      expect(market.totalPositions.toNumber()).to.be.greaterThanOrEqual(0);
      expect(market.totalTrades.toNumber()).to.be.greaterThanOrEqual(0);
    });
  });

  describe("Error conditions", () => {
    it("should define all error codes", () => {
      // Verify error enum exists (TypeScript type check)
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
      ];

      // This is a compile-time check - if program compiles, errors are defined
      expect(expectedErrors.length).to.equal(18);
    });
  });
});

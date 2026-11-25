import * as anchor from '@coral-xyz/anchor';
import { Program } from '@coral-xyz/anchor';
import { PublicKey, Keypair, Connection } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID, getAccount, getAssociatedTokenAddressSync } from '@solana/spl-token';
import * as fs from 'fs';
import * as path from 'path';

// Program ID
const PERPS_CORE_PROGRAM_ID = new PublicKey('Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS');
const BPF_LOADER_UPGRADEABLE = new PublicKey('BPFLoaderUpgradeab1e11111111111111111111111');

function getConfigPDA(): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([Buffer.from('config')], PERPS_CORE_PROGRAM_ID);
}

function getProgramDataPDA(): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([PERPS_CORE_PROGRAM_ID.toBuffer()], BPF_LOADER_UPGRADEABLE);
}

// USDC Mint (Testnet)
const USDC_MINT = new PublicKey('4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU');

// Pyth Price Feeds (Testnet)
const PYTH_PRICE_FEEDS: Record<string, PublicKey> = {
  OIL: new PublicKey('GVXRSBjFk6e6J3NbVPXohDJetcTjaeeuykUpbQF8UoMU'),
  GOLD: new PublicKey('sXgHcPCNsXM8KaC3CXNXQS8qprLR4dVxQyJyxNmBsLR'),
  SILVER: new PublicKey('77JipqJaP9LPFyEGjT2zqz5qxL6KBx3nVtbMd1PPdPr9'),
  NATGAS: new PublicKey('DBE3N8uNjhKPNAR4oJT8vKwZQN5yDsRXGBCQu4k3Gfgr'),
  COPPER: new PublicKey('4wxQsP2B7HNyH4sH3n2J1oKeW6vPExU6mBLgPswN8pqZ'),
};

// Market configurations per commodity
const MARKET_CONFIGS: Record<string, {
  maxLeverage: number;
  maintenanceMarginRatio: number;
  initialMarginRatio: number;
  takerFee: number;
  makerFee: number;
  liquidationFee: number;
  maxOpenInterest: number;
  fundingInterval: number;
}> = {
  OIL: {
    maxLeverage: 20000, // 20x (3 decimals)
    maintenanceMarginRatio: 500, // 5% (basis points)
    initialMarginRatio: 1000, // 10% (basis points) -> 10x effective max leverage
    takerFee: 50, // 0.5% (basis points)
    makerFee: 20, // 0.2% (basis points)
    liquidationFee: 100, // 1% of maintenance margin (basis points)
    maxOpenInterest: 10_000_000_000_000, // 10M contracts per side (6 decimals; D6)
    fundingInterval: 3600, // 1 hour
  },
  GOLD: {
    maxLeverage: 20000,
    maintenanceMarginRatio: 500,
    initialMarginRatio: 1000,
    takerFee: 50,
    makerFee: 20,
    liquidationFee: 100,
    maxOpenInterest: 10_000_000_000_000,
    fundingInterval: 3600,
  },
  SILVER: {
    maxLeverage: 15000, // 15x
    maintenanceMarginRatio: 600,
    initialMarginRatio: 1200,
    takerFee: 50,
    makerFee: 20,
    liquidationFee: 100,
    maxOpenInterest: 5_000_000_000_000,
    fundingInterval: 3600,
  },
  NATGAS: {
    maxLeverage: 10000, // 10x
    maintenanceMarginRatio: 800,
    initialMarginRatio: 1500,
    takerFee: 60,
    makerFee: 30,
    liquidationFee: 150,
    maxOpenInterest: 5_000_000_000_000,
    fundingInterval: 3600,
  },
  COPPER: {
    maxLeverage: 15000,
    maintenanceMarginRatio: 600,
    initialMarginRatio: 1200,
    takerFee: 50,
    makerFee: 20,
    liquidationFee: 100,
    maxOpenInterest: 5_000_000_000_000,
    fundingInterval: 3600,
  },
};

// Risk controls applied with update_market_config (see programs/perps-core/src/state/market.rs).
// Margin ratios are basis points (1000 = 10%); prices are 6 decimals.
const SCHEDULE_CME_GLOBEX = 1; // Sun 18:00 - Fri 17:00 ET, daily break 17:00-18:00 ET
const PRE_CLOSE_WINDOW_SECS = 2 * 3600; // weekend margins apply 2h before Friday's close
const MAX_CONFIDENCE_BPS = 100; // reject oracle prices with confidence wider than 1%

const RISK_CONFIGS: Record<string, {
  weekendInitialMarginRatio: number;
  weekendMaintenanceMarginRatio: number;
  minOraclePrice: number;
}> = {
  // Weekend margins are double the normal ones. The price floor (~10% of a
  // typical price) halts trading for review before prices approach zero, as
  // WTI did in April 2020; the authority can then settle the market.
  OIL: { weekendInitialMarginRatio: 2000, weekendMaintenanceMarginRatio: 1000, minOraclePrice: 7_500_000 },
  GOLD: { weekendInitialMarginRatio: 2000, weekendMaintenanceMarginRatio: 1000, minOraclePrice: 200_000_000 },
  SILVER: { weekendInitialMarginRatio: 2400, weekendMaintenanceMarginRatio: 1200, minOraclePrice: 2_400_000 },
  NATGAS: { weekendInitialMarginRatio: 3000, weekendMaintenanceMarginRatio: 1600, minOraclePrice: 250_000 },
  COPPER: { weekendInitialMarginRatio: 2400, weekendMaintenanceMarginRatio: 1200, minOraclePrice: 420_000 },
};

// PDA derivation functions
// One market per (collateral mint, commodity)
function getMarketPDA(collateralMint: PublicKey, commodity: string): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from('market'), collateralMint.toBuffer(), Buffer.from(commodity)],
    PERPS_CORE_PROGRAM_ID
  );
}

function getVaultPDA(market: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from('vault'), market.toBuffer()],
    PERPS_CORE_PROGRAM_ID
  );
}

function getVaultTokenAccountPDA(market: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from('vault_token'), market.toBuffer()],
    PERPS_CORE_PROGRAM_ID
  );
}

function getLpPoolPDA(market: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([Buffer.from('lp_pool'), market.toBuffer()], PERPS_CORE_PROGRAM_ID);
}

function getLpPositionPDA(lpPool: PublicKey, owner: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from('lp_position'), lpPool.toBuffer(), owner.toBuffer(), Buffer.from([LP_CLASS_COMMON])],
    PERPS_CORE_PROGRAM_ID
  );
}

// LP pool, the house counterparty (docs/lp-pool.md). No trading until it has
// LPs (D11): initialize_market, initialize_pool, init_lp_position and the
// protocol's seed lp_deposit go in one transaction.
const LP_CLASS_COMMON = 0;
const LP_DEAD_SHARES = 1_000_000; // locked for ever by the first deposit (§9.1)
// Protocol seed in USDC (6 decimals); LP_SEED_USDC overrides. The admin's
// USDC token account must hold it.
const LP_SEED = new anchor.BN(process.env.LP_SEED_USDC ?? '1000000').mul(new anchor.BN(1_000_000));

// PoolParams::production() (programs/perps-core/src/state/pool.rs, §15.1).
// Changing them later is timelocked (propose_pool_params / apply_pool_params).
function productionPoolParams() {
  return {
    skewScale: new anchor.BN('2500000000000'),
    insuranceTarget: new anchor.BN('250000000000'),
    insuranceClaimCapPerWindow: new anchor.BN(0),
    liqMinRemainingNotional: new anchor.BN(100_000_000),
    // 0.4×: 20% below α/G = 0.5× (G 20%), the weekend bound's edge (D43), so
    // LPs keep a 20% exit buffer with traders at the net cap (D45)
    kNetBps: 4_000,
    hardNetBps: 6_000,
    perAccountCapBps: 2_500,
    weekendLpLossBps: 1_000,
    weekendGapBps: 0,
    weekendCapWindowSecs: 86_400,
    withdrawalDelaySecs: 345_600,
    withdrawalClaimWindowSecs: 259_200,
    lpReopenQuietSecs: 1_800,
    adlNetTargetBps: 2_000,
    adlExposureScoreFloorBps: 0,
    ddFreezeBps: 1_000,
    ddAdlBps: 1_500,
    ddWithdrawBlockBps: 0,
    redeemLossHaircutBps: 5_000,
    openFeeBps: 5,
    maxImpactBps: 100,
    liqPenaltyBps: 200,
    liqKeeperShareBps: 2_000,
    backstopThresholdBps: 6_667,
    minLiqSliceBps: 2_000,
    treasuryFeeBps: 1_000,
    lpLossCapWindowBps: 500,
    profitCloseQuietSecs: 300, // D23
  };
}

// The LP exit buffer every market keeps at the net cap (D45): k_net·G ≤
// (1 − buffer)·α, so with traders at the cap LPs can still withdraw about
// this share of B (at constant price and B) before the weekend withdraw line
// G·net ≤ α·B' binds. Production OIL: 1 − 4_000·2_000/(1_000·10_000) = 20%.
const EXIT_BUFFER_BPS = 2_000;

/**
 * Production params for one commodity. With the weekend bound on, validate
 * needs `k_net·G ≤ α·10_000` (D43), G = max(IM, weekend IM): the pool is
 * created before applyRiskConfig sets the weekend IM, and the always-on
 * weekend cap would then bind below k_net anyway. Every market keeps the same
 * 20% exit buffer below that edge (D45): k_net = floor((1 − buffer)·α·10_000/G),
 * capped at production's (OIL and GOLD, G 20%, get exactly
 * PoolParams::production()), with hard_net and the drawdown-ADL target kept
 * at 1.5× and 0.5× of it as in production. Flooring keeps k_net·G below the
 * edge, and adl ≤ k_net ≤ hard_net.
 */
function poolParamsFor(commodity: string) {
  const p = productionPoolParams();
  const g = Math.max(MARKET_CONFIGS[commodity]?.initialMarginRatio ?? 0, RISK_CONFIGS[commodity]?.weekendInitialMarginRatio ?? 0);
  if (g > 0) {
    const kNet = Math.min(p.kNetBps, Math.floor((p.weekendLpLossBps * (10_000 - EXIT_BUFFER_BPS)) / g));
    if (kNet < p.kNetBps) {
      p.kNetBps = kNet;
      p.hardNetBps = Math.floor((kNet * 3) / 2);
      p.adlNetTargetBps = Math.floor(kNet / 2);
    }
  }
  return p;
}

// Seed failures that a later run can retry (session, quiet time, oracle, funding)
const RETRYABLE_SEED_ERRORS = [
  'MarketClosed', 'LpFlowWindowClosed', 'FundingStale', 'StaleOraclePrice', 'InvalidOraclePrice',
  'OracleConfidenceTooWide', 'OraclePriceBelowFloor',
];

function anchorErrorCode(error: unknown): string | undefined {
  const e = error as { error?: { errorCode?: { code?: string } }; logs?: string[]; message?: string };
  if (e?.error?.errorCode?.code) return e.error.errorCode.code;
  const text = `${e?.message ?? ''}\n${(e?.logs ?? []).join('\n')}`;
  return RETRYABLE_SEED_ERRORS.find((code) => text.includes(code));
}

function adlKeeper(authority: Keypair): PublicKey {
  return process.env.ADL_KEEPER ? new PublicKey(process.env.ADL_KEEPER) : authority.publicKey;
}

/**
 * The pool instructions that follow initialize_market (or that a later run
 * sends for a market whose pool is missing or unseeded): initialize_pool
 * unless it exists, then init_lp_position unless it exists and the seed
 * lp_deposit if the admin holds LP_SEED.
 */
async function poolBootstrapIxs(
  program: Program,
  connection: Connection,
  authority: Keypair,
  marketPda: PublicKey,
  pythPriceFeed: PublicKey,
  opts: { createPool: boolean; withSeed: boolean; commodity: string }
): Promise<{ ixs: anchor.web3.TransactionInstruction[]; seeded: boolean }> {
  const [lpPool] = getLpPoolPDA(marketPda);
  const [lpPosition] = getLpPositionPDA(lpPool, authority.publicKey);
  const ixs: anchor.web3.TransactionInstruction[] = [];
  if (opts.createPool) {
    ixs.push(
      await program.methods
        .initializePool(poolParamsFor(opts.commodity), adlKeeper(authority))
        .accounts({
          admin: authority.publicKey,
          config: getConfigPDA()[0],
          market: marketPda,
          lpPool,
          systemProgram: anchor.web3.SystemProgram.programId,
        })
        .instruction()
    );
  }
  if (!opts.withSeed) return { ixs, seeded: false };

  const adminUsdc = getAssociatedTokenAddressSync(USDC_MINT, authority.publicKey);
  let balance = BigInt(0);
  try {
    balance = (await getAccount(connection, adminUsdc)).amount;
  } catch {
    // no token account
  }
  if (balance < BigInt(LP_SEED.toString())) {
    console.warn(`Admin USDC ${adminUsdc} holds ${balance}, less than the LP seed ${LP_SEED}: the pool is not seeded, so no trading is possible yet.`);
    console.warn('Fund it (or set LP_SEED_USDC) and re-run this script to seed.');
    return { ixs, seeded: false };
  }
  if (!(await connection.getAccountInfo(lpPosition))) {
    ixs.push(
      await program.methods
        .initLpPosition(LP_CLASS_COMMON)
        .accounts({
          owner: authority.publicKey,
          lpPool,
          lpPosition,
          systemProgram: anchor.web3.SystemProgram.programId,
        })
        .instruction()
    );
  }
  ixs.push(
    await program.methods
      .lpDeposit(LP_SEED, LP_SEED.subn(LP_DEAD_SHARES))
      .accounts({
        owner: authority.publicKey,
        market: marketPda,
        lpPool,
        lpPosition,
        vaultTokenAccount: getVaultTokenAccountPDA(marketPda)[0],
        ownerTokenAccount: adminUsdc,
        pythPriceFeed,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .instruction()
  );
  return { ixs, seeded: true };
}

/** For an existing market: create its pool if missing and seed it if it has no LPs. */
async function ensurePool(
  program: Program,
  connection: Connection,
  authority: Keypair,
  commodity: string
): Promise<void> {
  const [marketPda] = getMarketPDA(USDC_MINT, commodity);
  const [lpPool] = getLpPoolPDA(marketPda);
  const info = await connection.getAccountInfo(lpPool);
  const exists = !!info && info.owner.equals(PERPS_CORE_PROGRAM_ID);
  if (exists) {
    const pool = (await program.account.lpPool.fetch(lpPool)) as { lpShares: anchor.BN };
    if (!pool.lpShares.isZero()) {
      console.log(`LP pool ${lpPool} is seeded (${pool.lpShares.toString()} shares)`);
      return;
    }
  }
  console.log(exists ? `LP pool ${lpPool} has no LPs yet: seeding` : `Market has no LP pool: creating ${lpPool}`);
  const { ixs, seeded } = await poolBootstrapIxs(program, connection, authority, marketPda, PYTH_PRICE_FEEDS[commodity], {
    createPool: !exists,
    withSeed: true,
    commodity,
  });
  if (ixs.length === 0) return;
  const tx = new anchor.web3.Transaction().add(...ixs);
  try {
    const sig = await (program.provider as anchor.AnchorProvider).sendAndConfirm(tx, [authority]);
    console.log(`LP pool ${seeded ? 'seeded' : 'created'}: ${sig}`);
  } catch (error) {
    const code = anchorErrorCode(error);
    if (exists && code && RETRYABLE_SEED_ERRORS.includes(code)) {
      console.warn(`The seed lp_deposit cannot go now (${code}): re-run in session, outside the reopen quiet time, with a fresh oracle.`);
      return;
    }
    // initialize_pool needs a fresh market (no open interest, empty insurance
    // fund, not settled): an older market cannot be migrated (redeploy)
    console.error('LP pool setup failed (MarketNotFresh means the market traded before the pool existed):', error);
    throw error;
  }
}

async function loadKeypair(): Promise<Keypair> {
  const keypairPath = process.env.ANCHOR_WALLET ||
    path.join(process.env.HOME || '', '.config/solana/id.json');

  const keypairData = JSON.parse(fs.readFileSync(keypairPath, 'utf-8'));
  return Keypair.fromSecretKey(Uint8Array.from(keypairData));
}

async function initializeMarket(
  program: Program,
  authority: Keypair,
  commodity: string
): Promise<string> {
  const config = MARKET_CONFIGS[commodity];
  if (!config) {
    throw new Error(`Unknown commodity: ${commodity}`);
  }

  const pythPriceFeed = PYTH_PRICE_FEEDS[commodity];
  if (!pythPriceFeed) {
    throw new Error(`No Pyth price feed for commodity: ${commodity}`);
  }

  const [marketPda] = getMarketPDA(USDC_MINT, commodity);
  const [vaultPda] = getVaultPDA(marketPda);
  const [vaultTokenAccountPda] = getVaultTokenAccountPDA(marketPda);

  console.log(`Initializing market for ${commodity}...`);
  console.log(`  Market PDA: ${marketPda.toString()}`);
  console.log(`  Vault PDA: ${vaultPda.toString()}`);
  console.log(`  Vault Token Account: ${vaultTokenAccountPda.toString()}`);
  console.log(`  Pyth Price Feed: ${pythPriceFeed.toString()}`);

  const params = {
    commodity,
    maxLeverage: config.maxLeverage,
    maintenanceMarginRatio: config.maintenanceMarginRatio,
    initialMarginRatio: config.initialMarginRatio,
    takerFee: config.takerFee,
    makerFee: config.makerFee,
    liquidationFee: config.liquidationFee,
    maxOpenInterest: new anchor.BN(config.maxOpenInterest),
    fundingInterval: new anchor.BN(config.fundingInterval),
  };

  const initMarket = () =>
    program.methods
      .initializeMarket(params)
      .accounts({
        authority: authority.publicKey,
        config: getConfigPDA()[0],
        market: marketPda,
        vault: vaultPda,
        vaultTokenAccount: vaultTokenAccountPda,
        collateralMint: USDC_MINT,
        pythPriceFeed,
        systemProgram: anchor.web3.SystemProgram.programId,
        tokenProgram: TOKEN_PROGRAM_ID,
        rent: anchor.web3.SYSVAR_RENT_PUBKEY,
      })
      .signers([authority]);

  // One transaction (D11): market, LP pool, the admin's LP position and the
  // protocol's seed deposit. The seed needs a fresh oracle; if it cannot go
  // now, the market and pool are created without it and a re-run seeds it.
  const connection = program.provider.connection;
  const withSeed = await poolBootstrapIxs(program, connection, authority, marketPda, pythPriceFeed, {
    createPool: true,
    withSeed: true,
    commodity,
  });
  let tx: string;
  try {
    tx = await initMarket().postInstructions(withSeed.ixs).rpc();
  } catch (error) {
    const code = anchorErrorCode(error);
    if (!withSeed.seeded || !code || !RETRYABLE_SEED_ERRORS.includes(code)) throw error;
    console.warn(`The seed lp_deposit failed (${code}): creating the market and pool without it. Re-run this script to seed.`);
    const poolOnly = await poolBootstrapIxs(program, connection, authority, marketPda, pythPriceFeed, {
      createPool: true,
      withSeed: false,
      commodity,
    });
    tx = await initMarket().postInstructions(poolOnly.ixs).rpc();
    withSeed.seeded = false;
  }

  console.log(`Market and LP pool initialized${withSeed.seeded ? ` and seeded with ${LP_SEED.toString()} USDC units` : ' (not seeded)'}! Transaction: ${tx}`);
  return tx;
}

async function applyRiskConfig(
  program: Program,
  authority: Keypair,
  commodity: string,
  isPaused: boolean
): Promise<string> {
  const risk = RISK_CONFIGS[commodity];
  if (!risk) {
    throw new Error(`No risk config for commodity: ${commodity}`);
  }
  const [marketPda] = getMarketPDA(USDC_MINT, commodity);

  const params = {
    // update_market_config also sets the pause flag: keep whatever it is now
    isPaused,
    sessionSchedule: SCHEDULE_CME_GLOBEX,
    preCloseWindowSecs: PRE_CLOSE_WINDOW_SECS,
    weekendInitialMarginRatio: risk.weekendInitialMarginRatio,
    weekendMaintenanceMarginRatio: risk.weekendMaintenanceMarginRatio,
    maxConfidenceBps: MAX_CONFIDENCE_BPS,
    minOraclePrice: new anchor.BN(risk.minOraclePrice),
    // Per-side contracts cap (D6); 0 keeps the current value (§4.12)
    maxOpenInterest: new anchor.BN(0),
  };
  console.log(`Applying risk config for ${commodity}:`, { ...params, minOraclePrice: risk.minOraclePrice });

  const tx = await program.methods
    .updateMarketConfig(params)
    .accounts({ authority: authority.publicKey, config: getConfigPDA()[0], market: marketPda })
    .signers([authority])
    .rpc();

  console.log(`Risk config applied! Transaction: ${tx}`);
  return tx;
}

/**
 * One-time protocol setup. Must be signed by the program's upgrade authority,
 * which becomes the admin (hand over later with propose_admin/accept_admin).
 * The guardian (pause-only) comes from GUARDIAN; the Pyth oracle program from
 * PYTH_PROGRAM_ID (required: it is what price feeds are checked against, so it
 * must not be read off a feed account).
 */
async function ensureProtocolConfig(
  program: Program,
  connection: Connection,
  authority: Keypair,
  commodity: string
): Promise<void> {
  const [configPda] = getConfigPDA();
  const existing = await connection.getAccountInfo(configPda);
  if (existing && existing.owner.equals(PERPS_CORE_PROGRAM_ID)) {
    const config = (await program.account.protocolConfig.fetch(configPda)) as {
      admin: PublicKey; guardian: PublicKey; oracleProgram: PublicKey;
    };
    console.log(`Protocol config: admin ${config.admin}, guardian ${config.guardian}, oracle program ${config.oracleProgram}`);
    if (!config.admin.equals(authority.publicKey)) {
      throw new Error(`This wallet (${authority.publicKey}) is not the protocol admin (${config.admin})`);
    }
    return;
  }

  const oracleProgram = requiredPythProgram();
  await assertPythFeed(connection, PYTH_PRICE_FEEDS[commodity], oracleProgram);
  let guardian = authority.publicKey;
  if (process.env.GUARDIAN) {
    guardian = new PublicKey(process.env.GUARDIAN);
  } else {
    console.warn('GUARDIAN not set: using the admin as guardian. Set a separate pause key later with update_roles.');
  }

  console.log(`Creating protocol config: admin ${authority.publicKey}, guardian ${guardian}, oracle program ${oracleProgram}`);
  const tx = await program.methods
    .initializeConfig({ guardian, oracleProgram })
    .accounts({
      upgradeAuthority: authority.publicKey,
      config: configPda,
      programData: getProgramDataPDA()[0],
      systemProgram: anchor.web3.SystemProgram.programId,
    })
    .signers([authority])
    .rpc();
  console.log(`Protocol config created: ${tx}`);
}

const PYTH_MAGIC = 0xa1b2c3d4;

function requiredPythProgram(): PublicKey {
  if (!process.env.PYTH_PROGRAM_ID) {
    throw new Error('Set PYTH_PROGRAM_ID to the Pyth oracle program for this cluster (see https://docs.pyth.network)');
  }
  return new PublicKey(process.env.PYTH_PROGRAM_ID);
}

/** Fail unless `feed` is a Pyth price account owned by `oracleProgram`. */
async function assertPythFeed(connection: Connection, feed: PublicKey, oracleProgram: PublicKey): Promise<void> {
  const info = await connection.getAccountInfo(feed);
  if (!info) throw new Error(`Price feed ${feed} does not exist`);
  if (!info.owner.equals(oracleProgram)) {
    throw new Error(`Price feed ${feed} is owned by ${info.owner}, not the Pyth program ${oracleProgram}`);
  }
  if (info.data.length < 4 || info.data.readUInt32LE(0) !== PYTH_MAGIC) {
    throw new Error(`Price feed ${feed} is not a Pyth price account`);
  }
}

async function checkMarketExists(connection: Connection, commodity: string): Promise<boolean> {
  const [marketPda] = getMarketPDA(USDC_MINT, commodity);
  const accountInfo = await connection.getAccountInfo(marketPda);
  // Anyone can send lamports to the address; only an initialized program
  // account counts as an existing market.
  return !!accountInfo && accountInfo.owner.equals(PERPS_CORE_PROGRAM_ID) && accountInfo.data.length > 0;
}

async function main() {
  const args = process.argv.slice(2);
  const updateConfig = args.includes('--update-config');
  const commodity = args.find((a) => !a.startsWith('--')) || 'OIL';
  const rpcUrl = process.env.RPC_URL || 'https://api.testnet.solana.com';

  console.log(`\n=== Initialize Market Script ===`);
  console.log(`Network: ${rpcUrl}`);
  console.log(`Commodity: ${commodity}`);
  console.log(`Program ID: ${PERPS_CORE_PROGRAM_ID.toString()}`);
  console.log(`USDC Mint: ${USDC_MINT.toString()}\n`);

  // Load keypair
  const authority = await loadKeypair();
  console.log(`Authority: ${authority.publicKey.toString()}`);

  // Setup connection and provider
  const connection = new Connection(rpcUrl, 'confirmed');
  const wallet = new anchor.Wallet(authority);
  const provider = new anchor.AnchorProvider(connection, wallet, {
    commitment: 'confirmed',
  });

  // Check balance
  const balance = await connection.getBalance(authority.publicKey);
  console.log(`Balance: ${balance / 1e9} SOL\n`);

  if (balance < 0.1 * 1e9) {
    console.error('Insufficient balance! Need at least 0.1 SOL');
    console.log('Run: solana airdrop 1 --url testnet');
    process.exit(1);
  }

  // Load IDL
  const idlPath = path.join(__dirname, '../target/idl/perps_core.json');
  if (!fs.existsSync(idlPath)) {
    console.error(`IDL not found at ${idlPath}`);
    console.log('Run: anchor build');
    process.exit(1);
  }

  const idl = JSON.parse(fs.readFileSync(idlPath, 'utf-8'));
  const program = new Program(idl, PERPS_CORE_PROGRAM_ID, provider);

  // Only the upgrade authority can create the config, and only the admin can
  // create markets: do this right after deploying.
  try {
    await ensureProtocolConfig(program, connection, authority, commodity);
  } catch (error) {
    console.error('Protocol config setup failed (is this wallet the upgrade authority / admin?):', error);
    process.exit(1);
  }

  // Initialize the market if needed, then (re)apply its risk controls, so
  // markets created before those existed pick them up too.
  try {
    let isPaused = false;
    if (await checkMarketExists(connection, commodity)) {
      const [marketPda] = getMarketPDA(USDC_MINT, commodity);
      const market = (await program.account.market.fetch(marketPda)) as Record<string, unknown> & { isPaused: boolean };
      isPaused = market.isPaused;
      console.log(`Market already exists: ${marketPda.toString()} (paused: ${isPaused})`);
      // Never adopt a market pointing at a different price feed (e.g. one
      // created before admin-only market creation existed)
      const feed = market.pythPriceFeed as PublicKey;
      if (!feed.equals(PYTH_PRICE_FEEDS[commodity])) {
        throw new Error(`Existing ${commodity} market uses price feed ${feed}, expected ${PYTH_PRICE_FEEDS[commodity]}. Pause or settle it instead of configuring it.`);
      }
      await assertPythFeed(connection, feed, requiredPythProgram());
      // Don't silently overwrite settings an operator may have tightened
      if (!updateConfig) {
        const current = ['sessionSchedule', 'preCloseWindowSecs', 'weekendInitialMarginRatio',
          'weekendMaintenanceMarginRatio', 'maxConfidenceBps', 'minOraclePrice']
          .map((k) => `  ${k}: ${String(market[k])}`).join('\n');
        console.log(`Current risk settings:\n${current}`);
        console.log(`Not changing them. Re-run with --update-config to apply this script's values:`, RISK_CONFIGS[commodity]);
        await ensurePool(program, connection, authority, commodity);
        return;
      }
      await ensurePool(program, connection, authority, commodity);
    } else {
      await assertPythFeed(connection, PYTH_PRICE_FEEDS[commodity], requiredPythProgram());
      const signature = await initializeMarket(program, authority, commodity);
      console.log(`https://explorer.solana.com/tx/${signature}?cluster=testnet`);
    }

    const signature = await applyRiskConfig(program, authority, commodity, isPaused);
    console.log(`\nSuccess! View transaction:`);
    console.log(`https://explorer.solana.com/tx/${signature}?cluster=testnet`);
  } catch (error) {
    console.error('Failed to set up market:', error);
    process.exit(1);
  }
}

main().catch(console.error);

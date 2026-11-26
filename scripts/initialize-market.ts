import * as anchor from '@anchor-lang/core';
import { Program } from '@anchor-lang/core';
// Generated with the IDL by scripts/gen-idl.sh (target/types/perps_core.ts)
import type { PerpsCore } from '../target/types/perps_core';
import { PublicKey, Keypair, Connection } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID, getAccount, getAssociatedTokenAddressSync } from '@solana/spl-token';
import {
  PERPS_CORE_PROGRAM_ID,
  USDC_MINT,
  getConfigPDA,
  getLpPoolPDA,
  getMarketPDA,
  getProgramDataPDA,
  loadKeypair,
  loadPerpsProgram,
} from './common';
import { PythPrograms, assertPythFeed, commodityFeed, feedIdToHex, pythPrograms } from './pyth-feeds';

// Pyth price feeds (docs/lp-pool.md D47, D51): each market pins the Pyth
// push oracle's shard-0 PriceUpdateV2 account for its Pyth Core feed id,
// PDA([0u16 LE, feed_id], push oracle), owned by the Pyth Receiver
// (ProtocolConfig.oracle_program). PYTH_PROGRAMS (required) picks the
// pro-compatible or legacy program pair; the feed ids are
// frontend/src/config/pythFeeds.ts. NATGAS and COPPER have no verified Core
// feed id and are refused. Pyth sponsors no commodity feed on Solana: the
// protocol's price pusher must have pushed the feed (which creates the
// account) before its market is created, and must keep it fresh.

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
  program: Program<PerpsCore>,
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
  program: Program<PerpsCore>,
  connection: Connection,
  authority: Keypair,
  commodity: string,
  pythPriceFeed: PublicKey
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
  const { ixs, seeded } = await poolBootstrapIxs(program, connection, authority, marketPda, pythPriceFeed, {
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

async function initializeMarket(
  program: Program<PerpsCore>,
  authority: Keypair,
  commodity: string,
  pyth: PythPrograms
): Promise<string> {
  const config = MARKET_CONFIGS[commodity];
  if (!config) {
    throw new Error(`Unknown commodity: ${commodity}`);
  }

  // Throws for a commodity without a verified feed id
  const feed = commodityFeed(commodity, pyth);
  const pythPriceFeed = feed.address;

  const [marketPda] = getMarketPDA(USDC_MINT, commodity);
  const [vaultPda] = getVaultPDA(marketPda);
  const [vaultTokenAccountPda] = getVaultTokenAccountPDA(marketPda);

  console.log(`Initializing market for ${commodity}...`);
  console.log(`  Market PDA: ${marketPda.toString()}`);
  console.log(`  Vault PDA: ${vaultPda.toString()}`);
  console.log(`  Vault Token Account: ${vaultTokenAccountPda.toString()}`);
  console.log(`  Pyth Price Feed: ${pythPriceFeed.toString()} (feed id ${feed.feedIdHex}, ${pyth.set} push oracle shard 0)`);

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
    // The feed account must be a Full PriceUpdateV2 for this id (D47)
    pythFeedId: feed.feedId,
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
  program: Program<PerpsCore>,
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
 * The guardian (pause-only) comes from GUARDIAN; the oracle program is the
 * Pyth Receiver of the PYTH_PROGRAMS pair (required: it is what price feeds
 * are checked against, so it must not be read off a feed account).
 */
async function ensureProtocolConfig(
  program: Program<PerpsCore>,
  connection: Connection,
  authority: Keypair,
  commodity: string,
  pyth: PythPrograms
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
    // New markets snapshot config.oracle_program as their feed owner (D47)
    if (!config.oracleProgram.equals(pyth.receiver)) {
      throw new Error(
        `The protocol's oracle program is ${config.oracleProgram}, not the ${pyth.set} Pyth Receiver ${pyth.receiver}: ` +
          'fix PYTH_PROGRAMS, or change it with update_roles (existing markets then need a feed rotation, oracle-feed.ts)'
      );
    }
    return;
  }

  const oracleProgram = pyth.receiver;
  const feed = commodityFeed(commodity, pyth);
  await assertPythFeed(connection, feed.address, oracleProgram, feed.feedId);
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
  console.log(`USDC Mint: ${USDC_MINT.toString()}`);

  // Refuse a commodity without a verified Pyth feed id, or no PYTH_PROGRAMS,
  // before touching the cluster
  let pyth: PythPrograms;
  let feed: ReturnType<typeof commodityFeed>;
  try {
    pyth = pythPrograms();
    feed = commodityFeed(commodity, pyth);
  } catch (error) {
    console.error((error as Error).message);
    process.exit(1);
  }
  console.log(`Pyth: ${pyth.set} programs (Receiver ${pyth.receiver}, push oracle ${pyth.pushOracle})`);
  console.log(`Feed: id ${feed.feedIdHex}, account ${feed.address}\n`);

  // Load keypair
  const authority = await loadKeypair();
  console.log(`Authority: ${authority.publicKey.toString()}`);

  // Setup connection
  const connection = new Connection(rpcUrl, 'confirmed');

  // Check balance
  const balance = await connection.getBalance(authority.publicKey);
  console.log(`Balance: ${balance / 1e9} SOL\n`);

  if (balance < 0.1 * 1e9) {
    console.error('Insufficient balance! Need at least 0.1 SOL');
    console.log('Run: solana airdrop 1 --url testnet');
    process.exit(1);
  }

  // perps-core from target/idl (exits if missing or for another program id)
  const program = loadPerpsProgram(connection, authority);

  // Only the upgrade authority can create the config, and only the admin can
  // create markets: do this right after deploying.
  try {
    await ensureProtocolConfig(program, connection, authority, commodity, pyth);
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
      // created before admin-only market creation existed). A deliberate
      // change of feed goes through the timelocked rotation (oracle-feed.ts).
      const pinned = market.pythPriceFeed as PublicKey;
      const pinnedId = feedIdToHex(market.pythFeedId as number[]);
      if (!pinned.equals(feed.address) || pinnedId !== feed.feedIdHex) {
        throw new Error(
          `Existing ${commodity} market uses price feed ${pinned} (feed id ${pinnedId}), expected ${feed.address} ` +
            `(feed id ${feed.feedIdHex}). Rotate it with scripts/oracle-feed.ts, or pause or settle it, instead of configuring it.`
        );
      }
      // Every price read checks the owner the market snapshotted
      await assertPythFeed(connection, pinned, market.oracleProgram as PublicKey, market.pythFeedId as number[]);
      // Don't silently overwrite settings an operator may have tightened
      if (!updateConfig) {
        const current = ['sessionSchedule', 'preCloseWindowSecs', 'weekendInitialMarginRatio',
          'weekendMaintenanceMarginRatio', 'maxConfidenceBps', 'minOraclePrice']
          .map((k) => `  ${k}: ${String(market[k])}`).join('\n');
        console.log(`Current risk settings:\n${current}`);
        console.log(`Not changing them. Re-run with --update-config to apply this script's values:`, RISK_CONFIGS[commodity]);
        await ensurePool(program, connection, authority, commodity, pinned);
        return;
      }
      await ensurePool(program, connection, authority, commodity, pinned);
    } else {
      await assertPythFeed(connection, feed.address, pyth.receiver, feed.feedId);
      const signature = await initializeMarket(program, authority, commodity, pyth);
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

// One-time setup of a fresh localnet validator (scripts/localnet/up.sh runs it):
// USDC-like mint, the mock-pyth OIL feed, the protocol config with mock-pyth as
// oracle program, the OIL market on the test schedule with its LP pool seeded,
// mm-registry and orderbook registered as venues, and funded bot wallets.
// Writes target/localnet/state.json.

import { Keypair, PublicKey, SystemProgram, SYSVAR_RENT_PUBKEY, Transaction } from "@solana/web3.js";
import {
  TOKEN_PROGRAM_ID,
  createAssociatedTokenAccountIdempotentInstruction,
  createInitializeMintInstruction,
  createMintToInstruction,
  getAssociatedTokenAddressSync,
  MINT_SIZE,
} from "@solana/spl-token";
import {
  BN,
  MM_REGISTRY_ID,
  MOCK_PYTH_ID,
  ORDERBOOK_ID,
  PERPS_ID,
  PRICE_ACCOUNT_SIZE,
  PROP_AMM_ID,
  RPC_URL,
  USDC,
  VAMM_ID,
  connect,
  loadKeypair,
  log,
  mockPythInitializeIx,
  mockPythSetPriceIx,
  mustSend,
  namedKey,
  send,
  big,
  pdas,
  programs,
  saveState,
  walletPath,
  chainTime,
  LocalnetState,
} from "./lib";
import {
  BOOK_VENUE_PARAMS,
  LP_SEED_USDC,
  MARKET_CONFIG,
  MARKET_PARAMS,
  MM_VENUE_PARAMS,
  REGISTRY_PARAMS,
  START_PRICE,
  WALLETS,
  localnetPoolParams,
} from "./params";
import { validatePoolParams, decodePoolParams } from "../../../frontend/src/utils/pool";
import { feedIdToHex } from "../../../frontend/src/config/pythFeeds";
import { fundingCurrent, isOpen, secondsSinceOpen } from "../../../frontend/src/utils/session";

const C = "setup";

async function main() {
  const conn = connect();
  const admin = loadKeypair(walletPath());
  const { perps, mm, book } = programs(conn, admin);
  log(C, `RPC ${RPC_URL}, admin ${admin.publicKey.toBase58()}`);

  // Every program must be loaded, perps-core and the venues upgradeable (venue
  // records pin their ProgramData) with the admin as upgrade authority
  for (const [name, id] of [
    ["perps-core", PERPS_ID], ["mm-registry", MM_REGISTRY_ID], ["orderbook", ORDERBOOK_ID],
    ["prop-amm", PROP_AMM_ID], ["vamm", VAMM_ID], ["mock-pyth", MOCK_PYTH_ID],
  ] as [string, PublicKey][]) {
    const info = await conn.getAccountInfo(id);
    if (!info?.executable) throw new Error(`${name} (${id.toBase58()}) is not deployed`);
  }

  const oracle = namedKey("oracle");
  const keeper = namedKey("keeper");
  const matcher = namedKey("matcher");
  const mintKp = namedKey("usdc-mint");
  const feedKp = namedKey("oil-feed");
  const mint = mintKp.publicKey;
  const feed = feedKp.publicKey;
  const market = pdas.market(mint);
  const vault = pdas.vault(market);
  const vaultToken = pdas.vaultToken(market);
  const lpPool = pdas.lpPool(market);
  const config = pdas.config();

  // --- SOL for every key ---------------------------------------------------
  const wallets: Record<string, Keypair> = {};
  for (const w of WALLETS) wallets[w.name] = namedKey(w.name);
  const solTargets = [oracle, keeper, matcher, ...Object.values(wallets)];
  for (let i = 0; i < solTargets.length; i += 10) {
    const ixs = solTargets.slice(i, i + 10).map((k) =>
      SystemProgram.transfer({ fromPubkey: admin.publicKey, toPubkey: k.publicKey, lamports: 50e9 })
    );
    await mustSend(conn, C, "fund_sol", ixs, [admin], { noFetch: true });
  }

  // --- mint and feed ---------------------------------------------------------
  const rentMint = await conn.getMinimumBalanceForRentExemption(MINT_SIZE);
  const rentFeed = await conn.getMinimumBalanceForRentExemption(PRICE_ACCOUNT_SIZE);
  const price = BigInt(Math.round(START_PRICE * 1e8));
  const conf = price / 3_000n; // 3.3 bps
  await mustSend(conn, C, "create_mint_and_feed", [
    SystemProgram.createAccount({ fromPubkey: admin.publicKey, newAccountPubkey: mint, lamports: rentMint, space: MINT_SIZE, programId: TOKEN_PROGRAM_ID }),
    createInitializeMintInstruction(mint, 6, admin.publicKey, null),
    SystemProgram.createAccount({ fromPubkey: admin.publicKey, newAccountPubkey: feed, lamports: rentFeed, space: PRICE_ACCOUNT_SIZE, programId: MOCK_PYTH_ID }),
    // A Full PriceUpdateV2 for the OIL feed id, owned by mock-pyth (D47)
    mockPythInitializeIx(feed, oracle.publicKey, price, conf, MARKET_PARAMS.pythFeedId),
  ], [admin, mintKp, feedKp, oracle]);
  log(C, `mint ${mint.toBase58()}, mock-pyth feed ${feed.toBase58()} (feed id ${feedIdToHex(MARKET_PARAMS.pythFeedId)}) at $${START_PRICE}`);

  // --- protocol config: mock-pyth is the oracle program (TEST ONLY) ---------
  await mustSend(conn, C, "initialize_config", [
    await perps.methods
      .initializeConfig({ guardian: admin.publicKey, oracleProgram: MOCK_PYTH_ID })
      .accounts({ upgradeAuthority: admin.publicKey, config, programData: pdas.programData(PERPS_ID), systemProgram: SystemProgram.programId })
      .instruction(),
  ], [admin]);

  // --- USDC for the seed, the keeper and every bot ---------------------------
  const ata = (owner: PublicKey) => getAssociatedTokenAddressSync(mint, owner);
  const mintTo = (owner: PublicKey, usdc: number) => [
    createAssociatedTokenAccountIdempotentInstruction(admin.publicKey, ata(owner), owner, mint),
    ...(usdc > 0 ? [createMintToInstruction(mint, ata(owner), admin.publicKey, BigInt(usdc) * BigInt(USDC))] : []),
  ];
  // mm-registry bonds come out of the wallet too
  const minted: { owner: PublicKey; usdc: number }[] = [
    { owner: admin.publicKey, usdc: LP_SEED_USDC },
    { owner: keeper.publicKey, usdc: 0 },
    { owner: matcher.publicKey, usdc: 0 },
    ...WALLETS.map((w) => ({ owner: wallets[w.name].publicKey, usdc: w.usdc + (w.role === "mm" ? 1_000 : 0) })),
  ];
  for (let i = 0; i < minted.length; i += 5) {
    await mustSend(conn, C, "mint_usdc", minted.slice(i, i + 5).flatMap((m) => mintTo(m.owner, m.usdc)), [admin], { noFetch: true });
  }

  // --- market on the test schedule, then its pool -------------------------
  // initialize_market starts always open; update_market_config switches it to
  // SCHEDULE_TEST_HOURLY (only a localnet build accepts that) before
  // initialize_pool, whose validate measures the D38 rule and the claim
  // window on the market's schedule: the scaled params are invalid on a CME
  // or always-open market (docs/localnet.md).
  const poolParams = localnetPoolParams();
  const seed = new BN(LP_SEED_USDC).mul(new BN(USDC));
  const seedPosition = pdas.lpPosition(lpPool, admin.publicKey);
  await mustSend(conn, C, "initialize_market+config+pool", [
    await perps.methods
      .initializeMarket(MARKET_PARAMS)
      .accounts({
        authority: admin.publicKey, config, market, vault, vaultTokenAccount: vaultToken, collateralMint: mint,
        pythPriceFeed: feed, systemProgram: SystemProgram.programId, tokenProgram: TOKEN_PROGRAM_ID, rent: SYSVAR_RENT_PUBKEY,
      })
      .instruction(),
    await perps.methods.updateMarketConfig(MARKET_CONFIG).accounts({ authority: admin.publicKey, config, market }).instruction(),
    await perps.methods
      .initializePool(poolParams, keeper.publicKey)
      .accounts({ admin: admin.publicKey, config, market, lpPool, systemProgram: SystemProgram.programId })
      .instruction(),
  ], [admin]);
  const m = (await perps.account.market.fetch(market)) as any;
  if (m.sessionSchedule !== MARKET_CONFIG.sessionSchedule) throw new Error("schedule not applied");
  // The market pins the feed, its id and mock-pyth as the owner every read checks (D47)
  if (!m.pythPriceFeed.equals(feed) || feedIdToHex(m.pythFeedId) !== feedIdToHex(MARKET_PARAMS.pythFeedId) || !m.oracleProgram.equals(MOCK_PYTH_ID)) {
    throw new Error("the market does not pin the mock-pyth feed");
  }
  log(C, `market ${market.toBase58()} on SCHEDULE_TEST_HOURLY; LP pool ${lpPool.toBase58()} (initialize_pool validated the scaled params on chain)`);

  // The seed lp_deposit needs the market open and outside the reopen quiet
  // time Q (LpFlowWindowClosed): wait for that if setup runs in a break, the
  // weekend or right after a reopen (at most 600 + 120 s)
  for (;;) {
    const t = await chainTime(conn);
    const since = secondsSinceOpen(MARKET_CONFIG.sessionSchedule, t);
    if (isOpen(MARKET_CONFIG.sessionSchedule, t) && since !== null && since >= poolParams.lpReopenQuietSecs + 2) break;
    log(C, `waiting for the LP deposit window to seed the pool (chain time ${t}, c = ${t % 3_600})`);
    await new Promise((r) => setTimeout(r, 10_000));
  }
  // The price driver starts only after setup: when setup waited (over a
  // weekend, say) the feed written at creation is older than 60 s by now
  // (StaleOraclePrice), so the seed transaction republishes it first. The
  // keeper also starts only after setup, and lp_deposit refuses a funding
  // accumulator more than one interval (60 s) of open time behind
  // (FundingStale): after a wait the seed cranks update_funding itself
  // (permissionless) between the republish and the deposit. Decided at the
  // chain clock; a decision the clock overtakes before the transaction lands
  // (FundingStale or FundingNotDue) is retried.
  for (let attempt = 1; ; attempt++) {
    const mk = (await perps.account.market.fetch(market)) as any;
    const t = await chainTime(conn);
    const current = fundingCurrent(
      MARKET_CONFIG.sessionSchedule,
      big(mk.settlementPrice) > 0n,
      Number(big(mk.lastFundingTime)),
      Number(big(mk.fundingCarrySecs)),
      Number(big(mk.fundingInterval)),
      t
    );
    const ixs = [mockPythSetPriceIx(feed, oracle.publicKey, price, conf)];
    if (!current) {
      ixs.push(await perps.methods.updateFunding().accounts({ keeper: admin.publicKey, market, pythPriceFeed: feed }).instruction());
    }
    ixs.push(
      await perps.methods
        .initLpPosition(0)
        .accounts({ owner: admin.publicKey, lpPool, lpPosition: seedPosition, systemProgram: SystemProgram.programId })
        .instruction(),
      await perps.methods
        .lpDeposit(seed, seed.subn(1_000_000))
        .accounts({
          owner: admin.publicKey, market, lpPool, lpPosition: seedPosition, vaultTokenAccount: vaultToken,
          ownerTokenAccount: ata(admin.publicKey), pythPriceFeed: feed, tokenProgram: TOKEN_PROGRAM_ID,
        })
        .instruction()
    );
    const r = await send(conn, C, current ? "seed_lp_deposit" : "seed_lp_deposit+update_funding", ixs, [admin, oracle]);
    if (r.ok) break;
    const raced = r.code === "perps_core:FundingStale" || r.code === "perps_core:FundingNotDue";
    if (!raced || attempt >= 3) throw new Error(`seed_lp_deposit failed: ${r.code}\n${(r.logs ?? []).join("\n")}`);
    log(C, `seed_lp_deposit raced the funding interval (${r.code}); retrying`);
    await new Promise((res) => setTimeout(res, 2_000));
  }
  log(C, `LP pool seeded with $${LP_SEED_USDC}`);

  // PoolParams::validate on the test schedule once more: the TS mirror, and
  // the program through propose_pool_params, which validates first (simulated
  // only: a seeded pool would timelock it)
  const mirror = validatePoolParams(decodePoolParams(poolParams as any), {
    maintenanceMarginRatio: BigInt(MARKET_PARAMS.maintenanceMarginRatio),
    takerFee: BigInt(MARKET_PARAMS.takerFee),
    initialMarginRatio: BigInt(MARKET_PARAMS.initialMarginRatio),
    weekendInitialMarginRatio: BigInt(MARKET_CONFIG.weekendInitialMarginRatio),
    sessionSchedule: MARKET_CONFIG.sessionSchedule,
  });
  if (mirror.length > 0) throw new Error(`pool params fail validate on the test schedule: ${mirror.join("; ")}`);
  const propose = new Transaction().add(
    await perps.methods.proposePoolParams(poolParams).accounts({ admin: admin.publicKey, config, market, lpPool }).instruction()
  );
  propose.feePayer = admin.publicKey;
  propose.recentBlockhash = (await conn.getLatestBlockhash()).blockhash;
  propose.sign(admin);
  const sim = await conn.simulateTransaction(propose);
  if (sim.value.err) throw new Error(`on-chain validate rejects the pool params on the test schedule: ${JSON.stringify(sim.value.err)}\n${(sim.value.logs ?? []).join("\n")}`);
  log(C, "pool params pass PoolParams::validate on SCHEDULE_TEST_HOURLY (TS mirror and simulated propose_pool_params)");

  // --- venues -------------------------------------------------------------
  const registry = pdas.registry(market);
  const mmVenue = pdas.venue(market, MM_REGISTRY_ID);
  const orderbook = pdas.orderbook(market);
  const bookVenue = pdas.venue(market, ORDERBOOK_ID);
  await mustSend(conn, C, "register_mm_registry", [
    await mm.methods
      .initializeRegistry(REGISTRY_PARAMS)
      .accounts({ authority: admin.publicKey, config, registry, market, collateralMint: mint, systemProgram: SystemProgram.programId, tokenProgram: TOKEN_PROGRAM_ID })
      .instruction(),
    await perps.methods
      .registerVenue(MM_VENUE_PARAMS, admin.publicKey)
      .accounts({
        admin: admin.publicKey, config, market, venueProgram: MM_REGISTRY_ID, venueProgramData: pdas.programData(MM_REGISTRY_ID),
        venue: mmVenue, systemProgram: SystemProgram.programId,
      })
      .instruction(),
  ], [admin]);
  await mustSend(conn, C, "register_orderbook", [
    await book.methods
      .initializeOrderbook()
      .accounts({ authority: admin.publicKey, config, market, orderbook, systemProgram: SystemProgram.programId })
      .instruction(),
    await perps.methods
      .registerVenue(BOOK_VENUE_PARAMS, admin.publicKey)
      .accounts({
        admin: admin.publicKey, config, market, venueProgram: ORDERBOOK_ID, venueProgramData: pdas.programData(ORDERBOOK_ID),
        venue: bookVenue, systemProgram: SystemProgram.programId,
      })
      .instruction(),
    await book.methods.setMatcher(matcher.publicKey).accounts({ admin: admin.publicKey, config, orderbook }).instruction(),
  ], [admin]);
  log(C, `venues: mm-registry ${mmVenue.toBase58()}, orderbook ${bookVenue.toBase58()} (matcher ${matcher.publicKey.toBase58()})`);

  const state: LocalnetState = {
    rpcUrl: RPC_URL,
    createdAt: await chainTime(conn),
    mint: mint.toBase58(),
    feed: feed.toBase58(),
    feedId: feedIdToHex(MARKET_PARAMS.pythFeedId),
    market: market.toBase58(),
    vault: vault.toBase58(),
    vaultToken: vaultToken.toBase58(),
    lpPool: lpPool.toBase58(),
    registry: registry.toBase58(),
    orderbook: orderbook.toBase58(),
    mmVenue: mmVenue.toBase58(),
    bookVenue: bookVenue.toBase58(),
    admin: admin.publicKey.toBase58(),
    oracleAuthority: oracle.publicKey.toBase58(),
    keeper: keeper.publicKey.toBase58(),
    matcher: matcher.publicKey.toBase58(),
    startPrice: START_PRICE,
    wallets: Object.fromEntries(Object.entries(wallets).map(([n, k]) => [n, k.publicKey.toBase58()])),
    marketParams: { ...MARKET_PARAMS, maxOpenInterest: MARKET_PARAMS.maxOpenInterest.toString(), fundingInterval: MARKET_PARAMS.fundingInterval.toNumber() },
    marketConfig: { ...MARKET_CONFIG, minOraclePrice: MARKET_CONFIG.minOraclePrice.toString(), maxOpenInterest: "0" },
    poolParams: Object.fromEntries(Object.entries(poolParams).map(([k, v]) => [k, v.toString()])),
  };
  saveState(state);
  log(C, `done: ${WALLETS.length} bot wallets funded; state in target/localnet/state.json`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});

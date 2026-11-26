// Market, risk and pool parameters of the localnet OIL market (docs/localnet.md).
// Production values (scripts/initialize-market.ts, PoolParams::production())
// for everything that is not time; time spans scaled to the one-hour test week
// of SCHEDULE_TEST_HOURLY (D46).

import { BN, OIL_FEED_ID, SCHEDULE_TEST_HOURLY, USDC } from "./lib";

/** initialize-market.ts MARKET_CONFIGS.OIL, funding interval 60 s (the minimum), the OIL feed id. */
export const MARKET_PARAMS = {
  commodity: "OIL",
  maxLeverage: 20_000,
  maintenanceMarginRatio: 500,
  initialMarginRatio: 1_000,
  takerFee: 50,
  makerFee: 20,
  liquidationFee: 100,
  maxOpenInterest: new BN(10_000_000_000_000),
  fundingInterval: new BN(Number(process.env.FUNDING_INTERVAL_SECS ?? 60)),
  // The production OIL feed id (USOILSPOT), which the mock-pyth feed carries (D47)
  pythFeedId: OIL_FEED_ID,
};

/** initialize-market.ts RISK_CONFIGS.OIL on the test schedule, W = 300 s. */
export const MARKET_CONFIG = {
  isPaused: false,
  sessionSchedule: SCHEDULE_TEST_HOURLY,
  preCloseWindowSecs: 300,
  weekendInitialMarginRatio: 2_000,
  weekendMaintenanceMarginRatio: 1_000,
  maxConfidenceBps: 100,
  minOraclePrice: new BN(7_500_000),
  maxOpenInterest: new BN(0),
};

/**
 * Insurance target of the localnet pool, USDC. Production's $250k fills from
 * fees within the first test weeks and then covers every deficit a live run
 * produces, so the LP-absorb step of the waterfall (§2.6) is never reached.
 * $20k keeps insurance small (fees above it go to LP cash) so a deficit
 * beyond it reaches the LPs. INSURANCE_TARGET_USDC=250000 restores production.
 */
export const INSURANCE_TARGET_USDC = Number(process.env.INSURANCE_TARGET_USDC ?? 20_000);

/**
 * PoolParams::production() with the time fields scaled to the test week:
 * W 300 s, Q 120 s, withdrawal delay 600 s, claim window 1_500 s (≥ the
 * schedule-aware minimum 300 + 4·120 + 600 = 1_380), profit-close quiet 60 s;
 * and the insurance target above.
 */
export function localnetPoolParams() {
  return {
    skewScale: new BN("2500000000000"),
    insuranceTarget: new BN(INSURANCE_TARGET_USDC).mul(new BN(USDC)),
    insuranceClaimCapPerWindow: new BN(0),
    liqMinRemainingNotional: new BN(100 * USDC),
    kNetBps: 4_000,
    hardNetBps: 6_000,
    perAccountCapBps: 2_500,
    weekendLpLossBps: 1_000,
    weekendGapBps: 0,
    weekendCapWindowSecs: 300,
    withdrawalDelaySecs: Number(process.env.LP_WITHDRAWAL_DELAY_SECS ?? 600),
    withdrawalClaimWindowSecs: 1_500,
    lpReopenQuietSecs: 120,
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
    profitCloseQuietSecs: 60,
  };
}

/** mm-registry deploy defaults (sim/src/world.rs): 50 bps band, 15 s oracle, $50k per fill, the taker signs. */
export const MM_VENUE_PARAMS = { maxPriceDeviationBps: 50, maxOracleAgeSecs: 15, minSigners: 1, maxFillNotional: new BN(50_000 * USDC) };
/** orderbook deploy defaults: 100 bps band, 30 s oracle, no signers. */
export const BOOK_VENUE_PARAMS = { maxPriceDeviationBps: 100, maxOracleAgeSecs: 30, minSigners: 0, maxFillNotional: new BN(50_000 * USDC) };
export const REGISTRY_PARAMS = {
  minCollateral: new BN(100 * USDC),
  maxSpread: 100,
  minQuoteSize: new BN(1_000_000),
  maxQuoteSize: new BN(1_000_000_000),
  mmFee: 10,
};

/**
 * The mid-run G raise (docs/localnet.md): G_RAISE_AFTER_SECS after the bots
 * start (0 = never), the admin raises the market's weekend IM from 20% to
 * G_RAISE_TO bps with update_market_config, as §3.1 lists ("a later raise
 * of G"). Above 2_500 the weekend line α/G drops under the 0.4× net cap, so
 * the always-on weekend cap binds on opens (PoolWeekendExposureLimit), the
 * weekend withdraw line has no room left at the cap (PoolCapWouldBreach),
 * and net put on before the raise sits above the line until it closes or
 * the keeper's wTrig ADL cuts it inside W.
 */
export const G_RAISE_AFTER_SECS = Number(process.env.G_RAISE_AFTER_SECS ?? 1_200);
export const G_RAISE_TO = Number(process.env.G_RAISE_TO ?? 3_000);

export const START_PRICE = Number(process.env.START_PRICE ?? 75);
export const LP_SEED_USDC = Number(process.env.LP_SEED_USDC ?? 1_000_000);

export type Role = "mm" | "retail" | "book" | "matcher" | "lp" | "whale" | "cross" | "gapper" | "prober";

/** Every bot wallet: name, role and the USDC it is minted. */
export const WALLETS: { name: string; role: Role; usdc: number }[] = [
  { name: "mm-tight", role: "mm", usdc: 150_000 },
  { name: "mm-wide", role: "mm", usdc: 100_000 },
  { name: "momentum-1", role: "retail", usdc: 20_000 },
  { name: "momentum-2", role: "retail", usdc: 12_000 },
  { name: "meanrev-1", role: "retail", usdc: 15_000 },
  { name: "degen-1", role: "retail", usdc: 8_000 },
  { name: "degen-2", role: "retail", usdc: 6_000 },
  { name: "house-1", role: "retail", usdc: 25_000 },
  { name: "swing-1", role: "retail", usdc: 30_000 },
  { name: "limit-1", role: "book", usdc: 60_000 },
  { name: "limit-2", role: "book", usdc: 60_000 },
  { name: "whale-1", role: "whale", usdc: 60_000 },
  { name: "whale-2", role: "whale", usdc: 60_000 },
  { name: "whale-3", role: "whale", usdc: 60_000 },
  { name: "whale-4", role: "whale", usdc: 60_000 },
  { name: "cross-a", role: "cross", usdc: 100_000 },
  { name: "cross-b", role: "cross", usdc: 100_000 },
  { name: "gapper-1", role: "gapper", usdc: 30_000 },
  { name: "gapper-2", role: "gapper", usdc: 30_000 },
  { name: "quiet-prober", role: "prober", usdc: 20_000 },
  { name: "lp-steady", role: "lp", usdc: 300_000 },
  { name: "lp-gap", role: "lp", usdc: 150_000 },
  { name: "lp-buffer", role: "lp", usdc: 200_000 },
  { name: "lp-lapse", role: "lp", usdc: 50_000 },
  { name: "lp-big", role: "lp", usdc: 700_000 },
];

/**
 * perps-core LP pool (docs/lp-pool.md §1.2, §1.3): PoolParams Borsh codec,
 * LpPool / LpPosition decoders by byte offset, and hand-encoded pool
 * instructions, so the SDK needs no perps-core IDL. Offsets include the
 * 8-byte Anchor discriminator; tests/oil-perps.ts checks them against the
 * deployed program.
 */
import { PublicKey, SystemProgram, TransactionInstruction } from "@solana/web3.js";
import BN from "bn.js";
import { PERPS_CORE_PROGRAM_ID, findLpPoolPda, findPerpsConfigPda } from "./pda";

export const POOL_PARAMS_LEN = 104;
export const LP_POOL_LEN = 601;
export const LP_POSITION_LEN = 166;
/** Shares the first deposit locks for ever (§9.1) */
export const LP_DEAD_SHARES = new BN(1_000_000);
export const LP_CLASS_COMMON = 0;

/** Anchor discriminators, sha256("account:<Name>")[..8] */
export const LP_POOL_DISCRIMINATOR = Buffer.from([185, 127, 131, 141, 197, 198, 170, 147]);
export const LP_POSITION_DISCRIMINATOR = Buffer.from([105, 241, 37, 200, 224, 2, 252, 90]);
/** sha256("global:<name>")[..8] */
export const INITIALIZE_POOL_DISCRIMINATOR = Buffer.from([95, 180, 10, 172, 84, 174, 232, 40]);
export const PROPOSE_POOL_PARAMS_DISCRIMINATOR = Buffer.from([57, 33, 221, 55, 150, 196, 88, 233]);

/** Market fields appended by the LP pool (§1.1, D35). The SDK's other Market offsets (104/144/152/247) do not move. */
export const MARKET_FUNDING_CARRY_SECS = 283;
export const MARKET_SETTLED_AT = 287;

/** LpPool byte offsets (§1.2) */
export const LP_POOL_OFFSETS = {
  market: 8,
  adlKeeper: 40,
  freeCollateral: 72,
  positionCollateral: 80,
  cash: 88,
  treasury: 96,
  longEntrySum: 104,
  shortEntrySum: 120,
  longFundingSum: 136,
  shortFundingSum: 152,
  openPositions: 168,
  lpShares: 176,
  pendingWithdrawShares: 184,
  maxPoolCash: 192,
  pendingBadDebtLong: 200,
  pendingBadDebtShort: 208,
  pendingSinceLong: 216,
  badDebtCum: 224,
  badDebtRecoveredCum: 232,
  badDebtWrittenOffCum: 240,
  lossWindowStart: 248,
  lossWindowNav: 256,
  lpAbsorbedWindow: 264,
  insPaidWindow: 272,
  npsPeak: 280,
  npsPeakAt: 288,
  vestAmount: 296,
  vestStart: 304,
  vestEnd: 312,
  donatedCum: 320,
  pendingParamsAt: 328,
  params: 336,
  pendingParams: 440,
  bump: 544,
  pendingSinceShort: 545,
} as const;

/** LpPosition byte offsets (§1.3) */
export const LP_POSITION_OFFSETS = {
  owner: 8,
  pool: 40,
  shares: 72,
  pendingShares: 80,
  requestedValue: 88,
  requestedAt: 96,
  unlockAt: 104,
  depositedCum: 112,
  withdrawnCum: 120,
  claimWindowSecs: 128,
  class: 132,
  bump: 133,
} as const;

/**
 * perps-core PoolParams (Borsh, 104 bytes). Field names match the IDL, so
 * the object can be passed to an Anchor client as is.
 */
export interface PoolParams {
  skewScale: BN;
  insuranceTarget: BN;
  insuranceClaimCapPerWindow: BN;
  liqMinRemainingNotional: BN;
  kNetBps: number;
  hardNetBps: number;
  perAccountCapBps: number;
  weekendLpLossBps: number;
  weekendGapBps: number;
  weekendCapWindowSecs: number;
  withdrawalDelaySecs: number;
  withdrawalClaimWindowSecs: number;
  lpReopenQuietSecs: number;
  adlNetTargetBps: number;
  adlExposureScoreFloorBps: number;
  ddFreezeBps: number;
  ddAdlBps: number;
  ddWithdrawBlockBps: number;
  redeemLossHaircutBps: number;
  openFeeBps: number;
  maxImpactBps: number;
  liqPenaltyBps: number;
  liqKeeperShareBps: number;
  backstopThresholdBps: number;
  minLiqSliceBps: number;
  treasuryFeeBps: number;
  lpLossCapWindowBps: number;
  /** D23: no profitable close this long after each session open; last field */
  profitCloseQuietSecs: number;
}

type FieldKind = "u64" | "u32" | "u16";
/** Borsh order of PoolParams; offsets are relative to the struct start */
export const POOL_PARAMS_LAYOUT: ReadonlyArray<[keyof PoolParams, FieldKind, number]> = [
  ["skewScale", "u64", 0],
  ["insuranceTarget", "u64", 8],
  ["insuranceClaimCapPerWindow", "u64", 16],
  ["liqMinRemainingNotional", "u64", 24],
  ["kNetBps", "u32", 32],
  ["hardNetBps", "u32", 36],
  ["perAccountCapBps", "u32", 40],
  ["weekendLpLossBps", "u32", 44],
  ["weekendGapBps", "u32", 48],
  ["weekendCapWindowSecs", "u32", 52],
  ["withdrawalDelaySecs", "u32", 56],
  ["withdrawalClaimWindowSecs", "u32", 60],
  ["lpReopenQuietSecs", "u32", 64],
  ["adlNetTargetBps", "u32", 68],
  ["adlExposureScoreFloorBps", "u32", 72],
  ["ddFreezeBps", "u16", 76],
  ["ddAdlBps", "u16", 78],
  ["ddWithdrawBlockBps", "u16", 80],
  ["redeemLossHaircutBps", "u16", 82],
  ["openFeeBps", "u16", 84],
  ["maxImpactBps", "u16", 86],
  ["liqPenaltyBps", "u16", 88],
  ["liqKeeperShareBps", "u16", 90],
  ["backstopThresholdBps", "u16", 92],
  ["minLiqSliceBps", "u16", 94],
  ["treasuryFeeBps", "u16", 96],
  ["lpLossCapWindowBps", "u16", 98],
  ["profitCloseQuietSecs", "u32", 100],
];

const U64_MAX = new BN("18446744073709551615");

/** PoolParams::production() (§15.1) */
export function productionPoolParams(): PoolParams {
  return {
    skewScale: new BN("2500000000000"),
    insuranceTarget: new BN("250000000000"),
    insuranceClaimCapPerWindow: new BN(0),
    liqMinRemainingNotional: new BN(100_000_000),
    kNetBps: 10_000,
    hardNetBps: 15_000,
    perAccountCapBps: 2_500,
    weekendLpLossBps: 1_000,
    weekendGapBps: 0,
    weekendCapWindowSecs: 86_400,
    withdrawalDelaySecs: 345_600,
    withdrawalClaimWindowSecs: 259_200,
    lpReopenQuietSecs: 1_800,
    adlNetTargetBps: 5_000,
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
    profitCloseQuietSecs: 300,
  };
}

/** PoolParams::neutral() (§15.2): reproduces the pre-pool numbers */
export function neutralPoolParams(): PoolParams {
  return {
    ...productionPoolParams(),
    skewScale: new BN(0),
    openFeeBps: 0,
    treasuryFeeBps: 0,
    insuranceTarget: U64_MAX.clone(),
    kNetBps: 100_000,
    hardNetBps: 0,
    perAccountCapBps: 0,
    weekendCapWindowSecs: 0,
    ddFreezeBps: 0,
    ddAdlBps: 0,
    ddWithdrawBlockBps: 0,
    backstopThresholdBps: 10_000,
    liqPenaltyBps: 0,
    lpLossCapWindowBps: 10_000,
    withdrawalDelaySecs: 0,
    withdrawalClaimWindowSecs: 0,
    lpReopenQuietSecs: 0,
    profitCloseQuietSecs: 0,
  };
}

/** Borsh-encode PoolParams (104 bytes, profit_close_quiet_secs last) */
export function encodePoolParams(p: PoolParams): Buffer {
  const out = Buffer.alloc(POOL_PARAMS_LEN);
  for (const [name, kind, off] of POOL_PARAMS_LAYOUT) {
    const v = p[name];
    if (kind === "u64") out.set((v as BN).toArrayLike(Buffer, "le", 8), off);
    else if (kind === "u32") out.writeUInt32LE(v as number, off);
    else out.writeUInt16LE(v as number, off);
  }
  return out;
}

/** Decode PoolParams from `data` at `offset` */
export function decodePoolParams(data: Buffer, offset = 0): PoolParams {
  const p: Record<string, BN | number> = {};
  for (const [name, kind, off] of POOL_PARAMS_LAYOUT) {
    const o = offset + off;
    if (kind === "u64") p[name] = new BN(data.subarray(o, o + 8), "le");
    else if (kind === "u32") p[name] = data.readUInt32LE(o);
    else p[name] = data.readUInt16LE(o);
  }
  return p as unknown as PoolParams;
}

export interface LpPoolInfo {
  market: PublicKey;
  adlKeeper: PublicKey;
  freeCollateral: BN;
  positionCollateral: BN;
  cash: BN;
  treasury: BN;
  longEntrySum: BN;
  shortEntrySum: BN;
  longFundingSum: BN;
  shortFundingSum: BN;
  openPositions: BN;
  lpShares: BN;
  pendingWithdrawShares: BN;
  maxPoolCash: BN;
  pendingBadDebtLong: BN;
  pendingBadDebtShort: BN;
  pendingSinceLong: BN;
  badDebtCum: BN;
  badDebtRecoveredCum: BN;
  badDebtWrittenOffCum: BN;
  lossWindowStart: BN;
  lossWindowNav: BN;
  lpAbsorbedWindow: BN;
  insPaidWindow: BN;
  npsPeak: BN;
  npsPeakAt: BN;
  vestAmount: BN;
  vestStart: BN;
  vestEnd: BN;
  donatedCum: BN;
  pendingParamsAt: BN;
  params: PoolParams;
  pendingParams: PoolParams;
  bump: number;
  pendingSinceShort: BN;
}

const u64At = (d: Buffer, o: number) => new BN(d.subarray(o, o + 8), "le");
const i64At = (d: Buffer, o: number) => new BN(d.subarray(o, o + 8), "le").fromTwos(64);
const u128At = (d: Buffer, o: number) => new BN(d.subarray(o, o + 16), "le");
const i128At = (d: Buffer, o: number) => new BN(d.subarray(o, o + 16), "le").fromTwos(128);
const keyAt = (d: Buffer, o: number) => new PublicKey(d.subarray(o, o + 32));

function checkAccount(data: Buffer, disc: Buffer, len: number, name: string): void {
  if (data.length < len || !data.subarray(0, 8).equals(disc)) {
    throw new Error(`Not a perps-core ${name} account`);
  }
}

/** Decode a perps-core LpPool account by byte offset */
export function decodeLpPool(data: Buffer): LpPoolInfo {
  checkAccount(data, LP_POOL_DISCRIMINATOR, LP_POOL_LEN, "LpPool");
  const o = LP_POOL_OFFSETS;
  return {
    market: keyAt(data, o.market),
    adlKeeper: keyAt(data, o.adlKeeper),
    freeCollateral: u64At(data, o.freeCollateral),
    positionCollateral: u64At(data, o.positionCollateral),
    cash: u64At(data, o.cash),
    treasury: u64At(data, o.treasury),
    longEntrySum: u128At(data, o.longEntrySum),
    shortEntrySum: u128At(data, o.shortEntrySum),
    longFundingSum: i128At(data, o.longFundingSum),
    shortFundingSum: i128At(data, o.shortFundingSum),
    openPositions: u64At(data, o.openPositions),
    lpShares: u64At(data, o.lpShares),
    pendingWithdrawShares: u64At(data, o.pendingWithdrawShares),
    maxPoolCash: u64At(data, o.maxPoolCash),
    pendingBadDebtLong: u64At(data, o.pendingBadDebtLong),
    pendingBadDebtShort: u64At(data, o.pendingBadDebtShort),
    pendingSinceLong: i64At(data, o.pendingSinceLong),
    badDebtCum: u64At(data, o.badDebtCum),
    badDebtRecoveredCum: u64At(data, o.badDebtRecoveredCum),
    badDebtWrittenOffCum: u64At(data, o.badDebtWrittenOffCum),
    lossWindowStart: i64At(data, o.lossWindowStart),
    lossWindowNav: u64At(data, o.lossWindowNav),
    lpAbsorbedWindow: u64At(data, o.lpAbsorbedWindow),
    insPaidWindow: u64At(data, o.insPaidWindow),
    npsPeak: u64At(data, o.npsPeak),
    npsPeakAt: i64At(data, o.npsPeakAt),
    vestAmount: u64At(data, o.vestAmount),
    vestStart: i64At(data, o.vestStart),
    vestEnd: i64At(data, o.vestEnd),
    donatedCum: u64At(data, o.donatedCum),
    pendingParamsAt: i64At(data, o.pendingParamsAt),
    params: decodePoolParams(data, o.params),
    pendingParams: decodePoolParams(data, o.pendingParams),
    bump: data.readUInt8(o.bump),
    pendingSinceShort: i64At(data, o.pendingSinceShort),
  };
}

export interface LpPositionInfo {
  owner: PublicKey;
  pool: PublicKey;
  /** Includes pendingShares */
  shares: BN;
  pendingShares: BN;
  requestedValue: BN;
  requestedAt: BN;
  unlockAt: BN;
  depositedCum: BN;
  withdrawnCum: BN;
  claimWindowSecs: number;
  class: number;
  bump: number;
}

/** Decode a perps-core LpPosition account by byte offset */
export function decodeLpPosition(data: Buffer): LpPositionInfo {
  checkAccount(data, LP_POSITION_DISCRIMINATOR, LP_POSITION_LEN, "LpPosition");
  const o = LP_POSITION_OFFSETS;
  return {
    owner: keyAt(data, o.owner),
    pool: keyAt(data, o.pool),
    shares: u64At(data, o.shares),
    pendingShares: u64At(data, o.pendingShares),
    requestedValue: u64At(data, o.requestedValue),
    requestedAt: i64At(data, o.requestedAt),
    unlockAt: i64At(data, o.unlockAt),
    depositedCum: u64At(data, o.depositedCum),
    withdrawnCum: u64At(data, o.withdrawnCum),
    claimWindowSecs: data.readUInt32LE(o.claimWindowSecs),
    class: data.readUInt8(o.class),
    bump: data.readUInt8(o.bump),
  };
}

/** Market.funding_carry_secs and settled_at (0 = not settled, or settled before the field existed) */
export function decodeMarketPoolFields(data: Buffer): { fundingCarrySecs: number; settledAt: BN } {
  return {
    fundingCarrySecs: data.readUInt32LE(MARKET_FUNDING_CARRY_SECS),
    settledAt: i64At(data, MARKET_SETTLED_AT),
  };
}

/**
 * perps-core initialize_pool(params, adl_keeper), admin-signed. The pool
 * must be created on a fresh market; send it with initialize_market,
 * init_lp_position and the seed lp_deposit in one transaction (D11).
 */
export function buildInitializePoolIx(
  admin: PublicKey,
  market: PublicKey,
  params: PoolParams,
  adlKeeper: PublicKey,
  perpsProgramId: PublicKey = PERPS_CORE_PROGRAM_ID
): TransactionInstruction {
  const data = Buffer.concat([INITIALIZE_POOL_DISCRIMINATOR, encodePoolParams(params), adlKeeper.toBuffer()]);
  return new TransactionInstruction({
    programId: perpsProgramId,
    keys: [
      { pubkey: admin, isSigner: true, isWritable: true },
      { pubkey: findPerpsConfigPda(perpsProgramId)[0], isSigner: false, isWritable: false },
      { pubkey: market, isSigner: false, isWritable: false },
      { pubkey: findLpPoolPda(market, perpsProgramId)[0], isSigner: false, isWritable: true },
      { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
    ],
    data,
  });
}

/**
 * perps-core propose_pool_params(params), admin-signed. Applies at once
 * before the seed; otherwise it is timelocked (§5) and applied with
 * apply_pool_params.
 */
export function buildProposePoolParamsIx(
  admin: PublicKey,
  market: PublicKey,
  params: PoolParams,
  perpsProgramId: PublicKey = PERPS_CORE_PROGRAM_ID
): TransactionInstruction {
  const data = Buffer.concat([PROPOSE_POOL_PARAMS_DISCRIMINATOR, encodePoolParams(params)]);
  return new TransactionInstruction({
    programId: perpsProgramId,
    keys: [
      { pubkey: admin, isSigner: true, isWritable: false },
      { pubkey: findPerpsConfigPda(perpsProgramId)[0], isSigner: false, isWritable: false },
      { pubkey: market, isSigner: false, isWritable: false },
      { pubkey: findLpPoolPda(market, perpsProgramId)[0], isSigner: false, isWritable: true },
    ],
    data,
  });
}

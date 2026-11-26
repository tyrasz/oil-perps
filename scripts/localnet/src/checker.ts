// Live invariant checker for the localnet run (docs/localnet.md). Every
// CHECK_SECS, and at every session close and reopen, it reads the whole
// market over RPC (a snapshot is used only when the market, pool and vault
// bytes are unchanged across all its reads) and checks:
//
//   I1 vault tokens == free + position collateral + insurance + cash + treasury (§11)
//   I2 LpPool.free_collateral == Σ UserAccount.collateral_balance
//   I3 LpPool.position_collateral == Σ open Position.collateral
//   I4 open_positions, Market long/short OI and the pool's entry and funding
//      sums == the sums over the open positions
//   I5 lp_shares == Σ LpPosition.shares + LP_DEAD_SHARES; pending shares likewise
//   I6 NAV_redeem ≤ NAV_mint (pool_view, frontend/src/utils/poolMath.ts)
//   I7 no open position stays below maintenance for LIQ_GRACE_SECS while the
//      market is open, the oracle fresh and the keeper running
//   I8 the oracle is fresh while open (the price driver is alive)
//   I9 no house open lands while the market is closed
//   I10 at each weekly close G·net_notional ≤ α·B, unless what happened since the
//      bound last held is one of the documented §3.1 exceptions (price rise,
//      cap-exempt liquidations, deficits); the ratio is reported at every close
//   I11 no unexpected error code in any bot or setup transaction (tx.jsonl);
//      expected refusals are counted
//   C0 the checker itself checked enough: at least one consistent snapshot,
//      at most MAX_UNCHECKED_FRAC of the cycles failed or found the market
//      busy, no more than MAX_CHECK_GAP_SECS (chain time) between checked
//      snapshots, and a checked snapshot on both sides of every session edge.
//      Without it a checker whose reads all fail (IDL drift, RPC down)
//      would report PASS having checked nothing.
//
// On SIGTERM (scripts/localnet/down.sh) or with --report it writes
// report.json and report.md to REPORT_DIR and exits 1 if anything failed.

import { PublicKey } from "@solana/web3.js";
import * as fs from "fs";
import * as path from "path";
import {
  CHECK_LOG,
  LOCALNET_DIR,
  LP_DEAD_SHARES,
  PERPS_ID,
  PRICE_LOG,
  TX_LOG,
  appendJsonl,
  big,
  connect,
  decodeEvents,
  eventSummary,
  loadKeypair,
  loadState,
  log,
  programs,
  readJsonl,
  sleep,
  walletPath,
} from "./lib";
import { View, oracleFresh, pm, pool, readView } from "./view";
import { badDebtEvents, coverageProblems, weekendGapAt } from "./logic";
import { isOpen } from "../../../frontend/src/utils/session";

const C = "checker";
const CHECK_SECS = Number(process.env.CHECK_SECS ?? 5);
const LIQ_GRACE_SECS = Number(process.env.LIQ_GRACE_SECS ?? 60);
const MAX_CHECK_GAP_SECS = Number(process.env.MAX_CHECK_GAP_SECS ?? 60);
const MAX_UNCHECKED_FRAC = Number(process.env.MAX_UNCHECKED_FRAC ?? 0.2);
const REPORT_DIR = process.env.REPORT_DIR ?? path.join(LOCALNET_DIR, "report");
const KEEPER_PID = path.join(LOCALNET_DIR, "pids", "keeper.pid");

const st = loadState();
const conn = connect();
const P = programs(conn, loadKeypair(walletPath()));
const market = new PublicKey(st.market);
const keeperKey = new PublicKey(st.keeper);

interface Violation {
  t: number;
  inv: string;
  msg: string;
}

const violations: Violation[] = [];
const warnings: Violation[] = [];
let snapshots = 0;
let consistent = 0;
let skippedBusy = 0;
let checkFailures = 0;
let maxGapSecs = 0;
let edgesMissed = 0;
const lastCheckErrors: string[] = [];
const firstBelow = new Map<string, number>();
const navPath: Record<string, unknown>[] = [];
const closes: Record<string, unknown>[] = [];
const reopens: Record<string, unknown>[] = [];
const breaks: { t: number; kind: string }[] = [];
const keeperTx: { sig: string; t: number; ok: boolean; ixs: string[]; cu: number | null; events: string[]; detail: Record<string, unknown>[]; err?: string }[] = [];
/** Keeper signatures already read (pollKeeper). */
const keeperSeen = new Set<string>();
let lastBoundHeld: { t: number; nn: bigint; b: bigint; badDebtCum: bigint; oracle: bigint; g: bigint } | null = null;
let liquidationsSinceHeld = 0;
let maxRatioBetweenCloses = 0;
let prev: { v: View; positions: number } | null = null;
let closedSpell: { totalPositions: bigint; since: number } | null = null;
const startedWall = Math.floor(Date.now() / 1000);
let firstChainT: number | null = null;
let lastChainT = 0;

function fail(t: number, inv: string, msg: string) {
  violations.push({ t, inv, msg });
  appendJsonl(CHECK_LOG, { t, inv, level: "violation", msg });
  log(C, `VIOLATION ${inv}: ${msg}`);
}

function warn(t: number, inv: string, msg: string) {
  warnings.push({ t, inv, msg });
  appendJsonl(CHECK_LOG, { t, inv, level: "warning", msg });
}

function keeperAlive(): boolean {
  try {
    process.kill(Number(fs.readFileSync(KEEPER_PID, "utf-8")), 0);
    return true;
  } catch {
    return false;
  }
}

interface Snapshot {
  v: View;
  positions: { address: string; owner: string; isLong: boolean; size: bigint; collateral: bigint; entry: bigint; lastFunding: bigint }[];
  users: bigint[];
  lp: { shares: bigint; pending: bigint }[];
}

async function snapshot(): Promise<Snapshot | null> {
  for (let attempt = 0; attempt < 10; attempt++) {
    const v1 = await readView(conn, P, st);
    const [posAccts, userAccts, lpAccts] = await Promise.all([
      conn.getProgramAccounts(PERPS_ID, {
        commitment: "confirmed",
        filters: [{ dataSize: 168 }, { memcmp: { offset: 40, bytes: market.toBase58() } }, { memcmp: { offset: 133, bytes: "1" } }],
      }),
      P.perps.account.userAccount.all(),
      P.perps.account.lpPosition.all(),
    ]);
    const v2 = await readView(conn, P, st);
    if (!v1.marketData.equals(v2.marketData) || !v1.poolData.equals(v2.poolData) || v1.vaultTokens !== v2.vaultTokens) {
      await sleep(150);
      continue;
    }
    const positions = posAccts.map((a) => {
      const p = P.perps.coder.accounts.decode("position", a.account.data) as any;
      return {
        address: a.pubkey.toBase58(),
        owner: (p.owner as PublicKey).toBase58(),
        isLong: "long" in p.side,
        size: big(p.size),
        collateral: big(p.collateral),
        entry: big(p.entryPrice),
        lastFunding: big(p.lastFundingPayment),
      };
    });
    // One market on this validator: every UserAccount and LpPosition is OIL's
    const users = userAccts.map((u) => big((u.account as any).collateralBalance));
    const lp = lpAccts.map((l) => ({ shares: big((l.account as any).shares), pending: big((l.account as any).pendingShares) }));
    return { v: v2, positions, users, lp };
  }
  return null;
}

function eq(t: number, inv: string, what: string, a: bigint, b: bigint) {
  if (a !== b) fail(t, inv, `${what}: account says ${a}, recomputed ${b} (diff ${a - b})`);
}

function checkSnapshot(s: Snapshot) {
  const { v } = s;
  const t = v.t;
  const p = v.pool;
  const sum = (xs: bigint[]) => xs.reduce((a, b) => a + b, 0n);

  // I1 backing
  const buckets = p.freeCollateral + p.positionCollateral + v.m.insuranceFund + p.cash + p.treasury;
  if (v.vaultTokens < buckets) fail(t, "I1", `vault holds ${v.vaultTokens} < buckets ${buckets}`);
  else if (v.vaultTokens > buckets) warn(t, "I1", `vault holds ${v.vaultTokens - buckets} more than the buckets (direct transfer?)`);

  // I2, I3 collateral buckets
  eq(t, "I2", "free_collateral vs Σ user balances", p.freeCollateral, sum(s.users));
  eq(t, "I3", "position_collateral vs Σ open position collateral", p.positionCollateral, sum(s.positions.map((x) => x.collateral)));

  // I4 aggregates
  const longs = s.positions.filter((x) => x.isLong);
  const shorts = s.positions.filter((x) => !x.isLong);
  eq(t, "I4", "open_positions", p.openPositions, BigInt(s.positions.length));
  eq(t, "I4", "long_open_interest", v.m.longOpenInterest, sum(longs.map((x) => x.size)));
  eq(t, "I4", "short_open_interest", v.m.shortOpenInterest, sum(shorts.map((x) => x.size)));
  eq(t, "I4", "long_entry_sum", p.longEntrySum, sum(longs.map((x) => x.size * x.entry)));
  eq(t, "I4", "short_entry_sum", p.shortEntrySum, sum(shorts.map((x) => x.size * x.entry)));
  eq(t, "I4", "long_funding_sum", p.longFundingSum, sum(longs.map((x) => x.size * x.lastFunding)));
  eq(t, "I4", "short_funding_sum", p.shortFundingSum, sum(shorts.map((x) => x.size * x.lastFunding)));

  // I5 shares
  eq(t, "I5", "lp_shares vs Σ LpPosition.shares + dead shares", p.lpShares, sum(s.lp.map((x) => x.shares)) + (p.lpShares > 0n ? LP_DEAD_SHARES : 0n));
  eq(t, "I5", "pending_withdraw_shares vs Σ pending", p.pendingWithdrawShares, sum(s.lp.map((x) => x.pending)));

  // I6 NAV bounds
  if (!v.pv) fail(t, "I6", "pool_view overflows");
  else if (v.pv.navRedeem > v.pv.navMint) fail(t, "I6", `NAV_redeem ${v.pv.navRedeem} > NAV_mint ${v.pv.navMint}`);

  // I7 liquidation liveness
  const [, mmr] = pool.marginRatios(v.m, t);
  const fresh = oracleFresh(v);
  const below = new Set<string>();
  for (const x of s.positions) {
    const equity = x.collateral + pool.unrealizedPnl(x.isLong, x.size, x.entry, v.oracle) + pool.accruedFunding(x.isLong, x.size, v.m.cumulativeFunding, x.lastFunding);
    const n = (x.size * v.oracle) / 1_000_000n;
    const liquidatable = n === 0n ? equity < 0n : equity * 10_000n < mmr * n;
    if (!liquidatable) continue;
    below.add(x.address);
    if (!(v.open && fresh)) {
      firstBelow.delete(x.address);
      continue;
    }
    const since = firstBelow.get(x.address) ?? t;
    firstBelow.set(x.address, since);
    if (t - since > LIQ_GRACE_SECS && keeperAlive()) {
      fail(t, "I7", `position ${x.address} (${x.owner.slice(0, 6)}) below maintenance for ${t - since} s with the keeper running (equity ${equity}, mmr ${mmr} bps of ${n})`);
      firstBelow.set(x.address, t); // report again only after another grace period
    }
  }
  for (const k of [...firstBelow.keys()]) if (!below.has(k)) firstBelow.delete(k);

  // I8 oracle freshness while open (a few seconds after a reopen are allowed)
  if (v.open && !fresh && (v.sinceOpen ?? 0) > 15) fail(t, "I8", `oracle published at ${v.feed.publishTime}, ${t - v.feed.publishTime} s ago, while open`);

  // I9 nothing opens while closed
  if (!v.open) {
    if (!closedSpell) closedSpell = { totalPositions: big(v.marketRaw.totalPositions), since: t };
    else if (big(v.marketRaw.totalPositions) !== closedSpell.totalPositions) {
      fail(t, "I9", `house opens landed while closed (total_positions ${closedSpell.totalPositions} → ${big(v.marketRaw.totalPositions)})`);
      closedSpell.totalPositions = big(v.marketRaw.totalPositions);
    }
  } else closedSpell = null;

  // I10 weekend bound tracking
  if (v.pv) {
    const g = pm.gBps(p.params.weekendGapBps, v.m.initialMarginRatio, v.m.weekendInitialMarginRatio);
    const alpha = p.params.weekendLpLossBps;
    const lhs = g * v.pv.netNotional;
    const rhs = alpha * v.pv.b;
    const ratio = rhs > 0n ? Number(lhs) / Number(rhs) : lhs > 0n ? Infinity : 0;
    if (v.open) maxRatioBetweenCloses = Math.max(maxRatioBetweenCloses, ratio);
    if (lhs <= rhs) {
      lastBoundHeld = { t, nn: v.pv.netNotional, b: v.pv.b, badDebtCum: p.badDebtCum, oracle: v.oracle, g };
      liquidationsSinceHeld = 0;
    }
  }
}

function sessionEdges(s: Snapshot) {
  const v = s.v;
  if (!prev) return;
  const was = prev.v;
  const c = v.t % 3_600;
  if (was.open && !v.open) {
    const weekend = c >= 3_000;
    if (!weekend) {
      breaks.push({ t: v.t, kind: "close" });
      return;
    }
    const p = v.pool;
    const g = pm.gBps(p.params.weekendGapBps, v.m.initialMarginRatio, v.m.weekendInitialMarginRatio);
    const alpha = p.params.weekendLpLossBps;
    const nn = v.pv?.netNotional ?? 0n;
    const b = v.pv?.b ?? 0n;
    const ratio = b > 0n ? Number(g * nn) / Number(alpha * b) : 0;
    const excess = g * nn > alpha * b + 64n * alpha + 2n * g;
    const explained: string[] = [];
    if (excess && lastBoundHeld) {
      if (v.oracle > lastBoundHeld.oracle && nn > lastBoundHeld.nn) explained.push("price_rise");
      if (liquidationsSinceHeld > 0) explained.push(`liquidation×${liquidationsSinceHeld}`);
      if (p.badDebtCum > lastBoundHeld.badDebtCum || b < lastBoundHeld.b) explained.push("deficit_or_payout");
      // §3.1: a later update_market_config raising G (the localnet G raise)
      if (g > lastBoundHeld.g) explained.push(`g_raised ${lastBoundHeld.g}→${g}`);
    }
    const rec = {
      t: v.t,
      oracle: Number(v.oracle) / 1e6,
      net: Number(v.pv?.net ?? 0n) / 1e6,
      netNotional: Number(nn) / 1e6,
      B: Number(b) / 1e6,
      G_bps: Number(g),
      alpha_bps: Number(alpha),
      ratio: Number(ratio.toFixed(4)),
      maxRatioDuringWeek: Number(maxRatioBetweenCloses.toFixed(4)),
      excess,
      explainedBy: explained,
      openPositions: Number(p.openPositions),
      nps: Number(v.pv?.nps ?? 0n) / 1e6,
    };
    closes.push(rec);
    log(C, `weekly close: G·nn/(α·B) = ${rec.ratio} (week max ${rec.maxRatioDuringWeek}), net ${rec.net}, B $${rec.B.toFixed(0)}`);
    if (excess && explained.length === 0) fail(v.t, "I10", `weekend bound exceeded at the close with no documented exception: ratio ${ratio}`);
    maxRatioBetweenCloses = 0;
  }
  if (!was.open && v.open) {
    const weekStart = v.sinceOpen !== null && (v.t - v.sinceOpen) % 3_600 === 0;
    if (!weekStart) {
      breaks.push({ t: v.t, kind: "reopen" });
      return;
    }
    // The gap is the driver's own gap print (fillReopen, report)
    const reopenT = v.t - (v.sinceOpen ?? 0);
    reopens.push({ t: reopenT, seenAt: v.t, oracleBefore: null, oracleAfter: null, gapPct: null, oracleAtFirstCheck: Number(v.oracle) / 1e6, nps: Number(v.pv?.nps ?? 0n) / 1e6 });
    log(C, `weekly reopen at ${reopenT}`);
  }
}

/** Complete the reopens' records with the price driver's gap print (price.jsonl). */
function fillReopens(final = false) {
  const open = reopens.filter((r: any) => r.gapPct === null);
  if (open.length === 0) return;
  const rows = readJsonl(PRICE_LOG);
  for (const r of open as any[]) {
    const g = weekendGapAt(rows, r.t);
    if (!g) {
      if (final) r.gapPct = "?";
      continue;
    }
    r.oracleBefore = g.before;
    r.oracleAfter = g.after;
    r.gapPct = g.gapPct;
    r.printedAt = g.printedAt;
    log(C, `weekly reopen gap ${g.gapPct}% ($${g.before.toFixed(3)} -> $${g.after.toFixed(3)}), printed at ${g.printedAt} (reopen ${r.t})`);
  }
}

/**
 * The keeper's transactions since the last poll. No `until` cursor: the
 * validator's `--limit-ledger-size` purges old slots, and once the cursor's
 * slot is purged every later call fails "Transaction … not found". That
 * happened when setup waited for the LP deposit window: the cursor was
 * setup's funding transfer, purged before the keeper's first transaction
 * (docs/lp-pool.md D52). The newest 1_000 are read and those already seen
 * skipped (the keeper sends at most 64 a pass).
 */
async function pollKeeper() {
  const sigs = (await conn.getSignaturesForAddress(keeperKey, { limit: 1_000 }, "confirmed")).filter((s) => !keeperSeen.has(s.signature));
  if (sigs.length === 0) return;
  for (const s of sigs.reverse()) {
    keeperSeen.add(s.signature);
    const tx = await conn.getTransaction(s.signature, { commitment: "confirmed", maxSupportedTransactionVersion: 0 });
    // Only what the keeper sent (setup's funding transfers also name its address)
    if (!tx || !tx.transaction.message.staticAccountKeys[0].equals(keeperKey)) continue;
    const logs = tx?.meta?.logMessages ?? [];
    const ixs = logs.filter((l) => l.startsWith("Program log: Instruction: ")).map((l) => l.slice("Program log: Instruction: ".length));
    const events = decodeEvents(logs);
    const detail = events.map(eventSummary).filter((x): x is Record<string, unknown> => x !== null);
    const row = { sig: s.signature, t: s.blockTime ?? 0, ok: !s.err, ixs, cu: tx?.meta?.computeUnitsConsumed ?? null, events: events.map((e) => e.name), detail, err: s.err ? JSON.stringify(s.err) : undefined };
    keeperTx.push(row);
    if (row.ok && ixs.includes("Liquidate")) liquidationsSinceHeld++;
  }
}

/** Session edges between two checked snapshots, and those not seen from both sides. */
function edgesBetween(a: View, b: View): number {
  const s = b.m.sessionSchedule;
  let edges = 0;
  let open = isOpen(s, a.t);
  for (let t = a.t + 1; t <= b.t && t - a.t <= 4_000; t++) {
    const o = isOpen(s, t);
    if (o !== open) edges++;
    open = o;
  }
  return edges;
}

async function check() {
  snapshots++;
  let s: Snapshot | null;
  try {
    s = await snapshot();
  } catch (e) {
    checkFailures++;
    const msg = (e as Error)?.message?.split("\n")[0] ?? String(e);
    lastCheckErrors.push(msg);
    if (lastCheckErrors.length > 10) lastCheckErrors.shift();
    throw e;
  }
  if (!s) {
    skippedBusy++;
    return;
  }
  consistent++;
  firstChainT ??= s.v.t;
  if (prev) {
    maxGapSecs = Math.max(maxGapSecs, s.v.t - prev.v.t);
    // One edge with the states differing is seen from both sides; any more is missed
    const e = edgesBetween(prev.v, s.v);
    edgesMissed += Math.max(0, e - (prev.v.open !== s.v.open ? 1 : 0));
  }
  lastChainT = s.v.t;
  checkSnapshot(s);
  sessionEdges(s);
  fillReopens();
  const v = s.v;
  if (navPath.length === 0 || v.t - (navPath[navPath.length - 1].t as number) >= 15 || (prev && prev.v.open !== v.open)) {
    navPath.push({
      t: v.t,
      c: v.t % 3_600,
      open: v.open,
      oracle: Number(v.oracle) / 1e6,
      nps: Number(v.pv?.nps ?? 0n) / 1e6,
      navRedeem: Number(v.pv?.navRedeem ?? 0n) / 1e6,
      navMint: Number(v.pv?.navMint ?? 0n) / 1e6,
      B: Number(v.pv?.b ?? 0n) / 1e6,
      net: Number(v.pv?.net ?? 0n) / 1e6,
      netNotional: Number(v.pv?.netNotional ?? 0n) / 1e6,
      cash: Number(v.pool.cash) / 1e6,
      lpShares: Number(v.pool.lpShares) / 1e6,
      openPositions: Number(v.pool.openPositions),
      insurance: Number(v.m.insuranceFund) / 1e6,
      pendingBadDebt: Number(v.pool.pendingBadDebtLong + v.pool.pendingBadDebtShort) / 1e6,
      badDebtCum: Number(v.pool.badDebtCum) / 1e6,
      cumulativeFunding: Number(v.m.cumulativeFunding),
    });
  }
  prev = { v, positions: s.positions.length };
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

function stats(xs: number[]) {
  if (xs.length === 0) return null;
  const s = [...xs].sort((a, b) => a - b);
  return { n: s.length, min: s[0], mean: Math.round(s.reduce((a, b) => a + b, 0) / s.length), p95: s[Math.min(s.length - 1, Math.floor(s.length * 0.95))], max: s[s.length - 1] };
}

function report() {
  const txs = readJsonl(TX_LOG);
  const byKind: Record<string, { ok: number; refused: number; unexpected: number }> = {};
  const errors: Record<string, { count: number; expected: boolean; kinds: Record<string, number> }> = {};
  const cu: Record<string, number[]> = {};
  const unexpected: any[] = [];
  const eventCounts: Record<string, number> = {};
  for (const r of txs) {
    const k = (byKind[r.kind] ??= { ok: 0, refused: 0, unexpected: 0 });
    if (r.ok) {
      k.ok++;
      if (r.cu) (cu[r.kind] ??= []).push(r.cu);
      for (const e of r.events ?? []) eventCounts[e] = (eventCounts[e] ?? 0) + 1;
    } else if (r.expected) k.refused++;
    else {
      k.unexpected++;
      unexpected.push(r);
    }
    if (!r.ok) {
      const e = (errors[r.code] ??= { count: 0, expected: !!r.expected, kinds: {} });
      e.count++;
      e.kinds[r.kind] = (e.kinds[r.kind] ?? 0) + 1;
    }
  }
  // I11: unexpected failures (infra ones are listed separately)
  const unexpectedProgram = unexpected.filter((r) => !String(r.code).startsWith("infra:"));
  for (const r of unexpectedProgram.slice(0, 50)) fail(r.t, "I11", `${r.actor} ${r.kind}: ${r.code}`);
  const keeperByIx: Record<string, { ok: number; failed: number; cu: number[] }> = {};
  for (const k of keeperTx) {
    const name = k.ixs.filter((x) => !["SetComputeUnitLimit", "Transfer", "TransferChecked"].includes(x)).join("+") || "other";
    const e = (keeperByIx[name] ??= { ok: 0, failed: 0, cu: [] });
    if (k.ok) e.ok++;
    else e.failed++;
    if (k.cu) e.cu.push(k.cu);
    for (const ev of k.events) eventCounts[ev] = (eventCounts[ev] ?? 0) + 1;
  }
  const liquidations = keeperTx.flatMap((k) => k.detail.filter((d) => d.name === "PositionLiquidated").map((d) => ({ t: k.t, ...d })));
  // Bad debt: BadDebt events once each, from keeper and bot transactions
  const badDebt = badDebtEvents([
    ...keeperTx.map((k) => ({ t: k.t, by: "keeper", detail: k.detail as any })),
    ...txs.filter((r) => r.ok).map((r) => ({ t: r.t, by: `${r.actor} ${r.kind}`, detail: r.detail })),
  ]);
  const badDebtTotal = badDebt.reduce((a, d) => a + Number(d.deficit ?? 0), 0) / 1e6;
  fillReopens(true);
  // C0: did the checker check enough to call it a pass?
  const coverage = { cycles: snapshots, consistent, failures: checkFailures, skippedBusy, maxGapSecs, edgesMissed };
  for (const msg of coverageProblems(coverage, { maxGapSecs: MAX_CHECK_GAP_SECS, maxUncheckedFrac: MAX_UNCHECKED_FRAC })) {
    if (!violations.some((v) => v.inv === "C0" && v.msg === msg)) fail(lastChainT || Math.floor(Date.now() / 1000), "C0", msg);
  }
  const adl = [...keeperTx.flatMap((k) => k.detail.filter((d) => d.name === "Adl").map((d) => ({ t: k.t, ...d })))];
  const prices = readJsonl(PRICE_LOG);
  const priceEvents = prices.filter((p) => p.event);
  const keeperLog = path.join(LOCALNET_DIR, "logs", "keeper.log");
  const keeperErrors = fs.existsSync(keeperLog) ? fs.readFileSync(keeperLog, "utf-8").split("\n").filter((l) => / ERROR /.test(l.replace(/\x1b\[[0-9;]*m/g, ""))) : [];
  const out = {
    generatedAt: new Date().toISOString(),
    wallSecs: Math.floor(Date.now() / 1000) - startedWall,
    chainTime: { first: firstChainT, last: lastChainT, secs: firstChainT ? lastChainT - firstChainT : 0 },
    sessions: { weeklyCloses: closes.length, weeklyReopens: reopens.length, dailyBreakEdges: breaks.length },
    checks: { snapshots, consistent, skippedBusy, failures: checkFailures, maxGapSecs, edgesMissed, lastErrors: lastCheckErrors, violations: violations.length, warnings: warnings.length },
    violations,
    warnings: warnings.slice(0, 100),
    transactions: { total: txs.length, byKind, cuByKind: Object.fromEntries(Object.entries(cu).map(([k, v]) => [k, stats(v)])) },
    errors,
    unexpectedFailures: unexpected.slice(0, 50),
    events: eventCounts,
    keeper: {
      transactions: keeperTx.length,
      byInstruction: Object.fromEntries(Object.entries(keeperByIx).map(([k, v]) => [k, { ok: v.ok, failed: v.failed, cu: stats(v.cu) }])),
      liquidations,
      adl,
      logErrorLines: keeperErrors.length,
      logErrorSamples: keeperErrors.slice(-10).map((l) => l.replace(/\x1b\[[0-9;]*m/g, "")),
    },
    badDebt,
    badDebtTotal,
    weeklyCloses: closes,
    weeklyReopens: reopens,
    priceEvents,
    lpNavPath: navPath,
  };
  fs.mkdirSync(REPORT_DIR, { recursive: true });
  fs.writeFileSync(path.join(REPORT_DIR, "report.json"), JSON.stringify(out, (_k, v) => (typeof v === "bigint" ? v.toString() : v), 2));
  fs.writeFileSync(path.join(REPORT_DIR, "report.md"), markdown(out));
  log(C, `report written to ${REPORT_DIR} (${violations.length} violations)`);
  return out;
}

function markdown(r: any): string {
  const L: string[] = [];
  const row = (...xs: unknown[]) => L.push(`| ${xs.map((x) => String(x)).join(" | ")} |`);
  L.push(`# Localnet run report`, ``);
  L.push(`Generated ${r.generatedAt}. Wall clock ${r.wallSecs} s; chain time ${r.chainTime.first} → ${r.chainTime.last} (${r.chainTime.secs} s).`, ``);
  L.push(`**Result: ${r.checks.violations === 0 ? "PASS (no invariant violations)" : `FAIL (${r.checks.violations} violations)`}.** ` +
    `${r.sessions.weeklyCloses} weekly closes, ${r.sessions.weeklyReopens} weekly reopens and ${r.sessions.dailyBreakEdges} daily-break edges crossed; ` +
    `${r.checks.consistent} of ${r.checks.snapshots} check cycles checked a consistent snapshot (${r.checks.skippedBusy} skipped while the market kept changing, ${r.checks.failures} failed); ` +
    `at most ${r.checks.maxGapSecs} s between checked snapshots, ${r.checks.edgesMissed} session edges missed; ${r.checks.warnings} warnings.`, ``);
  if (r.violations.length) {
    L.push(`## Violations`, ``, `| t | invariant | detail |`, `|---|---|---|`);
    for (const v of r.violations) row(v.t, v.inv, v.msg);
    L.push(``);
  }
  L.push(`## Transactions by kind`, ``, `| kind | ok | expected refusals | unexpected | CU mean | CU max |`, `|---|---|---|---|---|---|`);
  for (const [k, v] of Object.entries<any>(r.transactions.byKind).sort()) {
    const c = r.transactions.cuByKind[k];
    row(k, v.ok, v.refused, v.unexpected, c?.mean ?? "", c?.max ?? "");
  }
  L.push(``, `## Error histogram`, ``, `| code | count | expected | from |`, `|---|---|---|---|`);
  for (const [k, v] of Object.entries<any>(r.errors).sort((a: any, b: any) => b[1].count - a[1].count)) {
    row(k, v.count, v.expected ? "yes" : "**no**", Object.entries(v.kinds).map(([a, b]) => `${a} ${b}`).join(", "));
  }
  L.push(``, `## Keeper (backend/keeper binary)`, ``, `| instruction | ok | failed | CU mean | CU max |`, `|---|---|---|---|---|`);
  for (const [k, v] of Object.entries<any>(r.keeper.byInstruction)) row(k, v.ok, v.failed, v.cu?.mean ?? "", v.cu?.max ?? "");
  L.push(``, `${r.keeper.logErrorLines} ERROR lines in the keeper log.`, ``);
  if (r.keeper.liquidations.length) {
    L.push(`### Liquidations`, ``, `| t | position | stage | size closed | remaining | mark | penalty | keeper reward | deficit |`, `|---|---|---|---|---|---|---|---|---|`);
    for (const l of r.keeper.liquidations) row(l.t, String(l.position).slice(0, 8), l.stage, Number(l.size_closed ?? l.sizeClosed) / 1e6, Number(l.size_remaining ?? l.sizeRemaining) / 1e6, Number(l.mark) / 1e6, Number(l.penalty) / 1e6, Number(l.keeper_reward ?? l.keeperReward) / 1e6, Number(l.deficit) / 1e6);
    L.push(``);
  }
  const by = (k: string) => r.badDebt.filter((b: any) => (k === "keeper" ? b.by === "keeper" : b.by !== "keeper"));
  const sumOf = (xs: any[], f: string) => (xs.reduce((a, b) => a + Number(b[f] ?? 0), 0) / 1e6).toFixed(2);
  L.push(`### ADL and bad debt`, ``,
    `${r.keeper.adl.length} ADL events. ${r.badDebt.length} BadDebt events, $${r.badDebtTotal.toFixed(2)} of deficit: ` +
    `${by("keeper").length} from keeper liquidations, ${by("bots").length} from owner closes. ` +
    `Insurance paid $${sumOf(r.badDebt, "insurance_paid")}, LPs absorbed $${sumOf(r.badDebt, "lp_absorbed")}, $${sumOf(r.badDebt, "pending_added")} went pending.`, ``);
  if (r.badDebt.length) {
    L.push(`| t | by | position | deficit | insurance paid | LP absorbed | pending added |`, `|---|---|---|---|---|---|---|`);
    for (const b of r.badDebt.slice(0, 40)) row(b.t, b.by, String(b.position).slice(0, 8), (Number(b.deficit) / 1e6).toFixed(2), (Number(b.insurance_paid ?? 0) / 1e6).toFixed(2), (Number(b.lp_absorbed ?? 0) / 1e6).toFixed(2), (Number(b.pending_added ?? 0) / 1e6).toFixed(2));
    L.push(``);
  }
  L.push(``, `## Events seen`, ``, Object.entries(r.events).map(([k, v]) => `${k} ${v}`).join(", ") || "none", ``);
  L.push(`## Weekly closes (weekend bound G·net_notional ≤ α·B)`, ``, `| t | oracle | net (contracts) | net notional | B | ratio | week max | excess | explained by |`, `|---|---|---|---|---|---|---|---|---|`);
  for (const c of r.weeklyCloses) row(c.t, c.oracle.toFixed(3), c.net.toFixed(2), c.netNotional.toFixed(0), c.B.toFixed(0), c.ratio, c.maxRatioDuringWeek, c.excess, c.explainedBy.join(", "));
  L.push(``, `## Weekly reopens`, ``, `The gap is the price driver's gap print (sent in the last closed seconds) against its print before it.`, ``,
    `| reopen t | printed at | oracle before | after | gap % | oracle at first check | NAV/share |`, `|---|---|---|---|---|---|---|`);
  for (const c of r.weeklyReopens) row(c.t, c.printedAt ?? "?", c.oracleBefore === null ? "?" : c.oracleBefore.toFixed(3), c.oracleAfter === null ? "?" : c.oracleAfter.toFixed(3), c.gapPct ?? "?", c.oracleAtFirstCheck?.toFixed(3) ?? "?", c.nps);
  L.push(``, `## Price events`, ``);
  for (const p of r.priceEvents) L.push(`- t ${p.t}: ${p.event} → $${Number(p.price).toFixed(3)}`);
  L.push(``, `## LP NAV path (every ~60 s and at session edges)`, ``, `| t | c | open | oracle | NAV/share | NAV_redeem | B | net | net notional | open positions | pending bad debt |`, `|---|---|---|---|---|---|---|---|---|---|---|`);
  let last = -Infinity;
  let lastOpen: boolean | null = null;
  for (const n of r.lpNavPath) {
    if (n.t - last < 60 && n.open === lastOpen) continue;
    last = n.t;
    lastOpen = n.open;
    row(n.t, n.c, n.open, n.oracle.toFixed(3), n.nps.toFixed(6), n.navRedeem.toFixed(0), n.B.toFixed(0), n.net.toFixed(1), n.netNotional.toFixed(0), n.openPositions, n.pendingBadDebt.toFixed(2));
  }
  L.push(``);
  return L.join("\n");
}

// ---------------------------------------------------------------------------

let stopping = false;
async function finish(): Promise<never> {
  stopping = true;
  try {
    await pollKeeper();
    await check();
  } catch (e) {
    log(C, `final check failed: ${(e as Error).message}`);
  }
  const r = report();
  fs.writeFileSync(path.join(LOCALNET_DIR, "checker.status"), String(r.checks.violations === 0 ? 0 : 1));
  process.exit(r.checks.violations === 0 ? 0 : 1);
}

async function main() {
  if (process.argv.includes("--report")) {
    // One-off: check now, report what the logs hold
    await pollKeeper();
    await check();
    await finish();
  }
  process.on("SIGTERM", () => void finish());
  process.on("SIGINT", () => void finish());
  log(C, `checking every ${CHECK_SECS} s (liquidation grace ${LIQ_GRACE_SECS} s); report to ${REPORT_DIR}`);
  let lastOpen: boolean | null = null;
  while (!stopping) {
    try {
      await check();
      await pollKeeper();
    } catch (e) {
      log(C, `check failed: ${(e as Error).message.split("\n")[0]}`);
    }
    // Check right at session edges too: sleep until the next one if it is close
    const v = prev?.v;
    let wait = CHECK_SECS;
    if (v) {
      const c = v.t % 3_600;
      for (const edge of [750, 810, 1_500, 1_560, 2_250, 2_310, 3_000, 3_600]) {
        const d = edge - c;
        if (d > 0 && d < wait) wait = d + 1;
      }
      if (lastOpen !== null && lastOpen !== v.open) log(C, `session ${v.open ? "open" : "closed"} at c = ${c}`);
      lastOpen = v.open;
    }
    await sleep(wait * 1_000);
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});

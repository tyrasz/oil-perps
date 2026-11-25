// Price driver: writes the mock-pyth OIL feed (TEST ONLY) every PRICE_TICK_SECS
// while the test schedule is open, with a geometric Brownian motion plus
// Poisson jumps; freezes the price while closed (still republishing it so
// reads stay fresh, PRICE_CLOSED_REPUBLISH=0 lets the feed go stale); gaps it
// at every reopen: small after a daily break, sometimes large after the
// weekend (PRICE_WEEKEND_GAPS="-0.12,0.09" forces the first gaps, as
// fractions). The reopen print goes out in the last PRICE_REOPEN_LEAD_SECS
// closed seconds, while nothing can trade, so the first open slot already
// carries it. One daily break a week (PRICE_STALE_BREAK, default 2 = the one
// at c = 1500; 0 = none) is the exception: no republish during it and the
// reopen print held back PRICE_STALE_REOPEN_DELAY seconds after the reopen,
// so the market opens on a stale feed (StaleOraclePrice). Logs every
// publish to logs/price.jsonl.

import { PublicKey } from "@solana/web3.js";
import { PRICE_LOG, Rng, appendJsonl, chainTime, connect, loadState, log, mockPythSetPriceIx, namedKey, send, sleep, stopRequested } from "./lib";
import { isOpen, secondsSinceOpen } from "../../../frontend/src/utils/session";
import { breakIndexAt, nextReopen, printReopenNow } from "./logic";

const C = "price";
const TICK = Number(process.env.PRICE_TICK_SECS ?? 3);
const CLOSED_REPUBLISH = (process.env.PRICE_CLOSED_REPUBLISH ?? "1") !== "0";
// Volatility per sqrt(open second): ≈ 5% over a 2_820 s test week
const SIGMA = Number(process.env.PRICE_SIGMA ?? 0.05 / Math.sqrt(2_820));
const JUMP_RATE = Number(process.env.PRICE_JUMP_RATE ?? 1 / 600); // per open second
const JUMP_SIZE = Number(process.env.PRICE_JUMP_SIZE ?? 0.01);
const BREAK_GAP = Number(process.env.PRICE_BREAK_GAP ?? 0.004);
const FLOOR = 8; // stay above the market's $7.50 oracle floor
const LEAD = Number(process.env.PRICE_REOPEN_LEAD_SECS ?? 3);
const STALE_BREAK = Number(process.env.PRICE_STALE_BREAK ?? 2);
const STALE_DELAY = Number(process.env.PRICE_STALE_REOPEN_DELAY ?? 8);
const FORCED = (process.env.PRICE_WEEKEND_GAPS ?? "").split(",").map((s) => s.trim()).filter(Boolean).map(Number);

/** A weekend gap: half the time 8–15% either way, 10% of the time 18–25%, else ≈ N(0, 1.5%). */
function weekendGap(rng: Rng, n: number): number {
  if (n < FORCED.length) return FORCED[n];
  const sign = rng.chance(0.5) ? 1 : -1;
  const u = rng.next();
  if (u < 0.1) return sign * rng.range(0.18, 0.25);
  if (u < 0.6) return sign * rng.range(0.08, 0.15);
  return rng.normal() * 0.015;
}

async function main() {
  const st = loadState();
  const conn = connect();
  const authority = namedKey("oracle");
  const feed = new PublicKey(st.feed);
  const rng = new Rng(Number(process.env.PRICE_SEED ?? 7));
  let px = st.startPrice;
  let wasOpen: boolean | null = null;
  let weekends = 0;
  let lastT = await chainTime(conn);
  let published = 0;
  log(C, `driving ${feed.toBase58()} from $${px} every ${TICK} s (σ ${(SIGMA * Math.sqrt(2_820) * 100).toFixed(1)}%/week)`);

  // The reopen print of the current closed spell went out already
  let printed = false;
  // This spell is the stale break: no republish, and the print waits until after the reopen
  let staleSpell = false;
  const gapOf = (weekStart: boolean) => (weekStart ? weekendGap(rng, weekends++) : rng.normal() * BREAK_GAP);
  const publish = async (t: number, open: boolean, event: string | null) => {
    const p8 = BigInt(Math.round(px * 1e8));
    // The last digit alternates so a republished frozen price is never a duplicate transaction
    const conf = p8 / 3_000n + BigInt(published++ % 2);
    const r = await send(conn, "price-driver", "mock_pyth_set_price", [mockPythSetPriceIx(feed, authority.publicKey, p8, conf)], [authority], { cu: 20_000, noFetch: true });
    appendJsonl(PRICE_LOG, { t, wall: Math.floor(Date.now() / 1000), price: px, open, event, ok: r.ok });
    return r.ok;
  };
  const schedule = st.marketConfig.sessionSchedule as number;

  while (!stopRequested()) {
    const t = await chainTime(conn).catch(() => null);
    if (t === null) {
      await sleep(1_000);
      continue;
    }
    const open = isOpen(schedule, t);
    let event: string | null = null;
    let doPublish = open;
    if (open) {
      const since = secondsSinceOpen(schedule, t) ?? 0;
      if (wasOpen === false && !printed && !staleSpell) {
        // Missed the closed-second slot (the driver started or stalled late): gap now
        const weekStart = (t - since) % 3_600 === 0;
        const g = gapOf(weekStart);
        px = Math.max(FLOOR, px * (1 + g));
        event = `${weekStart ? "weekend" : "break"}_gap ${(g * 100).toFixed(2)}% (late)`;
        log(C, `reopen at ${t}: ${event} → $${px.toFixed(4)}`);
      } else if (staleSpell) {
        if (since < STALE_DELAY) {
          // Open on a stale feed: nothing published yet
          doPublish = false;
        } else {
          const g = gapOf(false);
          px = Math.max(FLOOR, px * (1 + g));
          event = `break_gap ${(g * 100).toFixed(2)}% (after a stale reopen, ${since} s late)`;
          log(C, `stale reopen ends at ${t}: ${event} → $${px.toFixed(4)}`);
          staleSpell = false;
        }
      } else if (wasOpen === true) {
        const dt = Math.max(0, Math.min(30, t - lastT));
        px *= Math.exp(-0.5 * SIGMA * SIGMA * dt + SIGMA * Math.sqrt(dt) * rng.normal());
        if (rng.chance(JUMP_RATE * dt)) {
          const j = rng.normal() * JUMP_SIZE;
          px *= 1 + j;
          event = `jump ${(j * 100).toFixed(2)}%`;
        }
        px = Math.max(FLOOR, px);
      }
      if (!staleSpell) printed = false;
    } else {
      if (wasOpen) {
        printed = false;
        staleSpell = STALE_BREAK > 0 && breakIndexAt(schedule, t) === STALE_BREAK;
        event = staleSpell ? "close (price frozen, feed left to go stale)" : "close (price frozen)";
        log(C, `close at ${t}: frozen at $${px.toFixed(4)}${staleSpell ? ", no republish this break" : ""}`);
      }
      const plan = nextReopen(schedule, t);
      if (!printed && printReopenNow(plan, LEAD, STALE_BREAK)) {
        // The reopen print, in the last closed seconds: nothing can trade on
        // the old price after the reopen
        const g = gapOf(plan!.weekStart);
        px = Math.max(FLOOR, px * (1 + g));
        event = `${plan!.weekStart ? "weekend" : "break"}_gap ${(g * 100).toFixed(2)}%`;
        log(C, `reopen in ${plan!.inSecs} s (chain ${t}): ${event} → $${px.toFixed(4)}`);
        printed = await publish(t, false, event);
        event = null;
        doPublish = false;
      } else {
        doPublish = CLOSED_REPUBLISH && !staleSpell;
      }
    }
    wasOpen = open;
    lastT = t;
    if (doPublish) await publish(t, open, event);
    else if (event) appendJsonl(PRICE_LOG, { t, wall: Math.floor(Date.now() / 1000), price: px, open, event, ok: null });
    // While closed, wake LEAD seconds before the reopen for its print
    let wait = TICK;
    if (!open) {
      const plan = nextReopen(schedule, t);
      wait = plan ? Math.max(1, Math.min(5, plan.inSecs - LEAD)) : 5;
      if (printed && plan) wait = Math.max(1, plan.inSecs);
    }
    await sleep(wait * 1_000);
  }
  log(C, "stop requested");
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});

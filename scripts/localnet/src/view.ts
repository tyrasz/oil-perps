// One consistent read of the market, its LP pool, the feed, the vault token
// account and the clock (a single getMultipleAccounts call, so one slot),
// decoded with the frontend's mirrors (frontend/src/utils/pool.ts, session.ts).

import { Connection, PublicKey, SYSVAR_CLOCK_PUBKEY } from "@solana/web3.js";
import { AccountLayout } from "@solana/spl-token";
import { FeedView, LocalnetState, Programs, decodeFeed } from "./lib";
import { feedIdToHex } from "../../../frontend/src/config/pythFeeds";
import { isFresh } from "../../../frontend/src/utils/oracleAccount";
import * as pool from "../../../frontend/src/utils/pool";
import * as pm from "../../../frontend/src/utils/poolMath";
import { inPreCloseWindow, isOpen, secondsSinceOpen, secondsUntilWeeklyClose } from "../../../frontend/src/utils/session";

export { pool, pm };

export interface View {
  slot: number;
  t: number;
  marketRaw: any;
  m: pool.MarketState;
  poolRaw: any;
  pool: pool.LpPoolAccount;
  feed: FeedView;
  oracle: bigint;
  vaultTokens: bigint;
  open: boolean;
  /** Market pre-close window (weekend margins). */
  preClose: boolean;
  /** Pool W (LP exits shut, wTrig). */
  inW: boolean;
  sinceOpen: number | null;
  untilClose: number | null;
  pv: pm.PoolView | null;
  /** The market's raw data bytes and the pool's, for stability checks. */
  marketData: Buffer;
  poolData: Buffer;
}

export async function readView(conn: Connection, p: Programs, st: LocalnetState): Promise<View> {
  const keys = [new PublicKey(st.market), new PublicKey(st.lpPool), new PublicKey(st.feed), new PublicKey(st.vaultToken), SYSVAR_CLOCK_PUBKEY];
  const res = await conn.getMultipleAccountsInfoAndContext(keys, "confirmed");
  const [mi, pi, fi, vi, ci] = res.value;
  if (!mi || !pi || !fi || !vi || !ci) throw new Error("market, pool, feed, vault or clock missing");
  const marketRaw = p.perps.coder.accounts.decode("market", mi.data);
  const poolRaw = p.perps.coder.accounts.decode("lpPool", pi.data);
  const m = pool.decodeMarket(marketRaw);
  const lp = pool.decodeLpPool(poolRaw);
  // As read_price checks (D47): the owner the market snapshotted and its feed
  // id; a mismatch is a broken setup, not a price to reason on
  const feed = decodeFeed(fi.data);
  if (!fi.owner.equals(marketRaw.oracleProgram)) throw new Error(`feed owner ${fi.owner.toBase58()} is not the market's oracle program`);
  if (feed.feedId !== feedIdToHex(marketRaw.pythFeedId)) throw new Error(`feed id ${feed.feedId} is not the market's`);
  const t = Number(ci.data.readBigInt64LE(32));
  const vaultTokens = AccountLayout.decode(vi.data).amount;
  const s = m.sessionSchedule;
  const oracle = feed.price6;
  return {
    slot: res.context.slot,
    t,
    marketRaw,
    m,
    poolRaw,
    pool: lp,
    feed,
    oracle,
    vaultTokens: BigInt(vaultTokens.toString()),
    open: isOpen(s, t),
    preClose: inPreCloseWindow(s, t, m.preCloseWindowSecs),
    inW: inPreCloseWindow(s, t, Number(lp.params.weekendCapWindowSecs)),
    sinceOpen: secondsSinceOpen(s, t),
    untilClose: secondsUntilWeeklyClose(s, t),
    pv: pool.poolView(lp, m, oracle, t),
    marketData: mi.data,
    poolData: pi.data,
  };
}

/** The highest leverage (3 dp) the initial margin in force allows, a little under it. */
export function maxLeverage(v: View): number {
  const [imr] = pool.marginRatios(v.m, v.t);
  return Math.floor((10_000_000 / Number(imr)) * 0.98);
}

/** Whether the feed is fresh for read_price: at most `maxAge` old, at most 60 s ahead (`pyth_receiver::is_fresh`, D52). */
export function oracleFresh(v: View, maxAge = 60): boolean {
  return isFresh(v.feed.publishTime, v.t, maxAge);
}

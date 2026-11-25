// Live trading bots for the localnet run (docs/localnet.md): market makers
// quoting on mm-registry, limit traders on the orderbook with a matcher,
// retail traders opening and closing house and venue trades, LPs depositing,
// requesting and claiming withdrawals, and a few adversarial behaviours from
// sim/src/adversary.rs that make sense live. Every transaction is logged to
// logs/tx.jsonl (lib.ts `send`), which the checker reads.

import { Keypair, PublicKey, SystemProgram, TransactionInstruction, ComputeBudgetProgram, SYSVAR_INSTRUCTIONS_PUBKEY } from "@solana/web3.js";
import { TOKEN_PROGRAM_ID, getAssociatedTokenAddressSync } from "@solana/spl-token";
import {
  BN,
  CONTRACT,
  MM_REGISTRY_ID,
  Mutex,
  ORDERBOOK_ID,
  PERPS_ID,
  ROLE_MAKER,
  ROLE_TAKER,
  Rng,
  SIDE_LONG_BIT,
  SIDE_SHORT_BIT,
  USDC,
  connect,
  loadKeypair,
  loadState,
  log,
  namedKey,
  pdas,
  programs,
  send,
  sleep,
  stopRequested,
  walletPath,
  big,
  TxResult,
} from "./lib";
import { G_RAISE_AFTER_SECS, G_RAISE_TO, MARKET_CONFIG, WALLETS } from "./params";
import { gapperLeverage, gapperTopUpTarget } from "./logic";
import { View, maxLeverage, oracleFresh, pool, readView } from "./view";

const C = "bots";
const st = loadState();
const conn = connect();
const admin = loadKeypair(walletPath());
const P = programs(conn, admin);
const market = new PublicKey(st.market);
const lpPool = new PublicKey(st.lpPool);
const feed = new PublicKey(st.feed);
const mint = new PublicKey(st.mint);
const vault = new PublicKey(st.vault);
const vaultToken = new PublicKey(st.vaultToken);
const registry = new PublicKey(st.registry);
const orderbook = new PublicKey(st.orderbook);
const mmVenue = new PublicKey(st.mmVenue);
const bookVenue = new PublicKey(st.bookVenue);
const config = pdas.config();
const rng = new Rng(Number(process.env.BOTS_SEED ?? 11));
const SCALE = 1_000_000n;

// ---------------------------------------------------------------------------
// Shared view, refreshed every second
// ---------------------------------------------------------------------------

let view: View | null = null;
const history: { t: number; px: number }[] = [];
let weekendCloses = 0;
let reopens = 0;

async function refreshLoop() {
  let wasOpen: boolean | null = null;
  while (!stopRequested()) {
    try {
      const v = await readView(conn, P, st);
      view = v;
      history.push({ t: v.t, px: Number(v.oracle) / 1e6 });
      if (history.length > 600) history.shift();
      // The test week's weekend is c ∈ [3000, 3600) (docs/localnet.md)
      if (wasOpen === true && !v.open && v.t % 3_600 >= 3_000) weekendCloses++;
      if (wasOpen === false && v.open && v.sinceOpen !== null && (v.t - v.sinceOpen) % 3_600 === 0) reopens++;
      wasOpen = v.open;
    } catch (e) {
      log(C, `view refresh failed: ${(e as Error).message}`);
    }
    await sleep(1_000);
  }
}

function V(): View {
  if (!view) throw new Error("no view yet");
  return view;
}

/** Trading is possible: open, the oracle fresh, not settled or paused. */
function tradable(v = V()): boolean {
  return v.open && oracleFresh(v, 20) && !pool.isSettled(v.m) && !v.m.isPaused;
}

/** Price `minutes` ago (from the shared history), for momentum signals. */
function pastPrice(secs: number): number | null {
  const v = V();
  for (let i = history.length - 1; i >= 0; i--) if (history[i].t <= v.t - secs) return history[i].px;
  return null;
}

// house opens seed their Position by Market.total_positions: one at a time
const openMutex = new Mutex();

// ---------------------------------------------------------------------------
// A wallet with a perps-core account
// ---------------------------------------------------------------------------

interface Pos {
  address: PublicKey;
  isLong: boolean;
  size: bigint;
  collateral: bigint;
  entry: bigint;
  openedAt: number;
  lastFunding: bigint;
  source: string;
}

class Trader {
  readonly kp: Keypair;
  readonly ata: PublicKey;
  readonly user: PublicKey;
  ready = false;
  positions: Pos[] = [];
  holdUntil = new Map<string, number>();

  constructor(readonly name: string, readonly usdc: number) {
    this.kp = namedKey(name);
    this.ata = getAssociatedTokenAddressSync(mint, this.kp.publicKey);
    this.user = pdas.user(this.kp.publicKey, market);
  }

  get key() {
    return this.kp.publicKey;
  }

  tx(kind: string, ixs: TransactionInstruction[], expect: string[] = [], cu?: number): Promise<TxResult> {
    return send(conn, this.name, kind, ixs, [this.kp], { expect, cu });
  }

  async walletUsdc(): Promise<bigint> {
    const b = await conn.getTokenAccountBalance(this.ata).catch(() => null);
    return b ? BigInt(b.value.amount) : 0n;
  }

  async freeCollateral(): Promise<bigint> {
    const info = await conn.getAccountInfo(this.user);
    if (!info) return 0n;
    return big((P.perps.coder.accounts.decode("UserAccount", info.data) as any).collateralBalance);
  }

  depositIx(amount: bigint) {
    return P.perps.methods
      .depositCollateral(new BN(amount.toString()))
      .accounts({ owner: this.key, userAccount: this.user, market, vault, vaultTokenAccount: vaultToken, userTokenAccount: this.ata, tokenProgram: TOKEN_PROGRAM_ID, lpPool })
      .instruction();
  }

  withdrawIx(amount: bigint) {
    return P.perps.methods
      .withdrawCollateral(new BN(amount.toString()))
      .accounts({ owner: this.key, userAccount: this.user, market, vault, vaultTokenAccount: vaultToken, userTokenAccount: this.ata, tokenProgram: TOKEN_PROGRAM_ID, lpPool })
      .instruction();
  }

  /** initialize_user and a first deposit of `share` of the wallet. */
  async ensureUser(share = 0.8): Promise<boolean> {
    if (this.ready) return true;
    if (await conn.getAccountInfo(this.user)) {
      this.ready = true;
      return true;
    }
    const amount = BigInt(Math.floor(this.usdc * share)) * BigInt(USDC);
    const r = await this.tx("initialize_user+deposit", [
      await P.perps.methods.initializeUser().accounts({ owner: this.key, market, userAccount: this.user, systemProgram: SystemProgram.programId }).instruction(),
      await this.depositIx(amount),
    ]);
    this.ready = r.ok;
    return r.ok;
  }

  async refreshPositions(): Promise<Pos[]> {
    const accts = await conn.getProgramAccounts(PERPS_ID, {
      commitment: "confirmed",
      filters: [{ dataSize: 168 }, { memcmp: { offset: 8, bytes: this.key.toBase58() } }, { memcmp: { offset: 133, bytes: "1" } }],
    });
    this.positions = accts
      .map((a) => {
        const p = P.perps.coder.accounts.decode("Position", a.account.data) as any;
        return {
          address: a.pubkey,
          isLong: "long" in p.side,
          size: big(p.size),
          collateral: big(p.collateral),
          entry: big(p.entryPrice),
          openedAt: Number(big(p.openedAt)),
          lastFunding: big(p.lastFundingPayment),
          source: Object.keys(p.executionSource)[0],
        };
      })
      .filter((p) => p.size > 0n);
    return this.positions;
  }

  equity(p: Pos, v = V()): bigint {
    return p.collateral + pool.unrealizedPnl(p.isLong, p.size, p.entry, v.oracle) + pool.accruedFunding(p.isLong, p.size, v.m.cumulativeFunding, p.lastFunding);
  }

  /** Equity / mark notional, bps. */
  healthBps(p: Pos, v = V()): number {
    const n = (p.size * v.oracle) / SCALE;
    return n === 0n ? 0 : Number((this.equity(p, v) * 10_000n) / n);
  }

  /** open_position against the house. `notionalUsd` at the oracle; leverage 3 dp. */
  async openHouse(isLong: boolean, notionalUsd: number, leverage: number, kind = "open_house", expect: string[] = [], slippageBps = 60): Promise<TxResult | null> {
    const v = V();
    const size = BigInt(Math.max(1, Math.floor((notionalUsd / (Number(v.oracle) / 1e6)) * CONTRACT)));
    const q = pool.houseQuote(v.m, v.pool, isLong, size, BigInt(leverage), v.oracle, v.t);
    if (!q.ok) return null;
    const free = await this.freeCollateral();
    if (free < q.value.margin + q.value.fee + 1_000_000n) {
      if (!(await this.topUp(q.value.margin + q.value.fee + 5_000_000n - free))) return null;
    }
    const acceptable = pool.acceptablePrice(isLong, q.value.exec, slippageBps);
    return openMutex.run(async () => {
      const info = await conn.getAccountInfo(market, "confirmed");
      const total = big((P.perps.coder.accounts.decode("Market", info!.data) as any).totalPositions);
      const position = pdas.position(this.key, market, total);
      const ix = await P.perps.methods
        .openPosition({ side: isLong ? 0 : 1, size: new BN(size.toString()), leverage, acceptablePrice: new BN(acceptable.toString()) })
        .accounts({ owner: this.key, userAccount: this.user, market, position, pythPriceFeed: feed, systemProgram: SystemProgram.programId, lpPool })
        .instruction();
      return this.tx(kind, [ix], expect);
    });
  }

  /** Move wallet USDC into the perps account. */
  async topUp(amount: bigint): Promise<boolean> {
    const w = await this.walletUsdc();
    const a = amount < w ? amount : w;
    if (a <= 1_000_000n) return false;
    return (await this.tx("deposit_collateral", [await this.depositIx(a)])).ok;
  }

  /** Positions a close was refused for, and until when not to retry them. */
  closeCooldown = new Map<string, number>();

  async close(p: Pos, kind = "close_position", expect: string[] = [], slippageBps = 80): Promise<TxResult | null> {
    const v = V();
    const key = p.address.toBase58();
    if ((this.closeCooldown.get(key) ?? 0) > v.t) return null;
    // A profitable close waits out the profit-close quiet time after an open (D23);
    // only the prober sends one inside it on purpose
    const profit = pool.unrealizedPnl(p.isLong, p.size, p.entry, v.oracle) > 0n;
    if (profit && !kind.startsWith("probe") && pool.profitCloseQuietLeft(v.m, v.pool, v.t) > 0) return null;
    const exec = pool.closeExecPrice(v.m, v.pool, p.isLong, p.size, v.oracle);
    if (exec === null) return null;
    const acceptable = pool.acceptablePrice(!p.isLong, exec, slippageBps);
    const ix = await P.perps.methods
      .closePosition(new BN(acceptable.toString()))
      .accounts({
        owner: this.key, userAccount: this.user, market, position: p.address, vault, vaultTokenAccount: vaultToken,
        userTokenAccount: this.ata, pythPriceFeed: feed, tokenProgram: TOKEN_PROGRAM_ID,
        userReferral: PERPS_ID, referralCode: PERPS_ID, referrerTokenAccount: PERPS_ID, lpPool,
      })
      .instruction();
    const r = await this.tx(kind, [ix], expect);
    if (r.ok) this.positions = this.positions.filter((x) => !x.address.equals(p.address));
    else this.closeCooldown.set(key, v.t + 30);
    return r;
  }

  async addMargin(p: Pos, amount: bigint, kind = "add_margin"): Promise<TxResult> {
    const free = await this.freeCollateral();
    if (free < amount) await this.topUp(amount - free + 1_000_000n);
    const ix = await P.perps.methods
      .addMargin(new BN(amount.toString()))
      .accounts({ owner: this.key, userAccount: this.user, market, position: p.address, lpPool })
      .instruction();
    return this.tx(kind, [ix]);
  }

  /** perps-core approve_venue / update_venue_approval for `venue` in `role`. */
  async approve(venue: PublicKey, role: number, allowance: bigint, maxLev: number, deviationBps: number, ttl: number): Promise<TxResult> {
    const approval = pdas.approval(this.key, venue, role);
    const params = {
      allowance: new BN(allowance.toString()),
      maxLeverage: maxLev,
      maxBuyPrice: new BN(0),
      minSellPrice: new BN(0),
      maxPriceDeviationBps: deviationBps,
      expiresAt: new BN(V().t + ttl),
      allowedSides: SIDE_LONG_BIT | SIDE_SHORT_BIT,
    };
    const exists = await conn.getAccountInfo(approval);
    const ix = exists
      ? await P.perps.methods.updateVenueApproval(params).accounts({ owner: this.key, market, venue, approval, instructions: SYSVAR_INSTRUCTIONS_PUBKEY }).instruction()
      : await P.perps.methods
          .approveVenue(role, params)
          .accounts({ owner: this.key, market, venue, approval, systemProgram: SystemProgram.programId, instructions: SYSVAR_INSTRUCTIONS_PUBKEY })
          .instruction();
    return this.tx(exists ? "update_venue_approval" : "approve_venue", [ix]);
  }

  /** Close every open position (end of a cycle, or to free collateral). */
  async closeAll(kind = "close_position", expect: string[] = []): Promise<void> {
    for (const p of [...this.positions]) {
      if (stopRequested() || !tradable()) return;
      await this.close(p, kind, expect);
    }
  }
}

// ---------------------------------------------------------------------------
// mm-registry: makers and takers
// ---------------------------------------------------------------------------

interface QuoteRow {
  address: PublicKey;
  mmOwner: PublicKey;
  marketMaker: PublicKey;
  bid: bigint;
  ask: bigint;
  bidRemaining: bigint;
  askRemaining: bigint;
  minFill: bigint;
  expiresAt: number;
  active: boolean;
}

async function fetchQuotes(): Promise<QuoteRow[]> {
  const accts = await P.mm.account.twoSidedQuote.all([{ memcmp: { offset: 8 + 32, bytes: registry.toBase58() } }]);
  const owners = new Map<string, PublicKey>();
  const out: QuoteRow[] = [];
  for (const a of accts) {
    const q = a.account as any;
    const mmKey = (q.marketMaker as PublicKey).toBase58();
    if (!owners.has(mmKey)) {
      const mm = (await P.mm.account.marketMaker.fetch(q.marketMaker)) as any;
      owners.set(mmKey, mm.owner);
    }
    out.push({
      address: a.publicKey,
      mmOwner: owners.get(mmKey)!,
      marketMaker: q.marketMaker,
      bid: big(q.bidPrice),
      ask: big(q.askPrice),
      bidRemaining: big(q.bidRemaining),
      askRemaining: big(q.askRemaining),
      minFill: big(q.minFillSize),
      expiresAt: Number(big(q.expiresAt)),
      active: !!q.isActive,
    });
  }
  return out;
}

// fill_quote closes a quote it fills completely: an update or cancel sent on
// a read from just before that finds the account gone (a client race)
const QUOTE_GONE = "mm_registry:AccountNotInitialized";

class MarketMaker extends Trader {
  quote: PublicKey | null = null;
  lastApproval = 0;
  registered = false;
  constructor(name: string, usdc: number, readonly halfSpreadBps: number, readonly clipContracts: number, readonly maxInventory: number) {
    super(name, usdc);
  }

  get mmPda() {
    return pdas.marketMaker(registry, this.key);
  }

  async setup(): Promise<boolean> {
    if (!(await this.ensureUser(0.9))) return false;
    if (!this.registered) {
      if (!(await conn.getAccountInfo(this.mmPda))) {
        const r = await this.tx("register_mm", [
          await P.mm.methods
            .registerMm(new BN(1_000 * USDC))
            .accounts({
              owner: this.key, registry, marketMaker: this.mmPda, mmCollateralAccount: pdas.mmCollateral(this.mmPda),
              ownerTokenAccount: this.ata, collateralMint: mint, systemProgram: SystemProgram.programId, tokenProgram: TOKEN_PROGRAM_ID,
              rent: new PublicKey("SysvarRent111111111111111111111111111111111"),
            })
            .instruction(),
        ]);
        if (!r.ok) return false;
      }
      this.registered = true;
    }
    const v = V();
    if (v.t - this.lastApproval > 120) {
      // Maker approval: the risk budget, a 30 bps adverse-deviation bound, both sides
      const r = await this.approve(mmVenue, ROLE_MAKER, BigInt(this.usdc) * BigInt(USDC), 10_000, 30, 30 * 86_400);
      if (r.ok) this.lastApproval = v.t;
    }
    return true;
  }

  inventory(): bigint {
    return this.positions.reduce((s, p) => s + (p.isLong ? p.size : -p.size), 0n);
  }

  async step() {
    if (!(await this.setup()) || !tradable()) return;
    await this.refreshPositions();
    const v = V();
    const inv = Number(this.inventory()) / CONTRACT;
    // Lean the quote against inventory
    const lean = Math.max(-1, Math.min(1, inv / this.maxInventory)) * this.halfSpreadBps;
    const mid = Number(v.oracle) * (1 - lean / 10_000);
    const bid = BigInt(Math.floor(mid * (1 - this.halfSpreadBps / 10_000)));
    const ask = BigInt(Math.ceil(mid * (1 + this.halfSpreadBps / 10_000)));
    const clip = new BN(this.clipContracts * CONTRACT);
    const mmInfo = (await P.mm.account.marketMaker.fetch(this.mmPda)) as any;
    let q: any = null;
    if (this.quote) q = await P.mm.account.twoSidedQuote.fetchNullable(this.quote);
    // MM leverage 4x: fills keep margin under the weekend IM (20%) in the pre-close window too
    const leverage = 4_000;
    if (q && q.isActive) {
      const filledBid = big(q.bidSize) - big(q.bidRemaining);
      const filledAsk = big(q.askSize) - big(q.askRemaining);
      const room = (filled: bigint) => new BN((filled + BigInt(this.clipContracts * CONTRACT)).toString());
      const sizeOk = (x: BN) => x.lte(new BN(1_000 * CONTRACT));
      const bidSize = room(filledBid);
      const askSize = room(filledAsk);
      if (sizeOk(bidSize) && sizeOk(askSize)) {
        const r = await this.tx("update_quote", [
          await P.mm.methods
            .updateQuote({ bidPrice: new BN(bid.toString()), bidSize, askPrice: new BN(ask.toString()), askSize, leverage, expiresIn: new BN(20) })
            .accounts({ owner: this.key, registry, marketMaker: this.mmPda, quote: this.quote! })
            .instruction(),
        ], [QUOTE_GONE]);
        if (r.code === QUOTE_GONE) this.quote = null;
      } else {
        await this.tx("cancel_quote", [
          await P.mm.methods.cancelQuote().accounts({ owner: this.key, registry, marketMaker: this.mmPda, quote: this.quote! }).instruction(),
        ], [QUOTE_GONE]);
        this.quote = null;
      }
    } else {
      if (q && this.quote && q.isActive === false) this.quote = null;
      if (Number(mmInfo.activeQuotes) >= Number(mmInfo.maxQuotes)) {
        // An old quote still counts as active (expired, not cancelled): cancel it
        const mine = (await fetchQuotes()).filter((x) => x.mmOwner.equals(this.key) && x.active);
        for (const x of mine) {
          await this.tx("cancel_quote", [
            await P.mm.methods.cancelQuote().accounts({ owner: this.key, registry, marketMaker: this.mmPda, quote: x.address }).instruction(),
          ]);
        }
        return;
      }
      const index = big(mmInfo.quotesPosted);
      const quote = pdas.quote(this.mmPda, index);
      const r = await this.tx("post_quote", [
        await P.mm.methods
          .postQuote({ bidPrice: new BN(bid.toString()), bidSize: clip, askPrice: new BN(ask.toString()), askSize: clip, minFillSize: new BN(CONTRACT), expiresIn: new BN(20), leverage })
          .accounts({ owner: this.key, registry, marketMaker: this.mmPda, quote, systemProgram: SystemProgram.programId })
          .instruction(),
      ]);
      if (r.ok) this.quote = quote;
    }
    // Inventory: hedge against the house when past the limit, and recycle
    // positions (each fill opens one) so the count stays small
    if (Math.abs(inv) > this.maxInventory || this.positions.length > 25) {
      const heavy = this.positions.filter((p) => (inv > 0 ? p.isLong : !p.isLong)).sort((a, b) => a.openedAt - b.openedAt);
      for (const p of heavy.slice(0, 4)) await this.close(p, "mm_hedge_close");
    }
  }
}

/** Fill the best quote for `isBuy`, `contracts` at most. */
async function takeQuote(t: Trader, isBuy: boolean, contracts: number, leverage: number): Promise<TxResult | null> {
  const v = V();
  const quotes = (await fetchQuotes()).filter(
    (q) => q.active && q.expiresAt > v.t + 2 && !q.mmOwner.equals(t.key) && (isBuy ? q.askRemaining : q.bidRemaining) >= BigInt(CONTRACT)
  );
  if (quotes.length === 0) return null;
  quotes.sort((a, b) => (isBuy ? Number(a.ask - b.ask) : Number(b.bid - a.bid)));
  const q = quotes[0];
  const remaining = isBuy ? q.askRemaining : q.bidRemaining;
  let size = BigInt(contracts * CONTRACT);
  if (size > remaining) size = remaining;
  if (size < q.minFill) return null;
  const price = isBuy ? q.ask : q.bid;
  const limit = pool.acceptablePrice(isBuy, price, 5);
  // Worst-case margin as perps-core sizes it (venue_margin at the limit, the stricter IM)
  const notional = (size * limit) / SCALE;
  const imr = BigInt(Math.max(Number(v.m.initialMarginRatio), Number(v.m.weekendInitialMarginRatio)));
  const allowance = (notional * 1000n) / BigInt(leverage) + (notional * imr) / 10_000n + notional / 50n + 1_000_000n;
  const free = await t.freeCollateral();
  if (free < allowance) await t.topUp(allowance - free + 1_000_000n);
  const approval = pdas.approval(t.key, mmVenue, ROLE_TAKER);
  const params = {
    allowance: new BN(allowance.toString()),
    maxLeverage: leverage,
    maxBuyPrice: new BN(isBuy ? limit.toString() : "0"),
    minSellPrice: new BN(isBuy ? "0" : limit.toString()),
    maxPriceDeviationBps: 0,
    expiresAt: new BN(v.t + 120),
    allowedSides: isBuy ? SIDE_LONG_BIT : SIDE_SHORT_BIT,
  };
  const exists = await conn.getAccountInfo(approval);
  const approveIx = exists
    ? await P.perps.methods.updateVenueApproval(params).accounts({ owner: t.key, market, venue: mmVenue, approval, instructions: SYSVAR_INSTRUCTIONS_PUBKEY }).instruction()
    : await P.perps.methods
        .approveVenue(ROLE_TAKER, params)
        .accounts({ owner: t.key, market, venue: mmVenue, approval, systemProgram: SystemProgram.programId, instructions: SYSVAR_INSTRUCTIONS_PUBKEY })
        .instruction();
  const nonce = new BN(Math.floor(rng.next() * 2 ** 48));
  const takerPosition = pdas.vposition(t.key, market, nonce);
  const fill = await P.mm.methods
    .fillQuote({ size: new BN(size.toString()), isBuy, limitPrice: new BN(limit.toString()), takerLeverage: leverage, allowPartial: true, positionNonce: nonce })
    .accounts({
      taker: t.key, registry, marketMaker: q.marketMaker, quote: q.address, mmOwner: q.mmOwner,
      venueAuthority: pdas.venueAuthority(MM_REGISTRY_ID, market), perpsConfig: config, perpsVenue: mmVenue,
      venueProgramData: pdas.programData(MM_REGISTRY_ID), market, pythPriceFeed: feed,
      takerUser: t.user, takerApproval: approval, takerPosition,
      mmUser: pdas.user(q.mmOwner, market), mmApproval: pdas.approval(q.mmOwner, mmVenue, ROLE_MAKER),
      mmPosition: pdas.vmaker(takerPosition), perpsLpPool: lpPool, perpsCoreProgram: PERPS_ID, systemProgram: SystemProgram.programId,
    })
    .instruction();
  // The quote can be gone by the time this lands: the MM cancelled it, or
  // another fill used up both sides (fill_quote closes it), after the read above
  return t.tx("fill_quote", [approveIx, fill], [QUOTE_GONE], 500_000);
}

// ---------------------------------------------------------------------------
// orderbook: limit traders and the matcher
// ---------------------------------------------------------------------------

interface OrderRow {
  address: PublicKey;
  owner: PublicKey;
  bid: boolean;
  price: bigint;
  remaining: bigint;
  seq: bigint;
  expiresAt: number;
  minFill: bigint;
  open: boolean;
  leverage: number;
}

async function fetchOrders(): Promise<OrderRow[]> {
  const accts = await P.book.account.order.all([{ memcmp: { offset: 8 + 32, bytes: market.toBase58() } }]);
  return accts.map((a) => {
    const o = a.account as any;
    const status = Object.keys(o.status)[0];
    return {
      address: a.publicKey,
      owner: o.owner,
      bid: "bid" in o.side,
      price: big(o.price),
      remaining: big(o.size) - big(o.filledSize),
      seq: big(o.sequenceNumber),
      expiresAt: Number(big(o.expiresAt)),
      minFill: big(o.minFillSize),
      open: status === "open" || status === "partiallyFilled",
      leverage: Number(o.leverage),
    };
  });
}

// place_order seeds the Order by the book's sequence number: one at a time
const bookMutex = new Mutex();

function placeOrder(t: Trader, bid: boolean, price: bigint, contracts: number, leverage: number, ttl: number, kind = "place_order"): Promise<TxResult> {
  return bookMutex.run(() => placeOrderNow(t, bid, price, contracts, leverage, ttl, kind));
}

async function placeOrderNow(t: Trader, bid: boolean, price: bigint, contracts: number, leverage: number, ttl: number, kind: string): Promise<TxResult> {
  const ob = (await P.book.account.orderBook.fetch(orderbook, "confirmed")) as any;
  const order = pdas.order(orderbook, t.key, big(ob.sequenceNumber));
  const ix = await P.book.methods
    .placeOrder({
      side: bid ? { bid: {} } : { ask: {} },
      orderType: { limit: {} },
      price: new BN(price.toString()),
      size: new BN(contracts * CONTRACT),
      leverage,
      expiresAt: new BN(V().t + ttl),
      maxFills: 4,
      minFillSize: new BN(0),
    })
    .accounts({ owner: t.key, orderbook, market, order, systemProgram: SystemProgram.programId })
    .instruction();
  return t.tx(kind, [ix]);
}

async function closeExpiredOrders(t: Trader) {
  const v = V();
  const mine = (await fetchOrders()).filter((o) => o.owner.equals(t.key) && (!o.open || o.expiresAt < v.t));
  for (const o of mine.slice(0, 3)) {
    await t.tx("close_expired_order", [
      await P.book.methods.closeExpiredOrder().accounts({ orderbook, order: o.address, owner: t.key }).instruction(),
    ]);
  }
}

class BookTrader extends Trader {
  approvedAt = 0;
  async setup(): Promise<boolean> {
    if (!(await this.ensureUser(0.8))) return false;
    if (V().t - this.approvedAt > 600) {
      // Resting orders end up either taker or maker: approve both roles
      const budget = BigInt(this.usdc) * BigInt(USDC);
      const a = await this.approve(bookVenue, ROLE_TAKER, budget, 5_000, 0, 30 * 86_400);
      const b = await this.approve(bookVenue, ROLE_MAKER, budget, 5_000, 0, 30 * 86_400);
      if (a.ok && b.ok) this.approvedAt = V().t;
    }
    return true;
  }
  async step() {
    if (!(await this.setup()) || !tradable()) return;
    await this.refreshPositions();
    const v = V();
    // Keep margin for fills in the perps account
    const free = await this.freeCollateral();
    const floor = (BigInt(this.usdc) * BigInt(USDC)) / 5n;
    if (free < floor) await this.topUp(floor - free);
    // Rest a bid or ask near the oracle; sometimes cross it so the matcher fills
    const bid = rng.chance(0.5);
    const off = rng.range(-12, 6) / 10_000;
    const px = BigInt(Math.round(Number(v.oracle) * (bid ? 1 - off : 1 + off)));
    await placeOrder(this, bid, px, rng.int(20, 120), 5_000, 90);
    if (rng.chance(0.3)) await closeExpiredOrders(this);
    // Positions from fills: close the older ones
    for (const p of this.positions) if (v.t - p.openedAt > rng.range(120, 600)) await this.close(p);
  }
}

const matcher = namedKey("matcher");
let matchNonce = BigInt(Math.floor(Date.now() / 1000)) << 16n;
// A pair perps-core refused (an owner short of margin, out of band) waits a minute
const refusedPairs = new Map<string, number>();

async function matchOnce(): Promise<void> {
  const v = V();
  if (!tradable(v)) return;
  const orders = (await fetchOrders()).filter((o) => o.open && o.expiresAt > v.t + 1 && o.remaining > 0n);
  const bids = orders.filter((o) => o.bid).sort((a, b) => Number(b.price - a.price) || Number(a.seq - b.seq));
  const asks = orders.filter((o) => !o.bid).sort((a, b) => Number(a.price - b.price) || Number(a.seq - b.seq));
  let done = 0;
  for (const b of bids) {
    for (const a of asks) {
      if (done >= 3) return;
      if (b.price < a.price) break;
      if (b.owner.equals(a.owner)) continue;
      const pairKey = `${b.address.toBase58()}:${a.address.toBase58()}`;
      if ((refusedPairs.get(pairKey) ?? 0) > v.t) continue;
      const takerIsBid = b.seq > a.seq;
      const taker = takerIsBid ? b : a;
      const maker = takerIsBid ? a : b;
      // Split at the venue's per-fill notional cap ($50k)
      const cap = (50_000n * BigInt(USDC) * SCALE) / maker.price;
      let fill = b.remaining < a.remaining ? b.remaining : a.remaining;
      if (fill > cap) fill = cap;
      if (fill < b.minFill && fill !== b.remaining) continue;
      if (fill < a.minFill && fill !== a.remaining) continue;
      const nonce = new BN((matchNonce++).toString());
      const takerPosition = pdas.vposition(taker.owner, market, nonce);
      const ix = await P.book.methods
        .matchOrders(nonce, new BN(fill.toString()))
        .accounts({
          matcher: matcher.publicKey, config, orderbook, bidOrder: b.address, askOrder: a.address, bidOwner: b.owner, askOwner: a.owner,
          venueAuthority: pdas.venueAuthority(ORDERBOOK_ID, market), perpsVenue: bookVenue, venueProgramData: pdas.programData(ORDERBOOK_ID),
          market, pythPriceFeed: feed, takerUser: pdas.user(taker.owner, market), takerApproval: pdas.approval(taker.owner, bookVenue, ROLE_TAKER),
          takerPosition, makerUser: pdas.user(maker.owner, market), makerApproval: pdas.approval(maker.owner, bookVenue, ROLE_MAKER),
          makerPosition: pdas.vmaker(takerPosition), perpsLpPool: lpPool, perpsCoreProgram: PERPS_ID, systemProgram: SystemProgram.programId,
        })
        .instruction();
      const r = await send(conn, "matcher", "match_orders", [ix], [matcher], { cu: 500_000 });
      done++;
      if (r.ok) {
        b.remaining -= fill;
        a.remaining -= fill;
      } else refusedPairs.set(pairKey, v.t + 60);
      if (b.remaining <= 0n) break;
    }
  }
}

// ---------------------------------------------------------------------------
// Retail
// ---------------------------------------------------------------------------

type Persona = "momentum" | "meanrev" | "degen" | "house" | "swing";

class Retail extends Trader {
  constructor(name: string, usdc: number, readonly persona: Persona) {
    super(name, usdc);
  }
  async step() {
    if (!(await this.ensureUser(0.7)) || !tradable()) return;
    await this.refreshPositions();
    const v = V();
    const nearClose = v.untilClose !== null && v.untilClose < 150;
    // Closes: hold time over, or flatten before the weekend (degens carry)
    for (const p of [...this.positions]) {
      const key = p.address.toBase58();
      if (!this.holdUntil.has(key)) this.holdUntil.set(key, p.openedAt + rng.range(60, this.persona === "swing" ? 1_200 : 400));
      const flatten = nearClose && this.persona !== "degen" && rng.chance(0.6);
      if (v.t >= this.holdUntil.get(key)! || flatten) await this.close(p);
      else if (this.persona === "swing" && this.healthBps(p) < 900) await this.addMargin(p, p.collateral / 3n);
    }
    if (this.positions.length >= (this.persona === "degen" ? 2 : 3) || !rng.chance(0.5)) return;
    const lmax = maxLeverage(v);
    const p30 = pastPrice(30);
    const now = Number(v.oracle) / 1e6;
    let isLong = rng.chance(0.5);
    if (p30 !== null && this.persona === "momentum") isLong = now >= p30;
    if (p30 !== null && this.persona === "meanrev") isLong = now < p30;
    const leverage = this.persona === "degen" ? lmax : Math.min(lmax, rng.int(2, 8) * 1_000);
    const free = Number(await this.freeCollateral()) / 1e6 + Number(await this.walletUsdc()) / 1e6;
    const notional = Math.min(free * (leverage / 1_000) * rng.range(0.2, 0.6), 40_000);
    if (notional < 200) return;
    if (this.persona !== "house" && rng.chance(0.4)) await takeQuote(this, isLong, Math.max(1, Math.floor(notional / now)), Math.min(leverage, 5_000));
    else await this.openHouse(isLong, notional, leverage);
    if (rng.chance(0.05)) {
      const free = await this.freeCollateral();
      if (free > 2_000_000_000n) await this.tx("withdraw_collateral", [await this.withdrawIx(free / 4n)]);
    }
  }
}

// ---------------------------------------------------------------------------
// Adversaries (sim/src/adversary.rs, the ones that make sense live)
// ---------------------------------------------------------------------------

const CAP_REFUSALS = ["perps_core:PoolExposureLimit", "perps_core:PoolWeekendExposureLimit", "perps_core:AccountExposureLimit"];

/**
 * Trades into the net cap from four accounts (so the per-account cap does
 * not bind first), all one way, until refused, and holds it there: the net
 * sits at the cap most of the week, where LP claims meet the weekend
 * withdraw line (D45) and, after the G raise, the weekend cap binds.
 */
class Whale extends Trader {
  static direction = true;
  static flipAt = 0;
  async step() {
    if (!(await this.ensureUser(0.9)) || !tradable()) return;
    const v = V();
    // One direction per test week; flip at the weekly reopen
    const weekStart = v.sinceOpen !== null && (v.t - v.sinceOpen) % 3_600 === 0;
    if (weekStart && v.sinceOpen! < 30 && v.t - Whale.flipAt > 600) {
      Whale.direction = rng.chance(0.5);
      Whale.flipAt = v.t;
    }
    await this.refreshPositions();
    const wrongWay = this.positions.filter((p) => p.isLong !== Whale.direction);
    for (const p of wrongWay) await this.close(p, "whale_close", CAP_REFUSALS);
    if (rng.chance(0.03)) {
      for (const p of this.positions) await this.close(p, "whale_close", CAP_REFUSALS);
      return;
    }
    await this.openHouse(Whale.direction, rng.range(20_000, 60_000), Math.min(maxLeverage(v), 5_000), "whale_open_into_cap", CAP_REFUSALS);
  }
}

/** Two wallets of one actor self-cross on the orderbook, then close one leg against the house to push the net (D25). */
class CrossPair {
  phase = 0;
  phaseAt = 0;
  constructor(readonly a: BookTrader, readonly b: BookTrader) {}
  async step() {
    if (!(await this.a.setup()) || !(await this.b.setup()) || !tradable()) return;
    const v = V();
    await this.a.refreshPositions();
    await this.b.refreshPositions();
    if (this.phase === 0) {
      // a bids, b asks at the oracle: the matcher crosses them
      const px = v.oracle;
      const size = rng.int(200, 500);
      await placeOrder(this.a, true, px, size, 5_000, 60, "self_cross_order");
      await placeOrder(this.b, false, px, size, 5_000, 60, "self_cross_order");
      this.phase = 1;
      this.phaseAt = v.t;
    } else if (this.phase === 1 && v.t - this.phaseAt > 20) {
      // Close one leg against the house: raises |net|, capped like an open
      const leg = rng.chance(0.5) ? this.a : this.b;
      for (const p of leg.positions.filter((p) => p.source === "venue")) await leg.close(p, "self_cross_leg_close", CAP_REFUSALS);
      this.phase = 2;
      this.phaseAt = v.t;
    } else if (this.phase === 2 && v.t - this.phaseAt > rng.range(60, 240)) {
      for (const t of [this.a, this.b]) for (const p of t.positions) await t.close(p, "self_cross_unwind", CAP_REFUSALS);
      await closeExpiredOrders(this.a);
      await closeExpiredOrders(this.b);
      this.phase = 0;
    }
  }
}

/**
 * Carries thin positions into the weekend (one long, one short): opens just
 * before the pre-close window at a leverage whose equity already covers the
 * weekend maintenance margin plus GAPPER_BUFFER_BPS (so the window's jump
 * to the weekend MMR does not liquidate it on the spot), and tops back up to
 * that from before the window to the close. A reopen gap of more than the
 * buffer against one of them liquidates it; a large one leaves bad debt.
 */
const GAPPER_BUFFER_BPS = 150;
class Gapper extends Trader {
  constructor(name: string, usdc: number, readonly isLong: boolean) {
    super(name, usdc);
  }
  async step() {
    if (!(await this.ensureUser(0.9)) || !tradable()) return;
    const v = V();
    await this.refreshPositions();
    const u = v.untilClose;
    const w = v.m.preCloseWindowSecs;
    const wMmr = Number(v.m.weekendMaintenanceMarginRatio);
    const mmr = Number(v.m.maintenanceMarginRatio);
    if (u !== null && u > w + 30 && u < w + 120 && this.positions.length === 0) {
      const lev = gapperLeverage(wMmr, mmr, GAPPER_BUFFER_BPS, Number(v.pool.params.maxImpactBps), maxLeverage(v));
      await this.openHouse(this.isLong, rng.range(60_000, 110_000), lev, "gapper_open");
    } else if (u !== null && u <= w + 30 && u > 30) {
      // From before the window (the MMR in force jumps to the weekend one at W)
      for (const p of this.positions) {
        const target = gapperTopUpTarget((p.size * v.oracle) / SCALE, wMmr, mmr, GAPPER_BUFFER_BPS);
        const e = this.equity(p, v);
        if (e < target - 1_000_000n) await this.addMargin(p, target - e, "gapper_top_up");
      }
    } else if (v.sinceOpen !== null && v.sinceOpen > 400 && u !== null && u > 600) {
      // Mid-week: flatten what survived the gap
      for (const p of this.positions) await this.close(p, "gapper_close");
    }
  }
}

/** Holds a small position over the weekend and closes right after the reopen, inside the profit-close quiet time (D23). */
class QuietProber extends Trader {
  probedClosed = 0;
  probedStale = 0;
  async step() {
    if (!(await this.ensureUser(0.8))) return;
    const v = V();
    if (!v.open) {
      // Probe the session gate once per closed spell (MarketClosed expected)
      if (v.t - this.probedClosed > 120 && oracleFresh(v)) {
        this.probedClosed = v.t;
        await this.openHouse(true, 1_000, 2_000, "probe_open_while_closed", ["perps_core:MarketClosed"]);
      }
      return;
    }
    if (v.open && !oracleFresh(v) && (v.sinceOpen ?? 99) < 15 && v.t - this.probedStale > 60) {
      // The price driver's stale break: open on a feed older than 60 s
      this.probedStale = v.t;
      await this.openHouse(true, 1_000, 2_000, "probe_open_stale_oracle", ["perps_core:StaleOraclePrice"]);
      return;
    }
    if (!tradable()) return;
    await this.refreshPositions();
    const u = v.untilClose;
    if (u !== null && u < 200 && u > 30 && this.positions.length < 2) {
      await this.openHouse(rng.chance(0.5), 5_000, 2_000, "probe_carry_open");
    } else if (v.sinceOpen !== null && v.sinceOpen < Number(v.pool.params.profitCloseQuietSecs) - 5) {
      // A profitable close is refused inside the quiet time; a losing one goes through
      for (const p of this.positions) await this.close(p, "probe_close_in_profit_quiet", ["perps_core:ProfitCloseQuiet"]);
    } else if (v.sinceOpen !== null && v.sinceOpen > 90 && u !== null && u > 600) {
      for (const p of this.positions) await this.close(p, "probe_close");
    }
  }
}

// ---------------------------------------------------------------------------
// LPs
// ---------------------------------------------------------------------------

type LpPersona = "steady" | "gap" | "buffer" | "lapse" | "big";

class Lp extends Trader {
  position: PublicKey;
  deposited = false;
  nextAction = 0;
  constructor(name: string, usdc: number, readonly persona: LpPersona) {
    super(name, usdc);
    this.position = pdas.lpPosition(lpPool, this.key);
  }

  async lpPos(): Promise<pool.LpPositionAccount | null> {
    const info = await conn.getAccountInfo(this.position);
    return info ? pool.decodeLpPosition(P.perps.coder.accounts.decode("LpPosition", info.data)) : null;
  }

  /** Crank funding first when it is due: LP flows refuse a stale accumulator (FundingStale). */
  async crankIfDue(v: View) {
    if (!pool.lpGates(v.m, v.pool, v.pv, v.t).needsFundingCrank) return;
    await this.tx("update_funding", [
      await P.perps.methods.updateFunding().accounts({ keeper: this.key, market, pythPriceFeed: feed }).instruction(),
    ], ["perps_core:FundingNotDue"]);
  }

  async deposit(amount: bigint, kind = "lp_deposit", expect: string[] = []) {
    const v = V();
    if (!v.pv) return;
    const shares = pool.depositShares(v.pool, v.pv, amount);
    const ixs: TransactionInstruction[] = [];
    if (!(await conn.getAccountInfo(this.position))) {
      ixs.push(await P.perps.methods.initLpPosition(0).accounts({ owner: this.key, lpPool, lpPosition: this.position, systemProgram: SystemProgram.programId }).instruction());
    }
    const minOut = shares === null ? 0n : (shares * 9_950n) / 10_000n;
    ixs.push(
      await P.perps.methods
        .lpDeposit(new BN(amount.toString()), new BN(minOut.toString()))
        .accounts({ owner: this.key, market, lpPool, lpPosition: this.position, vaultTokenAccount: vaultToken, ownerTokenAccount: this.ata, pythPriceFeed: feed, tokenProgram: TOKEN_PROGRAM_ID })
        .instruction()
    );
    const r = await this.tx(kind, ixs, expect);
    if (r.ok) this.deposited = true;
  }

  async request(shares: bigint, kind = "lp_request_withdrawal", expect: string[] = []) {
    await this.tx(kind, [
      await P.perps.methods
        .lpRequestWithdrawal(new BN(shares.toString()))
        .accounts({ owner: this.key, market, lpPool, lpPosition: this.position, pythPriceFeed: feed })
        .instruction(),
    ], expect);
  }

  async claim(kind = "lp_withdraw", expect: string[] = []) {
    return this.tx(kind, [
      await P.perps.methods
        .lpWithdraw()
        .accounts({ owner: this.key, market, lpPool, lpPosition: this.position, vault, vaultTokenAccount: vaultToken, ownerTokenAccount: this.ata, pythPriceFeed: feed, tokenProgram: TOKEN_PROGRAM_ID })
        .instruction(),
    ], expect);
  }

  async cancel(kind = "lp_cancel_withdrawal") {
    await this.tx(kind, [await P.perps.methods.lpCancelWithdrawal().accounts({ owner: this.key, lpPool, lpPosition: this.position }).instruction()]);
  }

  async step() {
    const v = V();
    if (!tradable(v) || v.t < this.nextAction) return;
    const gates = pool.lpGates(v.m, v.pool, v.pv, v.t);
    await this.crankIfDue(v);
    const wallet = await this.walletUsdc();
    const pos = await this.lpPos();
    const req = pos && v.pv ? pool.withdrawalRequest(v.m, v.pool, pos, v.pv, v.t) : null;
    const nn = v.pv ? Number(v.pv.netNotional) : 0;
    const b = v.pv ? Number(v.pv.b) : 1;
    switch (this.persona) {
      case "steady": {
        if (!this.deposited && gates.deposit.length === 0) await this.deposit((wallet * 2n) / 3n);
        else if (req?.state === "claimable" && gates.withdraw.length === 0) await this.claim("lp_withdraw", ["perps_core:PoolCapWouldBreach"]);
        else if (req?.state === "lapsed") await this.cancel();
        else if (pos && req?.state === "none" && rng.chance(0.3)) await this.request(pos.shares / 10n);
        else if (gates.deposit.length === 0 && wallet > 10_000_000_000n && rng.chance(0.3)) await this.deposit(wallet / 4n);
        this.nextAction = v.t + rng.range(40, 120);
        break;
      }
      case "gap": {
        // In right after the reopen quiet time, out before the window
        const since = v.sinceOpen ?? 0;
        if (gates.deposit.length === 0 && since < 300 && wallet > 10_000_000_000n) await this.deposit((wallet * 3n) / 4n);
        else if (pos && req?.state === "none" && v.untilClose !== null && v.untilClose < 600) await this.request(pos.shares / 2n);
        else if (req?.state === "claimable" && gates.withdraw.length === 0) await this.claim("lp_withdraw", ["perps_core:PoolCapWouldBreach"]);
        else if (req?.state === "lapsed") await this.cancel();
        this.nextAction = v.t + 20;
        break;
      }
      case "buffer": {
        // Request once traders sit near the cap; claim at the line (D45): the
        // exit buffer lets some out, the weekend withdraw line refuses the rest
        if (!this.deposited && gates.deposit.length === 0) await this.deposit((wallet * 3n) / 4n);
        else if (pos && req?.state === "none" && nn > 0.3 * b) await this.request(pos.shares / 4n, "lp_request_at_cap");
        else if (req?.state === "claimable" && !gates.inWindow && !gates.inQuiet) await this.claim("lp_withdraw_at_line", ["perps_core:PoolCapWouldBreach"]);
        else if (req?.state === "lapsed") await this.cancel();
        this.nextAction = v.t + 15;
        break;
      }
      case "big": {
        // A large LP (about a quarter of B) that asks for most of its shares
        // back once traders sit near the cap: a claim above the exit room
        // left under the weekend withdraw line is refused (D45,
        // PoolCapWouldBreach) and retried until it fits or lapses; one that
        // fits is paid and the LP comes back in later
        if (!this.deposited && !pos && gates.deposit.length === 0) await this.deposit((wallet * 17n) / 20n, "lp_deposit_big");
        else if (pos && pos.shares > 0n && req?.state === "none" && nn > 0.3 * b) await this.request((pos.shares * 9n) / 10n, "lp_request_big");
        else if (req?.state === "claimable" && !gates.inWindow && !gates.inQuiet) {
          const r = await this.claim("lp_withdraw_big_at_line", ["perps_core:PoolCapWouldBreach"]);
          if (r.ok) this.nextAction = v.t + 300;
        } else if (req?.state === "lapsed") await this.cancel();
        else if (req?.state === "none" && gates.deposit.length === 0 && wallet > 100_000_000_000n && nn < 0.2 * b) await this.deposit((wallet * 4n) / 5n, "lp_deposit_big");
        this.nextAction = Math.max(this.nextAction, v.t + 20);
        break;
      }
      case "lapse": {
        // Requests and never claims in time: the request lapses (WithdrawalExpired)
        if (!this.deposited && gates.deposit.length === 0) await this.deposit((wallet * 3n) / 4n);
        else if (pos && req?.state === "none") await this.request(pos.shares / 3n);
        else if (req?.state === "lapsed") {
          await this.claim("lp_withdraw_lapsed", ["perps_core:WithdrawalExpired"]);
          await this.cancel();
        }
        this.nextAction = v.t + 60;
        break;
      }
    }
  }
}

// ---------------------------------------------------------------------------
// The mid-run G raise (params.ts G_RAISE_*)
// ---------------------------------------------------------------------------

const startedAt = Date.now() / 1000;
let gRaised = false;

/** The admin raises the market's weekend IM once (update_market_config): §3.1 "a later raise of G". */
async function raiseG() {
  if (gRaised || G_RAISE_AFTER_SECS <= 0 || Date.now() / 1000 - startedAt < G_RAISE_AFTER_SECS) return;
  const v = V();
  // Mid-week, well before W, so the raise lands on open net (not into a close)
  if (!v.open || v.untilClose === null || v.untilClose < v.m.preCloseWindowSecs + 300) return;
  if (Number(v.m.weekendInitialMarginRatio) >= G_RAISE_TO) {
    gRaised = true;
    return;
  }
  const r = await send(conn, "admin", "update_market_config_raise_g", [
    await P.perps.methods
      .updateMarketConfig({ ...MARKET_CONFIG, weekendInitialMarginRatio: G_RAISE_TO })
      .accounts({ authority: admin.publicKey, config, market })
      .instruction(),
  ], [admin]);
  if (r.ok) {
    gRaised = true;
    const nn = v.pv ? Number(v.pv.netNotional) / 1e6 : 0;
    const b = v.pv ? Number(v.pv.b) / 1e6 : 0;
    log(C, `G raised: weekend IM ${v.m.weekendInitialMarginRatio} → ${G_RAISE_TO} bps (net notional $${nn.toFixed(0)}, B $${b.toFixed(0)}, line α·B/G now $${((b * Number(v.pool.params.weekendLpLossBps)) / G_RAISE_TO).toFixed(0)})`);
  }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

async function agentLoop(name: string, periodMs: [number, number], step: () => Promise<void>) {
  await sleep(rng.range(0, 3_000));
  while (!stopRequested()) {
    try {
      if (view) await step();
    } catch (e) {
      log(C, `${name}: ${(e as Error).message.split("\n")[0]}`);
    }
    await sleep(rng.range(periodMs[0], periodMs[1]));
  }
}

async function main() {
  const usdcOf = (n: string) => WALLETS.find((w) => w.name === n)!.usdc;
  const mms = [new MarketMaker("mm-tight", usdcOf("mm-tight"), 8, 60, 600), new MarketMaker("mm-wide", usdcOf("mm-wide"), 22, 150, 900)];
  const retail = [
    new Retail("momentum-1", usdcOf("momentum-1"), "momentum"),
    new Retail("momentum-2", usdcOf("momentum-2"), "momentum"),
    new Retail("meanrev-1", usdcOf("meanrev-1"), "meanrev"),
    new Retail("degen-1", usdcOf("degen-1"), "degen"),
    new Retail("degen-2", usdcOf("degen-2"), "degen"),
    new Retail("house-1", usdcOf("house-1"), "house"),
    new Retail("swing-1", usdcOf("swing-1"), "swing"),
  ];
  const book = [new BookTrader("limit-1", usdcOf("limit-1")), new BookTrader("limit-2", usdcOf("limit-2"))];
  const whales = ["whale-1", "whale-2", "whale-3", "whale-4"].map((n) => new Whale(n, usdcOf(n)));
  const cross = new CrossPair(new BookTrader("cross-a", usdcOf("cross-a")), new BookTrader("cross-b", usdcOf("cross-b")));
  const gappers = [new Gapper("gapper-1", usdcOf("gapper-1"), true), new Gapper("gapper-2", usdcOf("gapper-2"), false)];
  const prober = new QuietProber("quiet-prober", usdcOf("quiet-prober"));
  const lps = [
    new Lp("lp-steady", usdcOf("lp-steady"), "steady"),
    new Lp("lp-gap", usdcOf("lp-gap"), "gap"),
    new Lp("lp-buffer", usdcOf("lp-buffer"), "buffer"),
    new Lp("lp-lapse", usdcOf("lp-lapse"), "lapse"),
    new Lp("lp-big", usdcOf("lp-big"), "big"),
  ];
  log(C, `starting ${mms.length} MMs, ${retail.length} retail, ${book.length} limit traders + matcher, ${whales.length} whales, a self-cross pair, ${gappers.length} gappers, a quiet-time prober, ${lps.length} LPs`);
  process.on("SIGTERM", () => {
    log(C, "SIGTERM");
    process.exit(0);
  });

  const loops: Promise<void>[] = [refreshLoop()];
  for (const m of mms) loops.push(agentLoop(m.name, [2_500, 4_000], () => m.step()));
  for (const r of retail) loops.push(agentLoop(r.name, [4_000, 10_000], () => r.step()));
  for (const b of book) loops.push(agentLoop(b.name, [5_000, 9_000], () => b.step()));
  loops.push(agentLoop("matcher", [1_500, 2_500], () => matchOnce()));
  for (const w of whales) loops.push(agentLoop(w.name, [6_000, 12_000], () => w.step()));
  loops.push(agentLoop("cross", [5_000, 8_000], () => cross.step()));
  for (const g of gappers) loops.push(agentLoop(g.name, [4_000, 6_000], () => g.step()));
  loops.push(agentLoop(prober.name, [3_000, 5_000], () => prober.step()));
  for (const l of lps) loops.push(agentLoop(l.name, [3_000, 5_000], () => l.step()));
  loops.push(agentLoop("admin", [5_000, 5_000], () => raiseG()));
  loops.push(
    (async () => {
      while (!stopRequested()) {
        await sleep(60_000);
        log(C, `alive: weekend closes ${weekendCloses}, reopens ${reopens}, oracle $${view ? (Number(view.oracle) / 1e6).toFixed(3) : "?"}`);
      }
    })()
  );
  await Promise.all(loops);
  log(C, "stop requested");
}

void ComputeBudgetProgram;
main().catch((e) => {
  console.error(e);
  process.exit(1);
});

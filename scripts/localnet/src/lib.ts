// Shared pieces of the live local simulation (docs/localnet.md): localhost
// guard, program ids, keys, the state file written by setup.ts, Anchor
// programs from target/idl, PDAs, the mock-pyth instructions, transaction
// sending with error classification, and the JSONL logs the checker reads.
//
// LOCALNET ONLY. Every connection goes through `connect()`, which refuses any
// RPC URL that is not on this machine.

import * as anchor from "@coral-xyz/anchor";
import {
  ComputeBudgetProgram,
  Connection,
  Keypair,
  PublicKey,
  SYSVAR_CLOCK_PUBKEY,
  SendTransactionError,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import * as fs from "fs";
import * as path from "path";
import * as util from "util";

export { anchor };
export const BN = anchor.BN;
export type BN = anchor.BN;

export const ROOT = path.resolve(__dirname, "../../..");
export const LOCALNET_DIR = process.env.LOCALNET_DIR ?? path.join(ROOT, "target/localnet");
export const KEYS_DIR = path.join(LOCALNET_DIR, "keys");
export const LOG_DIR = path.join(LOCALNET_DIR, "logs");
export const STATE_FILE = path.join(LOCALNET_DIR, "state.json");
export const STOP_FILE = path.join(LOCALNET_DIR, "stop");
export const RPC_URL = process.env.RPC_URL ?? "http://127.0.0.1:8899";

export const PERPS_ID = new PublicKey("Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS");
export const MM_REGISTRY_ID = new PublicKey("MMReg11111111111111111111111111111111111111");
export const ORDERBOOK_ID = new PublicKey("2XhUqeGhdkhKdZXkjHmMmFMXBJfYVVCbFxKjKqkCdMzt");
export const PROP_AMM_ID = new PublicKey("PropAMM111111111111111111111111111111111111");
export const VAMM_ID = new PublicKey("3Vft6oJxPvHXELuqrPgCdPqJf1LxGpqDxBdqNhJxLZbm");
/** TEST ONLY oracle (programs/mock-pyth). Never on a public cluster. */
export const MOCK_PYTH_ID = new PublicKey("MockPyth11111111111111111111111111111111111");
export const BPF_LOADER_UPGRADEABLE = new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111");

export const SCHEDULE_TEST_HOURLY = 2;
export const USDC = 1_000_000;
export const CONTRACT = 1_000_000;
export const LP_DEAD_SHARES = 1_000_000n;

// ---------------------------------------------------------------------------
// Localhost guard
// ---------------------------------------------------------------------------

const LOCAL_HOSTS = new Set(["127.0.0.1", "localhost", "0.0.0.0", "[::1]", "::1"]);

/** Throws unless `url` points at this machine: the harness never talks to a public cluster. */
export function assertLocal(url: string): void {
  const host = new URL(url).hostname;
  if (!LOCAL_HOSTS.has(host)) {
    throw new Error(`localnet harness: refusing non-local RPC ${url} (only localhost is allowed)`);
  }
}

export function connect(url = RPC_URL): Connection {
  assertLocal(url);
  return new Connection(url, { commitment: "confirmed", disableRetryOnRateLimit: true });
}

// ---------------------------------------------------------------------------
// Keys and state
// ---------------------------------------------------------------------------

export function loadKeypair(file: string): Keypair {
  return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(file, "utf-8"))));
}

export function walletPath(): string {
  return process.env.ANCHOR_WALLET ?? path.join(process.env.HOME ?? "", ".config/solana/id.json");
}

/** The key `name` under target/localnet/keys, created on first use. */
export function namedKey(name: string): Keypair {
  fs.mkdirSync(KEYS_DIR, { recursive: true });
  const file = path.join(KEYS_DIR, `${name}.json`);
  if (fs.existsSync(file)) return loadKeypair(file);
  const kp = Keypair.generate();
  fs.writeFileSync(file, JSON.stringify(Array.from(kp.secretKey)));
  return kp;
}

export interface LocalnetState {
  rpcUrl: string;
  createdAt: number;
  mint: string;
  feed: string;
  market: string;
  vault: string;
  vaultToken: string;
  lpPool: string;
  registry: string;
  orderbook: string;
  mmVenue: string;
  bookVenue: string;
  admin: string;
  oracleAuthority: string;
  keeper: string;
  matcher: string;
  startPrice: number;
  /** name → pubkey of every bot wallet (keys in target/localnet/keys/<name>.json) */
  wallets: Record<string, string>;
  marketParams: Record<string, unknown>;
  marketConfig: Record<string, unknown>;
  poolParams: Record<string, unknown>;
}

export function loadState(): LocalnetState {
  if (!fs.existsSync(STATE_FILE)) throw new Error(`${STATE_FILE} is missing: run scripts/localnet/up.sh first`);
  return JSON.parse(fs.readFileSync(STATE_FILE, "utf-8"));
}

export function saveState(s: LocalnetState): void {
  fs.mkdirSync(LOCALNET_DIR, { recursive: true });
  fs.writeFileSync(STATE_FILE, JSON.stringify(s, null, 2));
}

export function stopRequested(): boolean {
  return fs.existsSync(STOP_FILE);
}

// ---------------------------------------------------------------------------
// Programs (IDLs from scripts/idlgen in target/idl)
// ---------------------------------------------------------------------------

export interface Programs {
  provider: anchor.AnchorProvider;
  perps: anchor.Program;
  mm: anchor.Program;
  book: anchor.Program;
}

function idl(name: string): anchor.Idl {
  const file = path.join(ROOT, "target/idl", `${name}.json`);
  if (!fs.existsSync(file)) throw new Error(`${file} is missing: run scripts/localnet/build.sh`);
  return JSON.parse(fs.readFileSync(file, "utf-8"));
}

export function programs(conn: Connection, payer: Keypair): Programs {
  const provider = new anchor.AnchorProvider(conn, new anchor.Wallet(payer), { commitment: "confirmed" });
  return {
    provider,
    perps: new anchor.Program(idl("perps_core"), PERPS_ID, provider),
    mm: new anchor.Program(idl("mm_registry"), MM_REGISTRY_ID, provider),
    book: new anchor.Program(idl("orderbook"), ORDERBOOK_ID, provider),
  };
}

// ---------------------------------------------------------------------------
// PDAs
// ---------------------------------------------------------------------------

const pda = (seeds: (Buffer | Uint8Array)[], program: PublicKey) => PublicKey.findProgramAddressSync(seeds, program)[0];
export const u64le = (v: number | bigint | BN) => {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(BigInt(v.toString()));
  return b;
};

export const pdas = {
  config: () => pda([Buffer.from("config")], PERPS_ID),
  market: (mint: PublicKey, commodity = "OIL") => pda([Buffer.from("market"), mint.toBuffer(), Buffer.from(commodity)], PERPS_ID),
  vault: (market: PublicKey) => pda([Buffer.from("vault"), market.toBuffer()], PERPS_ID),
  vaultToken: (market: PublicKey) => pda([Buffer.from("vault_token"), market.toBuffer()], PERPS_ID),
  lpPool: (market: PublicKey) => pda([Buffer.from("lp_pool"), market.toBuffer()], PERPS_ID),
  lpPosition: (lpPool: PublicKey, owner: PublicKey) => pda([Buffer.from("lp_position"), lpPool.toBuffer(), owner.toBuffer(), Buffer.from([0])], PERPS_ID),
  user: (owner: PublicKey, market: PublicKey) => pda([Buffer.from("user"), owner.toBuffer(), market.toBuffer()], PERPS_ID),
  position: (owner: PublicKey, market: PublicKey, nonce: number | bigint | BN) =>
    pda([Buffer.from("position"), owner.toBuffer(), market.toBuffer(), u64le(nonce)], PERPS_ID),
  venue: (market: PublicKey, program: PublicKey) => pda([Buffer.from("venue"), market.toBuffer(), program.toBuffer()], PERPS_ID),
  venueAuthority: (program: PublicKey, market: PublicKey) => pda([Buffer.from("perps_venue"), market.toBuffer()], program),
  approval: (owner: PublicKey, venue: PublicKey, role: number) =>
    pda([Buffer.from("venue_approval"), owner.toBuffer(), venue.toBuffer(), Buffer.from([role])], PERPS_ID),
  vposition: (taker: PublicKey, market: PublicKey, nonce: number | bigint | BN) =>
    pda([Buffer.from("vposition"), taker.toBuffer(), market.toBuffer(), u64le(nonce)], PERPS_ID),
  vmaker: (takerPosition: PublicKey) => pda([Buffer.from("vmaker"), takerPosition.toBuffer()], PERPS_ID),
  programData: (program: PublicKey) => pda([program.toBuffer()], BPF_LOADER_UPGRADEABLE),
  registry: (market: PublicKey) => pda([Buffer.from("mm_registry"), market.toBuffer()], MM_REGISTRY_ID),
  marketMaker: (registry: PublicKey, owner: PublicKey) => pda([Buffer.from("market_maker"), registry.toBuffer(), owner.toBuffer()], MM_REGISTRY_ID),
  mmCollateral: (marketMaker: PublicKey) => pda([Buffer.from("mm_collateral"), marketMaker.toBuffer()], MM_REGISTRY_ID),
  quote: (marketMaker: PublicKey, index: number | bigint | BN) => pda([Buffer.from("quote"), marketMaker.toBuffer(), u64le(index)], MM_REGISTRY_ID),
  orderbook: (market: PublicKey) => pda([Buffer.from("orderbook"), market.toBuffer()], ORDERBOOK_ID),
  order: (orderbook: PublicKey, owner: PublicKey, seq: number | bigint | BN) =>
    pda([Buffer.from("order"), orderbook.toBuffer(), owner.toBuffer(), u64le(seq)], ORDERBOOK_ID),
};

export const ROLE_TAKER = 0;
export const ROLE_MAKER = 1;
export const SIDE_LONG_BIT = 1;
export const SIDE_SHORT_BIT = 2;

// ---------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------

/** Clock::unix_timestamp, which every session decision on-chain uses. */
export async function chainTime(conn: Connection): Promise<number> {
  const info = await conn.getAccountInfo(SYSVAR_CLOCK_PUBKEY);
  if (!info) throw new Error("no clock sysvar");
  return Number(info.data.readBigInt64LE(32));
}

export const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
export const wallNow = () => Date.now() / 1000;

// ---------------------------------------------------------------------------
// mock-pyth (TEST ONLY)
// ---------------------------------------------------------------------------

export const PRICE_ACCOUNT_SIZE = 3_312;
export const PRICE_EXPO = -8;
export const STATUS_TRADING = 1;

export function mockPythInitializeIx(feed: PublicKey, authority: PublicKey, price: bigint, conf: bigint): TransactionInstruction {
  const d = Buffer.alloc(1 + 8 + 8 + 4);
  d.writeUInt8(0, 0);
  d.writeBigInt64LE(price, 1);
  d.writeBigUInt64LE(conf, 9);
  d.writeInt32LE(PRICE_EXPO, 17);
  return new TransactionInstruction({
    programId: MOCK_PYTH_ID,
    keys: [
      { pubkey: feed, isSigner: true, isWritable: true },
      { pubkey: authority, isSigner: true, isWritable: false },
    ],
    data: d,
  });
}

export function mockPythSetPriceIx(feed: PublicKey, authority: PublicKey, price: bigint, conf: bigint, status = STATUS_TRADING): TransactionInstruction {
  const d = Buffer.alloc(1 + 8 + 8 + 4 + 4);
  d.writeUInt8(1, 0);
  d.writeBigInt64LE(price, 1);
  d.writeBigUInt64LE(conf, 9);
  d.writeInt32LE(PRICE_EXPO, 17);
  d.writeUInt32LE(status, 21);
  return new TransactionInstruction({
    programId: MOCK_PYTH_ID,
    keys: [
      { pubkey: feed, isSigner: false, isWritable: true },
      { pubkey: authority, isSigner: true, isWritable: false },
    ],
    data: d,
  });
}

export interface FeedView {
  /** 6-dp price, as perps-core converts it (expo −8 → 6 dp, truncated). */
  price6: bigint;
  conf6: bigint;
  publishTime: number;
  trading: boolean;
}

/** The feed as read_price sees it: the aggregate while Trading, else prev_*. */
export function decodeFeed(data: Buffer): FeedView {
  const trading = data.readUInt32LE(224) === STATUS_TRADING;
  const price = trading ? data.readBigInt64LE(208) : data.readBigInt64LE(184);
  const conf = trading ? data.readBigUInt64LE(216) : data.readBigUInt64LE(192);
  const publishTime = Number(trading ? data.readBigInt64LE(96) : data.readBigInt64LE(200));
  return { price6: price / 100n, conf6: conf / 100n, publishTime, trading };
}

// ---------------------------------------------------------------------------
// Logs
// ---------------------------------------------------------------------------

export function appendJsonl(file: string, row: unknown): void {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.appendFileSync(file, JSON.stringify(row, (_k, v) => (typeof v === "bigint" ? v.toString() : v)) + "\n");
}

export function readJsonl<T = any>(file: string): T[] {
  if (!fs.existsSync(file)) return [];
  return fs
    .readFileSync(file, "utf-8")
    .split("\n")
    .filter((l) => l.trim().length > 0)
    .map((l) => {
      try {
        return JSON.parse(l) as T;
      } catch {
        return null as unknown as T;
      }
    })
    .filter((x) => x !== null);
}

export const TX_LOG = path.join(LOG_DIR, "tx.jsonl");
export const PRICE_LOG = path.join(LOG_DIR, "price.jsonl");
export const CHECK_LOG = path.join(LOG_DIR, "checks.jsonl");

export function log(component: string, msg: string): void {
  const t = new Date().toISOString().slice(11, 19);
  console.log(`${t} [${component}] ${msg}`);
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

const PROGRAM_NAMES: Record<string, string> = {
  [PERPS_ID.toBase58()]: "perps_core",
  [MM_REGISTRY_ID.toBase58()]: "mm_registry",
  [ORDERBOOK_ID.toBase58()]: "orderbook",
  [MOCK_PYTH_ID.toBase58()]: "mock_pyth",
  "11111111111111111111111111111111": "system",
  TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA: "spl_token",
};

// Custom error codes of the system and token programs that can show up
const RUNTIME_ERRORS: Record<string, Record<number, string>> = {
  system: { 0: "AccountAlreadyInUse", 1: "ResultWithNegativeLamports" },
  spl_token: { 1: "InsufficientFunds", 4: "OwnerMismatch" },
};

let ERROR_NAMES: Record<string, Record<number, string>> | null = null;
function errorNames(): Record<string, Record<number, string>> {
  if (ERROR_NAMES) return ERROR_NAMES;
  ERROR_NAMES = {};
  for (const name of ["perps_core", "mm_registry", "orderbook"]) {
    const map: Record<number, string> = {};
    for (const e of (idl(name) as any).errors ?? []) map[e.code] = e.name;
    ERROR_NAMES[name] = map;
  }
  return ERROR_NAMES;
}

// Anchor framework errors (anchor-lang 0.28 ErrorCode) that a client race can produce
const ANCHOR_ERRORS: Record<number, string> = {
  2000: "ConstraintMut",
  2001: "ConstraintHasOne",
  2002: "ConstraintSigner",
  2003: "ConstraintRaw",
  2006: "ConstraintSeeds",
  2012: "ConstraintAddress",
  3000: "AccountDiscriminatorAlreadySet",
  3001: "AccountDiscriminatorNotFound",
  3002: "AccountDiscriminatorMismatch",
  3007: "AccountOwnedByWrongProgram",
  3012: "AccountNotInitialized",
};

/** An error's text: the message of an Error, else the whole value (a thrown plain object keeps its fields). */
export function errorText(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (typeof err === "string") return err;
  return util.inspect(err, { depth: 4, breakLength: Infinity });
}

/**
 * The error a failed transaction reports, as `program:Name` (e.g.
 * `perps_core:MarketClosed`), from the logs: the Anchor error line when there
 * is one, else the innermost failing program's custom code, else the
 * runtime's message.
 */
export function classifyError(err: unknown, logs: string[] | null | undefined): string {
  const lines = logs ?? [];
  let failing: string | null = null;
  for (const l of lines) {
    const m = /^Program (\w+) failed: (.*)$/.exec(l);
    if (m) {
      failing = PROGRAM_NAMES[m[1]] ?? m[1].slice(0, 8);
      const custom = /custom program error: 0x([0-9a-f]+)/.exec(m[2]);
      if (custom) {
        const code = parseInt(custom[1], 16);
        const name = errorNames()[failing]?.[code] ?? RUNTIME_ERRORS[failing]?.[code] ?? ANCHOR_ERRORS[code] ?? `Custom${code}`;
        return `${failing}:${name}`;
      }
      return `${failing}:${m[2].replace(/\s+/g, "_").slice(0, 60)}`;
    }
  }
  for (const l of lines) {
    const m = /Error Code: (\w+)/.exec(l);
    if (m) return `anchor:${m[1]}`;
  }
  const text = errorText(err);
  if (/Blockhash not found|block height exceeded|expired/i.test(text)) return "infra:BlockhashExpired";
  if (/timeout|timed out|Timed out/i.test(text)) return "infra:Timeout";
  if (/ECONNREFUSED|fetch failed|socket hang up/i.test(text)) return "infra:RpcUnavailable";
  if (/insufficient funds|insufficient lamports/i.test(text)) return "runtime:InsufficientFunds";
  if (/AccountInUse|account in use/i.test(text)) return "infra:AccountInUse";
  if (/already been processed/i.test(text)) return "infra:AlreadyProcessed";
  const m = /Error processing Instruction \d+: (.*)/.exec(text);
  if (m) return `runtime:${m[1].replace(/\s+/g, "_").slice(0, 60)}`;
  return `other:${text.replace(/\s+/g, "_").slice(0, 60)}`;
}

/**
 * Refusals a live run expects: policy and race outcomes of the program's own
 * guards (session, caps, quiet times, slippage, approvals, quotes taken first).
 * Anything else a bot or the setup sees is an unexpected failure.
 */
export const EXPECTED_REFUSALS = new Set<string>([
  ...[
    "MarketPaused", "ExcessiveLeverage", "InsufficientCollateral", "PositionTooSmall", "PositionTooLarge",
    "OpenInterestCapExceeded", "BelowMaintenanceMargin", "NotLiquidatable", "StaleOraclePrice", "PositionAlreadyClosed",
    "MarketClosed", "MarketSettled", "OracleConfidenceTooWide", "FillPriceOutOfBand", "FillTooLarge", "SelfTrade",
    "ApprovalExpired", "ApprovalAllowanceExceeded", "ApprovalLeverageExceeded", "ApprovalLimitPrice",
    "ApprovalPriceDeviation", "ApprovalSideNotAllowed", "PoolExposureLimit", "PoolWeekendExposureLimit",
    "AccountExposureLimit", "PoolAdlPending", "PoolCashInsufficient", "PoolWithdrawalsBlocked", "PoolCapWouldBreach",
    "PoolCashCapExceeded", "SlippageExceeded", "InsufficientShares", "WithdrawalPending", "NoWithdrawalPending",
    "WithdrawalDelayNotElapsed", "NothingToSweep", "NothingToSettle", "SettleWouldBankrupt", "AdlNotRequired",
    "AdlWrongSide", "AdlCandidateNotProfitable", "AdlScoreBelowFloor", "AdlOvershoot", "BadDebtNotStale",
    "FundingNotDue", "ParamsTimelocked", "PoolDrawdownFreeze", "LpFlowWindowClosed", "FundingStale",
    "WithdrawalExpired", "ProfitCloseQuiet", "InvalidPositionReduction",
  ].map((n) => `perps_core:${n}`),
  ...[
    "QuoteExpired", "QuoteNotActive", "FillSizeExceedsRemaining", "PriceWorseThanLimit", "QuoteNotFillable",
    "FillSizeTooSmall", "MaxQuotesReached", "InsufficientCollateral",
  ].map((n) => `mm_registry:${n}`),
  ...["InvalidOrder", "OrderExpired", "FillTooSmall", "RentDepositExhausted", "OrderNotExpired"].map((n) => `orderbook:${n}`),
  // Two bots raced for the same nonce or account (client side, not a program fault)
  "perps_core:ConstraintSeeds",
  "orderbook:ConstraintSeeds",
  "perps_core:AccountDiscriminatorAlreadySet",
  "infra:BlockhashExpired",
  "infra:AccountInUse",
  // a bot resent an identical transaction within one blockhash
  "infra:AlreadyProcessed",
]);

/** Errors that point at a program fault even when a bot provoked them on purpose. */
export const ALWAYS_VIOLATIONS = new Set<string>(
  ["VaultInvariantViolated", "MathOverflow", "BucketUnderflow", "VenueMarginInvariant", "PoolInsolvent"].map((n) => `perps_core:${n}`)
);

// ---------------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------------

export interface TxResult {
  ok: boolean;
  sig?: string;
  code?: string;
  cu?: number;
  logs?: string[];
  events?: { name: string; data: any }[];
}

let EVENT_CODER: anchor.BorshCoder | null = null;
/** perps-core events (`emit!`, "Program data:" lines) in `logs`. */
export function decodeEvents(logs: string[] | null | undefined): { name: string; data: any }[] {
  if (!EVENT_CODER) EVENT_CODER = new anchor.BorshCoder(idl("perps_core"));
  const out: { name: string; data: any }[] = [];
  for (const l of logs ?? []) {
    if (!l.startsWith("Program data: ")) continue;
    try {
      const e = EVENT_CODER.events.decode(l.slice("Program data: ".length));
      if (e) out.push({ name: e.name, data: e.data });
    } catch {
      // another program's data
    }
  }
  return out;
}

/** The fields of the events the report follows (liquidations, bad debt, ADL), as plain values. */
export function eventSummary(e: { name: string; data: any }): Record<string, unknown> | null {
  if (!["PositionLiquidated", "BadDebt", "Adl", "BadDebtWrittenOff", "PositionPnlSettled"].includes(e.name)) return null;
  const out: Record<string, unknown> = { name: e.name };
  for (const [k, v] of Object.entries(e.data ?? {})) {
    out[k] = v instanceof PublicKey ? v.toBase58() : v && typeof (v as any).toString === "function" && typeof v === "object" ? (v as any).toString() : v;
  }
  return out;
}

export interface SendOpts {
  /** Compute unit limit to request (default 400_000). */
  cu?: number;
  /** Refusal codes this caller provokes on purpose (counted as expected). */
  expect?: string[];
  /** Skip the post-confirmation getTransaction (CU) call. */
  noFetch?: boolean;
}

let blockhashCache: { hash: string; at: number } | null = null;
async function recentBlockhash(conn: Connection): Promise<string> {
  const now = Date.now();
  if (blockhashCache && now - blockhashCache.at < 10_000) return blockhashCache.hash;
  const { blockhash } = await conn.getLatestBlockhash("confirmed");
  blockhashCache = { hash: blockhash, at: now };
  return blockhash;
}

function unitsFromLogs(logs: string[] | undefined): number | undefined {
  let total = 0;
  let seen = false;
  for (const l of logs ?? []) {
    // Top-level invocations only: "Program X consumed N of M compute units" at depth 1
    const m = /^Program (\w+) consumed (\d+) of \d+ compute units$/.exec(l);
    if (m && m[1] !== "ComputeBudget111111111111111111111111111111") {
      total += Number(m[2]);
      seen = true;
    }
  }
  return seen ? total : undefined;
}

/**
 * Send `ixs` signed by `signers` (the first pays), wait for confirmation and
 * record a row in tx.jsonl: actor, kind, ok, error code, CU. Never throws.
 */
export async function send(
  conn: Connection,
  actor: string,
  kind: string,
  ixs: TransactionInstruction[],
  signers: Keypair[],
  opts: SendOpts = {}
): Promise<TxResult> {
  const started = Date.now();
  let res: TxResult;
  // Outside the try: a throw after sendRawTransaction returned still has it
  let sig: string | undefined;
  let errDetail: string | undefined;
  try {
    const tx = new Transaction();
    tx.add(ComputeBudgetProgram.setComputeUnitLimit({ units: opts.cu ?? 400_000 }));
    tx.add(...ixs);
    tx.feePayer = signers[0].publicKey;
    tx.recentBlockhash = await recentBlockhash(conn);
    tx.sign(...signers);
    sig = await conn.sendRawTransaction(tx.serialize(), { skipPreflight: false, preflightCommitment: "confirmed", maxRetries: 3 });
    const conf = await conn.confirmTransaction(sig, "confirmed");
    if (conf.value.err) {
      const t = await conn.getTransaction(sig, { commitment: "confirmed", maxSupportedTransactionVersion: 0 });
      res = { ok: false, sig, logs: t?.meta?.logMessages ?? [], code: classifyError(conf.value.err, t?.meta?.logMessages) };
    } else {
      res = { ok: true, sig };
      if (!opts.noFetch) {
        const t = await conn.getTransaction(sig, { commitment: "confirmed", maxSupportedTransactionVersion: 0 });
        res.cu = t?.meta?.computeUnitsConsumed ?? unitsFromLogs(t?.meta?.logMessages ?? undefined);
        res.logs = t?.meta?.logMessages ?? [];
        res.events = decodeEvents(res.logs);
      }
    }
  } catch (err) {
    const logs: string[] = err instanceof SendTransactionError ? err.logs ?? [] : ((err as any)?.logs ?? []);
    errDetail = errorText(err).slice(0, 500);
    if (/Blockhash not found/i.test(errDetail)) blockhashCache = null;
    res = { ok: false, sig, logs, code: classifyError(err, logs), cu: unitsFromLogs(logs) };
    if (sig) {
      // Sent, then confirming threw: whether it landed is on chain
      const t = await conn.getTransaction(sig, { commitment: "confirmed", maxSupportedTransactionVersion: 0 }).catch(() => null);
      if (t?.meta) {
        const l = t.meta.logMessages ?? [];
        res = t.meta.err
          ? { ok: false, sig, logs: l, code: classifyError(t.meta.err, l) }
          : { ok: true, sig, logs: l, cu: t.meta.computeUnitsConsumed ?? unitsFromLogs(l), events: decodeEvents(l) };
      } else if (res.code?.startsWith("other:")) {
        res.code = "infra:Unconfirmed";
      }
    } else if (logs.length === 0 && res.code?.startsWith("other:")) {
      // No signature and no simulation logs: the transaction never reached
      // the program (a transport-level throw)
      res.code = "infra:SendFailed";
    }
  }
  const expected = !res.ok && !!res.code && (EXPECTED_REFUSALS.has(res.code) || (opts.expect ?? []).includes(res.code)) && !ALWAYS_VIOLATIONS.has(res.code);
  appendJsonl(TX_LOG, {
    t: Math.floor(started / 1000),
    ms: Date.now() - started,
    actor,
    kind,
    ok: res.ok,
    code: res.code ?? null,
    expected: res.ok ? null : expected,
    cu: res.cu ?? null,
    sig: res.sig ?? sig ?? null,
    // what was thrown, for unexpected failures (the logs may not explain them)
    err: !res.ok && !expected && errDetail !== undefined ? errDetail : undefined,
    events: res.events && res.events.length > 0 ? res.events.map((e) => e.name) : undefined,
    detail: res.events ? res.events.map(eventSummary).filter((x) => x !== null) : undefined,
    // keep the failing logs of unexpected failures for the report
    logs: !res.ok && !expected ? (res.logs ?? []).slice(-12) : undefined,
  });
  return res;
}

/** `send`, throwing on failure (setup). */
export async function mustSend(conn: Connection, actor: string, kind: string, ixs: TransactionInstruction[], signers: Keypair[], opts: SendOpts = {}): Promise<TxResult> {
  const r = await send(conn, actor, kind, ixs, signers, opts);
  if (!r.ok) throw new Error(`${kind} failed: ${r.code}\n${(r.logs ?? []).join("\n")}`);
  return r;
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

export const big = (x: { toString(): string } | number | bigint | null | undefined): bigint =>
  x === null || x === undefined ? 0n : typeof x === "bigint" ? x : BigInt(typeof x === "number" ? Math.trunc(x) : x.toString());

export const usd = (x: bigint | number) => Number(x) / 1e6;

export class Rng {
  private s: number;
  constructor(seed: number) {
    this.s = seed >>> 0 || 1;
  }
  next(): number {
    // mulberry32
    let t = (this.s += 0x6d2b79f5);
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  }
  range(a: number, b: number): number {
    return a + (b - a) * this.next();
  }
  int(a: number, b: number): number {
    return Math.floor(this.range(a, b + 1));
  }
  pick<T>(xs: T[]): T {
    return xs[Math.floor(this.next() * xs.length)];
  }
  chance(p: number): boolean {
    return this.next() < p;
  }
  normal(): number {
    const u = Math.max(this.next(), 1e-12);
    const v = this.next();
    return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v);
  }
}

/** A promise-chain mutex. */
export class Mutex {
  private tail: Promise<void> = Promise.resolve();
  async run<T>(f: () => Promise<T>): Promise<T> {
    const prev = this.tail;
    let release!: () => void;
    this.tail = new Promise<void>((r) => (release = r));
    await prev;
    try {
      return await f();
    } finally {
      release();
    }
  }
}

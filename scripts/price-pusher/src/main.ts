// Price pusher for the commodity markets' Pyth feeds (docs/lp-pool.md §18
// #19, #21; D54). Keeps each market's pinned account, the push oracle's
// shard-0 PriceUpdateV2 PDA for its feed id, fresh from Hermes, and creates
// it with the first push (before initialize_market can run).
//
//   PYTH_PROGRAMS=pro PYTH_API_KEY=... RPC_URLS=https://a,https://b \
//     npx tsx src/main.ts [--once] [--dry-run] [COMMODITY ...]
//
// Environment:
//   PYTH_PROGRAMS   pro | legacy (required): the program pair the markets pin
//   PYTH_API_KEY    Pyth API key (required): Hermes refuses requests without one
//   HERMES_URL      default https://hermes.pyth.network
//   RPC_URLS        comma-separated; the first reads and confirms, every one
//                   gets each transaction (default https://api.mainnet-beta.solana.com)
//   PUSHER_KEYPAIR  fee payer keypair file (default ANCHOR_WALLET, then
//                   ~/.config/solana/id.json); not needed with --dry-run
//   DEVIATION_BPS   push when Hermes moved this far (default 10, at most 10)
//   HEARTBEAT_SECS  push when the stored update is this old (default 10, at most 10)
//   LAG_ALERT_BPS   log a "lag" alert past this (default 10): pause the market
//   POLL_MS         loop period (default 1000)
//   PRIORITY_FEE    compute unit price in micro-lamports (default 50000)
//   MIN_BALANCE_SOL warn below this fee-payer balance (default 0.5)
//
// Commodities default to every one with a feed id in
// frontend/src/config/pythFeeds.ts. Output is one JSON object per line.
// The pusher only ever writes the push oracle's own accounts through Pyth's
// SDK: it holds no perps-core role and cannot move a market.
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { Wallet } from '@coral-xyz/anchor';
import { HermesClient } from '@pythnetwork/hermes-client';
import { PythSolanaReceiver } from '@pythnetwork/pyth-solana-receiver';
import { Connection, Keypair, PublicKey, VersionedTransaction } from '@solana/web3.js';
import {
  PYTH_FEED_IDS,
  PYTH_PROGRAM_SETS,
  PYTH_PUSH_SHARD_ID,
  feedIdFromHex,
  pushFeedSeeds,
  type PythProgramSet,
} from '../../../frontend/src/config/pythFeeds';
import { parsePriceUpdateV2, sameFeedId } from '../../../frontend/src/utils/oracleAccount';
import { DEFAULT_POLICY, checkPolicy, decide, lagging, type Decision, type PusherPolicy, type Quote } from './policy';

interface Feed {
  commodity: string;
  /** lowercase hex, no prefix */
  id: string;
  account: PublicKey;
}

interface Config {
  set: PythProgramSet;
  receiver: PublicKey;
  pushOracle: PublicKey;
  wormhole: PublicKey;
  hermesUrl: string;
  apiKey: string;
  rpcUrls: string[];
  policy: PusherPolicy;
  pollMs: number;
  priorityFee: number;
  minBalanceSol: number;
  once: boolean;
  dryRun: boolean;
  commodities: string[];
}

function log(event: string, fields: Record<string, unknown> = {}): void {
  const out = { t: new Date().toISOString(), event, ...fields };
  console.log(JSON.stringify(out, (_k, v) => (typeof v === 'bigint' ? v.toString() : v)));
}

function num(env: NodeJS.ProcessEnv, name: string, fallback: number): number {
  const raw = env[name];
  if (raw === undefined || raw === '') return fallback;
  const v = Number(raw);
  if (!Number.isFinite(v)) throw new Error(`${name} is not a number: ${raw}`);
  return v;
}

export function loadConfig(argv: string[], env: NodeJS.ProcessEnv = process.env): Config {
  const set = env.PYTH_PROGRAMS;
  if (set !== 'pro' && set !== 'legacy') {
    throw new Error('Set PYTH_PROGRAMS to "pro" or "legacy": the program pair the markets pin (docs/lp-pool.md §18 #18)');
  }
  const dryRun = argv.includes('--dry-run');
  const apiKey = env.PYTH_API_KEY ?? '';
  if (!apiKey) throw new Error('Set PYTH_API_KEY: Hermes requires a Pyth API key since 2026-08-26 (docs/lp-pool.md D53)');
  const named = argv.filter((a) => !a.startsWith('--'));
  const known = Object.keys(PYTH_FEED_IDS);
  for (const c of named) {
    if (!known.includes(c)) throw new Error(`Unknown commodity ${c} (known: ${known.join(', ')})`);
    if (!PYTH_FEED_IDS[c]) throw new Error(`${c} has no verified Pyth Core feed id (docs/lp-pool.md §18 #17)`);
  }
  const commodities = named.length ? named : known.filter((c) => PYTH_FEED_IDS[c]);
  const policy: PusherPolicy = {
    deviationBps: num(env, 'DEVIATION_BPS', DEFAULT_POLICY.deviationBps),
    heartbeatSecs: num(env, 'HEARTBEAT_SECS', DEFAULT_POLICY.heartbeatSecs),
    lagAlertBps: num(env, 'LAG_ALERT_BPS', DEFAULT_POLICY.lagAlertBps),
  };
  checkPolicy(policy);
  const rpcUrls = (env.RPC_URLS ?? 'https://api.mainnet-beta.solana.com').split(',').map((s) => s.trim()).filter(Boolean);
  if (!rpcUrls.length) throw new Error('RPC_URLS is empty');
  const programs = PYTH_PROGRAM_SETS[set];
  return {
    set,
    receiver: new PublicKey(programs.receiver),
    pushOracle: new PublicKey(programs.pushOracle),
    wormhole: new PublicKey(programs.wormhole),
    hermesUrl: env.HERMES_URL ?? 'https://hermes.pyth.network',
    apiKey,
    rpcUrls,
    policy,
    pollMs: num(env, 'POLL_MS', 1_000),
    priorityFee: num(env, 'PRIORITY_FEE', 50_000),
    minBalanceSol: num(env, 'MIN_BALANCE_SOL', 0.5),
    once: argv.includes('--once'),
    dryRun,
    commodities,
  };
}

function loadKeypair(env: NodeJS.ProcessEnv = process.env): Keypair {
  const file = env.PUSHER_KEYPAIR || env.ANCHOR_WALLET || path.join(os.homedir(), '.config/solana/id.json');
  return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(file, 'utf8'))));
}

/** The pinned account for `id`: the same derivation as scripts/pyth-feeds.ts and perps-core's pyth_push.rs. */
function feedAccount(id: string, pushOracle: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync(pushFeedSeeds(feedIdFromHex(id), PYTH_PUSH_SHARD_ID), pushOracle)[0];
}

/** The account's update, or null if there is none; throws on an account perps-core would refuse. */
function storedQuote(feed: Feed, info: { owner: PublicKey; data: Buffer } | null, receiver: PublicKey): Quote | null {
  if (!info) return null;
  if (!info.owner.equals(receiver)) throw new Error(`${feed.account} is owned by ${info.owner}, not the Receiver ${receiver}`);
  const p = parsePriceUpdateV2(info.data);
  if (!p.ok) throw new Error(`${feed.account} is not a usable PriceUpdateV2 (${p.reason})`);
  if (!sameFeedId(p.update.feedId, feedIdFromHex(feed.id))) throw new Error(`${feed.account} carries another feed id`);
  return { price: p.update.price, exponent: p.update.exponent, publishTime: Number(p.update.publishTime) };
}

interface HermesParsed {
  id: string;
  price: { price: string; conf: string; expo: number; publish_time: number };
}

async function hermesQuotes(hermes: HermesClient, ids: string[]): Promise<Map<string, Quote>> {
  const update = await hermes.getLatestPriceUpdates(ids, { parsed: true });
  const out = new Map<string, Quote>();
  for (const p of (update.parsed ?? []) as HermesParsed[]) {
    out.set(p.id.replace(/^0x/, '').toLowerCase(), {
      price: BigInt(p.price.price),
      exponent: p.price.expo,
      publishTime: p.price.publish_time,
    });
  }
  return out;
}

/**
 * Send `txs` in order (the SDK's VAA posting must land before the feed
 * update): each to every RPC, rebroadcast until the first RPC confirms it.
 */
async function sendInOrder(txs: VersionedTransaction[], connections: Connection[], timeoutMs = 45_000): Promise<string[]> {
  const sigs: string[] = [];
  for (const tx of txs) {
    const raw = tx.serialize();
    const deadline = Date.now() + timeoutMs;
    let sig: string | undefined;
    let confirmed = false;
    while (!confirmed && Date.now() < deadline) {
      const sent = await Promise.allSettled(
        connections.map((c, i) => c.sendRawTransaction(raw, { skipPreflight: i > 0 || sig !== undefined, maxRetries: 0 }))
      );
      for (const r of sent) {
        if (r.status === 'fulfilled') sig = r.value;
      }
      // A preflight failure on the first RPC is final (for instance the
      // push oracle refusing an update that is not newer)
      if (!sig && sent[0].status === 'rejected') throw sent[0].reason;
      if (!sig) continue;
      for (let i = 0; i < 4 && !confirmed; i++) {
        await new Promise((r) => setTimeout(r, 500));
        const st = (await connections[0].getSignatureStatuses([sig])).value[0];
        if (st?.err) throw new Error(`transaction ${sig} failed: ${JSON.stringify(st.err)}`);
        confirmed = st?.confirmationStatus === 'confirmed' || st?.confirmationStatus === 'finalized';
      }
    }
    if (!confirmed) throw new Error(`transaction ${sig ?? '(unsent)'} not confirmed in ${timeoutMs} ms`);
    sigs.push(sig!);
  }
  return sigs;
}

export async function run(cfg: Config): Promise<void> {
  const connections = cfg.rpcUrls.map((u) => new Connection(u, 'confirmed'));
  const primary = connections[0];
  const payer = cfg.dryRun ? Keypair.generate() : loadKeypair();
  const receiverSdk = new PythSolanaReceiver({
    connection: primary,
    wallet: new Wallet(payer),
    receiverProgramId: cfg.receiver,
    pushOracleProgramId: cfg.pushOracle,
    wormholeProgramId: cfg.wormhole,
  });
  const hermes = new HermesClient(cfg.hermesUrl, { accessToken: cfg.apiKey });
  const feeds: Feed[] = cfg.commodities.map((commodity) => {
    const id = PYTH_FEED_IDS[commodity]!;
    const account = feedAccount(id, cfg.pushOracle);
    // Our derivation and the SDK's must name the same account, or the pusher
    // would keep a feed fresh that no market pins
    const sdk = receiverSdk.getPriceFeedAccountAddress(PYTH_PUSH_SHARD_ID, id);
    if (!sdk.equals(account)) throw new Error(`${commodity}: SDK feed account ${sdk} differs from ${account}`);
    return { commodity, id, account };
  });
  log('start', {
    programs: cfg.set,
    receiver: cfg.receiver.toBase58(),
    pushOracle: cfg.pushOracle.toBase58(),
    rpcs: cfg.rpcUrls.length,
    payer: cfg.dryRun ? null : payer.publicKey.toBase58(),
    policy: cfg.policy,
    feeds: feeds.map((f) => ({ commodity: f.commodity, id: f.id, account: f.account.toBase58() })),
    dryRun: cfg.dryRun,
  });

  let lastBalanceCheck = 0;
  for (;;) {
    const started = Date.now();
    try {
      if (!cfg.dryRun && started - lastBalanceCheck > 60_000) {
        lastBalanceCheck = started;
        const sol = (await primary.getBalance(payer.publicKey)) / 1e9;
        if (sol < cfg.minBalanceSol) log('lowBalance', { sol, min: cfg.minBalanceSol });
      }
      await tick(cfg, feeds, hermes, receiverSdk, connections);
    } catch (e) {
      log('error', { message: e instanceof Error ? e.message : String(e) });
    }
    if (cfg.once) return;
    await new Promise((r) => setTimeout(r, Math.max(0, cfg.pollMs - (Date.now() - started))));
  }
}

async function tick(
  cfg: Config,
  feeds: Feed[],
  hermes: HermesClient,
  receiverSdk: PythSolanaReceiver,
  connections: Connection[]
): Promise<void> {
  const [quotes, infos] = await Promise.all([
    hermesQuotes(hermes, feeds.map((f) => f.id)),
    connections[0].getMultipleAccountsInfo(feeds.map((f) => f.account)),
  ]);
  const now = Math.floor(Date.now() / 1000);
  const due: Feed[] = [];
  feeds.forEach((feed, i) => {
    const hq = quotes.get(feed.id);
    if (!hq) {
      log('noHermesPrice', { commodity: feed.commodity, id: feed.id });
      return;
    }
    let stored: Quote | null;
    try {
      stored = storedQuote(feed, infos[i], cfg.receiver);
    } catch (e) {
      // Never push over an account the markets would refuse: report it
      log('badAccount', { commodity: feed.commodity, account: feed.account.toBase58(), message: (e as Error).message });
      return;
    }
    const d: Decision = decide(stored, hq, now, cfg.policy);
    if (lagging(stored, hq, cfg.policy)) {
      log('lag', {
        commodity: feed.commodity,
        hermesPrice: hq.price,
        storedPrice: stored?.price ?? null,
        exponent: hq.exponent,
        hermesPublishTime: hq.publishTime,
        storedPublishTime: stored?.publishTime ?? null,
      });
    }
    if (d.push) {
      due.push(feed);
      log('due', { commodity: feed.commodity, ...d });
    }
  });
  if (!due.length || cfg.dryRun) return;

  // The signed update for just the due feeds, fetched now so it is the newest
  const update = await hermes.getLatestPriceUpdates(due.map((f) => f.id), { encoding: 'base64' });
  const builder = receiverSdk.newTransactionBuilder({ closeUpdateAccounts: true });
  await builder.addUpdatePriceFeed(update.binary.data, PYTH_PUSH_SHARD_ID);
  const built = await builder.buildVersionedTransactions({ computeUnitPriceMicroLamports: cfg.priorityFee, tightComputeBudget: true });
  const payer = (receiverSdk.provider.wallet as Wallet).payer;
  const txs = built.map(({ tx, signers }) => {
    tx.sign([payer, ...(signers ?? [])]);
    return tx;
  });
  const sigs = await sendInOrder(txs, connections);
  log('pushed', { commodities: due.map((f) => f.commodity), signatures: sigs });
}

if (require.main === module) {
  let cfg: Config;
  try {
    cfg = loadConfig(process.argv.slice(2));
  } catch (e) {
    console.error((e as Error).message);
    process.exit(2);
  }
  run(cfg).catch((e) => {
    log('fatal', { message: e instanceof Error ? e.message : String(e) });
    process.exit(1);
  });
}

// Feed rotation for an existing market (docs/lp-pool.md D47, D51): perps-core's
// propose_oracle_feed / apply_oracle_feed / cancel_oracle_feed.
//
//   PYTH_PROGRAMS=pro npx ts-node oracle-feed.ts status  OIL
//   PYTH_PROGRAMS=pro npx ts-node oracle-feed.ts propose OIL [--feed-id <hex>] [--feed <pubkey>]   (admin)
//                     npx ts-node oracle-feed.ts apply   OIL                                     (anyone, after the timelock)
//                     npx ts-node oracle-feed.ts cancel  OIL                                     (admin)
//
// propose defaults to the commodity's table entry (frontend/src/config/pythFeeds.ts:
// its feed id and that id's shard-0 push-feed account under PYTH_PROGRAMS);
// --feed-id takes another id (its shard-0 account unless --feed names one).
// The new account must be a Full PriceUpdateV2 for that id owned by the
// protocol's current oracle program, as the program checks. While the
// market's pool has no LP shares the change applies at once; otherwise it is
// pending for the pool's params timelock (10 days at production params, so
// LPs can leave first), and apply_oracle_feed makes it effective. A new
// proposal replaces a pending one and restarts its clock. RPC_URL as in
// initialize-market.ts (default testnet).
import { Connection, PublicKey } from '@solana/web3.js';
import { USDC_MINT, chainTime, getConfigPDA, getLpPoolPDA, getMarketPDA, loadKeypair, loadPerpsProgram } from './common';
import { assertPythFeed, commodityFeed, feedIdFromHex, feedIdToHex, pushFeedAddress, pythPrograms } from './pyth-feeds';
import { parsePriceUpdateV2 } from '../frontend/src/utils/oracleAccount';

function flag(args: string[], name: string): string | undefined {
  const i = args.indexOf(name);
  if (i < 0) return undefined;
  const v = args[i + 1];
  if (!v || v.startsWith('--')) throw new Error(`${name} needs a value`);
  return v;
}


async function describeFeed(connection: Connection, feed: PublicKey, now: number): Promise<string> {
  const info = await connection.getAccountInfo(feed);
  if (!info) return 'does not exist';
  const p = parsePriceUpdateV2(info.data);
  if ('reason' in p) return `owner ${info.owner}, not usable (${p.reason})`;
  const u = p.update;
  const price = Number(u.price) * 10 ** u.exponent;
  return `owner ${info.owner}, feed id ${feedIdToHex(u.feedId)}, price ${price} published ${now - Number(u.publishTime)} s ago`;
}

async function main() {
  const args = process.argv.slice(2);
  const [command, commodity] = args.filter((a, i) => !a.startsWith('--') && !(i > 0 && args[i - 1].startsWith('--')));
  if (!['status', 'propose', 'apply', 'cancel'].includes(command ?? '') || !commodity) {
    console.error('usage: oracle-feed.ts status|propose|apply|cancel <COMMODITY> [--feed-id <hex>] [--feed <pubkey>]');
    process.exit(2);
  }
  const rpcUrl = process.env.RPC_URL || 'https://api.testnet.solana.com';
  const connection = new Connection(rpcUrl, 'confirmed');
  const signer = await loadKeypair();
  const program = loadPerpsProgram(connection, signer);
  const [config] = getConfigPDA();
  const [market] = getMarketPDA(USDC_MINT, commodity);
  const [lpPool] = getLpPoolPDA(market);
  console.log(`Network ${rpcUrl}, ${commodity} market ${market}, signer ${signer.publicKey}`);

  const m = await program.account.market.fetch(market);
  const cfg = await program.account.protocolConfig.fetch(config);
  const now = await chainTime(connection);
  const pendingAt = m.pendingOracleAt.toNumber();
  const pinnedId = feedIdToHex(m.pythFeedId);
  const pendingId = feedIdToHex(m.pendingFeedId);

  if (command === 'status') {
    console.log(`pinned feed   ${m.pythPriceFeed} (feed id ${pinnedId}, owner checked: ${m.oracleProgram})`);
    console.log(`  ${await describeFeed(connection, m.pythPriceFeed, now)}`);
    if (pendingAt === 0) {
      console.log('pending       none');
    } else {
      const left = pendingAt - now;
      console.log(`pending       ${m.pendingPriceFeed} (feed id ${pendingId}), applicable ${left > 0 ? `in ${left} s` : 'now'} (at ${pendingAt})`);
      console.log(`  ${await describeFeed(connection, m.pendingPriceFeed, now)}`);
    }
    console.log(`config oracle program ${cfg.oracleProgram}${cfg.oracleProgram.equals(m.oracleProgram) ? '' : ' (differs from the market snapshot: a rotation applies it)'}`);
    if (process.env.PYTH_PROGRAMS) {
      const want = commodityFeed(commodity, pythPrograms());
      const same = want.address.equals(m.pythPriceFeed) && want.feedIdHex === pinnedId;
      console.log(`table default ${want.address} (feed id ${want.feedIdHex})${same ? ': pinned' : ': NOT pinned'}`);
    }
    return;
  }

  if (command === 'propose') {
    const idHex = flag(args, '--feed-id');
    const feedArg = flag(args, '--feed');
    let feedId: number[];
    let feed: PublicKey;
    if (idHex) {
      feedId = Array.from(feedIdFromHex(idHex));
      feed = feedArg ? new PublicKey(feedArg) : pushFeedAddress(Uint8Array.from(feedId), pythPrograms().pushOracle);
    } else {
      const want = commodityFeed(commodity, pythPrograms());
      feedId = want.feedId;
      feed = feedArg ? new PublicKey(feedArg) : want.address;
    }
    if (feed.equals(m.pythPriceFeed) && feedIdToHex(feedId) === pinnedId && cfg.oracleProgram.equals(m.oracleProgram)) {
      console.log(`${feed} (feed id ${pinnedId}) is already pinned: nothing to propose`);
      return;
    }
    if (m.settledAt.toNumber() !== 0) throw new Error('The market is settled (MarketSettled): its feed cannot change');
    // As the program checks: owned by the config's oracle program, Full, this feed id
    await assertPythFeed(connection, feed, cfg.oracleProgram, feedId);
    const pool = await program.account.lpPool.fetch(lpPool);
    console.log(`Proposing ${feed} (feed id ${feedIdToHex(feedId)}); the pool has ${pool.lpShares.toString()} LP shares` +
      (pool.lpShares.isZero() ? ': it applies at once' : ': it is timelocked'));
    if (pendingAt !== 0) console.log(`(replaces the pending ${m.pendingPriceFeed}, restarting the timelock)`);
    const sig = await program.methods
      .proposeOracleFeed(feedId)
      .accounts({ admin: signer.publicKey, config, market, lpPool, newPriceFeed: feed })
      .signers([signer])
      .rpc();
    const after = await program.account.market.fetch(market);
    const at = after.pendingOracleAt.toNumber();
    console.log(`propose_oracle_feed: ${sig}`);
    console.log(at === 0
      ? `Applied: the market now pins ${after.pythPriceFeed}`
      : `Pending until ${at} (chain time; ${at - (await chainTime(connection))} s from now): then run "oracle-feed.ts apply ${commodity}"`);
    return;
  }

  if (command === 'apply') {
    if (pendingAt === 0) throw new Error('No feed change is pending (OracleChangeTimelocked)');
    if (now < pendingAt) throw new Error(`The change is timelocked for ${pendingAt - now} more seconds (OracleChangeTimelocked)`);
    // apply_oracle_feed re-validates against the config's oracle program as it is now
    await assertPythFeed(connection, m.pendingPriceFeed, cfg.oracleProgram, m.pendingFeedId);
    const sig = await program.methods
      .applyOracleFeed()
      .accounts({ market, config, newPriceFeed: m.pendingPriceFeed })
      .rpc();
    console.log(`apply_oracle_feed: ${sig}; the market now pins ${m.pendingPriceFeed} (feed id ${pendingId})`);
    return;
  }

  // cancel
  if (pendingAt === 0) throw new Error('No feed change is pending (NoPendingOracleChange)');
  const sig = await program.methods
    .cancelOracleFeed()
    .accounts({ admin: signer.publicKey, config, market })
    .signers([signer])
    .rpc();
  console.log(`cancel_oracle_feed: ${sig}; ${m.pendingPriceFeed} is no longer pending`);
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : error);
  process.exit(1);
});

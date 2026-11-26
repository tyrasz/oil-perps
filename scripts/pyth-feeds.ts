// Pyth on Solana for the deploy scripts (docs/lp-pool.md D47, D51): the
// program pair, each commodity's feed id and pinned feed account, and the
// check that an account is what perps-core will accept. The feed ids and
// program ids are the frontend's table (frontend/src/config/pythFeeds.ts),
// so the scripts and the app cannot disagree.
import { Connection, PublicKey } from '@solana/web3.js';
import {
  PYTH_FEED_IDS,
  PYTH_PROGRAM_SETS,
  PYTH_PUSH_SHARD_ID,
  feedIdFromHex,
  feedIdToHex,
  pushFeedSeeds,
  type PythProgramSet,
} from '../frontend/src/config/pythFeeds';
import { parsePriceUpdateV2, sameFeedId } from '../frontend/src/utils/oracleAccount';

export { PYTH_FEED_IDS, feedIdFromHex, feedIdToHex };

export interface PythPrograms {
  set: PythProgramSet;
  /** Pyth Solana Receiver: owns every PriceUpdateV2; ProtocolConfig.oracle_program */
  receiver: PublicKey;
  /** Pyth push oracle: the feed accounts are its PDAs */
  pushOracle: PublicKey;
}

/**
 * The Pyth program pair from PYTH_PROGRAMS ("pro" = pro-compatible
 * rec2HHDD…/pyt2F414…, "legacy" = rec5EKMG…/pythWSns…). Required, with no
 * default: the two pairs own different feed accounts, and which one a
 * cluster uses must be checked on chain (docs/lp-pool.md §18).
 */
export function pythPrograms(): PythPrograms {
  const set = process.env.PYTH_PROGRAMS;
  if (set !== 'pro' && set !== 'legacy') {
    throw new Error(
      'Set PYTH_PROGRAMS to "pro" (pro-compatible Receiver rec2HHDD… and push oracle pyt2F414…) or "legacy" ' +
        '(rec5EKMG… / pythWSns…), after checking on chain which pair this cluster uses (docs/lp-pool.md D51)'
    );
  }
  return {
    set,
    receiver: new PublicKey(PYTH_PROGRAM_SETS[set].receiver),
    pushOracle: new PublicKey(PYTH_PROGRAM_SETS[set].pushOracle),
  };
}

/** The push oracle's feed account for `feedId` on the sponsored shard: PDA([0u16 LE, feed_id], push oracle). */
export function pushFeedAddress(feedId: Uint8Array, pushOracle: PublicKey, shardId = PYTH_PUSH_SHARD_ID): PublicKey {
  return PublicKey.findProgramAddressSync(pushFeedSeeds(feedId, shardId), pushOracle)[0];
}

export interface CommodityFeed {
  feedIdHex: string;
  /** 32 bytes, as initialize_market / propose_oracle_feed take it */
  feedId: number[];
  /** The account the market pins */
  address: PublicKey;
}

/**
 * A commodity's Pyth Core feed id and its shard-0 push-feed account. Throws
 * for a commodity without a verified feed id (NATGAS, COPPER): no guessing.
 */
export function commodityFeed(commodity: string, programs: PythPrograms): CommodityFeed {
  if (!(commodity in PYTH_FEED_IDS)) throw new Error(`Unknown commodity: ${commodity}`);
  const hex = PYTH_FEED_IDS[commodity];
  if (!hex) {
    throw new Error(
      `${commodity} has no verified Pyth Core feed id (UNKNOWN; docs/lp-pool.md §18): it cannot launch until one is ` +
        'found, checked and added to frontend/src/config/pythFeeds.ts'
    );
  }
  const id = feedIdFromHex(hex);
  return { feedIdHex: feedIdToHex(id), feedId: Array.from(id), address: pushFeedAddress(id, programs.pushOracle) };
}

/**
 * Fail unless `feed` is what perps-core accepts as a market's feed for
 * `feedId` (initialize_market, propose/apply_oracle_feed): owned by
 * `oracleProgram`, a PriceUpdateV2 (≥ 133 bytes, discriminator), fully
 * verified, with that feed id. No age check, as on chain.
 */
export async function assertPythFeed(
  connection: Connection,
  feed: PublicKey,
  oracleProgram: PublicKey,
  feedId: Uint8Array | number[]
): Promise<void> {
  const info = await connection.getAccountInfo(feed);
  if (!info) {
    throw new Error(`Price feed ${feed} does not exist (Pyth sponsors no commodity feed: the protocol's price pusher must push it first)`);
  }
  if (!info.owner.equals(oracleProgram)) {
    throw new Error(`Price feed ${feed} is owned by ${info.owner}, not the oracle program ${oracleProgram}`);
  }
  const parsed = parsePriceUpdateV2(info.data);
  if ('reason' in parsed) {
    const why = {
      tooShort: `${info.data.length} bytes, fewer than a PriceUpdateV2`,
      wrongDiscriminator: 'not a PriceUpdateV2 (wrong discriminator; a legacy Pyth v2 price account?)',
      notFullyVerified: 'a Partially verified update (perps-core needs VerificationLevel::Full)',
      badVerificationLevel: 'an unknown verification level',
    }[parsed.reason];
    throw new Error(`Price feed ${feed} is ${why}`);
  }
  const want = Uint8Array.from(feedId);
  if (!sameFeedId(parsed.update.feedId, want)) {
    throw new Error(`Price feed ${feed} carries feed id ${feedIdToHex(parsed.update.feedId)}, not ${feedIdToHex(want)}`);
  }
}

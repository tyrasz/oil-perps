// Genesis price feeds for the TS suite (scripts/test-ts.sh; docs/lp-pool.md
// D47, D51): Pyth Solana Receiver PriceUpdateV2 accounts (134 bytes,
// pyth-solana-receiver-sdk 2.0.0's layout) owned by the system program,
// which the suite configures as ProtocolConfig.oracle_program. Nothing on the
// validator can write them, so genesis gets one OIL (USOILSPOT) feed every
// 50 s for the next three hours plus the special cases below; the suite picks
// a feed that is fresh when it needs one.
//
// Written by hand here, independently of the TS decoder
// (frontend/src/utils/oracleAccount.ts); the suite checks this writer against
// the SDK's own bytes (programs/perps-core/tests/fixtures/*.bin).
//
//   node scripts/test-oracles.js <dir>   → <dir>/accounts/*.json, <dir>/manifest.json
"use strict";
const fs = require("fs");
const path = require("path");
const crypto = require("crypto");

const DISCRIMINATOR = Buffer.from([34, 241, 35, 99, 157, 126, 244, 205]); // sha256("account:PriceUpdateV2")[..8]
const LEN = 134;
const FEED_IDS = {
  OIL: "925ca92ff005ae943c158e3563f59698ce7e75c5a8c8dd43303a0a154887b3e6", // Commodities.USOILSPOT
  GOLD: "765d2ba906dbc32ca17cc11f5310a89e9ee1f6420508c63861f2f8ba4ee34bb2", // XAU/USD
  SILVER: "f2fb02c32b055c805e7238d628e5e9dadef274376114eb1f012337cabe93871e", // XAG/USD
};

/**
 * A PriceUpdateV2 account: Full unless `partialSignatures` is a number
 * (Partial { num_signatures }, every field after the level one byte on).
 */
function priceUpdateV2({
  writeAuthority = Buffer.alloc(32),
  feedIdHex,
  price,
  conf,
  expo,
  publishTime,
  prevPublishTime = publishTime,
  emaPrice = price,
  emaConf = conf,
  postedSlot = 0,
  partialSignatures,
}) {
  const d = Buffer.alloc(LEN);
  DISCRIMINATOR.copy(d, 0);
  Buffer.from(writeAuthority).copy(d, 8);
  let o = 41;
  if (partialSignatures === undefined) {
    d.writeUInt8(1, 40); // VerificationLevel::Full
  } else {
    d.writeUInt8(0, 40); // VerificationLevel::Partial
    d.writeUInt8(partialSignatures, 41);
    o = 42;
  }
  const id = Buffer.from(feedIdHex, "hex");
  if (id.length !== 32) throw new Error(`feed id ${feedIdHex} is not 32 bytes`);
  id.copy(d, o);
  d.writeBigInt64LE(BigInt(price), o + 32);
  d.writeBigUInt64LE(BigInt(conf), o + 40);
  d.writeInt32LE(expo, o + 48);
  d.writeBigInt64LE(BigInt(publishTime), o + 52);
  d.writeBigInt64LE(BigInt(prevPublishTime), o + 60);
  d.writeBigInt64LE(BigInt(emaPrice), o + 68);
  d.writeBigUInt64LE(BigInt(emaConf), o + 76);
  d.writeBigUInt64LE(BigInt(postedSlot), o + 84);
  return d;
}

/** A legacy Pyth v2 PriceAccount header (magic, version 2, price account, 3_312 bytes): no longer accepted. */
function legacyPriceAccount() {
  const d = Buffer.alloc(3312);
  d.writeUInt32LE(0xa1b2c3d4, 0);
  d.writeUInt32LE(2, 4);
  d.writeUInt32LE(3, 8);
  d.writeUInt32LE(3312, 12);
  return d;
}

function main(dir) {
  // Required here, not at the top: the suite requires this file for
  // priceUpdateV2 only
  const { PublicKey } = require("@solana/web3.js");
  fs.mkdirSync(path.join(dir, "accounts"), { recursive: true });
  const now = Math.floor(Date.now() / 1000);
  const write = (name, data, owner = "11111111111111111111111111111111") => {
    const key = new PublicKey(crypto.createHash("sha256").update(`oil-perps-test-oracle-${name}`).digest());
    const account = {
      pubkey: key.toBase58(),
      account: {
        lamports: 1_000_000_000,
        data: [data.toString("base64"), "base64"],
        owner,
        executable: false,
        rentEpoch: 0,
        space: data.length,
      },
    };
    fs.writeFileSync(path.join(dir, "accounts", `feed-${name}.json`), JSON.stringify(account));
    return key.toBase58();
  };
  // $75.00 ± $0.05, expo −8
  const oil = (ts, extra = {}) => priceUpdateV2({ feedIdHex: FEED_IDS.OIL, price: 7_500_000_000, conf: 5_000_000, expo: -8, publishTime: ts, ...extra });
  const feeds = [];
  for (let i = 0, ts = now - 60; ts <= now + 3 * 3600; i++, ts += 50) feeds.push({ pubkey: write(i, oil(ts)), publishTime: ts });
  const special = {
    // OIL feed id, published long ago: StaleOraclePrice wherever a price is read
    stale: { pubkey: write("stale", oil(1_000_000_000)), publishTime: 1_000_000_000 },
    // A Full XAU/USD update (the GOLD market's feed)
    gold: { pubkey: write("gold", priceUpdateV2({ feedIdHex: FEED_IDS.GOLD, price: 200_000_000_000, conf: 10_000_000, expo: -8, publishTime: now })) },
    // A Full XAG/USD update: OracleFeedIdMismatch for an OIL market
    silver: { pubkey: write("silver", priceUpdateV2({ feedIdHex: FEED_IDS.SILVER, price: 2_400_000_000, conf: 1_000_000, expo: -8, publishTime: now })) },
    // The OIL feed id, Partially verified: OracleNotFullyVerified
    partial: { pubkey: write("partial", oil(now, { partialSignatures: 13 })) },
    // The OIL feed id, Full, but owned by another program (the TEST ONLY
    // mock-pyth id, not deployed here; solana-test-validator 4.4.1 panics on a
    // genesis account owned by the token program): InvalidOracleAccount
    wrongOwner: { pubkey: write("wrong-owner", oil(now), "MockPyth11111111111111111111111111111111111") },
    // A legacy v2 price account owned by the oracle program: InvalidOracleAccount
    legacy: { pubkey: write("legacy", legacyPriceAccount()) },
  };
  fs.writeFileSync(
    path.join(dir, "manifest.json"),
    JSON.stringify({ price: 75_000_000, feedIds: FEED_IDS, feeds, ...special })
  );
}

module.exports = { priceUpdateV2, FEED_IDS, LEN };

if (require.main === module) {
  if (!process.argv[2]) {
    console.error("usage: node scripts/test-oracles.js <dir>");
    process.exit(2);
  }
  main(process.argv[2]);
}

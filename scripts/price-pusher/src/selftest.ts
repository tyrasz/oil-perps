// The pusher's decisions and configuration, without a network:
//   npx tsx src/selftest.ts
import assert from 'assert';
import { DEFAULT_POLICY, checkPolicy, decide, deviationBps, lagging, type Quote } from './policy';
import { loadConfig } from './main';

const tests: [string, () => void][] = [];
const test = (name: string, f: () => void) => tests.push([name, f]);

// $3,623.512 at expo -3, published at t
const q = (price: bigint, publishTime: number, exponent = -3): Quote => ({ price, exponent, publishTime });
const T = 1_795_746_734;
const P = DEFAULT_POLICY;

test('deviation is in bps of the stored price, rounded up, across exponents', () => {
  assert.strictEqual(deviationBps(q(3_623_512n, T), q(3_623_512n, T)), 0);
  // 3.6235 of 3623.512 = 10.0000 bps
  assert.strictEqual(deviationBps(q(3_627_135n, T), q(3_623_512n, T)), 10);
  assert.strictEqual(deviationBps(q(3_627_134n, T), q(3_623_512n, T)), 10); // 9.997 → 10
  assert.strictEqual(deviationBps(q(3_623_513n, T), q(3_623_512n, T)), 1);
  // The same price at expo -3 and -8
  assert.strictEqual(deviationBps(q(362_351_200_000n, T, -8), q(3_623_512n, T)), 0);
  assert.strictEqual(deviationBps(q(1n, T), q(0n, T)), Number.POSITIVE_INFINITY);
});

test('a missing account is pushed (the first push creates it)', () => {
  assert.deepStrictEqual(decide(null, q(3_623_512n, T), T, P), { push: true, reason: 'missing' });
});

test('nothing is pushed unless Hermes is newer (off hours, or Hermes behind)', () => {
  const stored = q(3_623_512n, T);
  for (const t of [T, T - 1]) {
    // Even a big move or a stale account: the push oracle would refuse it
    assert.deepStrictEqual(decide(stored, q(4_000_000n, t), T + 3_600, P), { push: false, reason: 'nothingNewer' });
  }
});

test('a move at the threshold is pushed at once', () => {
  const d = decide(q(3_623_512n, T), q(3_627_135n, T + 1), T + 1, P);
  assert.deepStrictEqual(d, { push: true, reason: 'deviation', bps: 10 });
  const down = decide(q(3_623_512n, T), q(3_619_889n, T + 1), T + 1, P);
  assert.strictEqual(down.push && down.reason, 'deviation');
});

test('a quiet price is pushed on the heartbeat, not before', () => {
  const stored = q(3_623_512n, T);
  assert.deepStrictEqual(decide(stored, q(3_623_600n, T + 9), T + 9, P), { push: false, reason: 'withinPolicy', bps: 1, ageSecs: 9 });
  assert.deepStrictEqual(decide(stored, q(3_623_600n, T + 10), T + 10, P), { push: true, reason: 'heartbeat', ageSecs: 10 });
});

test('lag alerts only while Hermes is newer and past the line', () => {
  assert.strictEqual(lagging(null, q(1n, T), P), true);
  assert.strictEqual(lagging(q(3_623_512n, T), q(3_700_000n, T), P), false);
  assert.strictEqual(lagging(q(3_623_512n, T), q(3_627_135n, T + 1), P), true);
  assert.strictEqual(lagging(q(3_623_512n, T), q(3_626_000n, T + 1), P), false);
});

test('the policy stays inside §18 #21', () => {
  checkPolicy(DEFAULT_POLICY);
  assert.throws(() => checkPolicy({ ...P, deviationBps: 11 }), /deviation/);
  assert.throws(() => checkPolicy({ ...P, deviationBps: 0 }), /deviation/);
  assert.throws(() => checkPolicy({ ...P, heartbeatSecs: 30 }), /heartbeat/);
  assert.throws(() => checkPolicy({ ...P, lagAlertBps: 5 }), /lag alert/);
});

test('configuration: program pair and API key required, commodities from the feed table', () => {
  const env = { PYTH_PROGRAMS: 'pro', PYTH_API_KEY: 'k' };
  assert.throws(() => loadConfig([], { PYTH_API_KEY: 'k' }), /PYTH_PROGRAMS/);
  assert.throws(() => loadConfig([], { PYTH_PROGRAMS: 'pro' }), /PYTH_API_KEY/);
  assert.throws(() => loadConfig(['PLATINUM'], env), /Unknown commodity/);
  assert.throws(() => loadConfig([], { ...env, HEARTBEAT_SECS: '60' }), /heartbeat/);
  const all = loadConfig(['--once'], env);
  assert.deepStrictEqual(all.commodities, ['OIL', 'GOLD', 'SILVER', 'NATGAS', 'COPPER']);
  assert.strictEqual(all.receiver.toBase58(), 'rec2HHDDnjLfj4kE7VyEtFA1HPGQLK33259532cRyHp');
  assert.strictEqual(all.pushOracle.toBase58(), 'pyt2F414BA6dPttK6RddPZUdHfapoBN24GL5wbrPCou');
  assert.ok(all.once && !all.dryRun);
  const legacy = loadConfig(['--dry-run', 'GOLD'], { ...env, PYTH_PROGRAMS: 'legacy', RPC_URLS: 'http://a, http://b' });
  assert.deepStrictEqual(legacy.commodities, ['GOLD']);
  assert.deepStrictEqual(legacy.rpcUrls, ['http://a', 'http://b']);
  assert.strictEqual(legacy.pushOracle.toBase58(), 'pythWSnswVUd12oZpeFP8e9CVaEqJg25g1Vtc2biRsT');
});

let failed = 0;
for (const [name, f] of tests) {
  try {
    f();
    console.log(`ok - ${name}`);
  } catch (e) {
    failed++;
    console.log(`not ok - ${name}\n  ${(e as Error).stack}`);
  }
}
console.log(`selftest: ${tests.length - failed} passed${failed ? `, ${failed} failed` : ''}`);
process.exit(failed ? 1 : 0);

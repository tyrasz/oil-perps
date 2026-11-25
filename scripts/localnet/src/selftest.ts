// Checks of the harness's pure decisions (logic.ts, lib.ts errorText /
// classifyError) without a validator. CI runs it after type-checking
// scripts/localnet; `node ... ts-node ... selftest.ts` locally.

import * as assert from "assert";
import { classifyError, errorText } from "./lib";
import {
  badDebtEvents,
  breakIndexAt,
  coverageProblems,
  gapperLeverage,
  gapperTopUpTarget,
  nextReopen,
  printReopenNow,
  weekendGapAt,
} from "./logic";

const S = 2; // SCHEDULE_TEST_HOURLY
const W0 = 1_795_604_400; // a multiple of 3_600: c = 0, the weekly reopen
let n = 0;
function test(name: string, f: () => void) {
  f();
  n++;
  console.log(`ok - ${name}`);
}

test("reopen print lands in the last closed seconds, before the market can trade", () => {
  // Weekend: closed [3000, 3600)
  assert.deepStrictEqual(nextReopen(S, W0 - 3), { inSecs: 3, weekStart: true, breakIndex: 0 });
  assert.strictEqual(printReopenNow(nextReopen(S, W0 - 4), 3, 2), false);
  assert.strictEqual(printReopenNow(nextReopen(S, W0 - 3), 3, 2), true);
  assert.strictEqual(printReopenNow(nextReopen(S, W0 - 1), 3, 2), true);
  // Open: nothing to plan (the old driver printed here, at c = 0)
  assert.strictEqual(nextReopen(S, W0), null);
  // Daily breaks end at 810, 1560, 2310; the stale break (2) is withheld
  assert.deepStrictEqual(nextReopen(S, W0 + 807), { inSecs: 3, weekStart: false, breakIndex: 1 });
  assert.strictEqual(printReopenNow(nextReopen(S, W0 + 807), 3, 2), true);
  assert.strictEqual(printReopenNow(nextReopen(S, W0 + 1_557), 3, 2), false);
  assert.strictEqual(printReopenNow(nextReopen(S, W0 + 1_557), 3, 0), true);
  assert.strictEqual(breakIndexAt(S, W0 + 1_500), 2);
  assert.strictEqual(breakIndexAt(S, W0 + 3_100), 0);
  assert.strictEqual(breakIndexAt(S, W0 + 100), 0);
});

test("a checker that checked nothing cannot pass", () => {
  const lim = { maxGapSecs: 60, maxUncheckedFrac: 0.2 };
  const ok = { cycles: 100, consistent: 95, failures: 2, skippedBusy: 3, maxGapSecs: 12, edgesMissed: 0 };
  assert.deepStrictEqual(coverageProblems(ok, lim), []);
  // Every cycle threw (IDL drift, RPC down): the old checker reported PASS
  assert.strictEqual(coverageProblems({ ...ok, consistent: 0, failures: 100, skippedBusy: 0, maxGapSecs: 0 }, lim).length, 2);
  assert.strictEqual(coverageProblems({ ...ok, consistent: 70, failures: 30 }, lim).length, 1);
  assert.strictEqual(coverageProblems({ ...ok, maxGapSecs: 61 }, lim).length, 1);
  assert.strictEqual(coverageProblems({ ...ok, edgesMissed: 1 }, lim).length, 1);
});

test("weekend gap is the driver's gap print against the print before it", () => {
  const rows = [
    { t: W0 - 10, price: 80, event: null, ok: true },
    { t: W0 - 5, price: 80, event: null, ok: true },
    { t: W0 - 3, price: 60, event: "weekend_gap -25.00%", ok: true },
    { t: W0 + 2, price: 60.3, event: null, ok: true },
  ];
  // The old checker took the first print after the first snapshot (c = 2): −24.6%
  assert.deepStrictEqual(weekendGapAt(rows, W0), { before: 80, after: 60, gapPct: -25, printedAt: W0 - 3 });
  assert.strictEqual(weekendGapAt(rows, W0 + 3_600), null);
});

test("bad debt counts BadDebt events once, from keeper and bot transactions", () => {
  const keeper = { t: 1, by: "keeper", detail: [{ name: "PositionLiquidated", deficit: "5" }, { name: "BadDebt", deficit: "5" }] };
  const bot = { t: 2, by: "degen-1", detail: [{ name: "BadDebt", deficit: "7" }] };
  const other = { t: 3, by: "x", detail: [{ name: "PositionPnlSettled" }] };
  const out = badDebtEvents([keeper, bot, other]);
  assert.deepStrictEqual(out.map((e) => [e.by, e.deficit]), [["keeper", "5"], ["degen-1", "7"]]);
  // Decoded events carry camelCase fields (the rerun's report showed "None" for them)
  const [d] = badDebtEvents([{ t: 4, by: "keeper", detail: [{ name: "BadDebt", deficit: "9", insurancePaid: "6", lpAbsorbed: "3", pendingAdded: "0" }] }]);
  assert.deepStrictEqual([d.insurance_paid, d.lp_absorbed, d.pending_added], ["6", "3", "0"]);
});

test("the gapper opens covering the weekend maintenance margin", () => {
  // Weekend MMR 10%, 1.5% buffer, up to 1% impact: 8x, not the 9.8x maximum
  const lev = gapperLeverage(1_000, 500, 150, 100, 9_800);
  assert.strictEqual(lev, 8_000);
  // margin at 8x is 12.5% of notional: after a 1% impact 11.5% ≥ 10% + 1.5%
  assert.ok(10_000 / (lev / 1_000) - 100 >= 1_000 + 150);
  assert.strictEqual(gapperLeverage(1_000, 500, 150, 100, 5_000), 5_000);
  assert.strictEqual(gapperTopUpTarget(100_000_000_000n, 1_000, 500, 150), 11_500_000_000n);
});

test("thrown non-Error values keep their content", () => {
  assert.ok(errorText({ code: -32002, message: "boom", data: { x: 1 } }).includes("boom"));
  assert.ok(errorText({ code: -32002, message: "boom", data: { x: 1 } }).includes("x: 1"));
  assert.strictEqual(errorText(new Error("e")), "e");
  // classifyError on a plain object: its text, not [object Object]
  const code = classifyError({ reason: "socket closed" }, []);
  assert.ok(code.startsWith("other:") && code.includes("socket_closed"), code);
  assert.strictEqual(classifyError(new Error("fetch failed"), []), "infra:RpcUnavailable");
});

console.log(`selftest: ${n} passed`);

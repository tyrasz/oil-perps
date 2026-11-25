#!/usr/bin/env node
// Screenshots of the frontend pointed at the running localnet (docs/localnet.md):
// starts the Vite dev server with VITE_NETWORK=localnet and the validator's
// mint and mock-pyth feed (from target/localnet/state.json), opens the trading
// page and the LP vault with the localnet-only burner wallet connected, and
// saves PNGs (deviceScaleFactor 2) into SCREENSHOT_DIR.
//
//   node scripts/localnet/screenshots.js [label]

const { spawn } = require("child_process");
const fs = require("fs");
const path = require("path");
const http = require("http");
const { chromium } = require("playwright");

const ROOT = path.resolve(__dirname, "../..");
const LOCALNET_DIR = process.env.LOCALNET_DIR || path.join(ROOT, "target/localnet");
const OUT = process.env.SCREENSHOT_DIR || path.join(ROOT, "target/localnet-report/screenshots");
const PORT = Number(process.env.FRONTEND_PORT || 3100);
const label = process.argv[2] ? `-${process.argv[2]}` : "";

function get(url) {
  return new Promise((resolve) => {
    http.get(url, (res) => {
      res.resume();
      resolve(res.statusCode);
    }).on("error", () => resolve(0));
  });
}

async function main() {
  const st = JSON.parse(fs.readFileSync(path.join(LOCALNET_DIR, "state.json"), "utf-8"));
  const host = new URL(st.rpcUrl).hostname;
  if (!["127.0.0.1", "localhost"].includes(host)) throw new Error(`refusing non-local RPC ${st.rpcUrl}`);
  fs.mkdirSync(OUT, { recursive: true });
  const env = {
    ...process.env,
    VITE_NETWORK: "localnet",
    VITE_RPC_URL: st.rpcUrl,
    VITE_USDC_MINT: st.mint,
    VITE_PYTH_FEED_OIL: st.feed,
    VITE_ORACLE_SOURCE: "chain",
  };
  const vite = spawn(path.join(ROOT, "frontend/node_modules/.bin/vite"), ["--port", String(PORT), "--strictPort", "--host", "127.0.0.1"], {
    cwd: path.join(ROOT, "frontend"),
    env,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let viteLog = "";
  vite.stdout.on("data", (d) => (viteLog += d));
  vite.stderr.on("data", (d) => (viteLog += d));
  try {
    const url = `http://127.0.0.1:${PORT}/`;
    for (let i = 0; i < 60 && (await get(url)) !== 200; i++) await new Promise((r) => setTimeout(r, 1000));
    if ((await get(url)) !== 200) throw new Error(`vite did not start:\n${viteLog}`);
    const browser = await chromium.launch();
    const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, deviceScaleFactor: 2 });
    // Select the localnet burner wallet so wallet-only panels (LP vault) load
    await context.addInitScript(() => window.localStorage.setItem("walletName", JSON.stringify("Burner Wallet")));
    const page = await context.newPage();
    const consoleErrors = [];
    page.on("console", (m) => m.type() === "error" && consoleErrors.push(m.text()));
    await page.goto(url, { waitUntil: "networkidle" });
    await page.waitForTimeout(Number(process.env.SCREENSHOT_SETTLE_MS || 10_000));
    const trading = path.join(OUT, `trading${label}.png`);
    await page.screenshot({ path: trading, fullPage: true });
    await page.getByRole("button", { name: "LP Vault" }).click();
    await page.waitForTimeout(6_000);
    // The tab panel scrolls inside a fixed-height area: let it grow so the
    // whole vault is in the page
    await page.evaluate(() => {
      for (let el = document.querySelector(".lp-vault"); el && el !== document.body; el = el.parentElement) {
        el.style.overflow = "visible";
        el.style.maxHeight = "none";
        el.style.height = "auto";
      }
    });
    await page.waitForTimeout(500);
    const vault = path.join(OUT, `lp-vault${label}.png`);
    await page.screenshot({ path: vault, fullPage: true });
    // The vault panel alone (the page's fixed footer covers its lower part)
    const panel = page.locator(".lp-vault").first();
    let vaultPanel = null;
    if (await panel.count()) {
      vaultPanel = path.join(OUT, `lp-vault-panel${label}.png`);
      await panel.screenshot({ path: vaultPanel });
    }
    await browser.close();
    console.log(`screenshots: ${[trading, vault, vaultPanel].filter(Boolean).join(", ")}`);
    if (consoleErrors.length) console.log(`browser console errors (${consoleErrors.length}):\n${consoleErrors.slice(0, 10).join("\n")}`);
  } finally {
    vite.kill("SIGTERM");
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});

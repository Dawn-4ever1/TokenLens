#!/usr/bin/env node
// Visual and network checks against a real TokenLens server (no account or
// production data needed). Start an isolated server, then from app/ run:
//   node ../scripts/check-public-site.mjs http://127.0.0.1:8787
// Optional: TOKENLENS_SCREENSHOT_DIR=/existing/scratch/directory.
import { createRequire } from "node:module";
import { existsSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(new URL("../app/package.json", import.meta.url));
const browsers = join(dirname(fileURLToPath(import.meta.url)), "../app/.playwright-browsers");
if (existsSync(browsers)) process.env.PLAYWRIGHT_BROWSERS_PATH ??= browsers;
const { chromium, webkit } = require("playwright");
const base = (process.argv[2] || "http://127.0.0.1:8787").replace(/\/$/, "");
if (!base.startsWith("http://127.0.0.1:") && !base.startsWith("http://localhost:")) {
  throw new Error("Use an isolated loopback server, not production or a remote host.");
}
const screenshots = process.env.TOKENLENS_SCREENSHOT_DIR;
if (screenshots) mkdirSync(screenshots, { recursive: true });
const checks = [];

for (const [name, type, options] of [
  ["chrome", chromium, { channel: process.env.E2E_CHANNEL || "chrome" }],
  ["webkit", webkit, {}],
]) {
  let browser;
  try {
    browser = await type.launch({ headless: true, ...options });
  } catch (error) {
    if (name === "webkit") {
      console.log(`WebKit unavailable; Chrome still required. Install with npm run test:e2e:setup. (${error.message.split("\n")[0]})`);
      continue;
    }
    throw error;
  }
  try {
    for (const width of [1440, 1200, 900, 390, 320]) {
      const page = await browser.newPage({ viewport: { width, height: 1000 } });
      const requests = [];
      const errors = [];
      page.on("request", (request) => requests.push(request.url()));
      page.on("pageerror", (error) => errors.push(error.message));
      for (const path of ["/", "/privacy"]) {
        const response = await page.goto(`${base}${path}`, { waitUntil: "networkidle" });
        if (response?.status() !== 200) throw new Error(`${name} ${path}: HTTP ${response?.status()}`);
        const result = await page.evaluate(() => ({
          title: document.title,
          width: document.documentElement.scrollWidth,
          scripts: document.scripts.length,
          cookies: document.cookie,
        }));
        if (result.width > width) throw new Error(`${name} ${width} ${path}: overflow ${result.width}`);
        if (result.scripts || result.cookies) throw new Error(`${path}: scripts or cookies found`);
        if (screenshots && [1440, 390].includes(width)) {
          const slug = path === "/" ? "index" : "privacy";
          await page.screenshot({ path: join(screenshots, `${name}-${width}-${slug}.png`), fullPage: true });
        }
        checks.push({ browser: name, width, path, ...result });
      }
      const external = requests.filter((url) => !url.startsWith(`${base}/`));
      if (external.length) throw new Error(`Third-party resources: ${external.join(", ")}`);
      if (errors.length) throw new Error(errors.join("\n"));
      await page.close();
    }
  } finally {
    await browser.close();
  }
}
console.log(`Public pages passed ${checks.length} viewport/page checks; no overflow, scripts, cookies or third-party resources.`);

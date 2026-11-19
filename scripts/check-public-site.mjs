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
        if (path === "/") {
          const nav = page.locator(".site-nav .nav-download");
          if (await nav.textContent() !== "GitHub ↗") throw new Error("Top navigation must say GitHub");
          if (await nav.getAttribute("href") !== "https://github.com/Dawn-4ever1/TokenLens") throw new Error("Top navigation must link to the public GitHub project");
          const images = await page.locator("img").evaluateAll(async (items) => {
            await Promise.all(items.map(async (image) => {
              image.loading = "eager";
              if (!image.complete) await new Promise((resolve) => { image.addEventListener("load", resolve, { once: true }); image.addEventListener("error", resolve, { once: true }); });
            }));
            return items.map((image) => ({ src: image.getAttribute("src"), alt: image.alt, loaded: image.complete && image.naturalWidth > 0, width: image.getBoundingClientRect().width }));
          });
          if (images.length !== 6) throw new Error(`${name} ${width}: expected 6 product/brand images, found ${images.length}`);
          for (const image of images) {
            if (!image.loaded || !image.alt && image.src !== "/assets/tokenlens.svg") throw new Error(`${name} ${width}: broken or unlabelled image ${image.src}`);
            if (image.width <= 0) throw new Error(`${name} ${width}: invisible image ${image.src}`);
          }
          const internalLinks = await page.locator('a[href^="#"]').evaluateAll((links) => links.map((link) => link.getAttribute("href")));
          for (const href of internalLinks) {
            if (href === "#" || !await page.locator(href).count()) throw new Error(`${name} ${width}: broken section link ${href}`);
          }
          for (const selector of ["#features", "#showcase", "#workflow", "#getting-started", "#faq", "#download-title"]) {
            if (await page.locator(selector).count() !== 1) throw new Error(`${name} ${width}: missing ${selector}`);
          }
          const faq = page.locator(".faq-list details").nth(1);
          await faq.locator("summary").click();
          if (!await faq.evaluate((element) => element.open)) throw new Error(`${name} ${width}: FAQ must work without JavaScript`);
          await faq.locator("summary").click();
          if (await faq.evaluate((element) => element.open)) throw new Error(`${name} ${width}: FAQ did not close`);
        }
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
    for (const [platform, userAgent] of [
      ["macos", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 Version/18.1 Safari/605.1.15"],
      ["windows", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0.0.0 Safari/537.36"],
    ]) {
      const page = await browser.newPage({ userAgent, viewport: { width: 1440, height: 1000 } });
      const response = await page.goto(`${base}/`, { waitUntil: "networkidle" });
      if (response?.status() !== 200) throw new Error(`${name} ${platform}: HTTP ${response?.status()}`);
      for (const candidate of ["macos", "windows"]) {
        const primaryCount = await page.locator(`[data-download-platform="${candidate}"].button-primary`).count();
        if (primaryCount !== (candidate === platform ? 2 : 0)) {
          throw new Error(`${name} ${platform}: ${candidate} primary download count is ${primaryCount}`);
        }
      }
      for (const selector of [".download-actions", ".download-buttons"]) {
        const order = await page.locator(`${selector} [data-download-platform]`).evaluateAll((buttons) => buttons.map((button) => button.getAttribute("data-download-platform")));
        if (order.join(",") !== [platform, platform === "macos" ? "windows" : "macos"].join(",")) {
          throw new Error(`${name} ${platform} ${selector}: order is ${order.join(",")}`);
        }
      }
      const scripts = await page.locator("script").count();
      if (scripts) throw new Error(`${name} ${platform}: platform selection must not add scripts`);
      if (screenshots && platform === "macos") {
        await page.screenshot({ path: join(screenshots, `${name}-1440-index-macos.png`), fullPage: true });
      }
      checks.push({ browser: name, platform, path: "/", scripts });
      await page.close();
    }
  } finally {
    await browser.close();
  }
}
console.log(`Public pages passed ${checks.length} viewport/platform/page checks; matching desktop downloads come first, with no overflow, scripts, cookies or third-party resources.`);

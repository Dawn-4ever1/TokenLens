import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { defineConfig, devices } from "@playwright/test";

// WebKit is what the Mac app renders with; it is optional because the browser is a 300 MB download
// (`npm run test:e2e:setup`) kept inside the project instead of the user-wide cache.
const browsers = fileURLToPath(new URL("./.playwright-browsers", import.meta.url));
process.env.PLAYWRIGHT_BROWSERS_PATH ??= browsers;
const webkit = existsSync(browsers);

// Runs the Vite dev server, so every command goes to the in-browser mock backend.
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  // System Chrome occasionally stalls opening a context when four start at once.
  workers: 2,
  retries: 1,
  reporter: [["list"]],
  use: {
    baseURL: "http://127.0.0.1:1420",
    headless: true,
  },
  projects: [
    // E2E_CHANNEL=msedge uses the Edge that ships with Windows when Chrome is not installed.
    { name: "chrome", use: { ...devices["Desktop Chrome"], channel: process.env.E2E_CHANNEL || "chrome" } },
    ...(webkit ? [{ name: "webkit", use: { ...devices["Desktop Safari"] } }] : []),
  ],
  webServer: {
    command: "npm run dev",
    url: "http://127.0.0.1:1420",
    reuseExistingServer: true,
    timeout: 60_000,
  },
});

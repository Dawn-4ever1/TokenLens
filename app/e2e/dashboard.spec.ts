import { expect, test, type Page } from "@playwright/test";

type View = "overview" | "trends" | "sessions" | "devices" | "settings";

const VIEWS: { key: View; label: string }[] = [
  { key: "overview", label: "总览" },
  { key: "trends", label: "趋势" },
  { key: "sessions", label: "会话" },
  { key: "devices", label: "设备" },
  { key: "settings", label: "设置" },
];
const WIDTHS = [1440, 1200, 900];
const STORAGE_KEYS = ["tokenlens.filters.v1", "tokenlens.page.v1", "tokenlens.settings.v1"];

interface Options {
  view?: View;
  theme?: "light" | "dark";
  direction?: "editorial" | "graphite" | "paper";
  query?: string;
}

/** Seeds UI state once per tab and records console errors and page errors. */
async function open(page: Page, { view = "overview", theme = "light", direction = "editorial", query = "" }: Options = {}) {
  await page.addInitScript(
    ([settings, initialView]) => {
      if (sessionStorage.getItem("e2e-seeded")) return;
      localStorage.clear();
      localStorage.setItem("tokenlens.settings.v1", settings);
      localStorage.setItem("tokenlens.page.v1", initialView);
      sessionStorage.setItem("e2e-seeded", "1");
    },
    [JSON.stringify({ theme, direction, refreshSeconds: 0 }), JSON.stringify(view)] as const,
  );
  const errors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(`/${query}`);
  await expect(page).toHaveTitle("TokenLens");
  return errors;
}

async function settle(page: Page, label: string) {
  await expect(page.locator("header h1")).toHaveText(label);
  await expect(page.locator(".skeleton")).toHaveCount(0);
  // ECharts draws on the next animation frames.
  await page.waitForTimeout(300);
}

/** Elements that stick out of the viewport without a scrolling or clipping ancestor. */
function overflow(page: Page) {
  return page.evaluate(() => {
    const limit = window.innerWidth + 1;
    const offenders: string[] = [];
    for (const element of document.querySelectorAll("body *")) {
      const rect = element.getBoundingClientRect();
      if (rect.width === 0 || rect.right <= limit) continue;
      let clipped = false;
      for (let parent = element.parentElement; parent; parent = parent.parentElement) {
        const style = getComputedStyle(parent);
        if (["auto", "scroll", "hidden"].includes(style.overflowX) && parent.getBoundingClientRect().right <= limit) {
          clipped = true;
          break;
        }
      }
      if (!clipped) offenders.push(`${element.tagName.toLowerCase()} right=${Math.round(rect.right)}`);
    }
    const root = document.documentElement;
    const main = document.querySelector("main");
    return {
      document: root.scrollWidth > root.clientWidth,
      main: main ? main.scrollWidth > main.clientWidth + 1 : false,
      offenders: offenders.slice(0, 5),
    };
  });
}

function luminance(color: string): number {
  const [r = 0, g = 0, b = 0] = (color.match(/[\d.]+/g) ?? []).map(Number);
  return (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
}

for (const width of WIDTHS) {
  for (const view of VIEWS) {
    test(`${view.label} @${width}: no horizontal scroll, no console errors`, async ({ page }) => {
      await page.setViewportSize({ width, height: 900 });
      const errors = await open(page, { view: view.key });
      await settle(page, view.label);
      if (width >= 1100) await expect(page.getByText("TokenLens", { exact: true })).toBeVisible();
      expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
      expect(errors).toEqual([]);
    });
  }
}

test("sidebar buttons are reachable by name when labels are collapsed", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 800 });
  const errors = await open(page);
  await settle(page, "总览");
  const nav = page.getByRole("navigation", { name: "主导航" });
  for (const view of VIEWS) {
    await nav.getByRole("button", { name: view.label, exact: true }).click();
    await settle(page, view.label);
    await expect(nav.getByRole("button", { name: view.label, exact: true })).toHaveAttribute("aria-current", "page");
  }
  expect(errors).toEqual([]);
});

test("overview marks Qoder numbers as estimated", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  await open(page);
  await settle(page, "总览");
  await expect(page.getByText("含 Qoder 估算").first()).toBeVisible();
});

for (const direction of ["editorial", "graphite", "paper"] as const) {
  test(`dark mode works in the ${direction} direction`, async ({ page }) => {
    await page.setViewportSize({ width: 1200, height: 900 });
    const errors = await open(page, { theme: "dark", direction });
    await settle(page, "总览");
    const root = page.locator("html");
    await expect(root).toHaveClass(/\bdark\b/);
    await expect(root).toHaveAttribute("data-direction", direction);
    const background = await page.evaluate(() => getComputedStyle(document.body).backgroundColor);
    expect(luminance(background)).toBeLessThan(0.25);
    expect(errors).toEqual([]);
  });
}

test("switching the theme in settings applies immediately and persists", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  await open(page, { view: "settings" });
  await settle(page, "设置");
  await page.getByRole("radiogroup", { name: "主题" }).getByRole("radio", { name: "深色" }).click();
  await expect(page.locator("html")).toHaveClass(/\bdark\b/);
  await page.reload();
  await settle(page, "设置");
  await expect(page.locator("html")).toHaveClass(/\bdark\b/);
});

test("session drawer opens from a row and closes with Escape", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await open(page, { view: "sessions" });
  await settle(page, "会话");
  await page.locator("tbody tr").first().click();
  const drawer = page.getByRole("dialog");
  await expect(drawer).toBeVisible();
  await expect(drawer.locator(".skeleton")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(drawer).toBeHidden();
  expect(errors).toEqual([]);
});

test("a Qoder session shows the estimated badge in the drawer", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  await open(page, { view: "sessions" });
  await settle(page, "会话");
  await page.locator("tbody tr").filter({ hasText: "Qoder" }).first().click();
  const drawer = page.getByRole("dialog");
  await expect(drawer.locator(".skeleton")).toHaveCount(0);
  await expect(drawer.getByText("估算", { exact: true }).first()).toBeVisible();
});

test("offline state explains the problem without crashing", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await open(page, { query: "?offline" });
  await expect(page.getByText("连不上服务器").first()).toBeVisible();
  expect(errors.filter((error) => !error.includes("network"))).toEqual([]);
});

test("pairing never leaves the code or a credential in web storage or the URL", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  await open(page, { query: "?unpaired" });
  await expect(page.getByRole("heading", { name: "开始使用 TokenLens" })).toBeVisible();
  await page.getByRole("radio", { name: "连接服务器" }).click();
  const code = "QWER-TYUI";
  await page.getByLabel("服务器地址").fill("https://dash.example.com");
  await page.getByLabel("配对码").fill(code);
  await page.getByRole("button", { name: "配对", exact: true }).click();
  await settle(page, "总览");
  for (const view of VIEWS) {
    await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: view.label, exact: true }).click();
    await settle(page, view.label);
  }
  const stored = await page.evaluate(() => {
    const dump = (storage: Storage) => Object.fromEntries(Object.keys(storage).map((key) => [key, storage.getItem(key) ?? ""]));
    return { local: dump(localStorage), session: dump(sessionStorage), url: location.href, cookies: document.cookie };
  });
  expect(Object.keys(stored.local).sort()).toEqual(STORAGE_KEYS);
  // Key names are fixed above and contain "tokenlens", so only values are scanned.
  const everything = JSON.stringify([...Object.values(stored.local), ...Object.values(stored.session), stored.url, stored.cookies]);
  expect(everything).not.toContain(code);
  expect(everything).not.toContain(code.replace("-", ""));
  expect(everything).not.toMatch(/bearer|secret|token/i);
  expect(everything).not.toMatch(/[A-Za-z0-9_-]{32,}/);
  expect(stored.cookies).toBe("");
});

test("first run can start in local mode without a server or a code", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 800 });
  const errors = await open(page, { query: "?unpaired" });
  await expect(page.getByRole("heading", { name: "开始使用 TokenLens" })).toBeVisible();
  // Local mode is the default and asks for neither an address nor a code.
  await expect(page.getByRole("radio", { name: "只在这台电脑上" })).toHaveAttribute("aria-checked", "true");
  await expect(page.getByLabel("服务器地址")).toHaveCount(0);
  await expect(page.getByLabel("配对码")).toHaveCount(0);
  await page.getByRole("radio", { name: "连接服务器" }).click();
  await expect(page.getByRole("button", { name: "配对", exact: true })).toBeDisabled();
  await page.getByRole("radio", { name: "只在这台电脑上" }).click();
  await page.getByRole("button", { name: "开始使用", exact: true }).click();
  await settle(page, "总览");
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
  expect(errors).toEqual([]);
});

test("local mode hides server-only actions and offers to switch to a server", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await open(page, { view: "devices", query: "?local" });
  await settle(page, "设备");
  await expect(page.getByRole("button", { name: /添加设备/ })).toHaveCount(0);
  await expect(page.getByText("本机模式只统计这一台电脑")).toBeVisible();
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设置", exact: true }).click();
  await settle(page, "设置");
  await expect(page.getByText("只在这台电脑上", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "改为连接服务器" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "继续" }).click();
  await expect(page.getByRole("heading", { name: "开始使用 TokenLens" })).toBeVisible();
  expect(errors).toEqual([]);
});

test("clicking a token count switches every count to exact digits and remembers it", async ({ page }) => {
  for (const width of [900, 1200]) {
    await page.setViewportSize({ width, height: 900 });
    const errors = await open(page);
    await settle(page, "总览");
    const kpi = page.locator("section.card .num-fit").first().getByRole("button", { pressed: false });
    await expect(kpi).toHaveText(/^[\d.]+[KMBT]$/);
    await kpi.click();
    const exactKpi = page.locator("section.card .num-fit").first().getByRole("button", { pressed: true });
    await expect(exactKpi).toHaveText(/^\d{1,3}(,\d{3})+$/);
    // Every other count follows, e.g. today's tokens under the headline.
    await expect(page.locator("section.card").first().getByRole("button", { pressed: false })).toHaveCount(0);
    // Long exact counts must still fit their card.
    expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
    const fits = await exactKpi.evaluate((element) => element.getBoundingClientRect().right <= element.closest("section")!.getBoundingClientRect().right);
    expect(fits).toBe(true);
    await page.reload();
    await settle(page, "总览");
    await expect(page.locator("section.card .num-fit").first().getByRole("button", { pressed: true })).toHaveText(/,/);
    await page.locator("section.card .num-fit").first().getByRole("button", { pressed: true }).click();
    await expect(page.locator("section.card .num-fit").first().getByRole("button", { pressed: false })).toHaveText(/^[\d.]+[KMBT]$/);
    expect(errors).toEqual([]);
    await page.evaluate(() => sessionStorage.clear());
  }
});

test("agent coverage lists agents in use before the rest", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await open(page, { view: "devices" });
  await settle(page, "设备");
  const table = page.locator("table").last();
  await expect(table.getByRole("columnheader", { name: /已装或用过/ })).toBeVisible();
  await expect(table.getByRole("columnheader", { name: /未发现 · 按热度/ })).toBeVisible();
  const names = await table.locator("thead tr").last().locator("th").allInnerTexts();
  const firstUnused = names.indexOf("Cursor");
  expect(firstUnused).toBeGreaterThan(0);
  // Every agent with mock usage sits left of the unused group, which starts with the most popular.
  for (const used of ["Claude Code", "Codex", "Qoder", "DSH", "ZCode", "OpenClaw", "Gemini CLI"]) expect(names.indexOf(used)).toBeLessThan(firstUnused);
  expect(names.slice(firstUnused, firstUnused + 4)).toEqual(["Cursor", "Copilot", "Cline", "Trae"]);
  expect(errors).toEqual([]);
});

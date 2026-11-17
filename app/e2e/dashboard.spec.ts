import { expect, test, type Page } from "@playwright/test";
import { clientLabel } from "../src/lib/labels";

type View = "overview" | "trends" | "sessions" | "devices" | "cloud" | "settings";

const VIEWS: { key: View; label: string }[] = [
  { key: "overview", label: "总览" },
  { key: "trends", label: "趋势" },
  { key: "sessions", label: "会话" },
  { key: "devices", label: "设备" },
  { key: "cloud", label: "云端" },
  { key: "settings", label: "设置" },
];
const WIDTHS = [1440, 1200, 900];
const STORAGE_KEYS = ["tokenlens.filters.v1", "tokenlens.page.v1", "tokenlens.settings.v1", "tokenlens.source.v1"];

interface Options {
  view?: View;
  theme?: "light" | "dark";
  direction?: "editorial" | "graphite" | "paper";
  query?: string;
}

/** Seeds UI state once per tab and records console errors and page errors. */
async function open(page: Page, { view = "overview", theme = "light", direction = "editorial", query = "?server" }: Options = {}) {
  await page.addInitScript(
    ([settings, initialView]) => {
      const key = new URLSearchParams(location.search).has("brand-gallery")
        ? `e2e-seeded-${settings}-${initialView}`
        : "e2e-seeded";
      if (sessionStorage.getItem(key)) return;
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

test("overview prioritizes concise KPIs and a full-width trend", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 1000 });
  const errors = await open(page);
  await settle(page, "总览");
  const kpis = page.getByTestId("overview-kpis");
  await expect(kpis.locator("h2")).toHaveText(["Tokens", "成本", "缓存命中率", "会话"]);
  await expect(page.getByText("成本为 API 等价估算", { exact: true })).toHaveCount(0);
  await expect(page.getByText("部分未定价", { exact: true })).toHaveCount(0);
  await expect(kpis.getByText(/API 等价估算|不是账单|非缓存|今日|轮 ·/)).toHaveCount(0);
  const bounds = async (title: string) => page.locator("section.card").filter({ has: page.getByRole("heading", { name: title, exact: true }) }).boundingBox();
  const [tools, models, trend, composition, recent, devices] = await Promise.all(["工具", "模型", "趋势", "Token 构成", "最近会话", "设备"].map(bounds));
  expect(tools!.y).toBe(models!.y);
  expect(trend!.y).toBeGreaterThan(tools!.y + tools!.height);
  expect(trend!.width).toBeGreaterThan(tools!.width * 1.8);
  expect(composition!.y).toBeGreaterThan(trend!.y + trend!.height);
  expect(recent!.y).toBe(devices!.y);
  expect(recent!.y).toBeGreaterThan(composition!.y + composition!.height);
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
  expect(errors).toEqual([]);
});

test("every registered agent icon is bundled and loads in light and dark mode", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  const errors = await open(page, { view: "devices", query: "?server&brand-gallery" });
  await settle(page, "设备");
  const expected: Record<string, string> = {
    cline: "cline", trae: "trae", kiro: "kiro", augment: "augment",
    codebuddy: "codebuddy", zed: "zed", warp: "warp", goose: "goose",
    junie: "junie", crush: "crush", grok: "grok", "devin-cli": "devin",
    "devin-desktop": "devin", lmstudio: "lmstudio", cherrystudio: "cherrystudio",
    codebuff: "codebuff", hermes: "hermes", pi: "pi", kilo: "kilo",
  };
  for (const [id, brand] of Object.entries(expected)) {
    const image = page.getByTitle(clientLabel(id), { exact: true }).locator(`img[data-brand="${brand}"]`);
    await expect(image).toBeVisible();
    expect(await image.evaluate((element: HTMLImageElement) => element.complete && element.naturalWidth > 0)).toBe(true);
  }
  await expect(page.locator('img[data-brand="hermes"]')).toHaveCSS("filter", "invert(0)");
  await expect(page.locator('img[data-brand="pi"]')).toHaveCSS("filter", "invert(1)");
  await open(page, { view: "devices", theme: "dark", query: "?server&brand-gallery" });
  await settle(page, "设备");
  await expect(page.locator('img[data-brand="hermes"]')).toHaveCSS("filter", "invert(1)");
  await expect(page.locator('img[data-brand="pi"]')).toHaveCSS("filter", "invert(0)");
  expect(errors).toEqual([]);
});

test("overview no longer shows precision badges", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  await open(page);
  await settle(page, "总览");
  await expect(page.getByText(/^(估算|含估算|未核查)$/)).toHaveCount(0);
});

for (const direction of ["editorial", "graphite", "paper"] as const) {
  test(`dark mode works in the ${direction} direction`, async ({ page }) => {
    await page.setViewportSize({ width: 1200, height: 900 });
    const errors = await open(page, { theme: "dark", direction });
    await settle(page, "总览");
    const root = page.locator("html");
    await expect(root).toHaveClass(/\bdark\b/);
    await expect(root).toHaveAttribute("data-direction", direction);
    const fontFamilies = await page.evaluate(() => {
      const title = document.querySelector("h1");
      if (!title) throw new Error("The page title is missing");
      return { title: getComputedStyle(title).fontFamily, body: getComputedStyle(document.body).fontFamily };
    });
    if (direction === "paper") {
      expect(fontFamilies.title).toContain("Songti SC");
      expect(fontFamilies.title).not.toBe(fontFamilies.body);
    } else {
      expect(fontFamilies.title).toBe(fontFamilies.body);
    }
    const background = await page.evaluate(() => getComputedStyle(document.body).backgroundColor);
    expect(luminance(background)).toBeLessThan(0.25);
    expect(errors).toEqual([]);
  });
}

test("directions change typography, density and surfaces, not just colours", async ({ page, browser }) => {
  await page.close();
  for (const direction of ["editorial", "graphite", "paper"] as const) {
    page = await browser.newPage({ viewport: { width: 1200, height: 900 } });
    const errors = await open(page, { direction });
    await settle(page, "总览");
    const expectedPadding = direction === "graphite" ? "16px" : "20px";
    await expect.poll(() => page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--card-px").trim())).toBe(expectedPadding);
    const style = await page.evaluate(() => {
      const root = getComputedStyle(document.documentElement);
      const card = getComputedStyle(document.querySelector("#overview-kpis, [data-testid='overview-kpis'] section")!);
      const heading = getComputedStyle(document.querySelector("h1")!);
      const numeric = getComputedStyle(document.querySelector(".display.num-fit")!);
      return {
        padding: root.getPropertyValue("--card-px").trim(),
        radius: root.getPropertyValue("--radius-card").trim(),
        shadow: card.boxShadow,
        border: card.borderBottomWidth,
        title: heading.fontFamily,
        numeric: numeric.fontFamily,
      };
    });
    if (direction === "editorial") {
      expect(style.padding).toBe("20px");
      expect(style.radius).toBe("14px");
      expect(style.shadow).not.toBe("none");
      expect(style.border).toBe("1px");
    } else if (direction === "graphite") {
      expect(style.padding).toBe("16px");
      expect(style.radius).toBe("6px");
      expect(style.shadow).toBe("none");
      expect(style.numeric).toContain("Mono");
    } else {
      expect(style.radius).toBe("2px");
      expect(style.shadow).toBe("none");
      expect(style.border).toBe("1px");
      expect(style.title).toContain("Songti SC");
      expect(style.title).not.toBe(style.numeric);
    }
    expect(errors).toEqual([]);
    await page.close();
  }
});

test("direction previews mirror the real cards and preserve their own miniature fonts", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await open(page, { view: "settings", direction: "paper" });
  await settle(page, "设置");
  await expect(page.getByText("改变字体、密度、卡片和图表样式；数据、功能和操作方式保持一致。", { exact: true })).toBeVisible();
  const previews = page.locator("[data-direction-preview]");
  await expect(previews).toHaveCount(3);
  await expect(page.locator("[data-direction-preview='graphite'] svg rect")).toHaveCount(6);
  await expect(page.locator("[data-direction-preview='paper'] svg path[stroke='var(--primary)']")).toHaveAttribute("stroke-width", "1.2");
  const radii = await previews.evaluateAll((nodes) => nodes.map((node) => getComputedStyle(node).borderRadius));
  expect(radii).toEqual(["14px", "6px", "2px"]);
  const paperTitle = previews.nth(2).locator(".display");
  await expect(paperTitle).toHaveCSS("font-size", "10px");
  expect(await paperTitle.evaluate((node) => getComputedStyle(node).fontFamily)).toContain("Songti SC");
  await page.getByRole("radiogroup", { name: "主题" }).getByRole("radio", { name: "深色" }).click();
  await expect(previews.nth(0)).toHaveClass(/\bdark\b/);
  await expect(previews.nth(1)).toHaveClass(/\bdark\b/);
  await expect(previews.nth(2)).toHaveClass(/\bdark\b/);
  expect(errors).toEqual([]);
});

test("chart geometry follows its direction; the long-range boundary has a unit test", async ({ page, browser }) => {
  await page.close();
  for (const direction of ["editorial", "paper", "graphite"] as const) {
    page = await browser.newPage({ viewport: { width: 1200, height: 900 } });
    const errors = await open(page, { direction });
    await settle(page, "总览");
    const chart = page.getByRole("img", { name: /用量趋势/ });
    await expect(chart).toHaveAttribute("data-chart-direction", direction);
    const series = await chart.locator("canvas").evaluate(async (canvas) => {
      const url = performance.getEntriesByType("resource").map((entry) => entry.name)
        .find((entry) => new URL(entry).pathname === "/node_modules/.vite/deps/echarts_core.js");
      if (!url) throw new Error("ECharts module is missing");
      const { getInstanceByDom } = await import(url);
      const instance = getInstanceByDom(canvas.parentElement!.parentElement!);
      if (!instance) throw new Error("ECharts instance is missing");
      const option = instance.getOption() as { series: { type: string; lineStyle?: { width?: number }; areaStyle?: { opacity?: number } }[] };
      return option.series.map(({ type, lineStyle, areaStyle }) => ({ type, width: lineStyle?.width, area: areaStyle?.opacity }));
    });
    expect(series).toHaveLength(1);
    if (direction === "graphite") expect(series[0]!.type).toBe("bar");
    else expect(series[0]!.type).toBe("line");
    if (direction === "paper") {
      expect(series[0]!.width).toBe(1.25);
      expect(series[0]!.area ?? 0).toBe(0);
    }
    expect(errors).toEqual([]);
    await page.close();
  }
});

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

test("a Qoder session keeps credits without a precision badge", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  await open(page, { view: "sessions" });
  await settle(page, "会话");
  await page.locator("tbody tr").filter({ hasText: "Qoder" }).first().click();
  const drawer = page.getByRole("dialog");
  await expect(drawer.locator(".skeleton")).toHaveCount(0);
  await expect(drawer.getByText(/^(估算|含估算|未核查)$/)).toHaveCount(0);
  await expect(drawer.getByText(/Qoder credits/)).toBeVisible();
});

test("offline state explains the problem without crashing", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await open(page, { query: "?offline" });
  await expect(page.getByText("连不上服务器").first()).toBeVisible();
  expect(errors.filter((error) => !error.includes("network"))).toEqual([]);
});

/** Signs in through the connect dialog with the browser mock backend. */
async function signIn(dialog: import("@playwright/test").Locator, email = "user@example.com") {
  await dialog.getByRole("button", { name: "使用邮箱继续", exact: true }).click();
  await dialog.getByLabel("邮箱").fill(email);
  await dialog.getByRole("button", { name: "获取验证码", exact: true }).click();
  await dialog.getByLabel("验证码").fill("123456");
  await dialog.getByRole("button", { name: "登录并连接", exact: true }).click();
}

/** The cloud plan page owns sign-in; Add Device remains explanatory only. */
async function openCloudLogin(page: Page, query = "") {
  const errors = await open(page, { view: "cloud", query });
  await settle(page, "云端");
  await page.getByRole("button", { name: "免费使用云服务", exact: true }).click();
  return errors;
}

test("signing in never leaves the code or a session in web storage or the URL", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  await openCloudLogin(page);
  await signIn(page.getByRole("dialog"));
  await settle(page, "云端");
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
  expect(everything).not.toContain("123456");
  expect(everything).not.toMatch(/tls_|bearer|secret|tdi_|tdr_/i);
  expect(everything).not.toMatch(/[A-Za-z0-9_-]{32,}/);
  expect(stored.cookies).toBe("");
});


test("a clipboard failure is visible and the login code stays selectable", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  await page.addInitScript(() => {
    Object.defineProperty(Navigator.prototype, "clipboard", {
      value: { writeText: () => Promise.reject(new Error("clipboard denied")) },
      configurable: true,
    });
  });
  await openCloudLogin(page);
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  expect(await page.evaluate(() => typeof navigator.clipboard?.writeText)).toBe("function");
  await dialog.getByRole("button", { name: "使用 GitHub 继续", exact: true }).click();
  await dialog.getByRole("button", { name: "继续前往 GitHub", exact: true }).click();
  await expect(dialog.getByText("ABCD-1234", { exact: true })).toBeVisible();
  const copy = dialog.getByRole("button", { name: "复制 GitHub 授权码", exact: true });
  await copy.click();
  await expect(dialog.getByRole("status")).toHaveText("复制失败，请选中上方的授权码手动复制。");
  await expect(dialog.getByText("ABCD-1234", { exact: true })).toBeVisible();
});

test("the code input and the request button sit on the same aligned row", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  await openCloudLogin(page);
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("button", { name: "使用邮箱继续", exact: true }).click();
  const input = await dialog.getByLabel("验证码", { exact: true }).boundingBox();
  const button = await dialog.getByRole("button", { name: "获取验证码", exact: true }).boundingBox();
  if (!input || !button) throw new Error("code row missing");
  // The input and the button share a top edge and a height.
  expect(Math.abs(input.y - button.y)).toBeLessThanOrEqual(1);
  expect(Math.abs(input.height - button.height)).toBeLessThanOrEqual(1);
  // The button sits to the right of the input, with no overlap.
  expect(button.x).toBeGreaterThan(input.x + input.width - 1);
  // The hint lives below the row, not inside it.
  const hint = await dialog.getByText("6 位数字，10 分钟内有效", { exact: true }).boundingBox();
  if (!hint) throw new Error("hint missing");
  expect(hint.y).toBeGreaterThan(input.y + input.height);
});

test("a self-hosted server offers only the email code, never GitHub or Google", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  await openCloudLogin(page);
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("button", { name: "使用 GitHub 继续", exact: true })).toBeVisible();
  await expect(dialog.getByRole("button", { name: "使用 Google 继续", exact: true })).toBeVisible();
  await expect(dialog.getByText("默认连接 TokenLens 云服务，首次登录自动创建空间", { exact: true })).toBeVisible();
  await dialog.getByRole("button", { name: "使用自己的服务器", exact: true }).click();
  await expect(dialog.getByRole("button", { name: "使用 GitHub 继续", exact: true })).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: "使用 Google 继续", exact: true })).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: "继续前往 GitHub" })).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: "继续前往 Google" })).toHaveCount(0);
  await expect(dialog.getByLabel("服务器地址")).toBeVisible();
  await expect(dialog.getByLabel("邮箱")).toBeVisible();
  await expect(dialog.getByLabel("验证码")).toBeVisible();
  // Switching back restores both provider choices for the hosted service.
  await dialog.getByRole("button", { name: "返回登录方式", exact: true }).click();
  await expect(dialog.getByRole("button", { name: "使用 GitHub 继续", exact: true })).toBeVisible();
  await expect(dialog.getByRole("button", { name: "使用 Google 继续", exact: true })).toBeVisible();
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
});

test("GitHub sign-in shows a device code and finishes without any email", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await openCloudLogin(page);
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("button", { name: "使用 GitHub 继续", exact: true })).toBeVisible();
  await dialog.getByRole("button", { name: "使用 GitHub 继续", exact: true }).click();
  await dialog.getByRole("button", { name: "继续前往 GitHub", exact: true }).click();
  await expect(dialog.getByText("ABCD-1234", { exact: true })).toBeVisible();
  await expect(dialog.getByText("等待授权…", { exact: true })).toBeVisible();
  if (test.info().project.name === "chrome") {
    await page.context().grantPermissions(["clipboard-read", "clipboard-write"]);
    await dialog.getByRole("button", { name: "复制 GitHub 授权码", exact: true }).click();
    await expect(dialog.getByRole("status").filter({ hasText: "已复制，去 GitHub 粘贴。" })).toBeVisible();
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe("ABCD-1234");
  }
  // The mock approves on the second poll, so the dialog closes by itself.
  await expect(dialog).toBeHidden({ timeout: 15_000 });
  await settle(page, "云端");
  await expect(page.getByRole("group", { name: "查看范围", exact: true })).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("GitHub sign-in shows the server failure reason instead of silently returning", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await openCloudLogin(page, "?github-failure");
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("button", { name: "使用 GitHub 继续", exact: true }).click();
  await dialog.getByRole("button", { name: "继续前往 GitHub", exact: true }).click();
  // Current servers honor GitHub's device-flow interval before the first poll.
  await expect(dialog.getByRole("alert")).toContainText("没有允许读取邮箱", { timeout: 12_000 });
  await expect(dialog.getByRole("button", { name: "继续前往 GitHub", exact: true })).toBeVisible();
  await expect(dialog).toBeVisible();
  expect(errors).toEqual([]);
});

test("GitHub poll errors leave the waiting state and can be retried", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await openCloudLogin(page, "?github-poll-error");
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("button", { name: "使用 GitHub 继续", exact: true }).click();
  await dialog.getByRole("button", { name: "继续前往 GitHub", exact: true }).click();
  await expect(dialog.getByRole("alert").filter({ hasText: "另一个账号的空间" })).toBeVisible({ timeout: 12_000 });
  await expect(dialog.getByText("等待授权…", { exact: true })).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: "继续前往 GitHub", exact: true })).toBeVisible();
  expect(errors).toEqual([]);
});

test("Google sign-in finishes in the browser without any code or email", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await openCloudLogin(page);
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("button", { name: "使用 Google 继续", exact: true }).click();
  await dialog.getByRole("button", { name: "继续前往 Google", exact: true }).click();
  // Unlike GitHub there is no human code: the browser does the whole consent.
  await expect(dialog.getByText("在打开的 Google 页面完成登录，授权后这里会自动继续。", { exact: true })).toBeVisible();
  await expect(dialog.getByText("等待授权…", { exact: true })).toBeVisible();
  await expect(dialog.getByRole("button", { name: "重新打开授权页", exact: true })).toBeVisible();
  // The mock approves on the second poll, so the dialog closes by itself.
  await expect(dialog).toBeHidden({ timeout: 15_000 });
  await settle(page, "云端");
  const account = page.getByRole("group", { name: "Google 账号 G User", exact: true });
  await expect(account).toBeVisible();
  await expect(account).toContainText("G User");
  await expect(page.getByRole("group", { name: "查看范围", exact: true })).toHaveCount(0);
  // The session and profile never reach web storage or the URL.
  const stored = await page.evaluate(() => JSON.stringify({
    local: { ...localStorage }, session: { ...sessionStorage }, url: location.href, cookies: document.cookie,
  }));
  expect(stored).not.toMatch(/tls_|bearer|tdi_|tdr_/i);
  expect(errors).toEqual([]);
});

test("Google browser failures stay visible and retry preserves the sign-in flow", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await openCloudLogin(page, "?google-open-fail=2");
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("button", { name: "使用 Google 继续", exact: true }).click();
  await dialog.getByRole("button", { name: "继续前往 Google", exact: true }).click();
  const failure = dialog.getByRole("alert").filter({ hasText: "未能打开浏览器，请重试或复制链接到浏览器打开。" });
  await expect(failure).toBeVisible();
  await expect(dialog.getByText("等待授权…", { exact: true })).toBeVisible();
  await expect(dialog.getByRole("button", { name: "复制授权链接", exact: true })).toBeVisible();
  const retry = dialog.getByRole("button", { name: "重新打开授权页", exact: true });
  await retry.click();
  await expect(failure).toBeVisible();
  await retry.click();
  await expect(retry).toBeEnabled();
  await expect(failure).toHaveCount(0);
  // The original flow still polls and approves; opening again did not restart it.
  await expect(dialog).toBeHidden({ timeout: 15_000 });
  await expect(page.getByRole("group", { name: "Google 账号 G User", exact: true })).toBeVisible();
  expect(errors).toEqual([]);
});

test("Google authorization links show copy failures and copy the complete URL on retry", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  await page.addInitScript(() => {
    let attempts = 0;
    let copied = "";
    Object.defineProperty(Navigator.prototype, "clipboard", {
      value: {
        writeText: (text: string) => {
          if (++attempts === 1) return Promise.reject(new Error("clipboard denied"));
          copied = text;
          return Promise.resolve();
        },
        readText: () => Promise.resolve(copied),
      },
      configurable: true,
    });
  });
  const errors = await openCloudLogin(page, "?google-open-fail=1&google-pending");
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("button", { name: "使用 Google 继续", exact: true }).click();
  await dialog.getByRole("button", { name: "继续前往 Google", exact: true }).click();
  const copy = dialog.getByRole("button", { name: "复制授权链接", exact: true });
  await copy.click();
  await expect(dialog.getByRole("status").filter({ hasText: "复制失败，请重试。" })).toBeVisible();
  await copy.click();
  await expect(dialog.getByRole("status").filter({ hasText: "已复制授权链接，请粘贴到浏览器打开。" })).toBeVisible();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(
    "https://accounts.google.com/o/oauth2/v2/auth?client_id=fixture&response_type=code&scope=openid+email+profile&state=fixture%26state",
  );
  const stored = await page.evaluate(() => JSON.stringify({
    local: { ...localStorage }, session: { ...sessionStorage }, url: location.href,
  }));
  expect(stored).not.toContain("accounts.google.com");
  await expect(dialog.getByText("等待授权…", { exact: true })).toBeVisible();
  expect(errors).toEqual([]);
});

test("Google ignores a slow browser failure after cancelling and starting a new sign-in", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await openCloudLogin(page, "?google-pending&google-open-slow=2");
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("button", { name: "使用 Google 继续", exact: true }).click();
  await dialog.getByRole("button", { name: "继续前往 Google", exact: true }).click();
  await dialog.getByRole("button", { name: "重新打开授权页", exact: true }).click();
  await expect(dialog.getByRole("button", { name: "正在打开…", exact: true })).toBeDisabled();
  await dialog.getByRole("button", { name: "取消", exact: true }).click();
  await dialog.getByRole("button", { name: "继续前往 Google", exact: true }).click();
  const retry = dialog.getByRole("button", { name: "重新打开授权页", exact: true });
  await expect(retry).toBeEnabled();
  // The cancelled opener rejects after 1.5 seconds; its result belongs to the old flow.
  await page.waitForTimeout(1800);
  await expect(dialog.getByRole("alert")).toHaveCount(0);
  await expect(retry).toBeEnabled();
  await expect(dialog.getByText("等待授权…", { exact: true })).toBeVisible();
  expect(errors).toEqual([]);
});

test("Google ignores a slow clipboard result after cancelling and starting a new sign-in", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  await page.addInitScript(() => {
    let attempts = 0;
    Object.defineProperty(Navigator.prototype, "clipboard", {
      value: {
        writeText: () => ++attempts === 1
          ? new Promise<void>((resolve) => setTimeout(resolve, 1500))
          : Promise.reject(new Error("clipboard denied")),
      },
      configurable: true,
    });
  });
  const errors = await openCloudLogin(page, "?google-pending");
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("button", { name: "使用 Google 继续", exact: true }).click();
  await dialog.getByRole("button", { name: "继续前往 Google", exact: true }).click();
  const copy = dialog.getByRole("button", { name: "复制授权链接", exact: true });
  await copy.click();
  await expect(copy).toBeDisabled();
  await dialog.getByRole("button", { name: "取消", exact: true }).click();
  await dialog.getByRole("button", { name: "继续前往 Google", exact: true }).click();
  await copy.click();
  const failure = dialog.getByRole("status").filter({ hasText: "复制失败，请重试。" });
  await expect(failure).toBeVisible();
  await page.waitForTimeout(1800);
  await expect(failure).toBeVisible();
  await expect(dialog.getByText("已复制授权链接，请粘贴到浏览器打开。", { exact: true })).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("local and cloud plans are both free, with the cloud plan highlighted", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await open(page, { view: "cloud", query: "" });
  await settle(page, "云端");
  const local = page.locator('[data-plan="local"]');
  const cloud = page.locator('[data-plan="cloud"]');
  await expect(local).toContainText("免费本地版");
  await expect(local).toContainText("完整本地功能，目前免费。");
  await expect(local).toContainText("当前使用");
  await expect(cloud).toContainText("TokenLens 云服务");
  await expect(cloud).toContainText("完整云端功能，目前免费。");
  await expect(cloud).toContainText("推荐");
  await expect(cloud).toContainText("不限设备数量，跨设备汇总同步");
  await expect(page.getByRole("button", { name: "免费使用云服务", exact: true })).toBeVisible();
  const accent = await cloud.evaluate((card) => getComputedStyle(card).borderColor);
  const neutral = await local.evaluate((card) => getComputedStyle(card).borderColor);
  expect(accent).not.toBe(neutral);
  await page.getByRole("button", { name: "免费使用云服务", exact: true }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
  expect(errors).toEqual([]);
});

test("a connected cloud plan links to device management", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 900 });
  const errors = await open(page, { view: "cloud", query: "?server" });
  await settle(page, "云端");
  await expect(page.locator('[data-plan="local"]')).toContainText("本机保留");
  await expect(page.locator('[data-plan="cloud"]')).toContainText("已连接");
  await page.getByRole("button", { name: "管理我的设备", exact: true }).click();
  await settle(page, "设备");
  await expect(page.getByRole("heading", { name: "Mac mini", exact: true })).toBeVisible();
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
  expect(errors).toEqual([]);
});

test("the signed-out brand opens login and the signed-in account row has breathing room", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await open(page, { query: "" });
  await settle(page, "总览");
  await page.getByRole("button", { name: "登录 TokenLens", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await signIn(dialog);
  await expect(dialog).toBeHidden();
  await expect(page.getByRole("button", { name: "登录 TokenLens", exact: true })).toHaveCount(0);
  const spacing = await page.getByRole("group", { name: "账号 user@example.com", exact: true }).evaluate((row) => {
    const avatar = row.querySelector("[data-account-identity] > img, [data-account-identity] > svg");
    const name = row.querySelector("[data-account-name]");
    const logout = row.querySelector("button[aria-label='退出登录']");
    if (!avatar || !name || !logout) throw new Error("Account row pieces are missing");
    const a = avatar.getBoundingClientRect();
    const n = name.getBoundingClientRect();
    const b = logout.getBoundingClientRect();
    const r = row.getBoundingClientRect();
    const nav = row.closest("nav")?.getBoundingClientRect();
    if (!nav) throw new Error("Sidebar is missing");
    return { avatarToName: n.left - a.right, nameToLogout: b.left - n.right, rightEdge: nav.right - b.right, width: n.width };
  });
  expect(spacing.avatarToName).toBeGreaterThanOrEqual(11);
  expect(spacing.nameToLogout).toBeGreaterThanOrEqual(11);
  expect(spacing.width).toBeGreaterThan(80);
  expect(spacing.rightEdge).toBeLessThanOrEqual(22);
  expect(errors).toEqual([]);
});

test("the sidebar shows one sign-in entry, then the GitHub username and avatar", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const avatar = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='40' height='40'%3E%3Ccircle cx='20' cy='20' r='20' fill='%232563eb'/%3E%3C/svg%3E";
  await page.addInitScript((url) => {
    const existing = localStorage.getItem("tokenlens.settings.v1");
    sessionStorage.setItem("tokenlens.mock.avatar", url);
    if (!existing) localStorage.setItem("tokenlens.settings.v1", JSON.stringify({ theme: "light", direction: "editorial", refreshSeconds: 0 }));
  }, avatar);
  await open(page, { view: "devices", query: "" });
  await settle(page, "设备");
  const entry = page.getByRole("button", { name: "登录，同步多台设备", exact: true });
  await expect(entry).toBeVisible();
  // The sign-in entry belongs directly above Settings, not below the
  // collector status at the very bottom of the sidebar.
  const aboveSettings = await page.evaluate(() => {
    const settings = document.querySelector('nav[aria-label="主导航"] button[aria-label="设置"]');
    const login = document.querySelector('nav[aria-label="主导航"] button[aria-label="登录，同步多台设备"]');
    if (!settings || !login) return false;
    const a = login.getBoundingClientRect();
    const b = settings.getBoundingClientRect();
    return a.bottom <= b.top && b.top - a.bottom < 16;
  });
  expect(aboveSettings).toBe(true);
  await entry.click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("button", { name: "使用 GitHub 继续", exact: true }).click();
  await dialog.getByRole("button", { name: "继续前往 GitHub", exact: true }).click();
  await expect(dialog).toBeHidden({ timeout: 15_000 });
  const account = page.getByRole("group", { name: "GitHub 账号 octocat", exact: true });
  await expect(account).toBeVisible();
  await expect(account.locator("img")).toHaveAttribute("src", avatar);
  await expect(account).toContainText("octocat");
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设置", exact: true }).click();
  await settle(page, "设置");
  await expect(page.getByText("用邮箱登录你的空间；第一次登录会自动创建", { exact: true })).toHaveCount(0);
});

test("email sign-in keeps an account entry and sign-out preserves device collection", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await open(page, { query: "" });
  await settle(page, "总览");
  const signIn = page.getByRole("button", { name: "登录，同步多台设备", exact: true });
  await signIn.click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("button", { name: "使用邮箱继续", exact: true }).click();
  await dialog.getByLabel("邮箱", { exact: true }).fill("member@example.com");
  await dialog.getByLabel("验证码", { exact: true }).fill("123456");
  await dialog.getByRole("button", { name: "登录并连接", exact: true }).click();
  await expect(dialog).toBeHidden();
  const account = page.getByRole("group", { name: "账号 member@example.com", exact: true });
  await expect(account).toBeVisible();
  await expect(account).toContainText("member@example.com");
  await expect(signIn).toHaveCount(0);
  await expect(page.getByText(/^GitHub 账号/)).toHaveCount(0);
  await page.getByRole("button", { name: "退出登录", exact: true }).click();
  await page.getByRole("dialog").getByRole("button", { name: "退出登录", exact: true }).click();
  await expect(signIn).toBeVisible();
  await expect(account).toHaveCount(0);
  await expect(page.getByText("全部设备", { exact: true })).toHaveCount(0);
  await expect(page.getByText("后台采集中", { exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Tokens", exact: true })).toBeVisible();
  expect(errors).toEqual([]);
});

test("signing out shows only this computer; signing back in reads the cloud's current devices", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await open(page, { query: "" });
  await settle(page, "总览");
  await page.getByRole("button", { name: "登录，同步多台设备", exact: true }).click();
  let dialog = page.getByRole("dialog");
  await signIn(dialog);
  await expect(dialog).toBeHidden();
  const deviceId = await page.evaluate(async () => {
    const modulePath = performance.getEntriesByType("resource")
      .map((entry) => entry.name)
      .find((url) => new URL(url).pathname === "/src/lib/api.ts");
    if (!modulePath) throw new Error("The app API module was not loaded");
    if (!modulePath) throw new Error("The app API module was not loaded");
    const { app } = await import(modulePath);
    return (await app.status()).deviceId;
  });
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设备", exact: true }).click();
  await settle(page, "设备");
  await expect(page.getByRole("heading", { name: /MacBook Pro/ })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Mac mini", exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Windows 台式机", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "筛选", exact: true }).click();
  await page.getByRole("checkbox", { name: /Windows 台式机/ }).check();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("group", { name: "生效的筛选" })).toContainText("Windows 台式机");
  await page.getByRole("button", { name: "退出登录", exact: true }).click();
  await page.getByRole("dialog").getByRole("button", { name: "退出登录", exact: true }).click();
  await settle(page, "设备");
  await expect(page.getByRole("heading", { name: /MacBook Pro/ })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Mac mini", exact: true })).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "Windows 台式机", exact: true })).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "旧笔记本", exact: true })).toHaveCount(0);
  await expect(page.getByRole("group", { name: "生效的筛选" })).toHaveCount(0);
  await expect(page.getByText("后台采集中", { exact: true })).toBeVisible();
  // Other computers keep managing the cloud space while this one is signed
  // out. Reconnecting must not resurrect this browser's old device snapshot.
  await page.evaluate(async () => {
    const modulePath = performance.getEntriesByType("resource")
      .map((entry) => entry.name)
      .find((url) => new URL(url).pathname === "/src/lib/api.ts");
    if (!modulePath) throw new Error("The app API module was not loaded");
    const { app } = await import(modulePath);
    // These are cloud-space mutations by another computer, not local deletes.
    await app.revokeDevice("dev_mock_mini");
    await app.renameDevice("dev_mock_win", "改名后的 Windows");
  });
  await page.getByRole("button", { name: "登录，同步多台设备", exact: true }).click();
  dialog = page.getByRole("dialog");
  await signIn(dialog);
  await expect(dialog).toBeHidden();
  await settle(page, "设备");
  await expect(page.getByRole("heading", { name: /MacBook Pro/ })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Mac mini", exact: true })).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "Windows 台式机", exact: true })).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "改名后的 Windows", exact: true })).toBeVisible();
  const again = await page.evaluate(async () => {
    const modulePath = performance.getEntriesByType("resource")
      .map((entry) => entry.name)
      .find((url) => new URL(url).pathname === "/src/lib/api.ts");
    if (!modulePath) throw new Error("The app API module was not loaded");
    const { app } = await import(modulePath);
    return (await app.status()).deviceId;
  });
  expect(again).toBe(deviceId);
  await expect(page.getByText("后台采集中", { exact: true })).toBeVisible();
  await expect(page.getByText("全部设备", { exact: true })).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("clicking the account name never signs out; log-out asks for confirmation", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 800 });
  const errors = await open(page, { query: "" });
  await settle(page, "总览");
  await page.getByRole("button", { name: "登录，同步多台设备", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await signIn(dialog);
  await expect(dialog).toBeHidden();
  const account = page.getByRole("group", { name: "账号 user@example.com", exact: true });
  await expect(account).toBeVisible();
  // Clicking the avatar or the name must neither sign out nor open a dialog.
  await account.getByText("user@example.com", { exact: true }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(account).toBeVisible();
  // Only the small log-out button starts the flow, and it asks first.
  await page.getByRole("button", { name: "退出登录", exact: true }).click();
  const confirm = page.getByRole("dialog");
  await expect(confirm).toBeVisible();
  await expect(confirm).toContainText("退出后，这台电脑会继续采集用量");
  await confirm.getByRole("button", { name: "取消", exact: true }).click();
  await expect(confirm).toBeHidden();
  await expect(account).toBeVisible();
  // Confirming signs out; collection keeps running.
  await page.getByRole("button", { name: "退出登录", exact: true }).click();
  await page.getByRole("dialog").getByRole("button", { name: "退出登录", exact: true }).click();
  await expect(page.getByRole("button", { name: "登录，同步多台设备", exact: true })).toBeVisible();
  await expect(account).toHaveCount(0);
  await expect(page.getByText("后台采集中", { exact: true })).toBeVisible();
  expect(errors).toEqual([]);
});

test("GitHub sign-in against an older server stays signed in without profile fields", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 800 });
  const errors = await open(page, { query: "" });
  await settle(page, "总览");
  // Model the Rust response from a v8 server: sign-in works, but its optional
  // public profile fields are absent. Keep periodic status refreshes consistent.
  await page.evaluate(async () => {
    // Use the module URL already loaded by Vite, including its HMR version.
    const modulePath = performance.getEntriesByType("resource")
      .map((entry) => entry.name)
      .find((url) => new URL(url).pathname === "/src/lib/api.ts");
    if (!modulePath) throw new Error("The app API module was not loaded");
    const { app } = await import(modulePath);
    const status = app.status;
    const poll = app.pollGitHubLogin;
    const withoutProfile = (value: Record<string, unknown>) => ({ ...value, accountProvider: null, accountDisplayName: null, accountAvatarUrl: null });
    app.status = async () => withoutProfile(await status());
    app.pollGitHubLogin = async (input: Record<string, unknown>) => {
      const result = await poll(input);
      return result.app ? { ...result, app: withoutProfile(result.app) } : result;
    };
  });
  const signIn = page.getByRole("button", { name: "登录，同步多台设备", exact: true });
  await signIn.click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("button", { name: "使用 GitHub 继续", exact: true }).click();
  await dialog.getByRole("button", { name: "继续前往 GitHub", exact: true }).click();
  await expect(dialog).toBeHidden({ timeout: 15_000 });
  const account = page.getByRole("group", { name: "账号 octo@example.com", exact: true });
  await expect(account).toBeVisible();
  await expect(signIn).toHaveCount(0);
  // The collapsed sidebar still exposes a named, usable sign-out control.
  await page.getByRole("button", { name: "退出登录", exact: true }).click();
  await page.getByRole("dialog").getByRole("button", { name: "退出登录", exact: true }).click();
  await expect(signIn).toBeVisible();
  await expect(page.getByText("全部设备", { exact: true })).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("first run opens local data directly without asking for a server or a mode", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 800 });
  const errors = await open(page, { query: "" });
  await settle(page, "总览");
  await expect(page.getByText("本机数据", { exact: true })).toHaveCount(0);
  await expect(page.getByLabel("服务器地址")).toHaveCount(0);
  await expect(page.getByLabel("配对码")).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "开始使用 TokenLens" })).toHaveCount(0);
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设备", exact: true }).click();
  await settle(page, "设备");
  await expect(page.getByRole("button", { name: "添加设备", exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: /MacBook Pro/ })).toBeVisible();
  await expect(page.getByRole("heading", { name: /Mac mini|Windows 台式机|旧笔记本/ })).toHaveCount(0);
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
  expect(errors).toEqual([]);
});

test("a failed or cancelled sign-in keeps local collection and data available", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 800 });
  const errors = await openCloudLogin(page, "?pairfail");
  const dialog = page.getByRole("dialog");
  await signIn(dialog);
  await expect(dialog.getByRole("alert")).toContainText("连不上服务器");
  await dialog.getByRole("button", { name: "关闭对话框", exact: true }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByText("本机数据", { exact: true })).toHaveCount(0);
  await expect(page.getByText("后台采集中", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "查看本机设备", exact: true }).click();
  await expect(page.getByRole("heading", { name: /MacBook Pro/ })).toBeVisible();
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "云端", exact: true }).click();
  await page.getByRole("button", { name: "免费使用云服务", exact: true }).click();
  await dialog.getByRole("button", { name: "使用邮箱继续", exact: true }).click();
  await expect(dialog.getByLabel("验证码")).toHaveValue("");
  await expect(dialog.getByLabel("邮箱")).toHaveValue("");
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
  expect(errors).toEqual([]);
});

test("adding a device never requests a pair code, even with an existing cloud account", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await open(page, { view: "devices", query: "?server" });
  await settle(page, "设备");
  await page.evaluate(async () => {
    sessionStorage.setItem("tokenlens.test.pair-code-reads", "0");
    const modulePath = performance.getEntriesByType("resource").map((entry) => entry.name)
      .find((url) => new URL(url).pathname === "/src/lib/api.ts");
    if (!modulePath) throw new Error("The app API module was not loaded");
    const { app } = await import(modulePath);
    app.accountPairCode = async () => {
      const reads = Number(sessionStorage.getItem("tokenlens.test.pair-code-reads")) + 1;
      sessionStorage.setItem("tokenlens.test.pair-code-reads", String(reads));
      throw new Error("The explanatory Add Device dialog must not request a pair code");
    };
  });
  await page.getByRole("button", { name: "添加设备", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("在另一台电脑上安装并打开 TokenLens。");
  await expect(dialog).toContainText("GitHub、Google 或邮箱账号");
  await expect(dialog.getByLabel("服务器地址")).toHaveCount(0);
  await expect(dialog.getByLabel("配对码")).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: "复制配对码" })).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: "换一个配对码" })).toHaveCount(0);
  expect(await page.evaluate(() => Number(sessionStorage.getItem("tokenlens.test.pair-code-reads")))).toBe(0);
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
  expect(errors).toEqual([]);
});

test("cloud sign-in shows every device; adding a device is instructions only", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 900 });
  const errors = await openCloudLogin(page);
  const dialog = page.getByRole("dialog");
  await signIn(dialog);
  await expect(dialog).toBeHidden();
  await settle(page, "云端");
  await expect(page.getByRole("heading", { name: "TokenLens 云服务", exact: true })).toBeVisible();
  await expect(page.getByText("已连接", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "管理我的设备", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "管理我的设备", exact: true }).click();
  await settle(page, "设备");
  await expect(page.getByRole("group", { name: "查看范围", exact: true })).toHaveCount(0);
  await expect(page.getByRole("heading", { name: /MacBook Pro/ })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Mac mini", exact: true })).toBeVisible();
  await expect(page.getByText("全部设备", { exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "筛选", exact: true }).click();
  await page.getByRole("checkbox", { name: /Windows 台式机/ }).click();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("group", { name: "生效的筛选" })).toContainText("Windows 台式机");
  await page.getByRole("button", { name: "添加设备", exact: true }).click();
  await expect(dialog).toContainText("在另一台电脑上安装并打开 TokenLens。");
  await expect(dialog).toContainText("使用与这台电脑相同的账号登录。");
  await expect(dialog).toContainText("GitHub、Google 或邮箱账号");
  await expect(dialog.getByLabel("服务器地址")).toHaveCount(0);
  await expect(dialog.getByLabel("配对码")).toHaveCount(0);
  await expect(dialog.getByText("K7QM-4XPR", { exact: true })).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: "复制配对码" })).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: "换一个配对码" })).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: "完成", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
  expect(errors).toEqual([]);
});

test("the device menu explains credentials and keeps removal in the cloud view", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await open(page, { view: "devices", query: "?server" });
  await settle(page, "设备");
  await page.getByRole("button", { name: "MacBook Pro 的操作", exact: true }).click();
  await page.getByRole("button", { name: "更换这台电脑的凭据", exact: true }).click();
  const credentials = page.getByRole("dialog");
  await expect(credentials).toContainText("身份钥匙");
  await expect(credentials).toContainText("不是账号密码");
  await credentials.getByRole("button", { name: "取消", exact: true }).click();
  await page.getByRole("button", { name: "Mac mini 的操作", exact: true }).click();
  await expect(page.getByRole("button", { name: "重命名", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "更改设备性质", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "更换这台电脑的凭据", exact: true })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "移除设备", exact: true })).toBeVisible();
  expect(errors).toEqual([]);
});

test("removing the current cloud device preserves the local device and its history", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await open(page, { view: "devices", query: "?server" });
  await settle(page, "设备");
  await page.getByRole("button", { name: "MacBook Pro 的操作", exact: true }).click();
  await expect(page.getByRole("button", { name: "重命名", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "更改设备性质", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "更换这台电脑的凭据", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "移除设备", exact: true })).toBeVisible();
  // Removing the current cloud device preserves the old explicit local fallback.
  await page.getByRole("button", { name: "移除设备", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("本机记录继续运行，已上传的历史用量保留");
  await dialog.getByRole("button", { name: "移除", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  await expect(page.getByRole("heading", { name: /MacBook Pro/ })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Mac mini", exact: true })).toHaveCount(0);
  await expect(page.getByRole("group", { name: "查看范围" })).toHaveCount(0);
  await page.getByRole("button", { name: "MacBook Pro 的操作", exact: true }).click();
  await expect(page.getByRole("button", { name: "移除设备", exact: true })).toBeVisible();
  expect(errors).toEqual([]);
});

test("an unconnected local device can be removed with an explicit reset warning", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 900 });
  const errors = await open(page, { view: "devices", query: "" });
  await settle(page, "设备");
  await page.getByRole("button", { name: "MacBook Pro 的操作", exact: true }).click();
  await page.getByRole("button", { name: "移除设备", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("这台电脑及其记录将从本机看板移除，采集会暂停");
  await expect(dialog).toContainText("重新开始并恢复采集后，会扫描现有日志，历史用量可能再次出现");
  await expect(dialog).toContainText("已同步到其他设备或服务器的记录不受影响");
  await dialog.getByRole("button", { name: "移除", exact: true }).click();
  await expect(page.getByRole("heading", { name: "准备本机数据" })).toBeVisible();
  await page.getByRole("button", { name: "重新开始本机记录", exact: true }).click();
  await settle(page, "设备");
  await expect(page.getByRole("heading", { name: /MacBook Pro/ })).toBeVisible();
  expect(errors).toEqual([]);
});

test("disconnecting a server returns to local data without stopping collection", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await open(page, { view: "settings", query: "?server" });
  await settle(page, "设置");
  await page.getByRole("button", { name: "断开服务器", exact: true }).click();
  await page.getByRole("dialog").getByRole("button", { name: "断开服务器", exact: true }).click();
  await settle(page, "设置");
  await expect(page.getByRole("button", { name: "连接服务器", exact: true })).toBeVisible();
  await expect(page.getByText("本机数据", { exact: true })).toHaveCount(0);
  await expect(page.getByText("后台采集中", { exact: true }).first()).toBeVisible();
  expect(errors).toEqual([]);
});

test("server read failures keep Devices controls available without a misleading local fallback", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 800 });
  const errors = await open(page, { view: "devices", query: "?offline" });
  await expect(page.getByText("连不上服务器").first()).toBeVisible();
  await expect(page.getByRole("button", { name: "添加设备", exact: true })).toBeVisible();
  await expect(page.getByRole("group", { name: "查看范围", exact: true })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "重试", exact: true })).toBeVisible();
  // An offline cloud view must never silently substitute a different local
  // dashboard. Disconnecting explicitly returns to the preserved local copy.
  await page.getByRole("button", { name: "设置", exact: true }).click();
  await settle(page, "设置");
  await page.getByRole("button", { name: "断开服务器", exact: true }).click();
  await page.getByRole("dialog").getByRole("button", { name: "断开服务器", exact: true }).click();
  await page.getByRole("button", { name: "设备", exact: true }).click();
  await settle(page, "设备");
  await expect(page.getByRole("heading", { name: /MacBook Pro/ })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Mac mini", exact: true })).toHaveCount(0);
  await expect(page.getByText("连不上服务器")).toHaveCount(0);
  expect(errors).toEqual([]);
});

/** The first KPI shows tokens, without range labels or secondary notes. */
const tokensKpi = (page: Page) => page.getByTestId("overview-kpis").locator("section.card").first();

test("a drill-down shows its filter as a chip and is undone on the way back", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await open(page);
  await settle(page, "总览");
  const headline = tokensKpi(page).locator(".num-fit");
  const whole = await headline.innerText();
  const chips = page.getByRole("group", { name: "生效的筛选" });
  await expect(chips).toHaveCount(0);
  await page.getByRole("button", { name: /Claude Code/ }).first().click();
  await expect(page.getByRole("heading", { name: "会话", level: 1 })).toBeVisible();
  await expect(chips).toContainText("Claude Code");
  // Going back restores the unfiltered numbers.
  await page.getByRole("button", { name: "总览", exact: true }).click();
  await expect(chips).toHaveCount(0);
  await expect(headline).toHaveText(whole);
  // A filter the user picks stays, is visible on every page, and one click removes it.
  await page.getByRole("button", { name: /Claude Code/ }).first().click();
  await page.getByRole("button", { name: "筛选", exact: true }).click();
  await page.getByRole("checkbox", { name: /Codex/ }).click();
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "总览", exact: true }).click();
  await expect(chips).toContainText("Codex");
  await expect(headline).not.toHaveText(whole);
  await chips.getByRole("button", { name: "清除筛选" }).click();
  await expect(chips).toHaveCount(0);
  await expect(headline).toHaveText(whole);
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
  expect(errors).toEqual([]);
});

test("clicking a token count switches every count to exact digits and remembers it", async ({ page }) => {
  for (const width of [900, 1200]) {
    await page.setViewportSize({ width, height: 900 });
    const errors = await open(page);
    await settle(page, "总览");
    const kpi = tokensKpi(page).locator(".num-fit").getByRole("button", { pressed: false });
    await expect(kpi).toHaveText(/^[\d.]+[KMBT]$/);
    await kpi.click();
    const exactKpi = tokensKpi(page).locator(".num-fit").getByRole("button", { pressed: true });
    await expect(exactKpi).toHaveText(/^\d{1,3}(,\d{3})+$/);
    // The headline follows the shared exact-number preference.
    await expect(tokensKpi(page).getByRole("button", { pressed: false })).toHaveCount(0);
    // Long exact counts must still fit their card.
    expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
    const fits = await exactKpi.evaluate((element) => element.getBoundingClientRect().right <= element.closest("section")!.getBoundingClientRect().right);
    expect(fits).toBe(true);
    await page.reload();
    await settle(page, "总览");
    await expect(tokensKpi(page).locator(".num-fit").getByRole("button", { pressed: true })).toHaveText(/,/);
    await tokensKpi(page).locator(".num-fit").getByRole("button", { pressed: true }).click();
    await expect(tokensKpi(page).locator(".num-fit").getByRole("button", { pressed: false })).toHaveText(/^[\d.]+[KMBT]$/);
    expect(errors).toEqual([]);
    await page.evaluate(() => { localStorage.clear(); sessionStorage.clear(); });
  }
});

test("device form factor can be corrected without changing the device or its history", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 900 });
  const errors = await open(page, { view: "devices" });
  await settle(page, "设备");
  const card = page.locator("section.card").filter({ has: page.getByRole("heading", { name: /MacBook Pro/ }) });
  const total = card.locator(".display.text-2xl");
  const originalTotal = await total.textContent();
  await expect(card).toContainText("Mac 笔记本");
  await page.getByRole("button", { name: "MacBook Pro 的操作", exact: true }).click();
  await page.getByRole("button", { name: "更改设备性质", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("heading", { name: "更改「MacBook Pro」的设备性质" })).toBeVisible();
  const choices = dialog.getByRole("radiogroup", { name: "设备性质" });
  await expect(choices.getByRole("radio", { name: "笔记本" })).toHaveAttribute("aria-checked", "true");
  await choices.getByRole("radio", { name: "台式机" }).click();
  await dialog.getByRole("button", { name: "保存", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  await expect(card).toContainText("Mac 台式机");
  await expect(card.getByRole("heading", { name: /MacBook Pro/ })).toBeVisible();
  await expect(total).toHaveText(originalTotal ?? "");
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "总览", exact: true }).click();
  await settle(page, "总览");
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设备", exact: true }).click();
  await settle(page, "设备");
  await expect(card).toContainText("Mac 台式机");
  await expect(total).toHaveText(originalTotal ?? "");
  expect(errors).toEqual([]);
});

test("a local device's form factor can be edited without pairing", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 900 });
  const errors = await open(page, { view: "devices", query: "" });
  await settle(page, "设备");
  const card = page.locator("section.card").filter({ has: page.getByRole("heading", { name: /MacBook Pro/ }) });
  await page.getByRole("button", { name: "MacBook Pro 的操作", exact: true }).click();
  await page.getByRole("button", { name: "更改设备性质", exact: true }).click();
  await page.getByRole("dialog").getByRole("radio", { name: "台式机" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "保存", exact: true }).click();
  await expect(card).toContainText("Mac 台式机");
  expect(errors).toEqual([]);
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

test("update markers open plain-text release notes and close at 900px", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 800 });
  const errors = await open(page, { query: "?update" });
  await settle(page, "总览");
  const nav = page.getByRole("navigation", { name: "主导航" });
  await nav.getByRole("button", { name: "有新版本 0.4.1", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("heading", { name: "更新到 0.4.1" })).toBeVisible();
  // One short point per row, never a single wall of text.
  const points = dialog.locator("ul li");
  await expect(points).toHaveCount(4);
  await expect(points.first()).toHaveText("更新说明改成一条一条");
  await expect(dialog.getByText(/配对、数据和设置保持不变/)).toBeVisible();
  await expect(dialog.getByRole("button", { name: "下载并更新" })).toBeVisible();
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await nav.getByRole("button", { name: "设置", exact: true }).click();
  await settle(page, "设置");
  await page.getByRole("main").getByRole("button", { name: "有新版本 0.4.1", exact: true }).click();
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "稍后" }).click();
  await expect(dialog).toBeHidden();
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
  expect(errors).toEqual([]);
});

test("manual update check works with automatic checks disabled and shows latest", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 900 });
  const errors = await open(page, { view: "settings", query: "?update=latest" });
  await settle(page, "设置");
  const auto = page.getByRole("switch", { name: "自动检查更新", exact: true });
  await expect(auto).toHaveAttribute("aria-checked", "true");
  await auto.click();
  await expect(auto).toHaveAttribute("aria-checked", "false");
  await page.getByRole("button", { name: "检查更新", exact: true }).click();
  await expect(page.getByText("已是最新版本", { exact: true })).toBeVisible();
  await expect(auto).toHaveAttribute("aria-checked", "false");
  await expect(page.getByRole("button", { name: /^有新版本/ })).toHaveCount(0);
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
  expect(errors).toEqual([]);
});

test("download progress is visible and cannot start a second installation", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 800 });
  const errors = await open(page, { view: "settings", query: "?update=downloading" });
  await settle(page, "设置");
  await expect(page.getByRole("progressbar", { name: "更新下载进度" })).toHaveAttribute("aria-valuenow", "42");
  await page.getByRole("main").getByRole("button", { name: "有新版本 0.4.1", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("button", { name: "正在下载", exact: true })).toBeDisabled();
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
  await dialog.getByRole("button", { name: "关闭对话框", exact: true }).click();
  await expect(dialog).toBeHidden();
  expect(errors).toEqual([]);
});

test("Mac menu-bar number preference toggles and reports save failures", async ({ page }) => {
  const errors = await open(page, { query: "?server" });
  await settle(page, "总览");
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设置", exact: true }).click();
  await settle(page, "设置");
  const toggle = page.getByRole("switch", { name: "Mac 顶部显示今日用量", exact: true });
  await expect(toggle).toBeChecked();
  await expect(page.getByText("当前空间全部设备的今日总 Tokens，包含缓存读取", { exact: false })).toBeVisible();
  await toggle.click();
  await expect(toggle).not.toBeChecked();
  await page.reload();
  await settle(page, "设置");
  await expect(toggle).not.toBeChecked();
  await page.goto("/?menubar-error");
  await settle(page, "设置");
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设置", exact: true }).click();
  await settle(page, "设置");
  await page.getByRole("switch", { name: "Mac 顶部显示今日用量", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("服务器出错了");
  await expect(page.getByRole("switch", { name: "Mac 顶部显示今日用量", exact: true })).not.toBeChecked();
  expect(errors).toEqual([]);
});

test("check failures remain quiet outside settings", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 800 });
  const errors = await open(page, { query: "?update=error" });
  await settle(page, "总览");
  await expect(page.getByText("暂时连不上更新服务，请稍后再试。", { exact: true })).toHaveCount(0);
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设置", exact: true }).click();
  await settle(page, "设置");
  await expect(page.getByText("暂时连不上更新服务，请稍后再试。", { exact: true })).toBeVisible();
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
  expect(errors).toEqual([]);
});

for (const scenario of ["install-error", "async-install-error"]) test(`${scenario}: download failure closes the update dialog and explains only in settings`, async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 800 });
  const errors = await open(page, { query: `?update=${scenario}` });
  await settle(page, "总览");
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "有新版本 0.4.1", exact: true }).click();
  await page.getByRole("dialog").getByRole("button", { name: "下载并更新", exact: true }).click();
  await expect(page.getByRole("dialog").getByRole("button", { name: "正在下载", exact: true })).toBeDisabled();
  await expect(page.locator("header h1")).toHaveText("设置");
  await expect(page.getByRole("dialog")).toBeHidden();
  await expect(page.getByText("下载没有完成，请稍后重试。", { exact: true })).toBeVisible();
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
  expect(errors).toEqual([]);
});

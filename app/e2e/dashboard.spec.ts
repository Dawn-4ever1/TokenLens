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

/** Signs in through the connect dialog with the browser mock backend. */
async function signIn(dialog: import("@playwright/test").Locator, email = "user@example.com") {
  await dialog.getByRole("radio", { name: "邮箱验证码" }).click();
  await dialog.getByLabel("邮箱").fill(email);
  await dialog.getByRole("button", { name: "获取验证码", exact: true }).click();
  await dialog.getByLabel("验证码").fill("123456");
  await dialog.getByRole("button", { name: "登录并连接", exact: true }).click();
}

test("signing in never leaves the code or a session in web storage or the URL", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  await open(page, { view: "devices", query: "" });
  await settle(page, "设备");
  await page.getByRole("button", { name: "添加设备", exact: true }).click();
  await signIn(page.getByRole("dialog"));
  await settle(page, "设备");
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

test("the code input and the request button sit on the same aligned row", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  await open(page, { view: "devices", query: "" });
  await settle(page, "设备");
  await page.getByRole("button", { name: "添加设备", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("radio", { name: "邮箱验证码" }).click();
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

test("a self-hosted server offers only the email code, never GitHub", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  await open(page, { view: "devices", query: "" });
  await settle(page, "设备");
  await page.getByRole("button", { name: "添加设备", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("radio", { name: "GitHub（推荐）" })).toBeVisible();
  await expect(dialog.getByText("默认连接 https://tokenlens.online", { exact: true })).toBeVisible();
  await dialog.getByRole("radio", { name: "自己的服务器" }).click();
  await expect(dialog.getByRole("radio", { name: "GitHub（推荐）" })).toHaveCount(0);
  await expect(dialog.getByRole("button", { name: "用 GitHub 登录" })).toHaveCount(0);
  await expect(dialog.getByLabel("服务器地址")).toBeVisible();
  await expect(dialog.getByLabel("邮箱")).toBeVisible();
  await expect(dialog.getByLabel("验证码")).toBeVisible();
  // Switching back restores the GitHub choice for the hosted service.
  await dialog.getByRole("radio", { name: "TokenLens 云服务" }).click();
  await expect(dialog.getByRole("radio", { name: "GitHub（推荐）" })).toBeVisible();
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
});

test("GitHub sign-in shows a device code and finishes without any email", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await open(page, { view: "devices", query: "" });
  await settle(page, "设备");
  await page.getByRole("button", { name: "添加设备", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("radio", { name: "GitHub（推荐）" })).toHaveAttribute("aria-checked", "true");
  await dialog.getByRole("button", { name: "用 GitHub 登录", exact: true }).click();
  await expect(dialog.getByText("ABCD-1234", { exact: true })).toBeVisible();
  await expect(dialog.getByText("等待授权…", { exact: true })).toBeVisible();
  // The mock approves on the second poll, so the dialog closes by itself.
  await expect(dialog).toBeHidden({ timeout: 15_000 });
  await settle(page, "设备");
  await expect(page.getByRole("group", { name: "查看范围", exact: true })).toBeVisible();
  expect(errors).toEqual([]);
});

test("first run opens local data directly without asking for a server or a mode", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 800 });
  const errors = await open(page, { query: "" });
  await settle(page, "总览");
  await expect(page.getByText("本机数据", { exact: true })).toBeVisible();
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
  const errors = await open(page, { view: "devices", query: "?pairfail" });
  await settle(page, "设备");
  await page.getByRole("button", { name: "添加设备", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await signIn(dialog);
  await expect(dialog.getByRole("alert")).toContainText("连不上服务器");
  await dialog.getByRole("button", { name: "取消", exact: true }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByText("本机数据", { exact: true })).toBeVisible();
  await expect(page.getByText("后台采集中", { exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: /MacBook Pro/ })).toBeVisible();
  await page.getByRole("button", { name: "添加设备", exact: true }).click();
  await dialog.getByRole("radio", { name: "邮箱验证码" }).click();
  await expect(dialog.getByLabel("验证码")).toHaveValue("");
  await expect(dialog.getByLabel("邮箱")).toHaveValue("");
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
  expect(errors).toEqual([]);
});

test("signing in from Devices allows local and all-device views, clears old filters and keeps both connected", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 900 });
  const errors = await open(page, { view: "devices", query: "" });
  await settle(page, "设备");
  await page.getByRole("button", { name: "添加设备", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await signIn(dialog);
  await expect(dialog).toBeHidden();
  await settle(page, "设备");
  const source = page.getByRole("group", { name: "查看范围", exact: true });
  await expect(source.getByRole("button", { name: "全部设备", exact: true })).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByRole("heading", { name: "Mac mini", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "筛选", exact: true }).click();
  await page.getByRole("checkbox", { name: /Windows 台式机/ }).click();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("group", { name: "生效的筛选" })).toContainText("Windows 台式机");
  await source.getByRole("button", { name: "本机", exact: true }).click();
  await settle(page, "设备");
  await expect(page.getByRole("group", { name: "生效的筛选" })).toHaveCount(0);
  await expect(page.getByRole("heading", { name: /MacBook Pro/ })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Mac mini", exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "添加设备", exact: true }).click();
  await expect(dialog.getByRole("button", { name: "复制配对码" })).toBeVisible();
  await expect(dialog.getByText("K7QM-4XPR", { exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await source.getByRole("button", { name: "全部设备", exact: true }).click();
  await settle(page, "设备");
  await expect(page.getByRole("heading", { name: "Mac mini", exact: true })).toBeVisible();
  expect(await overflow(page)).toEqual({ document: false, main: false, offenders: [] });
  expect(errors).toEqual([]);
});

test("disconnecting a server returns to local data without stopping collection", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  const errors = await open(page, { view: "settings", query: "?server" });
  await settle(page, "设置");
  await page.getByRole("button", { name: "断开服务器", exact: true }).click();
  await page.getByRole("dialog").getByRole("button", { name: "断开服务器", exact: true }).click();
  await settle(page, "设置");
  await expect(page.getByRole("main").getByText("本机数据", { exact: true })).toBeVisible();
  await expect(page.getByText("后台采集中", { exact: true }).first()).toBeVisible();
  await expect(page.getByRole("button", { name: "连接服务器", exact: true })).toBeVisible();
  expect(errors).toEqual([]);
});

test("server read failures leave Devices controls available so the user can view local data", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 800 });
  const errors = await open(page, { view: "devices", query: "?offline" });
  await expect(page.getByText("连不上服务器").first()).toBeVisible();
  await expect(page.getByRole("button", { name: "添加设备", exact: true })).toBeVisible();
  await page.getByRole("group", { name: "查看范围", exact: true }).getByRole("button", { name: "本机", exact: true }).click();
  await settle(page, "设备");
  await expect(page.getByRole("heading", { name: /MacBook Pro/ })).toBeVisible();
  await expect(page.getByText("连不上服务器")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "添加设备", exact: true })).toBeVisible();
  expect(errors).toEqual([]);
});

/** The headline token card; cost comes first on the overview. */
const tokensKpi = (page: Page) => page.locator("section.card").filter({ hasText: "Tokens ·" });

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
  await page.getByRole("button", { name: "总览" }).click();
  await expect(chips).toHaveCount(0);
  await expect(headline).toHaveText(whole);
  // A filter the user picks stays, is visible on every page, and one click removes it.
  await page.getByRole("button", { name: /Claude Code/ }).first().click();
  await page.getByRole("button", { name: "筛选", exact: true }).click();
  await page.getByRole("checkbox", { name: /Codex/ }).click();
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "总览" }).click();
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
    // Every other count follows, e.g. today's tokens under the headline.
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

test("update markers open plain-text release notes and close at 900px", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 800 });
  const errors = await open(page, { query: "?update" });
  await settle(page, "总览");
  const nav = page.getByRole("navigation", { name: "主导航" });
  await nav.getByRole("button", { name: "有新版本 0.4.1", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("heading", { name: "更新到 0.4.1" })).toBeVisible();
  await expect(dialog.getByText(/改善自动更新体验/)).toBeVisible();
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
  await dialog.getByRole("button", { name: "关闭", exact: true }).click();
  await expect(dialog).toBeHidden();
  expect(errors).toEqual([]);
});

test("check failures remain quiet outside settings", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 800 });
  const errors = await open(page, { query: "?update=error" });
  await settle(page, "总览");
  await expect(page.getByText("连不上 GitHub，稍后会再试。", { exact: true })).toHaveCount(0);
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.getByRole("navigation", { name: "主导航" }).getByRole("button", { name: "设置", exact: true }).click();
  await settle(page, "设置");
  await expect(page.getByText("连不上 GitHub，稍后会再试。", { exact: true })).toBeVisible();
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

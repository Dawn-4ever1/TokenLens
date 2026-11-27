import { expect, test } from "@playwright/test";

async function openOverview(page: import("@playwright/test").Page, width = 1200) {
  await page.setViewportSize({ width, height: 900 });
  await page.addInitScript(() => {
    if (sessionStorage.getItem("ranges-seeded")) return;
    sessionStorage.setItem("ranges-seeded", "1");
    localStorage.clear();
    localStorage.setItem("tokenlens.settings.v1", JSON.stringify({ theme: "light", direction: "editorial", refreshSeconds: 0 }));
    localStorage.setItem("tokenlens.page.v1", "overview");
  });
  await page.goto("/?server");
  await expect(page.locator("header h1")).toHaveText("总览");
  await expect(page.locator(".skeleton")).toHaveCount(0);
}

function summaryCard(page: import("@playwright/test").Page) {
  return page.locator("section").filter({ has: page.getByRole("heading", { name: "TOKENS", exact: true }) }).first();
}

test("five calendar ranges update the figures, including custom dates", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await openOverview(page);
  const picker = page.getByRole("radiogroup", { name: "时间范围" });
  await expect(picker.getByRole("radio").allTextContents()).resolves.toEqual(["日", "周", "月", "总计", "自定义"]);
  const summary = summaryCard(page);
  // The original number display: plain text, no animation wrapper.
  await expect(summary.locator(".display .num")).toHaveCount(1);
  await expect.poll(() => summary.getByRole("button").first().evaluate((element) => getComputedStyle(element).animationName)).toBe("none");

  for (const range of ["日", "周", "月", "总计"]) {
    await picker.getByRole("radio", { name: range, exact: true }).click();
    await expect(summary.getByRole("heading", { name: "TOKENS", exact: true })).toBeVisible();
  }
  await picker.getByRole("radio", { name: "自定义", exact: true }).click();
  await expect(page.getByRole("button", { name: "修改日期", exact: true })).toBeVisible();
  await page.getByLabel("开始", { exact: true }).fill("2026-11-03");
  await page.getByLabel("结束", { exact: true }).fill("2026-11-05");
  await expect(summary.getByRole("heading", { name: "TOKENS", exact: true })).toBeVisible();
  const stored = await page.evaluate(() => JSON.parse(localStorage.getItem("tokenlens.filters.v1") ?? "{}"));
  expect(stored).toMatchObject({ preset: "custom", customFrom: "2026-11-03", customTo: "2026-11-05" });
  expect(errors).toEqual([]);
});

test("device picker lists every device without descriptions and filters every page", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await openOverview(page, 1440);
  const scope = page.getByRole("button", { name: "设备范围：全账号", exact: true });
  await expect(scope).toBeVisible();
  await expect(scope).toHaveAttribute("aria-haspopup", "menu");
  const order = await page.evaluate(() => ["时间范围", "设备范围：全账号", "筛选"].map((label) =>
    document.querySelector(`[aria-label="${label}"]`)!.getBoundingClientRect().left));
  expect(order[0]).toBeLessThan(order[1]!);
  expect(order[1]).toBeLessThan(order[2]!);
  const all = await summaryCard(page).locator(".display").innerText();
  await scope.click();
  await expect(page.getByRole("menuitemradio").allTextContents()).resolves.toEqual([
    "全账号", "MacBook Pro（本机）", "Mac mini", "Windows 台式机", "旧笔记本",
  ]);
  const menu = page.getByRole("menu", { name: "设备范围", exact: true });
  expect(await menu.locator("p").count()).toBe(0);
  await page.getByRole("menuitemradio", { name: "MacBook Pro（本机）", exact: true }).click();
  await expect(page.getByRole("button", { name: "设备范围：MacBook Pro（本机）", exact: true })).toBeVisible();
  await expect(page.getByRole("group", { name: "生效的筛选" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: /^筛选/ })).toContainText("1");
  const currentNumber = summaryCard(page).locator(".display");
  await expect(currentNumber).not.toHaveText(all);
  const current = await currentNumber.innerText();
  await page.getByRole("button", { name: "设备范围：MacBook Pro（本机）", exact: true }).click();
  await page.getByRole("menuitemradio", { name: "Mac mini", exact: true }).click();
  await expect(page.getByRole("group", { name: "生效的筛选" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: /^筛选/ })).toContainText("1");
  const otherNumber = summaryCard(page).locator(".display");
  await expect(otherNumber).not.toHaveText(current);
  const other = await otherNumber.innerText();
  expect(other).not.toBe(all);
  await page.getByRole("button", { name: "会话", exact: true }).click();
  await expect(page.locator("header h1")).toHaveText("会话");
  await expect(page.locator(".skeleton")).toHaveCount(0);
  const rows = page.locator("tbody tr");
  await expect(rows.first()).toBeVisible();
  for (const row of await rows.all()) await expect(row).toContainText("Mac mini");
  await page.getByRole("button", { name: "设备", exact: true }).click();
  await expect(page.locator("header h1")).toHaveText("设备");
  await expect(page.locator(".skeleton")).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "MacBook Pro 本机", exact: true })).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "Mac mini", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "设备范围：Mac mini", exact: true }).click();
  await page.getByRole("menuitemradio", { name: "全账号", exact: true }).click();
  await expect(page.getByRole("heading", { name: "MacBook Pro 本机", exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Mac mini", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "设备范围：全账号", exact: true }).click();
  await page.getByRole("menuitemradio", { name: "MacBook Pro（本机）", exact: true }).click();
  await expect(page.getByRole("heading", { name: "MacBook Pro 本机", exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Mac mini", exact: true })).toHaveCount(0);
  await page.reload();
  await expect(page.locator("header h1")).toHaveText("设备");
  await expect(page.getByRole("button", { name: "设备范围：MacBook Pro（本机）", exact: true })).toBeVisible();
  await page.getByRole("button", { name: /^筛选/ }).click();
  await page.getByRole("checkbox", { name: /Codex/ }).click();
  await expect(page.getByRole("button", { name: /^筛选/ })).toContainText("2");
  await page.getByRole("button", { name: "清除全部", exact: true }).click();
  await expect(page.getByRole("button", { name: "设备范围：全账号", exact: true })).toBeVisible();
  await expect(page.getByRole("group", { name: "生效的筛选" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "筛选", exact: true })).toBeVisible();
  expect(errors).toEqual([]);
});

test("local use still has a scope picker, without cloud data", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  await page.addInitScript(() => {
    localStorage.clear();
    localStorage.setItem("tokenlens.settings.v1", JSON.stringify({ theme: "light", direction: "editorial", refreshSeconds: 0 }));
    localStorage.setItem("tokenlens.filters.v1", JSON.stringify({ preset: "month", deviceScope: "other", devices: [], clients: [], models: [], projects: [] }));
  });
  await page.goto("/");
  await expect(page.locator(".skeleton")).toHaveCount(0);
  await page.getByRole("button", { name: "设备", exact: true }).click();
  await expect(page.locator(".skeleton")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "设备范围：MacBook Pro（本机）", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "设备范围：MacBook Pro（本机）", exact: true }).click();
  await expect(page.getByRole("menuitemradio").allTextContents()).resolves.toEqual(["MacBook Pro（本机）"]);
  await expect(page.getByRole("menuitemradio", { name: "MacBook Pro（本机）", exact: true })).toBeEnabled();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("heading", { name: "MacBook Pro 本机", exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Mac mini", exact: true })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "筛选", exact: true })).not.toContainText(/\d/);
  await page.getByRole("button", { name: "筛选", exact: true }).click();
  await page.getByRole("checkbox", { name: /Codex/ }).click();
  await expect(page.getByRole("button", { name: "筛选", exact: true })).toContainText("1");
  await page.getByRole("button", { name: "清除全部", exact: true }).click();
  await expect(page.getByRole("button", { name: "设备范围：MacBook Pro（本机）", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "筛选", exact: true })).not.toContainText(/\d/);
});

for (const width of [1440, 1200, 1100, 1000, 900]) {
  test(`headline token text stays left-aligned throughout format changes @${width}`, async ({ page }) => {
    await openOverview(page, width);
    expect(await page.evaluate(() => innerWidth)).toBe(width);
    const button = summaryCard(page).locator(".display button");
    // A range change and a reload must not bring the old number animation back.
    for (const range of ["月", "总计"]) {
      await page.getByRole("radiogroup", { name: "时间范围" }).getByRole("radio", { name: range, exact: true }).click();
      for (const exact of [true, false]) {
        await button.click();
        await expect(button).toHaveAttribute("aria-pressed", String(exact));
        // Check the actual text on every frame, including the old reel's 650ms
        // settle period. A button can be inside the card while its text drifts.
        const frames = await button.evaluate(async (element) => {
          const samples = [];
          const until = performance.now() + 750;
          do {
            const text = document.createRange();
            text.selectNodeContents(element);
            const rect = text.getBoundingClientRect();
            const box = element.parentElement!.getBoundingClientRect();
            const style = getComputedStyle(element);
            samples.push({
              left: Math.abs(rect.left - box.left) <= 1,
              inside: rect.right <= box.right + 1,
              plainText: element.childElementCount === 0,
              still: style.animationName === "none" && style.transform === "none",
            });
            await new Promise(requestAnimationFrame);
          } while (performance.now() < until);
          return samples;
        });
        expect(frames.length).toBeGreaterThan(1);
        for (const frame of frames) {
          expect(frame).toEqual({ left: true, inside: true, plainText: true, still: true });
        }
      }
    }
    await page.reload();
    await expect(button).toHaveAttribute("aria-pressed", "false");
    await expect(button).toHaveText(/^[\d.]+[KMB]$/);
  });
}

async function readChartOption(chart: import("@playwright/test").Locator) {
  return chart.locator("canvas").evaluate(async (canvas) => {
    const url = performance
      .getEntriesByType("resource")
      .map((entry) => entry.name)
      .find((entry) => new URL(entry).pathname === "/node_modules/.vite/deps/echarts_core.js");
    if (!url) throw new Error("ECharts module is missing");
    const { getInstanceByDom } = await import(url);
    const instance = getInstanceByDom(canvas.parentElement!.parentElement!);
    if (!instance) throw new Error("ECharts instance is missing");
    const option = instance.getOption() as {
      xAxis: { data: string[] }[];
      series: { type: string; data: (number | null)[] }[];
    };
    return {
      xAxisData: option.xAxis[0]?.data ?? [],
      series: option.series.map((s) => ({ type: s.type, data: s.data })),
    };
  });
}

test("Overview uses bars for every calendar preset and keeps the full week across directions and Trends", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.setViewportSize({ width: 1200, height: 900 });
  await page.addInitScript(() => {
    const fixedNow = Date.parse("2026-11-24T22:30:00+08:00");
    const RealDate = Date;
    class FixedDate extends RealDate {
      constructor(...args: unknown[]) {
        if (args.length === 0) super(fixedNow);
        else super(...(args as [number]));
      }
      static override now() {
        return fixedNow;
      }
    }
    window.Date = FixedDate as DateConstructor;
    if (sessionStorage.getItem("weekly-seeded")) return;
    sessionStorage.setItem("weekly-seeded", "1");
    localStorage.clear();
    localStorage.setItem("tokenlens.settings.v1", JSON.stringify({ theme: "light", direction: "editorial", refreshSeconds: 0 }));
    localStorage.setItem("tokenlens.page.v1", "overview");
  });
  await page.goto("/?server");
  await expect(page.locator("header h1")).toHaveText("总览");
  await expect(page.locator(".skeleton")).toHaveCount(0);

  const picker = page.getByRole("radiogroup", { name: "时间范围" });
  const trendCard = page.locator("section").filter({ has: page.getByRole("heading", { name: "趋势", exact: true }) }).first();
  const chart = trendCard.getByRole("img", { name: /用量趋势/ });
  const expectedWeek = ["2026-11-23", "2026-11-24", "2026-11-25", "2026-11-26", "2026-11-27", "2026-11-28", "2026-11-29"];
  const presets = [
    { name: "日", unit: "每小时", first: "2026-11-24T00:00", last: "2026-11-24T23:00", size: 24 },
    { name: "月", unit: "每天", first: "2026-11-01", last: "2026-11-24", size: 24 },
    // The mock has 151 days of history, so All rolls up to calendar weeks.
    { name: "总计", unit: "每周", first: "2026-06-22", last: "2026-11-23", size: 23 },
    { name: "周", unit: "每天", first: "2026-11-23", last: "2026-11-29", size: 7 },
  ];

  for (const direction of ["editorial", "graphite", "paper"] as const) {
    if (direction !== "editorial") {
      await page.evaluate((dir) => {
        const saved = JSON.parse(localStorage.getItem("tokenlens.settings.v1") ?? "{}");
        localStorage.setItem("tokenlens.settings.v1", JSON.stringify({ ...saved, direction: dir }));
      }, direction);
      await page.setViewportSize({ width: 900, height: 900 });
      await page.reload();
      await expect(page.locator("header h1")).toHaveText("总览");
    }
    await expect(chart).toHaveAttribute("data-chart-direction", direction);
    if (direction === "graphite") {
      // The former graphite cutoff switched daily series longer than 45 buckets to a line.
      await picker.getByRole("radio", { name: "自定义", exact: true }).click();
      await page.getByLabel("开始", { exact: true }).fill("2026-09-01");
      await page.getByLabel("结束", { exact: true }).fill("2026-11-24");
      await page.keyboard.press("Escape");
      await expect.poll(async () => {
        const { xAxisData, series } = await readChartOption(chart);
        return { days: xAxisData.length, types: series.map(({ type }) => type) };
      }).toEqual({ days: 85, types: ["bar"] });
    }
    for (const { name, unit, first, last, size } of presets) {
      await picker.getByRole("radio", { name, exact: true }).click();
      await expect(trendCard).toContainText(`${unit}总 tokens`);
      await expect.poll(async () => {
        const { xAxisData } = await readChartOption(chart);
        return { first: xAxisData[0], last: xAxisData.at(-1), size: xAxisData.length };
      }, { message: `${direction} / ${name} preserves its date buckets` }).toEqual({ first, last, size });
      const option = await readChartOption(chart);
      expect(option.series.map(({ type }) => type), `${direction} / ${name}`).toEqual(["bar"]);
      await expect(chart).toHaveAccessibleName("用量趋势柱状图");
      if (name === "周") {
        expect(option.xAxisData).toEqual(expectedWeek);
        expect(option.series[0]!.data.slice(0, 2)).toEqual([expect.any(Number), expect.any(Number)]);
        expect(option.series[0]!.data.slice(2)).toEqual([null, null, null, null, null]);
      }
    }
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth > innerWidth);
    expect(overflow).toBe(false);
  }

  // Trends page defaults to daily granularity on Week and shows Monday..Sunday on the x-axis.
  await page.setViewportSize({ width: 1200, height: 900 });
  await page.getByRole("button", { name: "趋势", exact: true }).click();
  await expect(page.locator("header h1")).toHaveText("趋势");
  await expect(page.locator(".skeleton")).toHaveCount(0);
  const granularity = page.getByRole("radiogroup", { name: "粒度" });
  await expect(granularity.getByRole("radio", { name: "日", exact: true })).toHaveAttribute("aria-checked", "true");
  const trendsChart = page.getByRole("img", { name: /按分组堆叠的用量趋势图/ });
  await expect.poll(async () => (await readChartOption(trendsChart)).xAxisData).toEqual(expectedWeek);

  expect(errors).toEqual([]);
});

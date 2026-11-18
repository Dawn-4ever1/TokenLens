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
  return page.locator("section").filter({ has: page.getByRole("heading", { name: "Tokens", exact: true }) }).first();
}

test("five calendar ranges update the figures, including custom dates", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await openOverview(page);
  const picker = page.getByRole("radiogroup", { name: "时间范围" });
  await expect(picker.getByRole("radio").allTextContents()).resolves.toEqual(["日", "周", "月", "总计", "自定义"]);
  const summary = summaryCard(page);
  // The original number display: plain text, no animation wrapper.
  await expect(summary.locator(".num-fit .num")).toHaveCount(1);
  await expect.poll(() => summary.getByRole("button").first().evaluate((element) => getComputedStyle(element).animationName)).toBe("none");

  for (const range of ["日", "周", "月", "总计"]) {
    await picker.getByRole("radio", { name: range, exact: true }).click();
    await expect(summary.getByRole("heading", { name: "Tokens", exact: true })).toBeVisible();
  }
  await picker.getByRole("radio", { name: "自定义", exact: true }).click();
  await expect(page.getByRole("button", { name: "修改日期", exact: true })).toBeVisible();
  await page.getByLabel("开始", { exact: true }).fill("2026-11-03");
  await page.getByLabel("结束", { exact: true }).fill("2026-11-05");
  await expect(summary.getByRole("heading", { name: "Tokens", exact: true })).toBeVisible();
  const stored = await page.evaluate(() => JSON.parse(localStorage.getItem("tokenlens.filters.v1") ?? "{}"));
  expect(stored).toMatchObject({ preset: "custom", customFrom: "2026-11-03", customTo: "2026-11-05" });
  expect(errors).toEqual([]);
});

test("device scope sits beside dates and really narrows every page", async ({ page }) => {
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
  const all = await summaryCard(page).locator(".num-fit").innerText();
  await scope.click();
  await expect(page.getByRole("menuitemradio").allTextContents()).resolves.toEqual([
    "全账号同一账号下的全部设备，包含本机",
    "本机只统计当前这台电脑",
    "其他设备同一账号下除本机外的设备",
  ]);
  await page.getByRole("menuitemradio", { name: /^本机/ }).click();
  await expect(page.getByRole("button", { name: "设备范围：本机", exact: true })).toBeVisible();
  await expect(page.getByRole("group", { name: "生效的筛选" })).toContainText("本机");
  const currentNumber = summaryCard(page).locator(".num-fit");
  await expect(currentNumber).not.toHaveText(all);
  const current = await currentNumber.innerText();
  await page.getByRole("button", { name: "设备范围：本机", exact: true }).click();
  await page.getByRole("menuitemradio", { name: /^其他设备/ }).click();
  await expect(page.getByRole("group", { name: "生效的筛选" })).toContainText("其他设备");
  const otherNumber = summaryCard(page).locator(".num-fit");
  await expect(otherNumber).not.toHaveText(current);
  const other = await otherNumber.innerText();
  expect(other).not.toBe(all);
  await page.getByRole("button", { name: "会话", exact: true }).click();
  await expect(page.locator("header h1")).toHaveText("会话");
  await expect(page.locator(".skeleton")).toHaveCount(0);
  await expect(page.locator("tbody tr").first()).not.toContainText("MacBook Pro");
  await page.getByRole("button", { name: "设备", exact: true }).click();
  await expect(page.locator("header h1")).toHaveText("设备");
  await expect(page.locator(".skeleton")).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "MacBook Pro 本机", exact: true })).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "Mac mini", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "设备范围：其他设备", exact: true }).click();
  await page.getByRole("menuitemradio", { name: /^全账号/ }).click();
  await expect(page.getByRole("heading", { name: "MacBook Pro 本机", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "设备范围：全账号", exact: true }).click();
  await page.getByRole("menuitemradio", { name: /^本机/ }).click();
  await expect(page.getByRole("heading", { name: "MacBook Pro 本机", exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Mac mini", exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "筛选", exact: true }).click();
  await page.getByRole("checkbox", { name: /Codex/ }).click();
  await page.getByRole("button", { name: "清除全部", exact: true }).click();
  await expect(page.getByRole("button", { name: "设备范围：全账号", exact: true })).toBeVisible();
  await expect(page.getByRole("group", { name: "生效的筛选" })).toHaveCount(0);
  await page.reload();
  await expect(page.locator("header h1")).toHaveText("设备");
  await expect(page.getByRole("button", { name: "设备范围：全账号", exact: true })).toBeVisible();
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
  await expect(page.getByRole("button", { name: "设备范围：本机", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "设备范围：本机", exact: true }).click();
  await expect(page.getByRole("menuitemradio", { name: /^全账号/ })).toBeDisabled();
  await expect(page.getByRole("menuitemradio", { name: /^其他设备/ })).toBeDisabled();
  await expect(page.getByRole("menuitemradio", { name: /^本机/ })).toBeEnabled();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("heading", { name: "MacBook Pro 本机", exact: true })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Mac mini", exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "筛选", exact: true }).click();
  await page.getByRole("checkbox", { name: /Codex/ }).click();
  await expect(page.getByRole("button", { name: "筛选", exact: true })).toContainText("2");
  await page.getByRole("button", { name: "清除全部", exact: true }).click();
  await expect(page.getByRole("button", { name: "设备范围：本机", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "筛选", exact: true })).toContainText("1");
});

for (const width of [1440, 1200, 1100, 1000, 900]) {
  test(`headline token text stays left-aligned throughout format changes @${width}`, async ({ page }) => {
    await openOverview(page, width);
    expect(await page.evaluate(() => innerWidth)).toBe(width);
    const button = summaryCard(page).locator(".num-fit button");
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

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
  return page.locator("section").filter({ has: page.getByRole("heading", { name: /^Tokens ·/ }) }).first();
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

  await picker.getByRole("radio", { name: "日", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Tokens · 日", exact: true })).toBeVisible();
  await picker.getByRole("radio", { name: "周", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Tokens · 周", exact: true })).toBeVisible();
  await picker.getByRole("radio", { name: "月", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Tokens · 月", exact: true })).toBeVisible();
  await picker.getByRole("radio", { name: "总计", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Tokens · 总计", exact: true })).toBeVisible();
  await picker.getByRole("radio", { name: "自定义", exact: true }).click();
  await expect(page.getByRole("button", { name: "修改日期", exact: true })).toBeVisible();
  await page.getByLabel("开始", { exact: true }).fill("2026-11-03");
  await page.getByLabel("结束", { exact: true }).fill("2026-11-05");
  await expect(page.getByRole("heading", { name: "Tokens · 2026-11-03 – 2026-11-05", exact: true })).toBeVisible();
  const stored = await page.evaluate(() => JSON.parse(localStorage.getItem("tokenlens.filters.v1") ?? "{}"));
  expect(stored).toMatchObject({ preset: "custom", customFrom: "2026-11-03", customTo: "2026-11-05" });
  expect(errors).toEqual([]);
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

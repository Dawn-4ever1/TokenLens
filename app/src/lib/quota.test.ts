import { describe, expect, it } from "vitest";
import { mockQuotas } from "./mock";
import { hasReset, quotaTone, resetCountdown, usedText, windowLabel } from "./quota";

const now = Date.parse("2026-11-27T09:00:00Z");
const later = (minutes: number) => new Date(now + minutes * 60_000).toISOString();

describe("windowLabel", () => {
  it("names the windows the way the agents do", () => {
    expect(windowLabel({ minutes: 300, scope: null })).toBe("5 小时");
    expect(windowLabel({ minutes: 10080, scope: null })).toBe("本周");
    expect(windowLabel({ minutes: 10080, scope: "Fable" })).toBe("本周 · Fable");
    expect(windowLabel({ minutes: 1440, scope: null })).toBe("今天");
    expect(windowLabel({ minutes: 4320, scope: null })).toBe("3 天");
    expect(windowLabel({ minutes: 90, scope: null })).toBe("90 分钟");
  });

  it("falls back to the scope or a neutral name without a length", () => {
    expect(windowLabel({ minutes: null, scope: "Fable" })).toBe("Fable");
    expect(windowLabel({ minutes: null, scope: null })).toBe("其他额度");
  });
});

describe("resetCountdown", () => {
  it("counts down in minutes, hours and days", () => {
    expect(resetCountdown(later(0.2), now)).toBe("1 分钟后重置");
    expect(resetCountdown(later(45), now)).toBe("45 分钟后重置");
    expect(resetCountdown(later(145), now)).toBe("2 小时 25 分后重置");
    expect(resetCountdown(later(180), now)).toBe("3 小时后重置");
    expect(resetCountdown(later(5 * 1440 + 20 * 60), now)).toBe("5 天 20 小时后重置");
    expect(resetCountdown(later(2 * 1440), now)).toBe("2 天后重置");
  });

  it("says nothing once the window has renewed or without a time", () => {
    expect(resetCountdown(later(-1), now)).toBeNull();
    expect(resetCountdown(null, now)).toBeNull();
    expect(resetCountdown("not a time", now)).toBeNull();
    expect(hasReset({ resetsAt: later(-1) }, now)).toBe(true);
    expect(hasReset({ resetsAt: later(1) }, now)).toBe(false);
    expect(hasReset({ resetsAt: null }, now)).toBe(false);
  });
});

describe("quota levels", () => {
  it("warns from 80% and marks a used-up window", () => {
    expect(quotaTone(79.9)).toBe("normal");
    expect(quotaTone(80)).toBe("warn");
    expect(quotaTone(100)).toBe("bad");
    expect(usedText(3.4)).toBe("已用 3%");
    expect(usedText(99.6)).toBe("已用 100%");
    expect(usedText(100)).toBe("已用完");
  });
});

describe("mock quotas", () => {
  it("cover the empty, alert, reset and unreadable cases", () => {
    expect(mockQuotas("none", now).subscriptions).toEqual([]);
    const normal = mockQuotas(null, now);
    expect(normal.subscriptions.map((s) => s.client)).toEqual(["claude", "codex"]);
    const alert = mockQuotas("alert", now).subscriptions;
    expect(alert[0]!.windows[0]!.usedPercent).toBe(100);
    expect(hasReset(mockQuotas("reset", now).subscriptions[0]!.windows[0]!, now)).toBe(true);
    expect(mockQuotas("unreadable", now).subscriptions[1]!.status).toBe("unreadable");
  });
});

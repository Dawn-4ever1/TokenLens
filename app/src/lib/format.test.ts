import { describe, expect, it } from "vitest";
import { compact, dayLabel, duration, percent, relativeTime, shortId, usd } from "./format";

describe("compact", () => {
  it("keeps small numbers exact", () => {
    expect(compact(0)).toBe("0");
    expect(compact(9_999)).toBe("9,999");
  });

  it("does not drop integer zeros", () => {
    expect(compact(190_000)).toBe("190K");
    expect(compact(580_000)).toBe("580K");
    expect(compact(100_000)).toBe("100K");
    expect(compact(8_000_000)).toBe("8M");
    expect(compact(20_000_000)).toBe("20M");
  });

  it("trims only fractional zeros", () => {
    expect(compact(12_000)).toBe("12K");
    expect(compact(12_340)).toBe("12.3K");
    expect(compact(1_500_000)).toBe("1.5M");
    expect(compact(1_234_567)).toBe("1.23M");
  });

  it("rolls over to the next unit after rounding", () => {
    expect(compact(999_600)).toBe("1M");
    expect(compact(999_999_999)).toBe("1B");
  });

  it("handles negatives", () => {
    expect(compact(-25_000)).toBe("-25K");
  });
});

describe("other formatters", () => {
  it("percent", () => {
    expect(percent(null)).toBe("—");
    expect(percent(0.9712)).toBe("97.1%");
    expect(percent(Number.NaN)).toBe("—");
  });

  it("usd", () => {
    expect(usd(null)).toBe("—");
    expect(usd(0.004)).toBe("<$0.01");
    expect(usd(0)).toBe("$0.00");
    expect(usd(12.345)).toBe("$12.35");
    expect(usd(2_500)).toBe("$2,500");
    expect(usd(25_000)).toBe("$25K");
  });

  it("duration drops zero parts", () => {
    expect(duration(null)).toBe("—");
    expect(duration(450)).toBe("450 毫秒");
    expect(duration(44_000)).toBe("44 秒");
    expect(duration(15 * 60_000)).toBe("15 分钟");
    expect(duration(15 * 60_000 + 3_000)).toBe("15 分 3 秒");
    expect(duration(72 * 3_600_000)).toBe("72 小时");
    expect(duration(90 * 60_000)).toBe("1 小时 30 分");
  });

  it("relativeTime", () => {
    const now = Date.parse("2026-11-11T12:00:00Z");
    expect(relativeTime(null, now)).toBe("从未");
    expect(relativeTime(now - 10_000, now)).toBe("刚刚");
    expect(relativeTime(now - 5 * 60_000, now)).toBe("5 分钟前");
    expect(relativeTime(now - 3 * 3_600_000, now)).toBe("3 小时前");
    expect(relativeTime(now - 3 * 86_400_000, now)).toBe("3 天前");
    expect(relativeTime(now + 60_000, now)).toBe("刚刚");
  });

  it("shortId and dayLabel", () => {
    expect(shortId("abc")).toBe("abc");
    expect(shortId("0123456789abcdef")).toBe("01234567…");
    expect(dayLabel("2026-11-03")).toBe("11月3日");
    expect(dayLabel("2026-11-03", true)).toBe("2026年11月3日");
  });
});

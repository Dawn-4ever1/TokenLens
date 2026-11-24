import { describe, expect, it } from "vitest";
import type { Point, Timeseries } from "@/lib/types";
import { validDisplayName } from "./Devices";
import { autoBucket, buildTrendPoints, compactTrendUsesBars, isCurrentWeekDaily, weekCalendarDates } from "./Overview";
import { bucketSpan, comparisonSpans, findAnomalies } from "./Trends";

describe("autoBucket", () => {
  it("picks a bucket from the preset and span length", () => {
    expect(autoBucket({ from: "2026-11-11", to: "2026-11-11" }, undefined, "2026-11-11")).toBe("hour");
    expect(autoBucket({ from: "2026-11-11", to: "2026-11-11" }, undefined, "2026-11-11", "today")).toBe("hour");
    // Week preset is always daily, even on Monday (1 day) or Tuesday (2 days).
    expect(autoBucket({ from: "2026-11-23", to: "2026-11-23" }, undefined, "2026-11-23", "week")).toBe("day");
    expect(autoBucket({ from: "2026-11-23", to: "2026-11-24" }, undefined, "2026-11-24", "week")).toBe("day");
    expect(autoBucket({ from: "2026-11-23", to: "2026-11-29" }, undefined, "2026-11-29", "week")).toBe("day");
    // Month preset is always daily, even on the 1st or 2nd of the month.
    expect(autoBucket({ from: "2026-11-01", to: "2026-11-01" }, undefined, "2026-11-01", "month")).toBe("day");
    expect(autoBucket({ from: "2026-11-01", to: "2026-11-02" }, undefined, "2026-11-02", "month")).toBe("day");
    // Custom spans keep hourly only for a single day and daily for 2+ days.
    expect(autoBucket({ from: "2026-11-23", to: "2026-11-23" }, undefined, "2026-11-24", "custom")).toBe("hour");
    expect(autoBucket({ from: "2026-11-23", to: "2026-11-24" }, undefined, "2026-11-24", "custom")).toBe("day");
    expect(autoBucket({ from: "2026-10-13", to: "2026-11-11" }, undefined, "2026-11-11")).toBe("day");
    expect(autoBucket({}, "2025-01-01", "2026-11-11")).toBe("week");
    expect(autoBucket({}, "2023-01-01", "2026-11-11")).toBe("month");
    expect(autoBucket({}, undefined, "2026-11-11")).toBe("day");
  });
});

describe("compactTrendUsesBars", () => {
  it("uses compact bars for short graphite series and lines beyond 45 points", () => {
    expect(compactTrendUsesBars("graphite", 0)).toBe(false);
    expect(compactTrendUsesBars("graphite", 0, true)).toBe(false);
    expect(compactTrendUsesBars("graphite", 1)).toBe(true);
    expect(compactTrendUsesBars("graphite", 45)).toBe(true);
    expect(compactTrendUsesBars("graphite", 46)).toBe(false);
  });

  it("keeps editorial area and paper line outside weekly, and uses bars for weekly in all directions", () => {
    expect(compactTrendUsesBars("editorial", 30)).toBe(false);
    expect(compactTrendUsesBars("paper", 30)).toBe(false);
    expect(compactTrendUsesBars("editorial", 7, true)).toBe(true);
    expect(compactTrendUsesBars("graphite", 7, true)).toBe(true);
    expect(compactTrendUsesBars("paper", 7, true)).toBe(true);
  });
});

function makePoint(overrides: Partial<Point> = {}): Point {
  return {
    totalTokens: 1000,
    inputTokens: 100,
    outputTokens: 200,
    cacheReadTokens: 600,
    cacheWriteTokens: 50,
    reasoningTokens: 50,
    costUsd: 1.5,
    unpricedTokens: 0,
    messageCount: 1,
    ...overrides,
  };
}

function makeSeries(from: string, to: string, buckets: string[], points: Point[], bucket: Timeseries["bucket"] = "day"): Timeseries {
  return {
    range: { from, to, timeZone: "Asia/Shanghai" },
    bucket,
    groupBy: "none",
    buckets,
    series: [
      {
        key: "all",
        label: "全部",
        totalTokens: 0,
        inputTokens: 0,
        outputTokens: 0,
        cacheReadTokens: 0,
        cacheWriteTokens: 0,
        reasoningTokens: 0,
        costUsd: 0,
        unpricedTokens: 0,
        credits: null,
        messageCount: 0,
        cacheHitRate: null,
        precision: "measured",
        points,
      },
    ],
    generatedAt: "2026-11-24T00:00:00Z",
  };
}

describe("weekly trend helpers", () => {
  it("builds Monday through Sunday calendar dates", () => {
    expect(weekCalendarDates("2026-11-24")).toEqual([
      "2026-11-23",
      "2026-11-24",
      "2026-11-25",
      "2026-11-26",
      "2026-11-27",
      "2026-11-28",
      "2026-11-29",
    ]);
  });

  it("projects Monday data onto 7 days with 6 null future slots", () => {
    const data = makeSeries("2026-11-23", "2026-11-23", ["2026-11-23"], [makePoint({ totalTokens: 5000 })]);
    expect(isCurrentWeekDaily(data, "week", "2026-11-23")).toBe(true);
    expect(buildTrendPoints(data, "total", "week", "2026-11-23")).toEqual({
      buckets: weekCalendarDates("2026-11-23"),
      values: [5000, null, null, null, null, null, null],
      weekly: true,
    });
  });

  it("keeps elapsed zero days as 0 while future days stay null", () => {
    const data = makeSeries(
      "2026-11-23",
      "2026-11-24",
      ["2026-11-23", "2026-11-24"],
      [
        makePoint({ totalTokens: 0, inputTokens: 0, outputTokens: 0, reasoningTokens: 0, costUsd: 0 }),
        makePoint({ totalTokens: 2000, inputTokens: 200, outputTokens: 300, reasoningTokens: 100, costUsd: 2.25 }),
      ],
    );
    expect(buildTrendPoints(data, "total", "week", "2026-11-24").values).toEqual([0, 2000, null, null, null, null, null]);
    expect(buildTrendPoints(data, "nonCache", "week", "2026-11-24").values).toEqual([0, 600, null, null, null, null, null]);
    expect(buildTrendPoints(data, "cost", "week", "2026-11-24").values).toEqual([0, 2.25, null, null, null, null, null]);
  });

  it("projects Sunday with all 7 elapsed days and no null slots", () => {
    const dates = weekCalendarDates("2026-11-29");
    const data = makeSeries("2026-11-23", "2026-11-29", dates, dates.map((_, i) => makePoint({ totalTokens: (i + 1) * 100 })));
    expect(buildTrendPoints(data, "total", "week", "2026-11-29")).toEqual({
      buckets: dates,
      values: [100, 200, 300, 400, 500, 600, 700],
      weekly: true,
    });
  });

  it("does not project stale non-week range data while switching presets", () => {
    const staleMonth = makeSeries("2026-11-01", "2026-11-24", ["2026-11-01", "2026-11-24"], [makePoint({ totalTokens: 10 }), makePoint({ totalTokens: 20 })]);
    expect(isCurrentWeekDaily(staleMonth, "week", "2026-11-24")).toBe(false);
    expect(buildTrendPoints(staleMonth, "total", "week", "2026-11-24")).toEqual({
      buckets: ["2026-11-01", "2026-11-24"],
      values: [10, 20],
      weekly: false,
    });
  });
});

describe("bucketSpan", () => {
  it("expands bucket labels to dates", () => {
    expect(bucketSpan("2026-11-10T13:00", "hour")).toEqual({ from: "2026-11-10", to: "2026-11-10" });
    expect(bucketSpan("2026-11-10", "day")).toEqual({ from: "2026-11-10", to: "2026-11-10" });
    expect(bucketSpan("2026-11-09", "week")).toEqual({ from: "2026-11-09", to: "2026-11-15" });
    expect(bucketSpan("2026-02", "month")).toEqual({ from: "2026-02-01", to: "2026-02-28" });
    expect(bucketSpan("2024-02", "month")).toEqual({ from: "2024-02-01", to: "2024-02-29" });
  });
});

describe("comparisonSpans", () => {
  it("compares the same number of elapsed days", () => {
    // 2026-11-11 is a Wednesday.
    expect(comparisonSpans("2026-11-11")).toEqual({
      thisWeek: { from: "2026-11-09", to: "2026-11-11" },
      lastWeek: { from: "2026-11-02", to: "2026-11-04" },
      thisMonth: { from: "2026-11-01", to: "2026-11-11" },
      lastMonth: { from: "2026-10-01", to: "2026-10-11" },
    });
  });

  it("clamps to the shorter previous month and handles January", () => {
    expect(comparisonSpans("2026-03-31").lastMonth).toEqual({ from: "2026-02-01", to: "2026-02-28" });
    expect(comparisonSpans("2026-01-15").lastMonth).toEqual({ from: "2025-12-01", to: "2025-12-15" });
  });

  it("Monday compares against last Monday only", () => {
    const spans = comparisonSpans("2026-11-09");
    expect(spans.thisWeek).toEqual({ from: "2026-11-09", to: "2026-11-09" });
    expect(spans.lastWeek).toEqual({ from: "2026-11-02", to: "2026-11-02" });
  });
});

describe("findAnomalies", () => {
  const dates = Array.from({ length: 10 }, (_, i) => `2026-11-${String(i + 1).padStart(2, "0")}`);

  it("needs at least seven active days", () => {
    expect(findAnomalies(dates.slice(0, 6), [1e6, 1e6, 1e6, 1e6, 1e6, 9e6])).toEqual({ median: 0, days: [] });
  });

  it("flags days at least 3x the median and 100K above it", () => {
    const totals = [1e6, 1e6, 1e6, 0, 1e6, 1e6, 1e6, 3.5e6, 2.9e6, 1e6];
    const result = findAnomalies(dates, totals);
    expect(result.median).toBe(1e6);
    expect(result.days).toEqual([{ date: "2026-11-08", totalTokens: 3.5e6, ratio: 3.5 }]);
  });

  it("ignores tiny absolute jumps", () => {
    const totals = [10_000, 10_000, 10_000, 10_000, 10_000, 10_000, 10_000, 50_000, 10_000, 10_000];
    expect(findAnomalies(dates, totals).days).toEqual([]);
  });
});

describe("validDisplayName", () => {
  it("accepts 1–64 visible characters", () => {
    expect(validDisplayName("MacBook Pro")).toBe(true);
    expect(validDisplayName("  办公室台式机  ")).toBe(true);
    expect(validDisplayName("字".repeat(64))).toBe(true);
  });

  it("rejects empty, too long and control characters", () => {
    expect(validDisplayName("   ")).toBe(false);
    expect(validDisplayName("字".repeat(65))).toBe(false);
    expect(validDisplayName("a\u0007b")).toBe(false);
  });
});

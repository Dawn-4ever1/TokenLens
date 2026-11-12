import { describe, expect, it } from "vitest";
import { validDisplayName } from "./Devices";
import { autoBucket } from "./Overview";
import { bucketSpan, comparisonSpans, findAnomalies } from "./Trends";

describe("autoBucket", () => {
  it("picks a bucket from the span length", () => {
    expect(autoBucket({ from: "2026-11-11", to: "2026-11-11" }, undefined, "2026-11-11")).toBe("hour");
    expect(autoBucket({ from: "2026-10-13", to: "2026-11-11" }, undefined, "2026-11-11")).toBe("day");
    expect(autoBucket({}, "2025-01-01", "2026-11-11")).toBe("week");
    expect(autoBucket({}, "2023-01-01", "2026-11-11")).toBe("month");
    expect(autoBucket({}, undefined, "2026-11-11")).toBe("hour");
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

import { describe, expect, it } from "vitest";
import { DEFAULT_FILTERS, activeFilterCount, addDays, daysBetween, filterQuery, previousSpan, resolveRange, todayIn } from "./range";

const now = new Date("2026-11-10T20:30:00Z");

describe("dates", () => {
  it("uses the dashboard time zone for today", () => {
    expect(todayIn("Asia/Shanghai", now)).toBe("2026-11-11");
    expect(todayIn("UTC", now)).toBe("2026-11-10");
  });

  it("adds days across month and year ends", () => {
    expect(addDays("2026-02-28", 1)).toBe("2026-03-01");
    expect(addDays("2026-01-01", -1)).toBe("2025-12-31");
    expect(daysBetween("2026-10-12", "2026-11-10")).toBe(29);
  });
});

describe("resolveRange", () => {
  const tz = "Asia/Shanghai";
  it("presets end today, inclusive", () => {
    expect(resolveRange({ ...DEFAULT_FILTERS, preset: "today" }, tz, now)).toEqual({ from: "2026-11-11", to: "2026-11-11" });
    expect(resolveRange({ ...DEFAULT_FILTERS, preset: "7d" }, tz, now)).toEqual({ from: "2026-11-05", to: "2026-11-11" });
    expect(resolveRange({ ...DEFAULT_FILTERS, preset: "30d" }, tz, now)).toEqual({ from: "2026-10-13", to: "2026-11-11" });
    expect(resolveRange({ ...DEFAULT_FILTERS, preset: "month" }, tz, now)).toEqual({ from: "2026-11-01", to: "2026-11-11" });
    expect(resolveRange({ ...DEFAULT_FILTERS, preset: "all" }, tz, now)).toEqual({});
  });

  it("swaps a reversed custom range", () => {
    expect(resolveRange({ ...DEFAULT_FILTERS, preset: "custom", customFrom: "2026-11-05", customTo: "2026-11-01" }, tz, now)).toEqual({
      from: "2026-11-01",
      to: "2026-11-05",
    });
  });

  it("previous span has the same length and ends the day before", () => {
    expect(previousSpan({ from: "2026-11-05", to: "2026-11-11" })).toEqual({ from: "2026-10-29", to: "2026-11-04" });
    expect(previousSpan({})).toBeNull();
  });
});

describe("filterQuery", () => {
  it("joins lists and omits empty ones", () => {
    const query = filterQuery({ ...DEFAULT_FILTERS, preset: "all", clients: ["codex", "claude"] }, "UTC");
    expect(query).toEqual({ from: undefined, to: undefined, devices: undefined, clients: "codex,claude", models: undefined, projects: undefined });
    expect(activeFilterCount({ ...DEFAULT_FILTERS, clients: ["a"], models: ["b", "c"] })).toBe(3);
  });
});

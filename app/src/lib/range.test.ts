import { describe, expect, it } from "vitest";
import { DEFAULT_FILTERS, RANGE_PRESETS, activeFilterCount, addDays, daysBetween, filterQuery, isValidDate, normalizeFilters, previousSpan, resolveRange, todayIn } from "./range";

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
    expect(resolveRange({ ...DEFAULT_FILTERS, preset: "week" }, tz, now)).toEqual({ from: "2026-11-09", to: "2026-11-11" });
    expect(resolveRange({ ...DEFAULT_FILTERS, preset: "month" }, tz, now)).toEqual({ from: "2026-11-01", to: "2026-11-11" });
    expect(resolveRange({ ...DEFAULT_FILTERS, preset: "all" }, tz, now)).toEqual({});
    expect(RANGE_PRESETS.map(({ label }) => label)).toEqual(["日", "周", "月", "总计", "自定义"]);
    expect(DEFAULT_FILTERS.preset).toBe("month");
  });

  it("starts the week on Monday in every timezone", () => {
    const monday = new Date("2026-11-09T00:30:00Z");
    const sunday = new Date("2026-11-08T15:30:00Z");
    expect(resolveRange({ ...DEFAULT_FILTERS, preset: "week" }, tz, monday)).toEqual({ from: "2026-11-09", to: "2026-11-09" });
    expect(resolveRange({ ...DEFAULT_FILTERS, preset: "week" }, tz, sunday)).toEqual({ from: "2026-11-02", to: "2026-11-08" });
    expect(resolveRange({ ...DEFAULT_FILTERS, preset: "week" }, "UTC", sunday)).toEqual({ from: "2026-11-02", to: "2026-11-08" });
  });

  it("maps saved old ranges and rejects unknown ones", () => {
    expect(normalizeFilters({ preset: "7d" })?.preset).toBe("week");
    expect(normalizeFilters({ preset: "30d" })?.preset).toBe("month");
    expect(normalizeFilters({ preset: "future" })?.preset).toBe("month");
    expect(normalizeFilters({ preset: "all", devices: "bad" })).toBeNull();
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
  it("joins lists and carries the selected device scope", () => {
    const query = filterQuery({ ...DEFAULT_FILTERS, preset: "all", clients: ["codex", "claude"] }, "UTC");
    expect(query).toEqual({ from: undefined, to: undefined, devices: undefined, deviceScope: "all", clients: "codex,claude", models: undefined, projects: undefined });
    expect(activeFilterCount({ ...DEFAULT_FILTERS, clients: ["a"], models: ["b", "c"] })).toBe(3);
    expect(filterQuery({ ...DEFAULT_FILTERS, deviceScope: "current" }, "UTC").deviceScope).toBe("current");
    expect(activeFilterCount({ ...DEFAULT_FILTERS, deviceScope: "current" })).toBe(1);
    expect(filterQuery({ ...DEFAULT_FILTERS, deviceScope: "all", devices: ["dev_work"] }, "UTC").devices).toBe("dev_work");
    expect(activeFilterCount({ ...DEFAULT_FILTERS, deviceScope: "all", devices: ["dev_work"] })).toBe(1);
  });

  it("restores a valid saved scope and falls back safely", () => {
    expect(normalizeFilters({ preset: "month", deviceScope: "current" })?.deviceScope).toBe("current");
    expect(normalizeFilters({ preset: "month", deviceScope: "other" })?.deviceScope).toBe("all");
    expect(normalizeFilters({ preset: "month", deviceScope: "foreign" })?.deviceScope).toBe("all");
  });

  it("ignores default current scope in local mode and strips invalid custom dates", () => {
    expect(activeFilterCount({ ...DEFAULT_FILTERS, deviceScope: "current" }, true)).toBe(0);
    expect(activeFilterCount({ ...DEFAULT_FILTERS, deviceScope: "current", clients: ["claude"] }, true)).toBe(1);
    expect(isValidDate("2026-11-10")).toBe(true);
    expect(isValidDate("2026-02-31")).toBe(false);
    expect(isValidDate("1999-12-31")).toBe(false);
    expect(isValidDate("not-a-date")).toBe(false);
    expect(normalizeFilters({ preset: "custom", customFrom: "2026-02-31", customTo: "1999-01-01" })).toEqual({
      ...DEFAULT_FILTERS,
      preset: "custom",
      customFrom: undefined,
      customTo: undefined,
    });
  });
});

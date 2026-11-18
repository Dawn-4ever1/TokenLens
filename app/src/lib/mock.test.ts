import { describe, expect, it, vi } from "vitest";
import type { Breakdown, Devices, SessionPage, Summary, Timeseries } from "./types";

vi.hoisted(() => {
  history.replaceState(null, "", "/?server");
});

const { command } = await import("./mock");

const get = <T,>(path: string, query: Record<string, string> = {}) => command("dashboard_get", { path, query }) as Promise<T>;

const parts = (b: Breakdown) => b.inputTokens + b.outputTokens + b.cacheReadTokens + b.cacheWriteTokens + b.reasoningTokens;

describe("mock backend keeps the API invariants", () => {
  it("summary total equals its parts and every slice dimension", async () => {
    const summary = await get<Summary>("summary", {});
    expect(summary.totalTokens).toBeGreaterThan(0);
    expect(parts(summary)).toBe(summary.totalTokens);
    for (const slices of [summary.byClient, summary.byModel, summary.byDevice, summary.byProject]) {
      expect(slices.reduce((sum, s) => sum + s.totalTokens, 0)).toBe(summary.totalTokens);
    }
  });

  it("timeseries buckets add up to the summary for the same range", async () => {
    const range = { from: "2026-10-01", to: "2026-11-30" };
    const summary = await get<Summary>("summary", range);
    const series = await get<Timeseries>("timeseries", { ...range, bucket: "day", groupBy: "client" });
    const total = series.series.reduce((sum, s) => sum + s.points.reduce((acc, p) => acc + p.totalTokens, 0), 0);
    expect(total).toBe(summary.totalTokens);
  });

  it("sessions never show a part larger than the total", async () => {
    const page = await get<SessionPage>("sessions", { limit: "200" });
    expect(page.sessions.length).toBeGreaterThan(0);
    for (const s of page.sessions) {
      expect(parts(s)).toBe(s.totalTokens);
      expect(s.cacheReadTokens).toBeLessThanOrEqual(s.totalTokens);
      expect(s.startedAt <= s.lastUsedAt).toBe(true);
    }
  });

  it("device scopes include, exclude and combine with other filters", async () => {
    const range = { from: "2026-06-01", to: "2026-11-30" };
    const all = await get<Summary>("summary", { ...range, deviceScope: "all" });
    const current = await get<Summary>("summary", { ...range, deviceScope: "current" });
    const other = await get<Summary>("summary", { ...range, deviceScope: "other" });
    expect(current.totalTokens).toBeGreaterThan(0);
    expect(other.totalTokens).toBeGreaterThan(0);
    expect(current.totalTokens + other.totalTokens).toBe(all.totalTokens);
    expect(current.byDevice.every((device) => device.key === "dev_mock_mbp")).toBe(true);
    expect(other.byDevice.every((device) => device.key !== "dev_mock_mbp")).toBe(true);
    const combined = await get<Summary>("summary", { ...range, deviceScope: "other", clients: "codex" });
    expect(combined.byClient.map((client) => client.key)).toEqual(["codex"]);
    expect(combined.byDevice.every((device) => device.key !== "dev_mock_mbp")).toBe(true);
    const devices = await get<Devices>("devices", { deviceScope: "other" });
    expect(devices.devices.length).toBeGreaterThan(0);
    expect(devices.devices.every((device) => !device.isCurrent)).toBe(true);
  });

  it("pagination walks every session exactly once", async () => {
    const seen = new Set<string>();
    let cursor: string | undefined;
    let total = 0;
    do {
      const page: SessionPage = await get<SessionPage>("sessions", { limit: "40", ...(cursor ? { cursor } : {}) });
      total = page.total;
      for (const s of page.sessions) {
        expect(seen.has(s.sessionKey)).toBe(false);
        seen.add(s.sessionKey);
      }
      cursor = page.nextCursor;
    } while (cursor);
    expect(seen.size).toBe(total);
  });
});

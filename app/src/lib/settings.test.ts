import { describe, expect, it } from "vitest";
import { normalizeSettings } from "./settings";

describe("normalizeSettings", () => {
  it("clamps corrupt values to safe defaults while keeping valid partial overrides", () => {
    expect(normalizeSettings(null)).toBeNull();
    expect(normalizeSettings([])).toBeNull();
    expect(normalizeSettings({ theme: "light", direction: "paper", refreshSeconds: 0 })).toEqual({
      theme: "light",
      direction: "paper",
      refreshSeconds: 0,
      exactNumbers: false,
    });
    expect(
      normalizeSettings({ theme: "neon", direction: "broken", refreshSeconds: Number.NaN, exactNumbers: "yes" }),
    ).toEqual({
      theme: "system",
      direction: "editorial",
      refreshSeconds: 60,
      exactNumbers: false,
    });
  });
});

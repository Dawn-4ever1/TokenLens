import { describe, expect, it } from "vitest";
import { drillStep } from "./nav";
import { DEFAULT_FILTERS, type Filters } from "./range";

const base: Filters = { ...DEFAULT_FILTERS, preset: "all" };

describe("drillStep", () => {
  it("narrows on drill-down and restores the earlier filters when leaving", () => {
    const down = drillStep(null, base, "sessions", { clients: ["claude"] });
    expect(down.filters).toEqual({ ...base, clients: ["claude"] });
    const back = drillStep(down.drill, down.filters!, "overview");
    expect(back.filters).toEqual(base);
    expect(back.drill).toBeNull();
  });

  it("keeps filters the user changed after drilling down", () => {
    const down = drillStep(null, base, "sessions", { clients: ["claude"] });
    const edited: Filters = { ...down.filters!, clients: ["claude", "codex"] };
    const back = drillStep(down.drill, edited, "overview");
    expect(back.filters).toBeNull();
    expect(back.drill).toBeNull();
  });

  it("does nothing while staying on the drilled page or without a drill-down", () => {
    const down = drillStep(null, base, "sessions", { clients: ["claude"] });
    expect(drillStep(down.drill, down.filters!, "sessions")).toEqual({ drill: down.drill, filters: null });
    expect(drillStep(null, base, "trends")).toEqual({ drill: null, filters: null });
  });

  it("returns to the original filters after two drill-downs in a row", () => {
    const first = drillStep(null, base, "sessions", { clients: ["claude"] });
    const second = drillStep(first.drill, first.filters!, "sessions", { preset: "custom", customFrom: "2026-11-01", customTo: "2026-11-01" });
    const back = drillStep(second.drill, second.filters!, "overview");
    expect(back.filters).toEqual(base);
  });
});

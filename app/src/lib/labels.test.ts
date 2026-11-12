import { describe, expect, it } from "vitest";
import { CLIENT_POPULARITY, clientIcon, clientLabel, coverageColumns, modelBrand, modelLabel } from "./labels";

describe("identity marks", () => {
  it("keeps known agent marks stable and gives unknown agents a fallback", () => {
    expect(clientIcon("claude")).toBe("claude");
    expect(clientIcon("codex")).toBe("openai");
    expect(clientIcon("openclaw")).toBe("openclaw");
    expect(clientIcon("dsh")).toBe("deepseek");
    expect(clientIcon("qoder-intl")).toBe("qoder");
    expect(clientIcon("workbuddy")).toBe("workbuddy");
    expect(clientIcon("antigravity-extension")).toBe("antigravity");
    expect(clientLabel("antigravity-extension")).toBe("Antigravity");
    expect(clientIcon("new-agent")).toBe("bot");
  });

  it("resolves model vendors from common model ids", () => {
    expect(modelBrand("anthropic/claude-3-5-sonnet")).toMatchObject({ label: "Anthropic", icon: "claude" });
    expect(modelBrand("gpt-4o")).toMatchObject({ label: "OpenAI", icon: "openai" });
    expect(modelBrand("google/gemini-2.5-pro")).toMatchObject({ label: "Google", icon: "gemini" });
    expect(modelBrand("glm-4.6")).toMatchObject({ label: "智谱", icon: "zai" });
    expect(modelBrand("kimi-k2-0905")).toMatchObject({ label: "Moonshot", icon: "kimi" });
    expect(modelBrand("qoder-auto")).toMatchObject({ label: "Qoder", icon: "qoder" });
    expect(modelBrand("some-new-model")).toMatchObject({ label: "模型厂商未知", icon: "cpu" });
  });

  it("still normalizes the visible model name", () => {
    expect(modelLabel("openai/gpt-4o-20240806")).toBe("gpt-4o");
  });
});

describe("agent coverage column order", () => {
  const device = (found: string[], all: string[]) => ({ clients: all.map((client) => ({ client, detected: found.includes(client) })) });
  const all = ["amp", "claude", "cline", "codex", "cursor", "dsh", "trae", "zcode", "zz-new-agent", "aa-new-agent"];

  it("puts agents installed or used on any device first, most tokens first", () => {
    const { used, unused } = coverageColumns({
      clients: all,
      devices: [device(["dsh", "zcode"], all), device(["codex"], all)],
      usage: [
        { byClient: [{ key: "dsh", totalTokens: 50 }, { key: "zcode", totalTokens: 900 }] },
        { byClient: [{ key: "codex", totalTokens: 300 }] },
      ],
    });
    expect(used).toEqual(["zcode", "codex", "dsh"]);
    expect(unused.slice(0, 4)).toEqual(["claude", "cursor", "cline", "trae"]);
  });

  it("counts an agent found on disk as used even with no tokens in the range", () => {
    const { used } = coverageColumns({ clients: all, devices: [device(["amp", "claude"], all)], usage: [{ byClient: [] }] });
    // Same token count (zero), so popularity decides.
    expect(used).toEqual(["claude", "amp"]);
  });

  it("counts usage in the range even when the latest scan did not find the agent", () => {
    const { used } = coverageColumns({ clients: all, devices: [device([], all)], usage: [{ byClient: [{ key: "trae", totalTokens: 1 }] }] });
    expect(used).toEqual(["trae"]);
  });

  it("orders unused agents by popularity, unknown ones last by name", () => {
    const { used, unused } = coverageColumns({ clients: all, devices: [device([], all)], usage: [] });
    expect(used).toEqual([]);
    expect(unused).toEqual(["claude", "codex", "cursor", "cline", "trae", "amp", "zcode", "dsh", "aa-new-agent", "zz-new-agent"]);
  });

  it("keeps the popularity list free of duplicates", () => {
    expect(new Set(CLIENT_POPULARITY).size).toBe(CLIENT_POPULARITY.length);
  });
});

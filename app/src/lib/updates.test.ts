import { describe, expect, it } from "vitest";
import { command, mockUpdateStatus } from "./mock";
import { downloadPercent, updateInProgress } from "./updates";
import type { UpdateStatus } from "./types";

describe("updater state shown by the UI", () => {
  it("supplies available, latest, downloading and failure fixtures", () => {
    const available = mockUpdateStatus("");
    expect(available.phase).toBe("available");
    expect(available.version).toBe("0.4.1");
    expect(available.notes).toContain("配对、数据和设置保持不变");
    expect(mockUpdateStatus("latest").version).toBeNull();
    expect(mockUpdateStatus("latest").phase).toBe("latest");
    expect(mockUpdateStatus("error").error).toContain("连不上 GitHub");
    expect(mockUpdateStatus("error").version).toBeNull();
  });

  it("keeps unknown lengths indeterminate and clamps download progress", () => {
    const downloading = mockUpdateStatus("downloading");
    expect(downloadPercent(downloading)).toBe(42);
    expect(downloadPercent({ ...downloading, totalBytes: null })).toBeNull();
    expect(downloadPercent({ ...downloading, totalBytes: 0 })).toBeNull();
    expect(downloadPercent({ ...downloading, downloadedBytes: -100 })).toBe(0);
    expect(downloadPercent({ ...downloading, downloadedBytes: 20_000_000 })).toBe(100);
  });

  it("locks install actions only during download and installation", () => {
    expect(updateInProgress(mockUpdateStatus("downloading"))).toBe(true);
    expect(updateInProgress({ ...mockUpdateStatus(""), phase: "installing" })).toBe(true);
    expect(updateInProgress(mockUpdateStatus(""))).toBe(false);
    expect(updateInProgress(mockUpdateStatus("error"))).toBe(false);
    expect(updateInProgress(null)).toBe(false);
  });

  it("manual checking still works when automatic checking is off", async () => {
    const disabled = await command("set_auto_check_updates", { enabled: false }) as UpdateStatus;
    expect(disabled.autoCheck).toBe(false);
    const latest = await command("check_update", {}) as UpdateStatus;
    expect(latest.phase).toBe("latest");
    expect(latest.autoCheck).toBe(false);
    await command("set_auto_check_updates", { enabled: true });
  });
});

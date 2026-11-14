import { StrictMode } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { StatusProvider, useStatus } from "./status";
import type { AppStatus } from "./types";

const api = vi.hoisted(() => ({ status: vi.fn(), refreshAccount: vi.fn() }));
vi.mock("./api", () => ({ app: api, isTauri: () => false }));

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function localStatus(accountEmail: string | null = null): AppStatus {
  return {
    paired: true, mode: "local", deviceId: "dev_fixture", server: null,
    appVersion: "0.0.0", autostart: false, platform: "macos", cloudServer: "https://example.test",
    collector: {
      running: true, paused: false, lastScanAt: null, lastUploadAt: null,
      lastHeartbeatAt: null, backlog: 0, uploadState: null, backfillComplete: true, lastError: null,
    },
    accountEmail, accountDisplayName: accountEmail ? "Restored account" : null,
    accountAvatarUrl: null, accountSpace: null,
  };
}

function StatusView() {
  const { status, error, set } = useStatus();
  return <>
    <output data-testid="account">{status ? status.accountEmail ?? "signed out" : "loading"}</output>
    <output data-testid="collector">{status?.collector.running ? "running" : "waiting"}</output>
    <output data-testid="error">{error ? "error" : "ok"}</output>
    <button onClick={() => set(localStatus("new@example.test"))}>Sign in</button>
    <button onClick={() => set(localStatus())}>Sign out</button>
  </>;
}

async function flush() { await act(async () => { await Promise.resolve(); }); }

beforeEach(() => {
  vi.useFakeTimers();
  api.status.mockReset().mockResolvedValue(localStatus());
  api.refreshAccount.mockReset();
});

afterEach(() => { cleanup(); vi.useRealTimers(); });

describe("saved account recovery", () => {
  it("restores once through StrictMode while local state remains available, without polling the account", async () => {
    const restoring = deferred<string | null>();
    api.refreshAccount.mockReturnValue(restoring.promise);
    render(<StrictMode><StatusProvider><StatusView /></StatusProvider></StrictMode>);
    await flush();
    expect(api.refreshAccount).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId("account").textContent).toBe("signed out");
    expect(screen.getByTestId("collector").textContent).toBe("running");

    api.status.mockResolvedValue(localStatus("saved@example.test"));
    await act(async () => { restoring.resolve("saved@example.test"); });
    expect(screen.getByTestId("account").textContent).toBe("saved@example.test");
    const localReads = api.status.mock.calls.length;
    await act(async () => { vi.advanceTimersByTime(15_000); });
    expect(api.status.mock.calls.length).toBeGreaterThan(localReads);
    expect(api.refreshAccount).toHaveBeenCalledTimes(1);
  });

  it("keeps local state usable when account recovery fails", async () => {
    const restoring = deferred<string | null>();
    api.refreshAccount.mockReturnValue(restoring.promise);
    render(<StatusProvider><StatusView /></StatusProvider>);
    await flush();
    await act(async () => { restoring.reject(new Error("offline")); });
    expect(screen.getByTestId("collector").textContent).toBe("running");
    expect(screen.getByTestId("error").textContent).toBe("ok");
    await act(async () => { vi.advanceTimersByTime(5_000); });
    expect(api.refreshAccount).toHaveBeenCalledTimes(1);
  });

  it.each(["Sign in", "Sign out"])("does not replace a concurrent %s with startup account data", async (action) => {
    const restoring = deferred<string | null>();
    api.refreshAccount.mockReturnValue(restoring.promise);
    render(<StatusProvider><StatusView /></StatusProvider>);
    await flush();
    fireEvent.click(screen.getByText(action));
    const localReads = api.status.mock.calls.length;
    api.status.mockResolvedValue(localStatus("old@example.test"));
    await act(async () => { restoring.resolve("old@example.test"); });
    expect(api.status).toHaveBeenCalledTimes(localReads);
    expect(screen.getByTestId("account").textContent).toBe(action === "Sign in" ? "new@example.test" : "signed out");
  });

  it("ignores account recovery after the provider has unmounted", async () => {
    const restoring = deferred<string | null>();
    api.refreshAccount.mockReturnValue(restoring.promise);
    const view = render(<StatusProvider><StatusView /></StatusProvider>);
    await flush();
    const localReads = api.status.mock.calls.length;
    view.unmount();
    await act(async () => { restoring.resolve("saved@example.test"); });
    expect(api.status).toHaveBeenCalledTimes(localReads);
  });
});

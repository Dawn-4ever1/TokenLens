import { Component, StrictMode, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { TipProvider } from "./components/ui";
import "./index.css";
import { SettingsProvider } from "./lib/settings";
import { StatusProvider } from "./lib/status";
import { UpdateProvider } from "./lib/updates";

class ErrorBoundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  render() {
    if (this.state.failed) {
      return (
        <div className="flex h-full items-center justify-center p-6">
          <div className="card max-w-md space-y-4 p-6 text-center">
            <h1 className="display text-lg text-ink">页面出现了意外错误</h1>
            <p className="text-sm leading-6 text-ink-2">这不会影响后台采集和本机记录，点击下方按钮即可恢复界面。</p>
            <button
              type="button"
              onClick={() => window.location.reload()}
              className="inline-flex h-9 items-center justify-center rounded-[var(--radius-control)] bg-primary px-4 text-[13px] font-medium text-primary-ink hover:opacity-90"
            >
              重新加载
            </button>
          </div>
        </div>
      );
    }
    return this.props.children;
  }
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ErrorBoundary>
      <SettingsProvider>
        <TipProvider>
          <StatusProvider>
            <UpdateProvider>
              <App />
            </UpdateProvider>
          </StatusProvider>
        </TipProvider>
      </SettingsProvider>
    </ErrorBoundary>
  </StrictMode>,
);

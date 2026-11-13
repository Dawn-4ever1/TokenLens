import { Shell } from "@/components/Shell";
import { ErrorState } from "@/components/ui";
import { errorMessage } from "@/lib/api";
import { DataProvider } from "@/lib/data";
import { NavProvider } from "@/lib/nav";
import { useStatus } from "@/lib/status";
import { Pairing } from "@/pages/Pairing";

export function App() {
  const { status, error, refresh } = useStatus();
  if (!status) {
    if (error) return <ErrorState message={errorMessage(error)} onRetry={() => void refresh()} />;
    return <div data-tauri-drag-region className="h-full" />;
  }
  if (!status.paired) return <Pairing />;
  const sourceKey = `${status.mode}:${status.mode === "server" ? status.server ?? "" : ""}:${status.deviceId ?? ""}`;
  return (
    <DataProvider key={sourceKey} enabled sourceKey={sourceKey}>
      <NavProvider>
        <Shell />
      </NavProvider>
    </DataProvider>
  );
}

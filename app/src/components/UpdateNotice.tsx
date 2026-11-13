import { ArrowDownToLine, LoaderCircle } from "lucide-react";
import { useEffect, useRef } from "react";
import { useNav } from "@/lib/nav";
import { downloadPercent, updateInProgress, useUpdates } from "@/lib/updates";
import type { UpdateStatus } from "@/lib/types";
import { Button, Dialog, Tip } from "./ui";

/**
 * Release notes arrive as untrusted plain text. Bullet lines become one short
 * point per row so an update reads at a glance; anything else stays a paragraph.
 */
function NoteBody({ text }: { text: string }) {
  const lines = text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
  if (lines.length >= 2 && lines.every((line) => line.startsWith("• "))) {
    return (
      <ul className="space-y-1.5">
        {lines.map((line) => (
          <li key={line} className="flex gap-2">
            <span aria-hidden className="mt-[9px] h-1 w-1 shrink-0 rounded-full bg-ink-3" />
            <span>{line.slice(2)}</span>
          </li>
        ))}
      </ul>
    );
  }
  return <p className="whitespace-pre-wrap">{text}</p>;
}

/** Kept in the bottom status area, including a named icon in the narrow sidebar. */
export function UpdateNotice() {
  const { status, setDialogOpen } = useUpdates();
  if (!status?.version) return null;
  const label = `有新版本 ${status.version}`;
  return (
    <Tip content={label} side="right">
      <button
        type="button"
        aria-label={label}
        onClick={() => setDialogOpen(true)}
        className="mb-1 flex min-h-8 w-full items-center justify-center gap-2 rounded-lg bg-primary-soft px-2 text-xs font-medium text-primary min-[1100px]:justify-start"
      >
        <ArrowDownToLine className="h-3.5 w-3.5 shrink-0" aria-hidden />
        <span className="hidden min-w-0 truncate min-[1100px]:inline">{label}</span>
      </button>
    </Tip>
  );
}

function bytes(value: number): string {
  return `${(Math.max(0, value) / 1024 / 1024).toFixed(1)} MB`;
}

export function UpdateProgress({ status }: { status: UpdateStatus }) {
  const percent = downloadPercent(status);
  if (status.phase === "installing") {
    return <p role="status" className="flex items-center gap-2 text-xs leading-5 text-ink-2"><LoaderCircle className="h-3.5 w-3.5 animate-spin" aria-hidden />正在安装，完成后会自动重启…</p>;
  }
  if (status.phase !== "downloading") return null;
  return (
    <div role="status" className="space-y-2 text-xs text-ink-2">
      <div className="flex flex-wrap justify-between gap-1">
        <span>正在下载更新{percent !== null ? ` ${percent}%` : "…"}</span>
        <span>{bytes(status.downloadedBytes)}{status.totalBytes ? ` / ${bytes(status.totalBytes)}` : ""}</span>
      </div>
      <div
        role="progressbar"
        aria-label="更新下载进度"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={percent ?? undefined}
        className="h-1.5 overflow-hidden rounded-full bg-line"
      >
        <div className="h-full rounded-full bg-primary transition-[width]" style={{ width: percent === null ? "20%" : `${percent}%` }} />
      </div>
    </div>
  );
}

export function UpdateDialog() {
  const { status, busy, dialogOpen, setDialogOpen, install } = useUpdates();
  const { go } = useNav();
  const previousPhase = useRef(status?.phase);
  useEffect(() => {
    const wasProgressing = previousPhase.current === "downloading" || previousPhase.current === "installing";
    previousPhase.current = status?.phase;
    if (dialogOpen && wasProgressing && status?.phase === "error") {
      // The command may return as soon as a Rust background task starts. Observe
      // later download/signature/install failures too, not only its first reply.
      setDialogOpen(false);
      go("settings");
    }
  }, [status?.phase, dialogOpen, setDialogOpen, go]);
  if (!status?.version) return null;
  const progressing = updateInProgress(status);
  const startInstall = async () => {
    const next = await install();
    if (next.phase === "error") {
      // Failures belong only to Settings: no alert or system notification.
      setDialogOpen(false);
      go("settings");
    }
  };
  return (
    <Dialog open={dialogOpen} onOpenChange={setDialogOpen} title={`更新到 ${status.version}`}>
      <p className="text-xs leading-5 text-ink-3">更新会保留配对、数据和设置。下载完成后会短暂停止采集，安装并自动重启。</p>
      {/* Release notes are untrusted plain text, never interpreted as HTML or links. */}
      <div className="selectable my-4 max-h-[min(40vh,280px)] overflow-y-auto break-words rounded-lg border border-line bg-surface-2/50 px-4 py-3 text-[13px] leading-6 text-ink-2">
        {status.notes?.trim() ? <NoteBody text={status.notes.trim()} /> : "这个版本没有提供更新说明。"}
      </div>
      <UpdateProgress status={status} />
      <div className="mt-5 flex flex-wrap justify-end gap-2">
        <Button onClick={() => setDialogOpen(false)}>{progressing ? "关闭" : "稍后"}</Button>
        <Button variant="primary" disabled={busy || progressing} onClick={() => void startInstall()}>
          {busy || progressing ? <LoaderCircle className="h-3.5 w-3.5 animate-spin" aria-hidden /> : <ArrowDownToLine className="h-3.5 w-3.5" aria-hidden />}
          {status.phase === "installing" ? "正在安装" : status.phase === "downloading" ? "正在下载" : status.phase === "checking" ? "正在检查" : "下载并更新"}
        </Button>
      </div>
    </Dialog>
  );
}

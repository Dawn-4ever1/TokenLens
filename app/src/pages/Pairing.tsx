import { HardDrive, ShieldCheck } from "lucide-react";
import { useState, type FormEvent } from "react";
import { Button, Field, Segmented, cn, inputClass } from "@/components/ui";
import { app, errorMessage } from "@/lib/api";
import { formFactorLabel, platformLabel } from "@/lib/labels";
import { useStatus } from "@/lib/status";
import { ApiError } from "@/lib/types";

type FormFactor = "laptop" | "desktop";
/** Local mode (D044) keeps the data on this computer; server mode pairs with one. */
type Mode = "local" | "server";

function pairError(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.kind === "unauthorized" || error.kind === "not_found") return "配对码无效或已过期。请在已配对的电脑上重新生成一个（10 分钟内有效）。";
    if (error.kind === "bad_request") return "服务器地址或配对码格式不对，请检查后重试。";
  }
  return errorMessage(error);
}

function localError(error: unknown): string {
  if (error instanceof ApiError && error.kind === "server") return "无法在这台电脑上建立本机数据库，请稍后重试。";
  return errorMessage(error);
}

export function Pairing() {
  const { status, set } = useStatus();
  const platform = status?.platform ?? "macos";
  const [mode, setMode] = useState<Mode>("local");
  const [server, setServer] = useState(status?.server ?? "");
  const [code, setCode] = useState("");
  const [formFactor, setFormFactor] = useState<FormFactor>("laptop");
  const [name, setName] = useState<string | null>(null);
  const [autostart, setAutostart] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const defaultName = `${platformLabel(platform)} ${formFactorLabel(formFactor)}`;
  const displayName = (name ?? defaultName).trim();
  const nameOk = displayName.length > 0 && displayName.length <= 64;
  const serverValid = /^https?:\/\/[^\s/]+/i.test(server.trim());
  const ready = nameOk && (mode === "local" || (serverValid && code.trim().length >= 6));

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!ready || busy) return;
    setBusy(true);
    setError(null);
    try {
      const next =
        mode === "local"
          ? await app.useLocal({ formFactor, name: displayName, autostart })
          : await app.pair({ server: server.trim(), code: code.trim(), formFactor, name: displayName, autostart });
      setCode("");
      set(next);
    } catch (next) {
      setError(mode === "local" ? localError(next) : pairError(next));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex h-full flex-col">
      <div data-tauri-drag-region className="h-12 shrink-0" />
      <div className="flex min-h-0 flex-1 items-start justify-center overflow-y-auto px-6 pb-10">
        <div className="w-full max-w-[460px] pt-[6vh]">
          <h1 className="display text-2xl text-ink">开始使用 TokenLens</h1>
          <p className="mt-2 text-sm leading-6 text-ink-2">先选数据放在哪里，以后可以在「设置」里改。</p>
          <form className="card mt-6 space-y-5 p-6" onSubmit={submit}>
            <div>
              <span className="text-[13px] font-medium text-ink">数据放在哪里</span>
              <div className="mt-1.5">
                <Segmented
                  label="数据放在哪里"
                  value={mode}
                  onChange={(next) => {
                    setMode(next);
                    setError(null);
                  }}
                  options={[
                    { key: "local", label: "只在这台电脑上" },
                    { key: "server", label: "连接服务器" },
                  ]}
                />
              </div>
              <p className="mt-2 text-xs leading-5 text-ink-3">
                {mode === "local"
                  ? "不需要服务器，打开就能看这台电脑上的用量。以后想把几台电脑合在一起看，再改为连接服务器，已有的历史会一起带过去。"
                  : "几台电脑的用量合在一起看。在一台已经连接好的电脑上打开「设备 → 添加设备」，会得到一个 10 分钟内有效的配对码；第一台电脑的配对码由服务器部署时给出。"}
              </p>
            </div>
            {mode === "server" && (
              <>
                <Field label="服务器地址" hint="例如 https://dash.example.com">
                  <input
                    className={inputClass}
                    value={server}
                    onChange={(event) => setServer(event.target.value)}
                    placeholder="https://"
                    autoComplete="off"
                    spellCheck={false}
                    inputMode="url"
                  />
                </Field>
                <Field label="配对码" hint="只能用一次，用完即失效">
                  <input
                    className={cn(inputClass, "num font-mono tracking-[0.2em]")}
                    value={code}
                    onChange={(event) => setCode(event.target.value.toUpperCase())}
                    placeholder="XXXX-XXXX"
                    autoComplete="one-time-code"
                    spellCheck={false}
                    maxLength={32}
                  />
                </Field>
              </>
            )}
            <div>
              <span className="text-[13px] font-medium text-ink">这台电脑是</span>
              <div className="mt-1.5 flex items-center gap-3">
                <Segmented
                  label="设备形态"
                  value={formFactor}
                  onChange={setFormFactor}
                  options={[
                    { key: "laptop", label: "笔记本" },
                    { key: "desktop", label: "台式机" },
                  ]}
                />
                <span className="text-xs text-ink-3">系统已识别为 {platformLabel(platform)}</span>
              </div>
            </div>
            <Field label="显示名称" hint="在看板上用来区分不同电脑，以后可以改">
              <input className={inputClass} value={name ?? defaultName} onChange={(event) => setName(event.target.value)} maxLength={64} />
            </Field>
            <label className="flex items-start gap-2.5 text-[13px] text-ink">
              <input type="checkbox" className="mt-0.5 h-4 w-4 accent-[var(--primary)]" checked={autostart} onChange={(event) => setAutostart(event.target.checked)} />
              <span>
                开机时自动在后台运行
                <span className="mt-0.5 block text-xs text-ink-3">
                  {mode === "local"
                    ? "不弹窗口，只在菜单栏/托盘留一个小图标，关机期间的用量开机后自动补上。"
                    : "不弹窗口，只在菜单栏/托盘留一个小图标，关机期间的用量开机后自动补传。"}
                </span>
              </span>
            </label>
            {error && (
              <p role="alert" className="rounded-lg bg-bad/10 px-3 py-2 text-[13px] leading-5 text-bad">
                {error}
              </p>
            )}
            <Button type="submit" variant="primary" size="lg" className="w-full" disabled={!ready || busy}>
              {mode === "local" ? (busy ? "正在准备…" : "开始使用") : busy ? "正在配对…" : "配对"}
            </Button>
          </form>
          <p className="mt-4 flex items-start gap-2 text-xs leading-5 text-ink-3">
            {mode === "local" ? (
              <>
                <HardDrive className="mt-0.5 h-4 w-4 shrink-0" aria-hidden />
                数据只保存在这台电脑上。成本按公开价格表估算，应用每天下载一次价格表，不上传任何数据。
              </>
            ) : (
              <>
                <ShieldCheck className="mt-0.5 h-4 w-4 shrink-0" aria-hidden />
                只上传数字（token 数量、模型名、时间），不上传提示词、回答、代码或文件内容。凭据保存在系统钥匙串里，看板页面拿不到。
              </>
            )}
          </p>
        </div>
      </div>
    </div>
  );
}

import { ChevronDown, Copy, ShieldCheck } from "lucide-react";
import { useEffect, useMemo, useState, type FormEvent } from "react";
import { Button, Dialog, Field, Segmented, cn, inputClass } from "@/components/ui";
import { app, errorMessage } from "@/lib/api";
import { formFactorLabel, platformLabel } from "@/lib/labels";
import { useStatus } from "@/lib/status";
import { ApiError } from "@/lib/types";

type FormFactor = "laptop" | "desktop";
type Where = "cloud" | "own";
type Method = "github" | "email";

function loginError(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.kind === "bad_code" || error.kind === "unauthorized") {
      return "验证码不对或已过期，请重新获取一封。";
    }
    if (error.kind === "bad_email") return "邮箱格式不对，请检查后重试。";
    if (error.kind === "rate_limited") return "发送太频繁，请稍等一分钟再试。";
    if (error.kind === "unavailable") return "服务器暂时不能发信，请稍后再试。";
  }
  return errorMessage(error);
}

/** Shown only if automatic local setup failed; normal launches go straight to the dashboard. */
export function Pairing() {
  const { set } = useStatus();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const retry = async () => {
    setBusy(true);
    setError(null);
    try {
      set(await app.useLocal({ formFactor: "laptop", name: "", autostart: true }));
    } catch {
      setError("无法打开这台电脑上的本机数据库，请稍后重试。");
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="flex h-full items-center justify-center p-6">
      <div className="card max-w-md space-y-4 p-6">
        <h1 className="display text-xl text-ink">准备本机数据</h1>
        <p className="text-sm leading-6 text-ink-2">TokenLens 默认在这台电脑上记录用量。本机数据库暂时无法打开，请重试。</p>
        {error && <p role="alert" className="text-sm text-bad">{error}</p>}
        <Button variant="primary" disabled={busy} onClick={() => void retry()}>{busy ? "正在准备…" : "重试"}</Button>
      </div>
    </div>
  );
}

/** Connecting adds server sync; it never cancels local collection or deletes local data. */
export function ConnectServer({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const [busy, setBusy] = useState(false);
  // Unmount the form on close so an emailed code is never retained in a hidden form.
  return (
    <Dialog open={open} onOpenChange={(next) => !busy && onOpenChange(next)} title="连接服务器">
      {open && <ConnectForm onClose={() => onOpenChange(false)} onBusy={setBusy} />}
    </Dialog>
  );
}

function ConnectForm({ onClose, onBusy }: { onClose: () => void; onBusy: (busy: boolean) => void }) {
  const { status, set } = useStatus();
  const platform = status?.platform ?? "macos";
  const cloud = status?.cloudServer ?? "https://tokenlens.online";
  const [where, setWhere] = useState<Where>("cloud");
  const [method, setMethod] = useState<Method>("github");
  const [gh, setGh] = useState<{ userCode: string; verificationUri: string } | null>(null);
  const [ghState, setGhState] = useState<"idle" | "pending" | "expired" | "denied" | "failed">("idle");
  // A self-hosted server has no TokenLens GitHub app, so GitHub is offered
  // only for the hosted service; choosing "own" falls back to the email code.
  useEffect(() => {
    if (where === "own") {
      setMethod("email");
      setGh(null);
      setGhState("idle");
    }
  }, [where]);
  const [ownServer, setOwnServer] = useState("");
  const [email, setEmail] = useState("");
  const [code, setCode] = useState("");
  const [formFactor, setFormFactor] = useState<FormFactor>("laptop");
  const [name, setName] = useState<string | null>(null);
  const [deviceOpen, setDeviceOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [sending, setSending] = useState(false);
  const [sentAt, setSentAt] = useState<number | null>(null);
  const [now, setNow] = useState(Date.now());
  const [error, setError] = useState<string | null>(null);

  const server = where === "cloud" ? cloud : ownServer.trim();
  const defaultName = `${platformLabel(platform)} ${formFactorLabel(formFactor)}`;
  const displayName = (name ?? defaultName).trim();
  const emailOk = /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim());
  const codeOk = code.replace(/[^0-9]/g, "").length === 6;
  const serverOk = /^https?:\/\/[^\s/]+/i.test(server);
  const ready = emailOk && codeOk && serverOk && displayName.length > 0 && [...displayName].length <= 64;

  useEffect(() => {
    if (sentAt === null) return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [sentAt]);
  const waitLeft = sentAt === null ? 0 : Math.max(0, 60_000 - (now - sentAt));

  const sendCode = async () => {
    if (!emailOk || !serverOk || sending) return;
    setSending(true);
    setError(null);
    try {
      await app.requestLoginCode(server, email.trim());
      setSentAt(Date.now());
    } catch (next) {
      setError(loginError(next));
    } finally {
      setSending(false);
    }
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!ready || busy) return;
    setBusy(true);
    onBusy(true);
    setError(null);
    try {
      const next = await app.loginAccount({
        server,
        email: email.trim(),
        code: code.trim(),
        formFactor,
        name: displayName,
        autostart: status?.autostart === true,
      });
      setCode("");
      setEmail("");
      set(next);
      onClose();
    } catch (next) {
      setError(loginError(next));
    } finally {
      setBusy(false);
      onBusy(false);
    }
  };

  const startGitHub = async () => {
    if (!serverOk || busy || where !== "cloud") return;
    setBusy(true);
    setError(null);
    setGhState("idle");
    try {
      const next = await app.startGitHubLogin(server);
      setGh(next);
      setGhState("pending");
      await app.openGitHubDevicePage(next.verificationUri).catch(() => undefined);
    } catch (next) {
      setError(loginError(next));
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    if (!gh) return;
    let alive = true;
    const tick = async () => {
      try {
        const next = await app.pollGitHubLogin({
          server,
          formFactor,
          name: displayName,
          autostart: status?.autostart === true,
        });
        if (!alive) return;
        if (next.status === "ok" && next.app) {
          set(next.app);
          onClose();
          return;
        }
        if (next.status === "expired" || next.status === "denied" || next.status === "failed") {
          setGhState(next.status);
          setGh(null);
        }
      } catch (next) {
        if (alive) setError(loginError(next));
      }
    };
    void tick();
    const id = setInterval(() => void tick(), 5000);
    return () => {
      alive = false;
      clearInterval(id);
    };
    // The flow restarts only when a new device code arrives.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [gh]);

  const remaining = useMemo(() => Math.ceil(waitLeft / 1000), [waitLeft]);

  return (
    <form className="max-h-[75vh] space-y-5 overflow-y-auto" onSubmit={submit}>
      <p className="text-[13px] leading-6 text-ink-2">
        登录你的空间，多台电脑共用一个看板。第一次登录会自动创建你的空间，本机记录继续保留。
      </p>

      <div>
        <span className="text-[13px] font-medium text-ink">服务器</span>
        <div className="mt-1.5">
          <Segmented
            label="服务器"
            value={where}
            onChange={setWhere}
            options={[
              { key: "cloud", label: "TokenLens 云服务" },
              { key: "own", label: "自己的服务器" },
            ]}
          />
        </div>
        <p className="mt-1.5 text-xs leading-5 text-ink-3">
          {where === "cloud" ? `默认连接 ${cloud}` : "填写你自己的 TokenLens 服务器地址"}
        </p>
      </div>
      {where === "own" && (
        <Field label="服务器地址" hint="必须支持 HTTPS">
          <input aria-label="服务器地址" className={inputClass} value={ownServer} onChange={(event) => setOwnServer(event.target.value)} placeholder="https://" autoComplete="off" spellCheck={false} inputMode="url" />
        </Field>
      )}

      {where === "cloud" && (
        <div>
          <span className="text-[13px] font-medium text-ink">登录方式</span>
          <div className="mt-1.5">
            <Segmented
              label="登录方式"
              value={method}
              onChange={setMethod}
              options={[
                { key: "github", label: "GitHub（推荐）" },
                { key: "email", label: "邮箱验证码" },
              ]}
            />
          </div>
        </div>
      )}

      {method === "github" ? (
        <div className="space-y-4 rounded-xl border border-line bg-surface-2 px-5 py-5">
          {gh ? (
            <>
              <p className="text-[13px] leading-6 text-ink-2">
                在打开的 GitHub 页面输入下面的代码，授权后这里会自动完成登录。
              </p>
              <button
                type="button"
                className="flex w-full items-center justify-center gap-2 rounded-lg border border-line bg-surface px-4 py-3 text-center transition hover:border-primary"
                title="复制授权码"
                aria-label="复制授权码"
                onClick={() => void navigator.clipboard?.writeText(gh.userCode)}
              >
                <span className="selectable font-mono text-2xl font-semibold tracking-[0.15em] text-ink">{gh.userCode}</span>
                <Copy className="h-4 w-4 shrink-0 text-ink-3" aria-hidden />
              </button>
              <div className="flex flex-wrap justify-center gap-2">
                <Button type="button" size="sm" onClick={() => void app.openGitHubDevicePage(gh.verificationUri)}>
                  重新打开授权页
                </Button>
                <Button type="button" size="sm" variant="ghost" onClick={() => { setGh(null); setGhState("idle"); }}>
                  取消
                </Button>
              </div>
              <p className="text-center text-xs text-ink-3">等待授权…</p>
            </>
          ) : (
            <>
              <p className="text-[13px] leading-6 text-ink-2">
                用 GitHub 账号登录，不需要密码，也不会收到邮件。第一次登录会自动创建你的空间。
              </p>
              {ghState !== "idle" && (
                <p role="alert" className="text-[13px] leading-5 text-bad">
                  {ghState === "expired" ? "授权超时了，请重新开始。" : ghState === "denied" ? "你取消了授权。" : "GitHub 授权没有完成，请重试。"}
                </p>
              )}
              <Button type="button" variant="primary" className="w-full" disabled={!serverOk || busy} onClick={() => void startGitHub()}>
                {busy ? "正在打开…" : "用 GitHub 登录"}
              </Button>
            </>
          )}
        </div>
      ) : (
        <>
          <Field label="邮箱" hint="用来登录，也用来接收验证码">
            <input aria-label="邮箱" className={inputClass} value={email} onChange={(event) => setEmail(event.target.value)} placeholder="you@example.com" autoComplete="email" spellCheck={false} inputMode="email" />
          </Field>
          <div>
            <span className="text-[13px] font-medium text-ink">验证码</span>
            <div className="mt-1.5 grid grid-cols-[minmax(0,1fr)_auto] gap-2">
              <input aria-label="验证码" className={cn(inputClass, "num font-mono tracking-[0.2em]")} value={code} onChange={(event) => setCode(event.target.value.replace(/[^0-9]/g, "").slice(0, 6))} placeholder="000000" autoComplete="one-time-code" spellCheck={false} inputMode="numeric" />
              <Button type="button" className="h-9 shrink-0" disabled={!emailOk || !serverOk || sending || waitLeft > 0} onClick={() => void sendCode()}>
                {sending ? "发送中…" : waitLeft > 0 ? `${remaining} 秒后可重发` : sentAt === null ? "获取验证码" : "重新发送"}
              </Button>
            </div>
            <span className="mt-1 block text-xs leading-5 text-ink-3">
              {sentAt === null ? "6 位数字，10 分钟内有效" : "已发送，请查收邮件（也看看垃圾箱）"}
            </span>
          </div>
        </>
      )}

      <div className="rounded-xl border border-line">
        <button
          type="button"
          className="flex w-full items-center justify-between gap-2 px-4 py-3 text-left"
          aria-expanded={deviceOpen}
          onClick={() => setDeviceOpen((value) => !value)}
        >
          <span className="text-[13px] font-medium text-ink">这台电脑</span>
          <span className="flex items-center gap-2 text-xs text-ink-3">
            {name ?? defaultName}
            <ChevronDown className={cn("h-4 w-4 transition-transform", deviceOpen && "rotate-180")} aria-hidden />
          </span>
        </button>
        {deviceOpen && (
          <div className="space-y-4 border-t border-line px-4 py-4">
            <div>
              <span className="text-[13px] font-medium text-ink">设备形态</span>
              <div className="mt-1.5">
                <Segmented label="设备形态" value={formFactor} onChange={setFormFactor} options={[{ key: "laptop", label: "笔记本" }, { key: "desktop", label: "台式机" }]} />
              </div>
            </div>
            <Field label="显示名称" hint="用来区分不同电脑，以后可以改">
              <input className={inputClass} value={name ?? defaultName} onChange={(event) => setName(event.target.value)} maxLength={64} />
            </Field>
          </div>
        )}
      </div>

      <p className="flex gap-2 text-xs leading-5 text-ink-3"><ShieldCheck className="mt-0.5 h-4 w-4 shrink-0" aria-hidden />只同步数字，不上传提示词、回答、代码或文件内容。登录后这台电脑已有历史会被补传。</p>
      {error && <p role="alert" className="rounded-lg bg-bad/10 px-3 py-2 text-[13px] leading-5 text-bad">{error}</p>}
      <div className="flex justify-end gap-2">
        <Button type="button" onClick={onClose} disabled={busy}>取消</Button>
        {method === "email" && (
          <Button type="submit" variant="primary" disabled={!ready || busy}>{busy ? "正在登录…" : "登录并连接"}</Button>
        )}
      </div>
    </form>
  );
}

import { Check, ChevronDown, Copy, Mail, ShieldCheck } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { Button, Dialog, Field, Segmented, cn, inputClass } from "@/components/ui";
import { app, errorMessage } from "@/lib/api";
import { AppMark } from "@/components/AppMark";
import { copyText } from "@/lib/clipboard";
import { formFactorLabel, platformLabel } from "@/lib/labels";
import { useStatus } from "@/lib/status";
import { ApiError } from "@/lib/types";

type FormFactor = "laptop" | "desktop";
type Where = "cloud" | "own";
type Method = "github" | "google" | "email";

function GitHubIcon({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" aria-hidden className={className} fill="currentColor">
      <path d="M12 .5a12 12 0 0 0-3.79 23.39c.6.11.82-.26.82-.58v-2.23c-3.34.73-4.04-1.61-4.04-1.61-.55-1.39-1.34-1.76-1.34-1.76-1.09-.74.08-.73.08-.73 1.2.09 1.84 1.24 1.84 1.24 1.07 1.84 2.81 1.31 3.5 1 .11-.78.42-1.31.76-1.61-2.66-.3-5.47-1.33-5.47-5.93 0-1.31.47-2.38 1.24-3.22-.12-.3-.54-1.52.12-3.18 0 0 1.01-.32 3.3 1.23a11.5 11.5 0 0 1 6.01 0c2.29-1.55 3.3-1.23 3.3-1.23.66 1.66.24 2.88.12 3.18.77.84 1.24 1.91 1.24 3.22 0 4.61-2.81 5.63-5.49 5.92.43.37.81 1.1.81 2.22v3.29c0 .32.21.7.82.58A12 12 0 0 0 12 .5Z" />
    </svg>
  );
}

function GoogleIcon({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" aria-hidden className={className}>
      <path fill="#4285F4" d="M23.52 12.27c0-.85-.08-1.67-.22-2.45H12v4.64h6.46a5.52 5.52 0 0 1-2.4 3.62v3h3.88c2.27-2.09 3.58-5.17 3.58-8.81Z" />
      <path fill="#34A853" d="M12 24c3.24 0 5.96-1.07 7.94-2.91l-3.88-3c-1.08.72-2.45 1.15-4.06 1.15-3.13 0-5.78-2.11-6.73-4.96H1.29v3.1A12 12 0 0 0 12 24Z" />
      <path fill="#FBBC05" d="M5.27 14.28A7.2 7.2 0 0 1 4.89 12c0-.79.14-1.56.38-2.28v-3.1H1.29a12 12 0 0 0 0 10.76l3.98-3.1Z" />
      <path fill="#EA4335" d="M12 4.76c1.76 0 3.34.61 4.58 1.8l3.44-3.44C17.95 1.19 15.24 0 12 0A12 12 0 0 0 1.29 6.62l3.98 3.1C6.22 6.87 8.87 4.76 12 4.76Z" />
    </svg>
  );
}

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
        <p className="text-sm leading-6 text-ink-2">本机设备已移除或暂时无法打开。点击下方按钮会新建本机设备并重新开始记录；已经移除的本机历史不会恢复，服务器上已有的历史不受影响。</p>
        {error && <p role="alert" className="text-sm text-bad">{error}</p>}
        <Button variant="primary" disabled={busy} onClick={() => void retry()}>{busy ? "正在准备…" : "重新开始本机记录"}</Button>
      </div>
    </div>
  );
}

/** Connecting adds server sync; it never cancels local collection or deletes local data. */
export function ConnectServer({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const [busy, setBusy] = useState(false);
  // Unmount the form on close so an emailed code is never retained in a hidden form.
  return (
    <Dialog open={open} onOpenChange={(next) => !busy && onOpenChange(next)} title="登录 TokenLens" brand={<><AppMark className="h-8 w-8" /><span className="display text-xl text-ink">TokenLens</span></>}>
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
  const [gg, setGg] = useState<{ authUrl: string } | null>(null);
  const [ggState, setGgState] = useState<"idle" | "pending" | "expired" | "denied" | "failed">("idle");
  const [ggOpening, setGgOpening] = useState(false);
  const [ggOpenError, setGgOpenError] = useState<string | null>(null);
  const [copyState, setCopyState] = useState<"idle" | "busy" | "copied" | "failed">("idle");
  const copyTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const googleGeneration = useRef(0);
  const googleStartOwner = useRef<number | null>(null);
  const invalidateGoogle = () => {
    const generation = ++googleGeneration.current;
    if (copyTimer.current) clearTimeout(copyTimer.current);
    setGgOpening(false);
    setGgOpenError(null);
    setCopyState("idle");
    return generation;
  };
  // A self-hosted server has no TokenLens GitHub or Google app, so both are
  // offered only for the hosted service; "own" falls back to the email code.
  useEffect(() => {
    if (where === "own") {
      invalidateGoogle();
      setMethod("email");
      setGh(null);
      setGhState("idle");
      setGg(null);
      setGgState("idle");
      setCopyState("idle");
      setChooser(true);
    }
  }, [where]);
  useEffect(() => () => {
    googleGeneration.current += 1;
    googleStartOwner.current = null;
    if (copyTimer.current) clearTimeout(copyTimer.current);
  }, []);
  const [ownServer, setOwnServer] = useState("");
  const [email, setEmail] = useState("");
  const [code, setCode] = useState("");
  const [formFactor, setFormFactor] = useState<FormFactor>("laptop");
  const [name, setName] = useState<string | null>(null);
  const [deviceOpen, setDeviceOpen] = useState(false);
  const [chooser, setChooser] = useState(true);
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
      setCopyState("idle");
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
          setCopyState("idle");
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

  const openGoogle = async (authUrl: string, generation = googleGeneration.current) => {
    if (generation !== googleGeneration.current) return;
    setGgOpening(true);
    setGgOpenError(null);
    try {
      await app.openGoogleAuthPage(authUrl);
    } catch {
      if (generation === googleGeneration.current) setGgOpenError("未能打开浏览器，请重试或复制链接到浏览器打开。");
    } finally {
      if (generation === googleGeneration.current) setGgOpening(false);
    }
  };

  const copyGoogleLink = async (authUrl: string) => {
    const generation = googleGeneration.current;
    if (copyTimer.current) clearTimeout(copyTimer.current);
    setCopyState("busy");
    try {
      await copyText(authUrl);
      if (generation !== googleGeneration.current) return;
      setCopyState("copied");
      copyTimer.current = setTimeout(() => {
        if (generation === googleGeneration.current) setCopyState("idle");
      }, 2000);
    } catch {
      if (generation === googleGeneration.current) setCopyState("failed");
    }
  };

  const startGoogle = async () => {
    if (!serverOk || busy || where !== "cloud") return;
    const generation = invalidateGoogle();
    googleStartOwner.current = generation;
    setBusy(true);
    setError(null);
    setGgState("idle");
    setGgOpenError(null);
    setCopyState("idle");
    try {
      let next: { authUrl: string };
      try {
        next = await app.startGoogleLogin(server);
      } finally {
        if (googleStartOwner.current === generation) {
          googleStartOwner.current = null;
          setBusy(false);
        }
      }
      if (generation !== googleGeneration.current) return;
      setGg(next);
      setGgState("pending");
      await openGoogle(next.authUrl, generation);
    } catch (next) {
      if (generation === googleGeneration.current) setError(loginError(next));
    }
  };

  useEffect(() => {
    if (!gg) return;
    const generation = googleGeneration.current;
    let alive = true;
    const tick = async () => {
      try {
        const next = await app.pollGoogleLogin({
          server,
          formFactor,
          name: displayName,
          autostart: status?.autostart === true,
        });
        if (!alive || generation !== googleGeneration.current) return;
        if (next.status === "ok" && next.app) {
          set(next.app);
          onClose();
          return;
        }
        if (next.status === "expired" || next.status === "denied" || next.status === "failed") {
          invalidateGoogle();
          setBusy(false);
          setGgState(next.status);
          setGg(null);
        }
      } catch (next) {
        if (alive && generation === googleGeneration.current) setError(loginError(next));
      }
    };
    void tick();
    const id = setInterval(() => void tick(), 5000);
    return () => {
      alive = false;
      clearInterval(id);
    };
    // The flow restarts only when a new consent page arrives.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [gg]);

  const remaining = useMemo(() => Math.ceil(waitLeft / 1000), [waitLeft]);

  if (chooser && !gh && !gg && where === "cloud") {
    return (
      <div className="space-y-3">
        {where === "cloud" && (
          <>
            <Button type="button" className="h-11 w-full justify-center gap-2.5 text-sm" onClick={() => { setChooser(false); setMethod("github"); }}>
              <GitHubIcon className="h-5 w-5" /> 使用 GitHub 继续
            </Button>
            <Button type="button" className="h-11 w-full justify-center gap-2.5 text-sm" onClick={() => { setChooser(false); setMethod("google"); }}>
              <GoogleIcon className="h-5 w-5" /> 使用 Google 继续
            </Button>
          </>
        )}
        <div className="flex items-center gap-3 py-2 text-xs text-ink-3" aria-hidden>
          <span className="h-px flex-1 bg-line" />或<span className="h-px flex-1 bg-line" />
        </div>
        <Button type="button" className="h-11 w-full justify-center gap-2.5 text-sm" onClick={() => { setChooser(false); setMethod("email"); }}>
          <Mail className="h-5 w-5" /> 使用邮箱继续
        </Button>
        <p className="pt-3 text-center text-xs leading-5 text-ink-3">默认连接 TokenLens 云服务，首次登录自动创建空间</p>
        {(ghState !== "idle" || ggState !== "idle") && (
          <p role="alert" className="text-center text-[13px] leading-5 text-bad">
            {ghState === "expired" || ggState === "expired" ? "授权超时了，请重新开始。"
              : ghState === "denied" || ggState === "denied" ? "你取消了授权。"
              : ghState !== "idle" ? "GitHub 授权没有完成，请重试。"
              : "Google 授权没有完成，请重试。"}
          </p>
        )}
        {error && <p role="alert" className="text-center text-[13px] leading-5 text-bad">{error}</p>}
        <button type="button" className="mx-auto block pt-1 text-xs text-ink-3 underline decoration-line underline-offset-4 hover:text-ink" onClick={() => { setWhere("own"); setChooser(false); setMethod("email"); }}>
          使用自己的服务器
        </button>
      </div>
    );
  }
  return (
    <form className="max-h-[75vh] space-y-5 overflow-y-auto" onSubmit={submit}>
      {method === "github" && !gh ? (
        <div className="space-y-4">
          <p className="text-[13px] leading-6 text-ink-2">在 GitHub 上确认授权，不需要密码，也不会收到邮件。第一次登录会自动创建你的空间。</p>
          {error && <p role="alert" className="text-[13px] leading-5 text-bad">{error}</p>}
          <Button type="button" variant="primary" className="h-11 w-full justify-center gap-2.5" disabled={!serverOk || busy} onClick={() => void startGitHub()}>
            <GitHubIcon className="h-5 w-5" /> {busy ? "正在打开…" : "继续前往 GitHub"}
          </Button>
          <button type="button" className="text-xs text-ink-3 hover:text-ink" onClick={() => setChooser(true)}>返回登录方式</button>
        </div>
      ) : method === "github" ? (
        <div className="space-y-4">
          <p className="text-[13px] leading-6 text-ink-2">在打开的 GitHub 页面输入下面的代码，授权后这里会自动完成登录。</p>
          {gh && (
            <>
              <button
                type="button"
                className="flex w-full items-center justify-center gap-2 rounded-lg border border-line bg-surface-2 px-4 py-3 text-center transition hover:border-primary"
                title="复制 GitHub 授权码"
                aria-label={copyState === "copied" ? "已复制 GitHub 授权码" : "复制 GitHub 授权码"}
                disabled={copyState === "busy"}
                onClick={async () => {
                  setCopyState("busy");
                  try {
                    await copyText(gh.userCode);
                    setCopyState("copied");
                    if (copyTimer.current) clearTimeout(copyTimer.current);
                    copyTimer.current = setTimeout(() => setCopyState("idle"), 2000);
                  } catch {
                    setCopyState("failed");
                  }
                }}
              >
                <span className="selectable font-mono text-2xl font-semibold tracking-[0.15em] text-ink">{gh.userCode}</span>
                {copyState === "copied" ? <Check className="h-4 w-4 shrink-0 text-good" aria-hidden /> : <Copy className="h-4 w-4 shrink-0 text-ink-3" aria-hidden />}
              </button>
              <p role="status" aria-live="polite" className={cn("min-h-5 text-center text-xs", copyState === "failed" ? "text-bad" : "text-good")}>
                {copyState === "failed" ? "复制失败，请选中上方的授权码手动复制。" : copyState === "copied" ? "已复制，去 GitHub 粘贴。" : ""}
              </p>
              <div className="flex flex-wrap justify-center gap-2">
                <Button type="button" size="sm" onClick={() => void app.openGitHubDevicePage(gh.verificationUri)}>重新打开授权页</Button>
                <Button type="button" size="sm" variant="ghost" onClick={() => { setGh(null); setGhState("idle"); setCopyState("idle"); }}>取消</Button>
              </div>
              <p className="text-center text-xs text-ink-3">等待授权…</p>
            </>
          )}
        </div>
      ) : method === "google" && !gg ? (
        <div className="space-y-4">
          <p className="text-[13px] leading-6 text-ink-2">在 Google 上确认授权，不需要密码，也不会收到邮件。第一次登录会自动创建你的空间。</p>
          {error && <p role="alert" className="text-[13px] leading-5 text-bad">{error}</p>}
          <Button type="button" variant="primary" className="h-11 w-full justify-center gap-2.5" disabled={!serverOk || busy} onClick={() => void startGoogle()}>
            <GoogleIcon className="h-5 w-5" /> {busy ? "正在打开…" : "继续前往 Google"}
          </Button>
          <button type="button" className="text-xs text-ink-3 hover:text-ink" onClick={() => { invalidateGoogle(); setChooser(true); }}>返回登录方式</button>
        </div>
      ) : method === "google" ? (
        <div className="space-y-4">
          <p className="text-[13px] leading-6 text-ink-2">在打开的 Google 页面完成登录，授权后这里会自动继续。</p>
          {ggOpenError && <p role="alert" className="text-[13px] leading-5 text-bad">{ggOpenError}</p>}
          {error && <p role="alert" className="text-[13px] leading-5 text-bad">{error}</p>}
          {gg && (
            <>
              <div className="flex flex-wrap justify-center gap-2">
                <Button type="button" size="sm" disabled={ggOpening} onClick={() => void openGoogle(gg.authUrl)}>{ggOpening ? "正在打开…" : "重新打开授权页"}</Button>
                <Button type="button" size="sm" disabled={copyState === "busy"} onClick={() => void copyGoogleLink(gg.authUrl)}>复制授权链接</Button>
                <Button type="button" size="sm" variant="ghost" onClick={() => { invalidateGoogle(); setGg(null); setGgState("idle"); setBusy(false); }}>取消</Button>
              </div>
              <p role="status" aria-live="polite" className={cn("min-h-5 text-center text-xs", copyState === "failed" ? "text-bad" : "text-good")}>
                {copyState === "failed" ? "复制失败，请重试。" : copyState === "copied" ? "已复制授权链接，请粘贴到浏览器打开。" : ""}
              </p>
              <p role="status" aria-live="polite" className="text-center text-xs text-ink-3">等待授权…</p>
            </>
          )}
        </div>
      ) : (
        <div className="space-y-4">
          {where === "own" && (
            <Field label="服务器地址" hint="自建服务器使用邮箱验证码登录">
              <input aria-label="服务器地址" className={inputClass} value={ownServer} onChange={(event) => setOwnServer(event.target.value)} placeholder="https://" autoComplete="off" spellCheck={false} inputMode="url" />
            </Field>
          )}
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
            <span className="mt-1 block text-xs leading-5 text-ink-3">{sentAt === null ? "6 位数字，10 分钟内有效" : "已发送，请查收邮件（也看看垃圾箱）"}</span>
          </div>
          {error && <p role="alert" className="text-[13px] leading-5 text-bad">{error}</p>}
          <Button type="submit" variant="primary" className="w-full" disabled={!ready || busy}>{busy ? "正在登录…" : "登录并连接"}</Button>
          <button type="button" className="text-xs text-ink-3 hover:text-ink" onClick={() => { if (where === "cloud") { setChooser(true); } else { setWhere("cloud"); setChooser(true); } }}>返回登录方式</button>
        </div>
      )}
      {method === "email" || gh || gg ? (
        <div className="border-t border-line/70 pt-4">
          <button type="button" className="flex w-full items-center justify-between gap-2 text-left" aria-expanded={deviceOpen} onClick={() => setDeviceOpen((value) => !value)}>
            <span className="text-xs font-medium text-ink-2">设备与隐私</span>
            <ChevronDown className={cn("h-4 w-4 text-ink-3 transition-transform", deviceOpen && "rotate-180")} aria-hidden />
          </button>
          {deviceOpen && (
            <div className="mt-4 space-y-4">
              <Segmented label="设备形态" value={formFactor} onChange={setFormFactor} options={[{ key: "laptop", label: "笔记本" }, { key: "desktop", label: "台式机" }]} />
              <Field label="显示名称"><input className={inputClass} value={name ?? defaultName} onChange={(event) => setName(event.target.value)} maxLength={64} /></Field>
              <p className="flex gap-2 text-xs leading-5 text-ink-3"><ShieldCheck className="mt-0.5 h-4 w-4 shrink-0" aria-hidden />只同步数字，不上传提示词、回答、代码或文件内容。登录后已有历史会补传。</p>
            </div>
          )}
        </div>
      ) : null}
    </form>

  );
}

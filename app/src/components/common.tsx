import {
  ArrowDownRight,
  ArrowUpRight,
  Bot,
  Circle,
  Cpu,
  Info,
  Minus,
  type LucideIcon,
} from "lucide-react";
import type { CSSProperties, ReactNode } from "react";
import { app, errorMessage } from "@/lib/api";
import { compact, exact, percent } from "@/lib/format";
import { useExactNumbers } from "@/lib/settings";
import { clientColor, clientIcon, clientLabel, isBrandIcon, modelBrand, modelLabel, precisionLabel, type BrandName, type IdentityIconName } from "@/lib/labels";
import { useStatus } from "@/lib/status";
import { ApiError, type Caveat, type Precision } from "@/lib/types";
import { BrandMark } from "./brandMarks";
import { Badge, Button, ErrorState, Tip, cn } from "./ui";

/**
 * A token count: K/M/B by default, every digit after a click. The choice is
 * shared by all counts and remembered. Inside a row that is itself clickable,
 * pass `plain` so the count follows the choice without taking the click.
 */
export function Num({ value, className, suffix, plain }: { value: number; className?: string; suffix?: string; plain?: boolean }) {
  const [showExact, toggle] = useExactNumbers();
  const short = compact(value);
  const full = exact(Math.round(value));
  const unit = suffix ? ` ${suffix}` : "";
  if (short === full) return <span className={cn("num", className)}>{short}</span>;
  const text = showExact ? full : short;
  const other = showExact ? `约 ${short}${unit}` : `精确值 ${full}${unit}`;
  // Lets a `.num-fit` container shrink a long exact count instead of overflowing.
  const fit = showExact ? ({ "--num-chars": text.length } as CSSProperties) : undefined;
  if (plain) {
    return (
      <Tip content={other}>
        <span data-exact={showExact || undefined} style={fit} className={cn("num", className)}>
          {text}
        </span>
      </Tip>
    );
  }
  return (
    <Tip
      content={
        <>
          {other}
          <div className="text-ink-3">{showExact ? "点击切回简写" : "点击显示精确数字"}</div>
        </>
      }
    >
      <button
        type="button"
        aria-pressed={showExact}
        data-exact={showExact || undefined}
        style={fit}
        onClick={(event) => {
          event.stopPropagation();
          toggle();
        }}
        className={cn("num rounded-sm decoration-current/40 decoration-dotted underline-offset-[3px] hover:underline", className)}
      >
        {text}
      </button>
    </Tip>
  );
}

/** Period-over-period change; usage going up is not "bad", so it stays neutral. */
export function Delta({ current, previous, points, label = "与上一周期相比" }: { current: number | null; previous: number | null | undefined; points?: boolean; label?: string }) {
  if (current === null || previous === null || previous === undefined) return null;
  let text: string;
  let direction: number;
  if (points) {
    const diff = (current - previous) * 100;
    direction = Math.abs(diff) < 0.05 ? 0 : Math.sign(diff);
    text = `${diff >= 0 ? "+" : ""}${diff.toFixed(1)} 个百分点`;
  } else {
    if (previous === 0) return null;
    const rate = (current - previous) / previous;
    direction = Math.abs(rate) < 0.0005 ? 0 : Math.sign(rate);
    text = `${rate >= 0 ? "+" : ""}${percent(rate)}`;
  }
  const Icon = direction > 0 ? ArrowUpRight : direction < 0 ? ArrowDownRight : Minus;
  if (direction === 0) text = "持平";
  return (
    <Tip content={label}>
      <span className="num inline-flex items-center gap-0.5 text-xs text-ink-2">
        <Icon className="h-3.5 w-3.5" aria-hidden />
        {text}
      </span>
    </Tip>
  );
}

const PRECISION_HINT: Record<Precision, string> = {
  measured: "来自 agent 日志里的实际 token 计数",
  estimated: "由 Qoder credits × 校准汇率折算，不是官方 token 计数",
  mixed: "其中一部分是折算或未核查的数据，详见页面上方说明",
  unknown: "这个 agent 的解析尚未核查，数字可能含字数估算",
};

export function PrecisionBadge({ precision, always }: { precision: Precision; always?: boolean }) {
  if (precision === "measured" && !always) return null;
  const tone = precision === "measured" ? "good" : precision === "unknown" ? "neutral" : "accent";
  return (
    <Tip content={PRECISION_HINT[precision]}>
      <span>
        <Badge tone={tone}>{precisionLabel(precision)}</Badge>
      </span>
    </Tip>
  );
}

const GENERIC_ICONS: Record<Exclude<IdentityIconName, BrandName>, LucideIcon> = {
  bot: Bot,
  cpu: Cpu,
  circle: Circle,
};

export function IdentityIcon({ icon, color, label, className }: { icon: IdentityIconName; color: string; label?: string; className?: string }) {
  if (isBrandIcon(icon)) {
    // Official marks keep their own colours; monochrome ones use the text colour.
    return (
      <span aria-hidden className={cn("inline-flex shrink-0 items-center justify-center text-ink", className)} title={label}>
        <BrandMark name={icon} className="h-full w-full" />
      </span>
    );
  }
  const Icon = GENERIC_ICONS[icon];
  return (
    <span aria-hidden className={cn("inline-flex shrink-0 items-center justify-center", className)} style={{ color }} title={label}>
      <Icon className="h-full w-full" strokeWidth={2.25} />
    </span>
  );
}

export function ClientName({ id, label, className }: { id: string; label?: string; className?: string }) {
  const name = clientLabel(id, label);
  return (
    <span className={cn("inline-flex min-w-0 items-center gap-2", className)}>
      <IdentityIcon icon={clientIcon(id)} color={clientColor(id)} label={name} className="h-4 w-4" />
      <span className="truncate">{name}</span>
    </span>
  );
}

export function ModelName({ id, className }: { id: string; className?: string }) {
  const brand = modelBrand(id);
  return (
    <span className={cn("inline-flex min-w-0 items-center gap-1.5", className)}>
      <IdentityIcon icon={brand.icon} color={brand.color} label={brand.label} className="h-3.5 w-3.5" />
      <span className="truncate">{modelLabel(id)}</span>
    </span>
  );
}

export function ShareBar({ value, max, color, className }: { value: number; max: number; color: string; className?: string }) {
  const width = max > 0 ? Math.max(value > 0 ? 1.5 : 0, (value / max) * 100) : 0;
  return (
    <div className={cn("h-1.5 w-full overflow-hidden rounded-full bg-surface-2", className)}>
      <div className="h-full rounded-full" style={{ width: `${width}%`, background: color }} />
    </div>
  );
}

const CAVEAT_TONE = { info: "neutral", warning: "warn", error: "bad" } as const;

export function CaveatList({ caveats, className }: { caveats: Caveat[]; className?: string }) {
  if (!caveats.length) return null;
  return (
    <div className={cn("flex flex-wrap items-center gap-1.5", className)}>
      {caveats.map((caveat, index) => (
        <Tip key={`${caveat.code}-${caveat.deviceId ?? ""}-${caveat.client ?? ""}-${index}`} content={caveat.message}>
          <span>
            <Badge tone={CAVEAT_TONE[caveat.severity]} className="max-w-80">
              <Info className="h-3 w-3 shrink-0" aria-hidden />
              <span className="truncate">{caveatTitle(caveat)}</span>
            </Badge>
          </span>
        </Tip>
      ))}
    </div>
  );
}

function caveatTitle(caveat: Caveat): string {
  switch (caveat.code) {
    case "estimated_qoder":
      return "含 Qoder 估算";
    case "unknown_precision":
      return "含未核查数据";
    case "estimated_cost":
      return "成本为 API 等价估算";
    case "partial_cost":
      return "部分价格不全";
    case "unpriced_tokens":
      return "部分未定价";
    case "device_long_offline":
      return "设备长期未上线";
    case "collector_outdated":
      return "采集端需要升级";
    case "app_outdated":
      return "应用需要升级";
    case "backfill_incomplete":
      return "历史数据仍在上传";
    case "client_error":
      return "某个 agent 读取出错";
    case "imported_history":
      return "含导入的历史";
  }
}

/** Error with a re-pair shortcut when this computer's credentials stopped working. */
export function LoadError({ error, onRetry, compact: small }: { error: unknown; onRetry?: () => void; compact?: boolean }) {
  const { set } = useStatus();
  const kind = error instanceof ApiError ? error.kind : null;
  if (kind === "unauthorized" || kind === "not_paired") {
    return (
      <div role="alert" className="flex flex-col items-center gap-3 px-6 py-12 text-center">
        <p className="text-sm leading-6 text-ink-2">{errorMessage(error)}</p>
        <Button variant="primary" onClick={() => void app.unpair().then(set)}>
          重新配对
        </Button>
      </div>
    );
  }
  return <ErrorState message={errorMessage(error)} onRetry={onRetry} compact={small} />;
}

export function Stat({ label, children, hint }: { label: ReactNode; children: ReactNode; hint?: ReactNode }) {
  return (
    <div className="min-w-0">
      <div className="text-xs text-ink-3">{label}</div>
      <div className="num mt-0.5 text-sm font-medium text-ink">{children}</div>
      {hint && <div className="mt-0.5 text-[11px] text-ink-3">{hint}</div>}
    </div>
  );
}

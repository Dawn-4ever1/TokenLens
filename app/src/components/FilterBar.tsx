import { CalendarRange, Check, ListFilter, RefreshCw } from "lucide-react";
import { useEffect, useState } from "react";
import { ClientName, ModelName } from "@/components/common";
import { compact, relativeTime } from "@/lib/format";
import { useData } from "@/lib/data";
import { clientLabel, modelLabel, NO_PROJECT_KEY } from "@/lib/labels";
import { RANGE_PRESETS, activeFilterCount, resolveRange, todayIn, type Filters, type RangePreset } from "@/lib/range";
import type { FilterOption } from "@/lib/types";
import { Badge, Button, Popover, PopoverContent, PopoverTrigger, Segmented, Tip, cn, inputClass } from "./ui";

type ListKey = "devices" | "clients" | "models" | "projects";

const SECTIONS: { key: ListKey; label: string; display: (option: FilterOption) => string }[] = [
  { key: "devices", label: "设备", display: (option) => option.label },
  { key: "clients", label: "工具", display: (option) => clientLabel(option.key, option.label) },
  { key: "models", label: "模型", display: (option) => modelLabel(option.key) },
  { key: "projects", label: "项目", display: (option) => (option.key === NO_PROJECT_KEY ? "未标记项目" : option.label) },
];

export function RangePicker() {
  const { filters, setFilters, timeZone } = useData();
  const [open, setOpen] = useState(false);
  const span = resolveRange(filters, timeZone);
  const choose = (preset: RangePreset) => {
    if (preset === "custom") {
      const current = resolveRange(filters, timeZone);
      const today = todayIn(timeZone);
      setFilters((previous) => ({
        ...previous,
        preset,
        customFrom: previous.customFrom ?? current.from ?? today,
        customTo: previous.customTo ?? current.to ?? today,
      }));
      setOpen(true);
    } else {
      setFilters((previous) => ({ ...previous, preset }));
    }
  };
  return (
    <div className="flex items-center gap-1.5">
      <Segmented label="时间范围" value={filters.preset} options={RANGE_PRESETS} onChange={choose} />
      {filters.preset === "custom" && (
        <Popover open={open} onOpenChange={setOpen}>
          <PopoverTrigger asChild>
            <Button size="md" aria-label="修改日期">
              <CalendarRange className="h-3.5 w-3.5" aria-hidden />
              <span className="num">
                {span.from?.slice(5) ?? "…"} – {span.to?.slice(5) ?? "…"}
              </span>
            </Button>
          </PopoverTrigger>
          <PopoverContent className="w-64">
            <CustomDates filters={filters} onChange={(patch) => setFilters((previous) => ({ ...previous, ...patch }))} />
          </PopoverContent>
        </Popover>
      )}
    </div>
  );
}

function CustomDates({ filters, onChange }: { filters: Filters; onChange: (patch: Partial<Filters>) => void }) {
  return (
    <div className="space-y-3">
      <p className="text-xs text-ink-3">包含起止两天，按看板时区的自然日。</p>
      <label className="block text-xs text-ink-2">
        开始
        <input
          type="date"
          className={cn(inputClass, "mt-1")}
          value={filters.customFrom ?? ""}
          min="2000-01-01"
          max="2100-12-31"
          onChange={(event) => event.target.value && onChange({ customFrom: event.target.value })}
        />
      </label>
      <label className="block text-xs text-ink-2">
        结束
        <input
          type="date"
          className={cn(inputClass, "mt-1")}
          value={filters.customTo ?? ""}
          min="2000-01-01"
          max="2100-12-31"
          onChange={(event) => event.target.value && onChange({ customTo: event.target.value })}
        />
      </label>
    </div>
  );
}

export function FilterMenu() {
  const { filters, setFilters, options } = useData();
  const count = activeFilterCount(filters);
  const toggle = (key: ListKey, value: string) =>
    setFilters((previous) => {
      const list = previous[key];
      return { ...previous, [key]: list.includes(value) ? list.filter((item) => item !== value) : [...list, value] };
    });
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button aria-label="筛选">
          <ListFilter className="h-3.5 w-3.5" aria-hidden />
          筛选
          {count > 0 && <Badge tone="primary" className="num ml-0.5 px-1">{count}</Badge>}
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-[min(560px,90vw)] p-0">
        <div className="flex items-center justify-between border-b border-line px-4 py-2.5">
          <span className="text-[13px] font-medium">筛选</span>
          <Button
            variant="ghost"
            size="sm"
            disabled={count === 0}
            onClick={() => setFilters((previous) => ({ ...previous, devices: [], clients: [], models: [], projects: [] }))}
          >
            清除全部
          </Button>
        </div>
        {!options ? (
          <p className="px-4 py-6 text-center text-xs text-ink-3">正在读取可选项…</p>
        ) : (
          <div className="grid max-h-[60vh] grid-cols-2 gap-x-2 overflow-y-auto p-2">
            {SECTIONS.map((section) => (
              <FilterSection
                key={section.key}
                label={section.label}
                sectionKey={section.key}
                options={options[section.key]}
                selected={filters[section.key]}
                display={section.display}
                onToggle={(value) => toggle(section.key, value)}
                onClear={() => setFilters((previous) => ({ ...previous, [section.key]: [] }))}
              />
            ))}
          </div>
        )}
      </PopoverContent>
    </Popover>
  );
}

function FilterSection({
  label,
  sectionKey,
  options,
  selected,
  display,
  onToggle,
  onClear,
}: {
  label: string;
  sectionKey: ListKey;
  options: FilterOption[];
  selected: string[];
  display: (option: FilterOption) => string;
  onToggle: (value: string) => void;
  onClear: () => void;
}) {
  return (
    <fieldset className="min-w-0 p-2">
      <legend className="flex w-full items-center justify-between px-1 pb-1 text-xs text-ink-3">
        {label}
        {selected.length > 0 && (
          <button type="button" className="text-primary hover:underline" onClick={onClear}>
            全部
          </button>
        )}
      </legend>
      {options.length === 0 && <p className="px-1 py-1 text-xs text-ink-3">暂无</p>}
      <ul className="space-y-px">
        {options.slice(0, 40).map((option) => {
          const checked = selected.includes(option.key);
          return (
            <li key={option.key}>
              <button
                type="button"
                role="checkbox"
                aria-checked={checked}
                onClick={() => onToggle(option.key)}
                className="flex h-7 w-full items-center gap-2 rounded-md px-1.5 text-left text-[13px] hover:bg-surface-2"
              >
                <span
                  className={cn(
                    "flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded border",
                    checked ? "border-primary bg-primary text-primary-ink" : "border-line",
                  )}
                >
                  {checked && <Check className="h-3 w-3" strokeWidth={3} aria-hidden />}
                </span>
                <Tip content={option.key !== display(option) ? option.key : undefined}>
                  {sectionKey === "clients" ? (
                    <ClientName id={option.key} label={option.label} className="min-w-0 flex-1 text-ink" />
                  ) : sectionKey === "models" ? (
                    <ModelName id={option.key} className="min-w-0 flex-1 text-ink" />
                  ) : (
                    <span className="min-w-0 flex-1 truncate">{display(option)}</span>
                  )}
                </Tip>
                <span className="num shrink-0 text-[11px] text-ink-3">{compact(option.totalTokens)}</span>
              </button>
            </li>
          );
        })}
      </ul>
    </fieldset>
  );
}

export function RefreshStatus() {
  const { refresh, lastLoadedAt } = useData();
  const [, setNow] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setNow((n) => n + 1), 30_000);
    return () => clearInterval(id);
  }, []);
  const [spinning, setSpinning] = useState(false);
  return (
    <div className="flex items-center gap-1 text-xs text-ink-3">
      <span className="hidden whitespace-nowrap min-[1000px]:inline">{lastLoadedAt ? `更新于${relativeTime(lastLoadedAt)}` : "加载中"}</span>
      <Tip content="立即刷新">
        <Button
          variant="ghost"
          size="icon"
          aria-label="刷新"
          onClick={() => {
            refresh();
            setSpinning(true);
            setTimeout(() => setSpinning(false), 700);
          }}
        >
          <RefreshCw className={cn("h-4 w-4", spinning && "animate-spin")} aria-hidden />
        </Button>
      </Tip>
    </div>
  );
}

import { ArrowRight, Check, Cloud as CloudIcon, Laptop, ShieldCheck } from "lucide-react";
import { useState } from "react";
import { Badge, Button, Card, cn } from "@/components/ui";
import { useNav } from "@/lib/nav";
import { useStatus } from "@/lib/status";
import { ConnectServer } from "./Pairing";

const LOCAL_FEATURES = [
  "记录当前电脑的 AI 用量",
  "完整的工具、模型与会话分析",
  "本机历史，离线也能查看",
  "无需登录即可使用",
];

const CLOUD_FEATURES = [
  "不限设备数量，跨设备汇总同步",
  "包含全部本地版功能",
  "同一账号的设备共享看板",
  "本机继续记录，网络恢复后自动同步",
  "只同步数字统计、会话标题和项目名，不上传提示词、回答或代码",
];

export function Cloud() {
  const { status } = useStatus();
  const { go } = useNav();
  const [connecting, setConnecting] = useState(false);
  const connected = Boolean(status?.server);
  const selfHosted = Boolean(status?.server && status.server !== status.cloudServer && !status.accountEmail);

  return (
    <div className="space-y-[var(--stack-space)]">
      <div className="max-w-2xl">
        <p className="text-sm leading-6 text-ink-2">本地统计免费，云服务支持跨设备同步与历史管理。</p>
        <p className="mt-1 text-xs leading-5 text-ink-3">推荐使用云服务，在多台电脑上用同一个账号查看完整用量。</p>
      </div>

      <div className="grid grid-cols-1 items-stretch gap-[var(--grid-gap)] min-[1100px]:grid-cols-2">
        <Card data-plan="local" className="min-w-0" bodyClassName="flex flex-1 flex-col pt-1">
          <div className="flex items-start justify-between gap-3">
            <div>
              <h2 className="display flex items-center gap-2 text-[length:var(--section-title-size)] leading-6 text-ink">
                <Laptop className="h-[18px] w-[18px] text-ink-3" aria-hidden /> 免费本地版
              </h2>
              <p className="mt-3 text-[13px] leading-6 text-ink-2">在自己的电脑上，完整记录和分析 AI 用量。</p>
            </div>
            <Badge tone={connected ? "neutral" : "primary"}>{connected ? "本机保留" : "当前使用"}</Badge>
          </div>
          <div className="mt-6">
            <div className="display text-3xl text-ink">免费</div>
            <p className="mt-2 text-xs leading-5 text-ink-3">完整本地功能，目前免费。</p>
          </div>
          <FeatureList items={LOCAL_FEATURES} />
          <div className="mt-auto pt-6">
            <Button className="w-full" onClick={() => go("devices")}>
              查看本机设备 <ArrowRight className="h-3.5 w-3.5" aria-hidden />
            </Button>
          </div>
        </Card>

        <Card
          data-plan="cloud"
          className={cn("min-w-0 border-primary bg-primary-soft/20 ring-1 ring-primary")}
          bodyClassName="flex flex-1 flex-col pt-1"
        >
          <div className="flex items-start justify-between gap-3">
            <div>
              <h2 className="display flex items-center gap-2 text-[length:var(--section-title-size)] leading-6 text-ink">
                <CloudIcon className="h-[18px] w-[18px] text-primary" aria-hidden /> TokenLens 云服务
              </h2>
              <p className="mt-3 text-[13px] leading-6 text-ink-2">跨设备同步与用量历史，无需自行维护服务器。</p>
            </div>
            <div className="flex shrink-0 flex-wrap justify-end gap-1.5">
              <Badge tone="primary">推荐</Badge>
              {connected && <Badge tone="good">已连接</Badge>}
            </div>
          </div>
          <div className="mt-6">
            <div className="display text-3xl text-ink">免费</div>
            <p className="mt-2 text-xs leading-5 text-ink-3">完整云端功能，目前免费。</p>
          </div>
          <FeatureList items={CLOUD_FEATURES} />
          <div className="mt-auto pt-6">
            {connected ? (
              <Button variant="primary" className="w-full" onClick={() => go("devices")}>
                管理我的设备 <ArrowRight className="h-3.5 w-3.5" aria-hidden />
              </Button>
            ) : (
              <Button variant="primary" className="w-full" onClick={() => setConnecting(true)}>
                免费使用云服务 <ArrowRight className="h-3.5 w-3.5" aria-hidden />
              </Button>
            )}
          </div>
        </Card>
      </div>

      <div className="border-t border-line/70 pt-4">
        <p className="flex items-start gap-2 text-xs leading-5 text-ink-3">
          <ShieldCheck className="mt-0.5 h-4 w-4 shrink-0" aria-hidden />
          {selfHosted ? (
            <>
              当前已连接到自建服务器（<span className="font-mono text-ink-2">{status?.server}</span>）。添加另一台电脑时，可在那台电脑打开登录窗口底部的「使用自己的服务器」，输入服务器地址和配对码。
            </>
          ) : (
            "添加另一台电脑时，只需安装 TokenLens、选择云服务并用同一账号登录。GitHub、Google 或邮箱账号均可；无需复制服务器地址或配对码。"
          )}
        </p>
      </div>
      <ConnectServer open={connecting} onOpenChange={setConnecting} />
    </div>
  );
}

function FeatureList({ items }: { items: string[] }) {
  return (
    <ul className="mt-6 space-y-3 border-t border-line/70 pt-5">
      {items.map((item) => (
        <li key={item} className="flex items-start gap-2.5 text-[13px] leading-5 text-ink-2">
          <Check className="mt-0.5 h-3.5 w-3.5 shrink-0 text-good" aria-hidden />
          <span>{item}</span>
        </li>
      ))}
    </ul>
  );
}

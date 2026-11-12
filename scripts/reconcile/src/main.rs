use std::collections::BTreeSet;
use std::process::ExitCode;
use std::time::Duration;

use chrono::{Local, NaiveDate};
use clap::Parser;
use collector_core::{Config, DateRange, QODER_CLIENT, ScanOptions};
use reconcile::{
    HUB_QODER_CLIENT, HubRequest, SECRET_ENV, URL_ENV, hub_counts_reasoning, hub_devices, hub_grid,
    hub_time_zone, our_grid, qoder_section, reconcile, render, tz_mismatch_days,
};
use serde_json::Value;

const MAX_HUB_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;

/// 和 Token Monitor Hub 按 client × 日对账（只读）。报告只含数字和日期。
#[derive(Parser)]
#[command(name = "reconcile")]
struct Args {
    /// Hub 里「这台电脑」的设备 id；不填时只在终端列出 Hub 里的设备
    #[arg(long)]
    hub_device: Option<String>,
    /// 对比最近多少个已结束的自然日
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=370))]
    days: u32,
    /// 最后一天（本机日期，YYYY-MM-DD），默认昨天
    #[arg(long)]
    until: Option<NaiveDate>,
    /// 内部用：在本进程里扫描（外层进程负责准备和后台采集一样的上游环境，并把提示里的用户文件夹换成 ~）
    #[arg(long, hide = true)]
    direct: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    if !args.direct {
        return match collector_core::interactive::rerun_isolated("--direct", None) {
            Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
            Err(error) => {
                eprintln!("对账没有完成：无法启动扫描子进程（{error}）");
                ExitCode::from(1)
            }
        };
    }
    let Some(secret) = std::env::var(SECRET_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
    else {
        eprintln!("{}", missing_secret_help());
        return ExitCode::from(2);
    };
    match run(&args, &secret) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("对账没有完成：{message}");
            ExitCode::from(1)
        }
    }
}

fn missing_secret_help() -> String {
    format!(
        "没有读到 Hub 密钥（环境变量 {SECRET_ENV}）。对账需要你自己在终端里设置一次：\n\
         \n\
         Mac：在「终端」App 里运行\n\
         \x20 security add-generic-password -a \"$USER\" -s tokendash-hub-secret -w\n\
         按提示粘贴密钥后回车。之后这样运行对账：\n\
         \x20 {SECRET_ENV}=\"$(security find-generic-password -s tokendash-hub-secret -w)\" cargo run -p reconcile -- --hub-device <设备 id>\n\
         系统可能弹窗问是否允许访问钥匙串，点「允许」。\n\
         \n\
         Windows：在 PowerShell 里运行\n\
         \x20 [Environment]::SetEnvironmentVariable(\"{SECRET_ENV}\", (Read-Host \"粘贴密钥\"), \"User\")\n\
         然后重新打开终端。用完后把同一条命令里的 (Read-Host ...) 换成 $null 再运行一次，即可删除。"
    )
}

fn run(args: &Args, secret: &str) -> Result<ExitCode, String> {
    let base = std::env::var(URL_ENV)
        .ok()
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| {
            format!("没有读到 Hub 地址：请设置环境变量 {URL_ENV}，例如 https://hub.example.com")
        })?;
    let request = HubRequest::devices(&base, secret).map_err(|error| error.to_string())?;
    let body = fetch(&request)?;

    let Some(device_id) = args.hub_device.as_deref() else {
        list_devices(&body);
        return Ok(ExitCode::from(3));
    };
    let device = reconcile::find_device(&body, device_id)
        .ok_or("Hub 里没有这个设备 id；不带 --hub-device 运行一次可以看到设备列表")?;

    let until = args
        .until
        .unwrap_or_else(|| Local::now().date_naive() - chrono::Duration::days(1));
    let since = until - chrono::Duration::days(i64::from(args.days) - 1);
    let range = DateRange {
        since: Some(since),
        until: Some(until),
    };

    let options = ScanOptions::for_current_user(range).map_err(|e| e.to_string())?;
    let config = Config::load(&collector_core::config_path_in(&options.home))
        .map_err(|e| format!("采集配置读不了：{e}"))?;
    let report =
        collector_core::scan_with(&config, &options).map_err(|e| format!("本机扫描失败：{e}"))?;
    let without_usage = collector_core::qoder::scan_home(&options.home, range)
        .map_err(|e| format!("Qoder 扫描失败：{e}"))?
        .assistant_without_usage;

    let ours = our_grid(&report.daily, since, until);
    let hub = hub_grid(device, since, until);
    let our_clients: BTreeSet<String> = report.daily.iter().map(|d| d.client.clone()).collect();
    let tz = hub_time_zone(device).and_then(|zone| tz_mismatch_days(zone, &Local, since, until));
    let rec = reconcile(&our_clients, &ours, &hub, since, until, tz);

    let qoder_events: Vec<(i64, i64)> = report
        .events
        .iter()
        .filter(|event| event.client == QODER_CLIENT)
        .map(|event| {
            let reasoning = if hub_counts_reasoning(HUB_QODER_CLIENT) {
                event.reasoning
            } else {
                0
            };
            let tokens =
                event.input + event.output + event.cache_read + event.cache_write + reasoning;
            (event.ts, i64::try_from(tokens).unwrap_or(i64::MAX))
        })
        .collect();
    let qoder = qoder_section(
        &Local,
        &ours,
        &hub,
        &qoder_events,
        &without_usage,
        since,
        until,
    );
    let hub_has_qoder = hub.keys().any(|(client, _)| client == HUB_QODER_CLIENT);
    print!(
        "{}",
        render(&rec, &qoder, Local::now().date_naive(), hub_has_qoder)
    );
    Ok(ExitCode::SUCCESS)
}

fn fetch(request: &HubRequest) -> Result<Value, String> {
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        // A redirect could carry the secret header to another host.
        .max_redirects(0)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(120)))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut response = agent
        .get(&request.url)
        .header("Accept", "application/json")
        .header("Authorization", request.authorization())
        .call()
        .map_err(|_| "连不上 Hub（网络不通、地址不对或超时）".to_owned())?;
    match response.status().as_u16() {
        200 => {}
        401 | 403 => return Err("Hub 不接受这个密钥，请确认密钥是否正确".to_owned()),
        300..=399 => {
            return Err("Hub 要求跳转到别的地址；为防止密钥被带走，没有跟随".to_owned());
        }
        status => return Err(format!("Hub 返回了状态码 {status}")),
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_HUB_RESPONSE_BYTES)
        .read_to_vec()
        .map_err(|_| "Hub 的回复没读完整（或超过 64 MiB）".to_owned())?;
    serde_json::from_slice(&body).map_err(|_| "Hub 的回复不是有效的 JSON".to_owned())
}

/// Terminal only: device names never go to stdout or the report.
fn list_devices(body: &Value) {
    let devices = hub_devices(body);
    if devices.is_empty() {
        eprintln!("Hub 里没有设备记录。");
        return;
    }
    eprintln!("Hub 里的设备（只显示在这个终端里，不会写进报告）：\n");
    for device in &devices {
        eprintln!(
            "  --hub-device {}    名称：{}    最后上传：{}    时区：{}    历史天数：{}",
            device.id,
            device.label,
            device.last_update.as_deref().unwrap_or("未知"),
            device.time_zone.as_deref().unwrap_or("未报告"),
            device.history_days
        );
    }
    eprintln!("\n请确认哪一台是这台电脑，然后带上对应的 --hub-device 重新运行。");
}

//! Synthetic agent logs below a temporary home, shared by the end-to-end tests.
#![allow(dead_code)]

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

pub const BIN: &str = env!("CARGO_BIN_EXE_collector");

pub fn iso_now(offset_ms: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::milliseconds(offset_ms))
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

pub fn append(path: &Path, lines: &[String]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("file");
    for line in lines {
        writeln!(file, "{line}").expect("write");
    }
    file.sync_all().expect("sync");
}

pub fn claude(index: u32) -> String {
    format!(
        r#"{{"type":"assistant","uuid":"synthetic-uuid-{index}","sessionId":"synthetic-claude-session","timestamp":"{ts}","requestId":"synthetic-req-{index}","message":{{"id":"synthetic-msg-{index}","model":"claude-sonnet-4-5","content":[{{"type":"text","text":"ok"}}],"usage":{{"input_tokens":{input},"output_tokens":5,"cache_read_input_tokens":100}}}}}}"#,
        ts = iso_now(-60_000 + i64::from(index) * 10),
        input = 10 + index,
    )
}

pub fn codex_header() -> Vec<String> {
    vec![
        r#"{"type":"session_meta","payload":{"id":"synthetic-codex-session","source":"cli"}}"#
            .to_owned(),
        r#"{"type":"turn_context","payload":{"model":"gpt-5.4"}}"#.to_owned(),
    ]
}

pub fn codex_count(total: u32, last: u32) -> String {
    format!(
        r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{total},"cached_input_tokens":0,"output_tokens":10,"reasoning_output_tokens":0,"total_tokens":{t2}}},"last_token_usage":{{"input_tokens":{last},"cached_input_tokens":0,"output_tokens":10,"reasoning_output_tokens":0,"total_tokens":{l2}}},"model_context_window":258400}}}}}}"#,
        ts = iso_now(-30_000 + i64::from(total)),
        t2 = total + 10,
        l2 = last + 10,
    )
}

pub fn qoder_fragment(fragment: u32, credits: f64) -> String {
    format!(
        r#"{{"type":"assistant","uuid":"synthetic-qoder-fragment-{fragment}","sessionId":"synthetic-qoder-session","timestamp":"{ts}","message":{{"id":"synthetic-qoder-message","model":"gmodel","usage":{{"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"credits":{credits}}}}}}}"#,
        ts = iso_now(-20_000 + i64::from(fragment) * 1000),
    )
}

pub struct Paths {
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub qoder: PathBuf,
    pub mirror: PathBuf,
}

pub fn paths(home: &Path) -> Paths {
    let today = chrono::Local::now().format("%Y/%m/%d").to_string();
    Paths {
        claude: home.join(".claude/projects/synthetic-project/synthetic-claude-session.jsonl"),
        codex: home
            .join(".codex/sessions")
            .join(today)
            .join("rollout-synthetic-codex-session.jsonl"),
        qoder: home.join(".qoder/projects/synthetic-project/synthetic-qoder-session.jsonl"),
        mirror: home.join(".qoder-cn/projects/synthetic-project/synthetic-mirror.jsonl"),
    }
}

pub fn full_scan_digest(home: &Path) -> Value {
    let today = chrono::Local::now().date_naive().to_string();
    let output = Command::new(BIN)
        .args(["scan", "--dry-run", "--json", "--since", "1970-01-01"])
        .args(["--until", &today])
        .arg("--home")
        .arg(home)
        .env("TOKSCALE_CONFIG_DIR", home.join("unused-tokscale-config"))
        .output()
        .expect("scan");
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).expect("report");
    report["event_digest"].clone()
}

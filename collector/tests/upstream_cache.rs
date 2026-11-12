//! The upstream persistent source cache may only be used if, after every kind
//! of file change, a cached worker reports exactly the events an uncached
//! worker reports.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_collector");

fn claude(index: u32, input: u32) -> String {
    format!(
        r#"{{"type":"assistant","uuid":"synthetic-uuid-{index}","sessionId":"synthetic-claude-session","timestamp":"2026-11-0{day}T10:00:{index:02}.000Z","requestId":"synthetic-req-{index}","message":{{"id":"synthetic-msg-{index}","model":"claude-sonnet-4-5","content":[{{"type":"text","text":"ok"}}],"usage":{{"input_tokens":{input},"output_tokens":5,"cache_read_input_tokens":100}}}}}}"#,
        day = 1 + index % 5,
    )
}

fn codex_lines(counts: &[(u32, u32)]) -> Vec<String> {
    let mut lines = vec![
        r#"{"type":"session_meta","payload":{"id":"synthetic-codex-session","source":"cli"}}"#
            .to_owned(),
        r#"{"type":"turn_context","payload":{"model":"gpt-5.4"}}"#.to_owned(),
    ];
    for (step, (total, last)) in counts.iter().enumerate() {
        lines.push(format!(
            r#"{{"timestamp":"2026-11-03T09:00:{step:02}.000Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{total},"cached_input_tokens":0,"output_tokens":10,"reasoning_output_tokens":0,"total_tokens":{t2}}},"last_token_usage":{{"input_tokens":{last},"cached_input_tokens":0,"output_tokens":10,"reasoning_output_tokens":0,"total_tokens":{l2}}},"model_context_window":258400}}}}}}"#,
            t2 = total + 10,
            l2 = last + 10,
        ));
    }
    lines
}

fn write(path: &Path, lines: &[String]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    let mut text = lines.join("\n");
    text.push('\n');
    std::fs::write(path, text).expect("write");
}

fn append(path: &Path, lines: &[String]) {
    let mut file = OpenOptions::new().append(true).open(path).expect("file");
    for line in lines {
        writeln!(file, "{line}").expect("write");
    }
    file.sync_all().expect("sync");
}

/// Events from one worker run sorted by event key, and whether upstream's
/// persistent cache was actually used.
fn worker_run(home: &Path, cache: Option<&Path>, clients: Option<&str>) -> (Vec<Value>, bool) {
    let mut command = Command::new(BIN);
    command
        .arg("scan-worker")
        .arg("--home")
        .arg(home)
        .args([
            "--no-env-roots",
            "--since",
            "1970-01-01",
            "--until",
            "2026-12-31",
        ])
        .env("HOME", home);
    if let Some(clients) = clients {
        command.args(["--clients", clients]);
    }
    match cache {
        Some(dir) => command
            .arg("--upstream-cache")
            .env("TOKSCALE_CONFIG_DIR", dir),
        None => command.env("TOKSCALE_CONFIG_DIR", home.join("unused-tokscale-config")),
    };
    let output = command.output().expect("worker");
    assert!(output.status.success(), "worker failed");
    let lines = String::from_utf8(output.stdout)
        .expect("utf8")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("json"))
        .collect::<Vec<_>>();
    let cached = lines.last().expect("done")["cached"] == true;
    let mut events = lines
        .into_iter()
        .filter(|line| line["type"] == "event")
        .map(|line| line["event"].clone())
        .collect::<Vec<_>>();
    events.sort_by(|a, b| a["eventKey"].as_str().cmp(&b["eventKey"].as_str()));
    (events, cached)
}

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    cache: PathBuf,
    clients: Option<&'static str>,
}

impl Fixture {
    fn new(clients: Option<&'static str>) -> Self {
        let temp = tempfile::tempdir().expect("temp");
        let home = temp.path().join("home");
        let cache = temp.path().join("cache").join("tokenlens").join("tokscale");
        std::fs::create_dir_all(&home).expect("home");
        Self {
            _temp: temp,
            home,
            cache,
            clients,
        }
    }

    /// Runs cached twice (cold after the change, then warm) and uncached once.
    fn assert_equal(&self, step: &str, expected_count: usize, expect_cached: bool) {
        let (uncached, _) = worker_run(&self.home, None, self.clients);
        let (first, first_cached) = worker_run(&self.home, Some(&self.cache), self.clients);
        let (warm, warm_cached) = worker_run(&self.home, Some(&self.cache), self.clients);
        assert_eq!(uncached.len(), expected_count, "{step}: uncached count");
        assert_eq!(first, uncached, "{step}: first cached run");
        assert_eq!(warm, uncached, "{step}: warm cached run");
        assert_eq!(first_cached, expect_cached, "{step}: cache use");
        assert_eq!(warm_cached, expect_cached, "{step}: cache use");
    }
}

fn set_mtime(path: &Path, modified: std::time::SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .expect("open")
        .set_modified(modified)
        .expect("set mtime");
}

fn mtime(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path)
        .expect("meta")
        .modified()
        .expect("mtime")
}

#[test]
fn cached_codex_worker_matches_uncached_after_every_kind_of_change() {
    let fixture = Fixture::new(Some("codex"));
    let day = fixture.home.join(".codex/sessions/2026/11/03");
    let first = day.join("rollout-synthetic-codex-session.jsonl");
    let second = day.join("rollout-synthetic-second-session.jsonl");

    write(&first, &codex_lines(&[(100, 100)]));
    fixture.assert_equal("initial", 1, true);

    append(&first, &codex_lines(&[(100, 100), (250, 150)])[3..]);
    fixture.assert_equal("append", 2, true);

    write(&first, &codex_lines(&[(100, 100), (250, 151)]));
    fixture.assert_equal("same-size rewrite", 2, true);

    let old = mtime(&first);
    write(&first, &codex_lines(&[(100, 100), (250, 152)]));
    set_mtime(&first, old);
    fixture.assert_equal("same-size rewrite, mtime rolled back", 2, true);

    write(&first, &codex_lines(&[(100, 100), (2500, 2400)]));
    fixture.assert_equal("growing rewrite", 2, true);

    write(&first, &codex_lines(&[(100, 100)]));
    fixture.assert_equal("truncate", 1, true);

    write(&second, &codex_lines(&[(70, 70), (90, 20)]));
    fixture.assert_equal("new file", 3, true);

    std::fs::remove_file(&first).expect("delete");
    fixture.assert_equal("delete", 2, true);

    std::fs::remove_file(&second).expect("delete");
    fixture.assert_equal("empty", 0, true);
    assert!(
        fixture.cache.is_dir(),
        "the cache stays in its own directory"
    );
}

/// Upstream's persistent cache deliberately keeps Claude turns that a
/// transcript rewrite removed ("retained history"). Workers therefore parse
/// any scope that includes Claude without the cache, and only Claude-free
/// scopes use it.
#[test]
fn claude_scopes_bypass_the_cache_and_match_uncached() {
    let fixture = Fixture::new(None);
    let path = fixture
        .home
        .join(".claude/projects/synthetic-project/synthetic-claude-session.jsonl");
    let codex = fixture
        .home
        .join(".codex/sessions/2026/11/03/rollout-synthetic-codex-session.jsonl");
    write(&path, &[claude(1, 11), claude(2, 12), claude(3, 13)]);
    write(&codex, &codex_lines(&[(100, 100)]));
    fixture.assert_equal("initial", 4, false);

    write(&path, &[claude(1, 11)]);
    fixture.assert_equal("truncate", 2, false);
    let (events, cached) = worker_run(&fixture.home, Some(&fixture.cache), Some("claude"));
    assert!(!cached);
    assert_eq!(events.len(), 1);
    let (events, cached) = worker_run(&fixture.home, Some(&fixture.cache), Some("codex"));
    assert!(cached, "a Claude-free scope uses the cache");
    assert_eq!(events.len(), 1);

    std::fs::remove_file(&path).expect("delete");
    fixture.assert_equal("delete", 1, false);
}

#[test]
fn cached_worker_refuses_a_foreign_cache_directory() {
    let fixture = Fixture::new(None);
    for dir in [None, Some(fixture.home.join(".config").join("tokscale"))] {
        let mut command = Command::new(BIN);
        command
            .arg("scan-worker")
            .arg("--home")
            .arg(&fixture.home)
            .args(["--no-env-roots", "--upstream-cache"]);
        match &dir {
            Some(dir) => command.env("TOKSCALE_CONFIG_DIR", dir),
            None => command.env_remove("TOKSCALE_CONFIG_DIR"),
        };
        let output = command.output().expect("worker");
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    assert!(!fixture.home.join(".config").exists());
}

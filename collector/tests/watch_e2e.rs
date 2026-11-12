//! End-to-end: `watch` on a synthetic home must end with exactly the snapshot
//! a single uncached full scan produces.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

mod common;
use common::*;

#[test]
fn watch_snapshot_matches_a_full_uncached_scan() {
    let temp = tempfile::tempdir().expect("home");
    let home = temp.path();
    let files = paths(home);
    append(&files.claude, &[claude(1), claude(2)]);
    let mut codex = codex_header();
    codex.push(codex_count(100, 100));
    append(&files.codex, &codex);
    append(&files.qoder, &[qoder_fragment(1, 0.5)]);
    // The mirror must never be read or counted.
    append(&files.mirror, &[qoder_fragment(9, 7.0)]);

    let mut child = Command::new(BIN)
        .arg("watch")
        .arg("--home")
        .arg(home)
        .args(["--debounce-ms", "300", "--max-delay-ms", "1500"])
        .args(["--cooldown-ms", "500", "--exit-after-secs", "14"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("watch");
    let stdout = child.stdout.take().expect("stdout");
    let (sender, lines) = mpsc::channel::<Value>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let value: Value = serde_json::from_str(&line.expect("line")).expect("json line");
            let _ = sender.send(value);
        }
    });
    let mut seen = Vec::new();
    loop {
        let line = lines
            .recv_timeout(Duration::from_secs(30))
            .expect("initial full scan");
        let is_roots = line["watch"] == "roots";
        seen.push(line);
        if is_roots {
            break;
        }
    }
    assert_eq!(seen[0]["scan"], "full");
    assert_eq!(seen[0]["ok"], true);
    assert_eq!(seen[1]["ok"], true);
    assert_eq!(seen[1]["roots"], 3, "claude, codex and qoder roots");

    for index in 3..6 {
        append(&files.claude, &[claude(index)]);
        std::thread::sleep(Duration::from_millis(200));
    }
    append(&files.codex, &[codex_count(250, 150)]);
    // Same message.id: merges into the existing event (updated, not new).
    append(&files.qoder, &[qoder_fragment(2, 0.75)]);
    append(&files.mirror, &[qoder_fragment(10, 7.0)]);
    std::thread::sleep(Duration::from_secs(3));
    append(&files.claude, &[claude(6)]);
    append(&files.qoder, &[qoder_fragment(3, 0.75)]);

    let status = child.wait().expect("exit");
    reader.join().expect("reader");
    assert!(status.success());
    seen.extend(lines.try_iter());
    let summary = seen
        .iter()
        .find_map(|line| line.get("summary"))
        .expect("summary")
        .clone();
    let scans = seen
        .iter()
        .filter(|line| line.get("scan").is_some())
        .collect::<Vec<_>>();
    assert!(scans.iter().all(|line| line["ok"] == true));
    assert!(summary["triggeredScans"].as_u64().unwrap() >= 1);
    assert_eq!(summary["failures"], 0);
    assert_eq!(summary["vanished"], 0);
    let updated: u64 = scans
        .iter()
        .map(|line| line["updated"].as_u64().unwrap())
        .sum();
    assert!(updated >= 1, "the merged Qoder fragment updates its event");

    let full = full_scan_digest(home);
    // claude 6 + codex 2 + qoder 1 merged turn; nothing from the mirror.
    assert_eq!(full["event_count"], 9);
    assert_eq!(summary["snapshotEvents"], full["event_count"]);
    assert_eq!(summary["keySetSha256"], full["key_set_sha256"]);
    assert_eq!(summary["valueSha256"], full["value_sha256"]);
}

#[test]
fn worker_nonzero_exit_is_reported_without_touching_the_snapshot() {
    let temp = tempfile::tempdir().expect("home");
    // An invalid config makes every worker fail with a non-zero exit.
    let config = temp.path().join(".config/tokenlens/collector.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "clients = 3\n").unwrap();
    let output = Command::new(BIN)
        .arg("watch")
        .arg("--home")
        .arg(temp.path())
        .args(["--exit-after-secs", "2"])
        .output()
        .expect("watch");
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).expect("utf8");
    let lines = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("json"))
        .collect::<Vec<_>>();
    assert_eq!(lines[0]["scan"], "full");
    assert_eq!(lines[0]["ok"], false);
    assert_eq!(lines[0]["error"], "exit-1");
    let summary = &lines.last().unwrap()["summary"];
    assert_eq!(summary["failures"], 1);
    assert_eq!(summary["snapshotEvents"], 0);
    // Numbers only: no path fragment of the temporary home is printed.
    let name = temp.path().file_name().unwrap().to_string_lossy();
    assert!(!text.contains(name.as_ref()));
}

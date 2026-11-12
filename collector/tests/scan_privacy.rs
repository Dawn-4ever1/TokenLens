//! Upstream warns about broken DSH transcripts with their full paths; the
//! scan command must show that warning without the home folder.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_collector");

#[test]
fn scan_warnings_hide_the_home_folder() {
    let temp = tempfile::tempdir().expect("temp");
    let home = temp.path().join("home");
    let session = home
        .join(".dsh")
        .join("sessions")
        .join("synthetic-project")
        .join("synthetic-session");
    std::fs::create_dir_all(&session).expect("dir");
    std::fs::write(session.join("session.jsonl"), "not json\n").expect("write");

    for json in [false, true] {
        let mut command = Command::new(BIN);
        command
            .args(["scan", "--dry-run", "--since", "1970-01-01", "--home"])
            .arg(&home);
        if json {
            command.arg("--json");
        }
        let output = command.output().expect("run");
        assert!(output.status.success(), "json={json}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let home = home.to_string_lossy();
        assert!(stderr.contains("incomplete DSH transcript"), "{stderr}");
        assert!(stderr.contains('~'), "{stderr}");
        assert!(!stderr.contains(home.as_ref()));
        assert!(!stderr.contains(&home.escape_debug().to_string()));
        assert!(!String::from_utf8_lossy(&output.stdout).contains(home.as_ref()));
    }
}

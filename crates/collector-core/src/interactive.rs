//! One-off commands (`scan --dry-run`, reconcile) re-run themselves in a child
//! that sees the same upstream environment as a background worker. The parent
//! shows the child's stderr with the home folder replaced by `~`, because
//! upstream prints some source diagnostics (for example broken DSH
//! transcripts) with full file paths.

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// Re-run this program with `direct_flag` appended and return the child's exit
/// code. stdout is passed through unchanged.
pub fn rerun_isolated(direct_flag: &str, explicit_home: Option<&Path>) -> std::io::Result<i32> {
    let upstream_dir = crate::roots::own_upstream_dir(explicit_home)
        .ok_or_else(|| std::io::Error::other("cannot determine this project's cache directory"))?;
    let mut homes: Vec<String> = explicit_home
        .map(Path::to_path_buf)
        .into_iter()
        .chain(dirs::home_dir())
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|path| !path.is_empty())
        .collect();
    homes.sort_by_key(|path| std::cmp::Reverse(path.len()));
    let mut child = child_command(
        &std::env::current_exe()?,
        std::env::args_os().skip(1),
        direct_flag,
        &upstream_dir,
    )
    .stdin(Stdio::null())
    .stderr(Stdio::piped())
    .spawn()?;
    if let Some(child_stderr) = child.stderr.take() {
        let mut stderr = std::io::stderr().lock();
        for line in BufReader::new(child_stderr).split(b'\n') {
            let line = String::from_utf8_lossy(&line?).into_owned();
            let line = homes
                .iter()
                .fold(line, |line, home| redact_home(&line, home));
            writeln!(stderr, "{}", line.trim_end_matches('\r'))?;
        }
    }
    Ok(child.wait()?.code().unwrap_or(1))
}

/// Upstream reads its recovery ledger and several vendor sync caches (Cursor,
/// Amp, Trae, ...) from its config folder; background workers never see them.
fn child_command(
    program: &Path,
    args: impl IntoIterator<Item = OsString>,
    direct_flag: &str,
    upstream_dir: &Path,
) -> Command {
    let mut command = Command::new(program);
    command
        .args(args)
        .arg(OsStr::new(direct_flag))
        .env("TOKSCALE_RECOVERY_DISABLE", "1")
        .env("TOKSCALE_CONFIG_DIR", upstream_dir);
    command
}

/// Also covers the `{:?}` form, where Windows backslashes appear doubled.
pub fn redact_home(line: &str, home: &str) -> String {
    let escaped = home.escape_debug().to_string();
    let mut line = line.replace(&escaped, "~");
    if escaped != home {
        line = line.replace(home, "~");
    }

    // Some upstream diagnostics turn an absolute Windows path into a slug;
    // redact that username too.
    if let Some(username) = Path::new(home).file_name().and_then(OsStr::to_str) {
        let slug = format!("C-Users-{username}");
        line = line.replace(&slug, "~");
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_folder_is_hidden_in_both_spellings() {
        assert_eq!(
            redact_home(
                r#"Examples: "/Users/someone/.dsh/a.jsonl" (truncated)."#,
                "/Users/someone"
            ),
            r#"Examples: "~/.dsh/a.jsonl" (truncated)."#
        );
        let windows = r"C:\Users\someone";
        assert_eq!(
            redact_home(
                r#"Examples: "C:\\Users\\someone\\.dsh\\a.jsonl", C:\Users\someone\b"#,
                windows
            ),
            r#"Examples: "~\\.dsh\\a.jsonl", ~\b"#
        );
        assert_eq!(redact_home("no paths here", windows), "no paths here");
        assert_eq!(
            redact_home(
                r#"Examples: "~\\.dsh\\sessions\\--C-Users-someone-Documents-demo""#,
                windows
            ),
            r#"Examples: "~\\.dsh\\sessions\\--~-Documents-demo""#
        );
    }

    #[test]
    fn child_scans_with_the_worker_environment() {
        let upstream = Path::new("/synthetic/cache/tokenlens/tokscale");
        let command = child_command(
            Path::new("/synthetic/collector"),
            ["scan".into(), "--dry-run".into()],
            "--direct",
            upstream,
        );
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["scan", "--dry-run", "--direct"]);
        let env: Vec<_> = command.get_envs().collect();
        assert!(env.contains(&(
            OsStr::new("TOKSCALE_RECOVERY_DISABLE"),
            Some(OsStr::new("1"))
        )));
        assert!(env.contains(&(
            OsStr::new("TOKSCALE_CONFIG_DIR"),
            Some(upstream.as_os_str())
        )));
    }
}

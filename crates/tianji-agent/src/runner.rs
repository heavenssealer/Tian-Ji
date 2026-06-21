use std::sync::mpsc;
use std::time::Duration;

const TIMEOUT_SECS: u64 = 30;

pub trait CommandRunner: Send + Sync {
    fn run(&self, tool: &str, argv: &[String]) -> String;
}

pub struct ProcessRunner;

impl CommandRunner for ProcessRunner {
    fn run(&self, tool: &str, argv: &[String]) -> String {
        let tool_s = tool.to_string();
        let argv_s = argv.to_vec();
        let (tx, rx) = mpsc::channel();

        std::thread::spawn(move || {
            let out = platform_spawn(&tool_s, &argv_s);
            let _ = tx.send(out);
        });

        match rx.recv_timeout(Duration::from_secs(TIMEOUT_SECS)) {
            Ok(Ok(o)) => {
                let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
                let err = String::from_utf8_lossy(&o.stderr);
                if !err.trim().is_empty() {
                    s.push_str(&err);
                }
                if s.trim().is_empty() {
                    format!("(exit {:?}, no output)", o.status.code())
                } else {
                    s
                }
            }
            Ok(Err(e)) => format!("ERROR: failed to run `{tool}`: {e}"),
            Err(_) => format!("TIMEOUT: `{tool}` did not complete within {TIMEOUT_SECS}s"),
        }
    }
}

/// On Windows, Tauri (a GUI app) inherits PATH from the process that launched it — usually
/// Explorer, which has the PATH from login time. Tools installed later (nmap, git, etc.) add
/// registry PATH entries that never reach the running Tauri process.
///
/// PowerShell reads PATH fresh from the Windows registry at startup, so routing tool calls
/// through `powershell -Command` gives us the same PATH a freshly-opened terminal window has.
///
/// Argument quoting: each argv element is single-quoted (PowerShell literal strings) with
/// embedded single-quotes doubled (`''`). The tool name gets the same treatment.
#[cfg(windows)]
fn platform_spawn(tool: &str, argv: &[String]) -> std::io::Result<std::process::Output> {
    let path_refresh =
        "$env:Path=[Environment]::GetEnvironmentVariable('Path','Machine')+';'\
         +[Environment]::GetEnvironmentVariable('Path','User')";

    let quoted_tool = format!("'{}'", tool.replace('\'', "''"));
    let quoted_args = argv
        .iter()
        .map(|a| format!("'{}'", a.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(" ");

    let ps_cmd = format!("{path_refresh}; & {quoted_tool} {quoted_args}");

    std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps_cmd])
        .output()
}

#[cfg(not(windows))]
fn platform_spawn(tool: &str, argv: &[String]) -> std::io::Result<std::process::Output> {
    std::process::Command::new(tool).args(argv).output()
}

use std::sync::mpsc;
use std::time::Duration;

fn timeout_for(tool: &str) -> Duration {
    match tool {
        "nmap" | "masscan" | "rustscan" => Duration::from_secs(1200),
        "gobuster" | "ffuf" | "feroxbuster" | "wfuzz" | "dirsearch" | "nikto" => Duration::from_secs(900),
        "hydra" | "medusa" | "patator" | "sqlmap" | "john" | "hashcat" => Duration::from_secs(1800),
        _ => Duration::from_secs(300),
    }
}

pub trait CommandRunner: Send + Sync {
    fn run(&self, tool: &str, argv: &[String]) -> String;
}

/// Spawns real subprocesses. Holds an optional sudo password so privileged tools can be
/// elevated without requiring NOPASSWD sudoers configuration.
pub struct ProcessRunner {
    pub sudo_password: Option<String>,
}

impl ProcessRunner {
    pub fn new() -> Self {
        Self { sudo_password: None }
    }

    pub fn with_sudo_password(password: Option<String>) -> Self {
        Self { sudo_password: password }
    }
}

impl Default for ProcessRunner {
    fn default() -> Self { Self::new() }
}

impl CommandRunner for ProcessRunner {
    fn run(&self, tool: &str, argv: &[String]) -> String {
        let tool_s = tool.to_string();
        let argv_s = argv.to_vec();
        let password = self.sudo_password.clone();
        let (tx, rx) = mpsc::channel();

        std::thread::spawn(move || {
            let out = platform_spawn(&tool_s, &argv_s, password.as_deref());
            let _ = tx.send(out);
        });

        let timeout = timeout_for(tool);
        match rx.recv_timeout(timeout) {
            Ok(Ok(o)) => {
                let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
                let err = String::from_utf8_lossy(&o.stderr);
                // Strip the sudo password prompt line from stderr so it doesn't leak into context.
                let filtered: String = err
                    .lines()
                    .filter(|l| {
                        let low = l.to_lowercase();
                        !low.contains("[sudo]") && !low.trim_start().starts_with("password")
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !filtered.trim().is_empty() {
                    s.push_str(&filtered);
                }
                if s.trim().is_empty() {
                    format!("(exit {:?}, no output)", o.status.code())
                } else {
                    s
                }
            }
            Ok(Err(e)) => format!("ERROR: failed to run `{tool}`: {e}"),
            Err(_) => format!("TIMEOUT: `{tool}` did not complete within {}s", timeout.as_secs()),
        }
    }
}

/// Tools that require raw socket / packet capture access. Auto-elevated with sudo on non-Windows.
#[cfg(not(windows))]
fn needs_root(tool: &str) -> bool {
    matches!(tool,
        "nmap" | "masscan" | "rustscan" |
        "tcpdump" | "tshark" | "wireshark" |
        "arp-scan" | "netdiscover" | "arping"
    )
}

#[cfg(not(windows))]
fn platform_spawn(
    tool: &str,
    argv: &[String],
    sudo_password: Option<&str>,
) -> std::io::Result<std::process::Output> {
    use std::io::Write;
    use std::process::Stdio;

    // Already going through sudo explicitly — just run it, piping password if we have one.
    if tool == "sudo" {
        if let Some(pw) = sudo_password {
            let mut child = std::process::Command::new("sudo")
                .arg("-S")
                .args(argv)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(format!("{pw}\n").as_bytes());
            }
            return child.wait_with_output();
        }
        return std::process::Command::new("sudo").args(argv).output();
    }

    // Tools that always need root: elevate automatically.
    if needs_root(tool) {
        if let Some(pw) = sudo_password {
            let mut child = std::process::Command::new("sudo")
                .args(["-S", tool])
                .args(argv)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(format!("{pw}\n").as_bytes());
            }
            return child.wait_with_output();
        }
        // No password stored — try non-interactive (works if NOPASSWD or already root).
        let mut sudo_argv = vec!["-n".to_string(), tool.to_string()];
        sudo_argv.extend_from_slice(argv);
        return std::process::Command::new("sudo").args(&sudo_argv).output();
    }

    std::process::Command::new(tool).args(argv).output()
}

#[cfg(windows)]
fn platform_spawn(
    tool: &str,
    argv: &[String],
    _sudo_password: Option<&str>,
) -> std::io::Result<std::process::Output> {
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

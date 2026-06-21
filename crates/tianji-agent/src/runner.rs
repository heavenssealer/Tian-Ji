use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

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
/// elevated without requiring NOPASSWD sudoers configuration, and a shared `cancel` flag so the
/// operator's Stop button can interrupt a long-running tool (otherwise the async turn would block
/// inside `run()` for up to the per-tool timeout, making Stop appear to do nothing).
pub struct ProcessRunner {
    pub sudo_password: Option<String>,
    /// Shared with the [`Orchestrator`](crate::Orchestrator): set true by `cancel()`. Polled
    /// while a tool runs; when it flips we kill the process group and return promptly.
    cancel: Arc<AtomicBool>,
}

impl ProcessRunner {
    pub fn new() -> Self {
        Self { sudo_password: None, cancel: Arc::new(AtomicBool::new(false)) }
    }

    pub fn with_sudo_password(password: Option<String>) -> Self {
        Self { sudo_password: password, cancel: Arc::new(AtomicBool::new(false)) }
    }

    /// Share the orchestrator's cancellation flag so Stop can interrupt a running tool.
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = cancel;
        self
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
        let elevated = is_elevated_tool(tool);

        // The child's pid (== process-group id, since we put it in its own group) is published
        // here as soon as it spawns, so the polling loop below can kill the whole group.
        let pid_slot: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
        let pid_for_thread = pid_slot.clone();

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let out = run_to_completion(&tool_s, &argv_s, password.as_deref(), &pid_for_thread);
            let _ = tx.send(out);
        });

        let timeout = timeout_for(tool);
        let start = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(150)) {
                Ok(Ok(o)) => return format_output(tool, o),
                Ok(Err(e)) => return format_spawn_error(tool, &e),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if self.cancel.load(Ordering::SeqCst) {
                        let pid = *pid_slot.lock().unwrap();
                        kill_process_group(pid, elevated, self.sudo_password.as_deref());
                        return format!(
                            "CANCELLED: `{tool}` was stopped on operator request before it finished."
                        );
                    }
                    if start.elapsed() >= timeout {
                        let pid = *pid_slot.lock().unwrap();
                        kill_process_group(pid, elevated, self.sudo_password.as_deref());
                        return format!(
                            "TIMEOUT: `{tool}` did not complete within {}s (process killed).",
                            timeout.as_secs()
                        );
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return format!("ERROR: `{tool}` runner thread exited unexpectedly");
                }
            }
        }
    }
}

/// Format a finished process's stdout+stderr, stripping the sudo password prompt so it never
/// leaks into the model's context.
fn format_output(tool: &str, o: std::process::Output) -> String {
    let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
    let err = String::from_utf8_lossy(&o.stderr);
    let filtered: String = err
        .lines()
        .filter(|l| {
            let low = l.to_lowercase();
            !low.contains("[sudo]") && !low.trim_start().starts_with("password")
        })
        .collect::<Vec<_>>()
        .join("\n");
    if !filtered.trim().is_empty() {
        if !s.is_empty() && !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(&filtered);
    }
    if s.trim().is_empty() {
        format!("(`{tool}` exited {:?}, no output)", o.status.code())
    } else {
        s
    }
}

/// Turn a spawn failure into an actionable message. A missing executable is the common case the
/// model must recognise: in OPEN mode it can install the tool itself, in CONTROLLED mode it
/// should ask the operator. The message stays mode-neutral; the system prompt drives the choice.
fn format_spawn_error(tool: &str, e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        format!("ERROR: `{tool}` is not installed on this machine (command not found).")
    } else {
        format!("ERROR: failed to run `{tool}`: {e}")
    }
}

/// Whether a tool is run under sudo (so killing it requires elevated privileges too).
fn is_elevated_tool(tool: &str) -> bool {
    #[cfg(not(windows))]
    {
        tool == "sudo" || needs_root(tool)
    }
    #[cfg(windows)]
    {
        let _ = tool;
        false
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
fn run_to_completion(
    tool: &str,
    argv: &[String],
    sudo_password: Option<&str>,
    pid_slot: &Mutex<Option<u32>>,
) -> std::io::Result<std::process::Output> {
    use std::io::Write;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let mut cmd;
    let mut writes_pw = false;

    if tool == "sudo" {
        cmd = Command::new("sudo");
        if sudo_password.is_some() {
            cmd.arg("-S");
            writes_pw = true;
        }
        cmd.args(argv);
    } else if needs_root(tool) {
        cmd = Command::new("sudo");
        if sudo_password.is_some() {
            cmd.args(["-S", tool]);
            writes_pw = true;
        } else {
            // No password stored — try non-interactive (works if NOPASSWD or already root).
            cmd.args(["-n", tool]);
        }
        cmd.args(argv);
    } else {
        cmd = Command::new(tool);
        cmd.args(argv);
    }

    // Put the child in its own process group so we can later signal the whole tree (including a
    // sudo parent and its tool child) with a single negative-pid kill.
    cmd.process_group(0);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if writes_pw {
        cmd.stdin(Stdio::piped());
    }

    let mut child = cmd.spawn()?;
    *pid_slot.lock().unwrap() = Some(child.id());

    if writes_pw {
        if let (Some(mut stdin), Some(pw)) = (child.stdin.take(), sudo_password) {
            let _ = stdin.write_all(format!("{pw}\n").as_bytes());
        }
    }

    child.wait_with_output()
}

#[cfg(not(windows))]
fn kill_process_group(pid: Option<u32>, elevated: bool, sudo_password: Option<&str>) {
    let Some(pid) = pid else { return };
    use std::process::Command;
    // Negative pid targets the whole process group (the child is its own group leader).
    let target = format!("-{pid}");

    if elevated {
        // The process runs as root; only root can signal it, so kill through sudo.
        if let Some(pw) = sudo_password {
            use std::io::Write;
            use std::process::Stdio;
            if let Ok(mut c) = Command::new("sudo")
                .args(["-S", "kill", "-9", &target])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                if let Some(mut s) = c.stdin.take() {
                    let _ = s.write_all(format!("{pw}\n").as_bytes());
                }
                let _ = c.wait();
                return;
            }
        }
        let _ = Command::new("sudo").args(["-n", "kill", "-9", &target]).status();
    } else {
        let _ = Command::new("kill").args(["-9", &target]).status();
    }
}

#[cfg(windows)]
fn run_to_completion(
    tool: &str,
    argv: &[String],
    _sudo_password: Option<&str>,
    pid_slot: &Mutex<Option<u32>>,
) -> std::io::Result<std::process::Output> {
    use std::process::{Command, Stdio};

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

    let mut child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps_cmd])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    *pid_slot.lock().unwrap() = Some(child.id());
    child.wait_with_output()
}

#[cfg(windows)]
fn kill_process_group(pid: Option<u32>, _elevated: bool, _sudo_password: Option<&str>) {
    if let Some(pid) = pid {
        use std::process::Command;
        // /T kills the whole tree spawned by the PowerShell host, /F forces it.
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status();
    }
}

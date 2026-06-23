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

/// Tools that are genuine RTK (Rust Token Killer) sub-commands — running them as `rtk <tool> …`
/// compresses their output 60–90% before it reaches the model. We only wrap this curated set (the
/// commands RTK actually understands); pentest/network tools run raw and go through our own
/// summarizer instead. Read-only, so they stay cache-eligible.
const RTK_TOOLS: &[&str] =
    &["ls", "grep", "rg", "find", "git", "cat", "head", "tail", "tree", "cargo", "pytest", "jest", "docker"];

/// Resolve the `rtk` binary once per process, returning the path to invoke it by (or `None` if it
/// isn't installed). A GUI-launched app (Finder/desktop icon) gets a minimal `PATH` that usually
/// excludes `~/.cargo/bin` and Homebrew, so a `cargo install rtk` / `brew install rtk` wouldn't be
/// found by name — we probe the common install locations by absolute path as a fallback.
pub fn detect_rtk() -> Option<String> {
    use std::sync::OnceLock;
    // Cache only a *successful* resolution (permanent once found). A negative result is NOT cached,
    // so installing `rtk` mid-session is picked up on the next call — the negative probe is cheap
    // (a fast NotFound spawn + a few path-existence checks).
    static RTK: OnceLock<String> = OnceLock::new();
    if let Some(p) = RTK.get() {
        return Some(p.clone());
    }
    let resolved = resolve_rtk();
    if let Some(p) = &resolved {
        let _ = RTK.set(p.clone());
    }
    resolved
}

fn resolve_rtk() -> Option<String> {
    let exe = if cfg!(windows) { "rtk.exe" } else { "rtk" };

    // 1) Already reachable by name on the process PATH.
    if probe_rtk(exe) {
        return Some(exe.to_string());
    }

    // 2) Common install dirs that a GUI app's PATH typically omits.
    let home = std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).ok();
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(h) = &home {
        candidates.push(std::path::Path::new(h).join(".cargo").join("bin").join(exe));
        candidates.push(std::path::Path::new(h).join(".local").join("bin").join(exe));
    }
    if !cfg!(windows) {
        candidates.push("/usr/local/bin/rtk".into());
        candidates.push("/opt/homebrew/bin/rtk".into());
        candidates.push("/usr/bin/rtk".into());
    }
    candidates
        .into_iter()
        .find(|c| c.exists() && probe_rtk(&c.to_string_lossy()))
        .map(|c| c.to_string_lossy().into_owned())
}

/// `<bin> --version` succeeds.
fn probe_rtk(bin: &str) -> bool {
    std::process::Command::new(bin)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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
    /// Route RTK-supported commands through `rtk` to shrink their output (no-op if `rtk` isn't
    /// installed). Set from the operator's `use_rtk` setting.
    use_rtk: bool,
}

impl ProcessRunner {
    pub fn new() -> Self {
        Self { sudo_password: None, cancel: Arc::new(AtomicBool::new(false)), use_rtk: false }
    }

    pub fn with_sudo_password(password: Option<String>) -> Self {
        Self { sudo_password: password, cancel: Arc::new(AtomicBool::new(false)), use_rtk: false }
    }

    /// Share the orchestrator's cancellation flag so Stop can interrupt a running tool.
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = cancel;
        self
    }

    /// Enable RTK output compression for supported commands (effective only when `rtk` is on PATH).
    pub fn with_rtk(mut self, enabled: bool) -> Self {
        self.use_rtk = enabled;
        self
    }

    /// Rewrite `(tool, argv)` to `(<rtk>, [tool, ..argv])` when RTK can compress this command.
    fn rtk_wrap(&self, tool: &str, argv: &[String]) -> (String, Vec<String>) {
        if self.use_rtk && RTK_TOOLS.contains(&tool) {
            if let Some(rtk) = detect_rtk() {
                let mut a = Vec::with_capacity(argv.len() + 1);
                a.push(tool.to_string());
                a.extend_from_slice(argv);
                return (rtk, a);
            }
        }
        (tool.to_string(), argv.to_vec())
    }
}

impl Default for ProcessRunner {
    fn default() -> Self { Self::new() }
}

impl CommandRunner for ProcessRunner {
    fn run(&self, tool: &str, argv: &[String]) -> String {
        // Execution may be rewritten to `rtk <tool> …`; messages, timeout and elevation still key
        // off the original tool name.
        let (tool_s, argv_s) = self.rtk_wrap(tool, argv);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtk_wrap_is_noop_when_disabled() {
        let r = ProcessRunner::new(); // use_rtk = false
        let (tool, argv) = r.rtk_wrap("grep", &["-r".into(), "flag".into()]);
        assert_eq!(tool, "grep");
        assert_eq!(argv, vec!["-r".to_string(), "flag".to_string()]);
    }

    #[test]
    fn rtk_wrap_leaves_unsupported_tools_alone() {
        // Even enabled, a non-RTK tool (nmap) is never rewritten.
        let r = ProcessRunner::new().with_rtk(true);
        let (tool, _) = r.rtk_wrap("nmap", &["-sV".into()]);
        assert_eq!(tool, "nmap");
    }
}

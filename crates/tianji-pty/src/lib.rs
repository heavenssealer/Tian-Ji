//! # tianji-pty — terminal manager (DESIGN.md §3.4)
//!
//! Spawn/track/close real PTYs via `portable-pty`. Each terminal runs the user's shell; a
//! reader thread pumps its output into a `tokio::broadcast` channel that the Tauri layer
//! forwards to the matching xterm pane (and, later, the event log). Agents and the human share
//! these terminals — when an approved command runs, the human watches it happen.
//!
//! v0.1 handles **one-shot** commands written into the shell. Interactive/stateful tools
//! (msfconsole, sqlmap REPLs) need a different send-keys/read-until-prompt model and are
//! deferred (DESIGN.md §11.2).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Mutex;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use tianji_types::TerminalId;
use tokio::sync::broadcast;

#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("terminal not found: {0}")]
    NotFound(TerminalId),
    #[error("pty io error: {0}")]
    Io(String),
}

type Result<T> = std::result::Result<T, PtyError>;

/// A chunk of terminal output, broadcast to subscribers. Must be `Clone` for `broadcast`.
#[derive(Debug, Clone)]
pub struct PtyChunk {
    pub terminal_id: TerminalId,
    pub bytes: Vec<u8>,
}

struct Handle {
    writer: Box<dyn Write + Send>,
    tx: broadcast::Sender<PtyChunk>,
    /// Kept alive so the PTY isn't closed; also used to resize the PTY.
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

#[derive(Default)]
pub struct PtyManager {
    terminals: Mutex<HashMap<TerminalId, Handle>>,
}

impl PtyManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a PTY, spawn the user's shell, and start pumping its output to a broadcast channel.
    pub fn spawn(&self, _title: &str) -> Result<TerminalId> {
        let sys = native_pty_system();
        let pair = sys
            .openpty(PtySize { rows: 24, cols: 100, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| PtyError::Io(e.to_string()))?;

        let shell = default_shell();
        let child = pair
            .slave
            .spawn_command(CommandBuilder::new(shell))
            .map_err(|e| PtyError::Io(e.to_string()))?;

        let mut reader = pair.master.try_clone_reader().map_err(|e| PtyError::Io(e.to_string()))?;
        let writer = pair.master.take_writer().map_err(|e| PtyError::Io(e.to_string()))?;

        let id = TerminalId::new();
        let (tx, _rx) = broadcast::channel(1024);
        let tx_reader = tx.clone();

        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        // No receivers yet is fine; send() only errs when all are dropped.
                        let _ = tx_reader.send(PtyChunk { terminal_id: id, bytes: buf[..n].to_vec() });
                    }
                }
            }
        });

        self.terminals
            .lock()
            .unwrap()
            .insert(id, Handle { writer, tx, master: pair.master, child });
        Ok(id)
    }

    /// Resize the PTY to match the front-end terminal. Without this the shell's cursor math
    /// drifts from what xterm shows (prompt lands mid-screen, bottom rows clip).
    pub fn resize(&self, id: TerminalId, rows: u16, cols: u16) -> Result<()> {
        let map = self.terminals.lock().unwrap();
        let h = map.get(&id).ok_or(PtyError::NotFound(id))?;
        h.master
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| PtyError::Io(e.to_string()))?;
        Ok(())
    }

    /// Raw bytes from the user's keyboard into the pty.
    pub fn write(&self, id: TerminalId, bytes: &[u8]) -> Result<()> {
        let mut map = self.terminals.lock().unwrap();
        let h = map.get_mut(&id).ok_or(PtyError::NotFound(id))?;
        h.writer.write_all(bytes).map_err(|e| PtyError::Io(e.to_string()))?;
        h.writer.flush().map_err(|e| PtyError::Io(e.to_string()))?;
        Ok(())
    }

    /// Run an *already policy-approved* command by writing it into the shell.
    pub fn run(&self, id: TerminalId, tool: &str, argv: &[String]) -> Result<()> {
        let line = format!("{} {}\r\n", tool, argv.join(" "));
        self.write(id, line.as_bytes())
    }

    pub fn subscribe(&self, id: TerminalId) -> Result<broadcast::Receiver<PtyChunk>> {
        let map = self.terminals.lock().unwrap();
        let h = map.get(&id).ok_or(PtyError::NotFound(id))?;
        Ok(h.tx.subscribe())
    }

    pub fn close(&self, id: TerminalId) -> Result<()> {
        if let Some(mut h) = self.terminals.lock().unwrap().remove(&id) {
            let _ = h.child.kill();
        }
        Ok(())
    }
}

fn default_shell() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
}

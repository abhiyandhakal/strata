use std::io::{self, IsTerminal, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClipboardBackend {
    Arboard,
    Command(&'static str),
    Osc52,
    Memory,
}

impl ClipboardBackend {
    pub fn label(&self) -> String {
        match self {
            ClipboardBackend::Arboard => "system clipboard".to_string(),
            ClipboardBackend::Command(name) => (*name).to_string(),
            ClipboardBackend::Osc52 => "OSC52".to_string(),
            ClipboardBackend::Memory => "memory".to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardResult {
    pub backend: ClipboardBackend,
    pub bytes: usize,
}

#[derive(Clone)]
pub struct Clipboard {
    mode: ClipboardMode,
}

#[derive(Clone)]
#[cfg_attr(not(test), allow(dead_code))]
enum ClipboardMode {
    System,
    Memory(Arc<Mutex<Vec<String>>>),
}

impl Clipboard {
    pub fn system() -> Self {
        Self {
            mode: ClipboardMode::System,
        }
    }

    #[cfg(test)]
    pub fn memory() -> (Self, Arc<Mutex<Vec<String>>>) {
        let store = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                mode: ClipboardMode::Memory(store.clone()),
            },
            store,
        )
    }

    pub fn write_text(&self, text: &str) -> Result<ClipboardResult> {
        match &self.mode {
            ClipboardMode::Memory(store) => {
                store.lock().expect("clipboard memory lock").push(text.to_string());
                Ok(ClipboardResult {
                    backend: ClipboardBackend::Memory,
                    bytes: text.len(),
                })
            }
            ClipboardMode::System => write_to_system_clipboard(text),
        }
    }
}

fn write_to_system_clipboard(text: &str) -> Result<ClipboardResult> {
    #[cfg(target_os = "linux")]
    {
        for (program, args, label) in [
            ("wl-copy", &[][..], "wl-copy"),
            ("xclip", &["-selection", "clipboard"][..], "xclip"),
            ("xsel", &["--clipboard", "--input"][..], "xsel"),
        ] {
            if command_exists(program) && write_via_command(program, args, text).is_ok() {
                return Ok(ClipboardResult {
                    backend: ClipboardBackend::Command(label),
                    bytes: text.len(),
                });
            }
        }
    }

    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        if clipboard.set_text(text.to_string()).is_ok() {
            return Ok(ClipboardResult {
                backend: ClipboardBackend::Arboard,
                bytes: text.len(),
            });
        }
    }

    #[cfg(target_os = "macos")]
    {
        if write_via_command("pbcopy", &[], text).is_ok() {
            return Ok(ClipboardResult {
                backend: ClipboardBackend::Command("pbcopy"),
                bytes: text.len(),
            });
        }
    }

    #[cfg(target_os = "windows")]
    {
        if write_via_command(
            "powershell",
            &["-NoProfile", "-Command", "Set-Clipboard"],
            text,
        )
        .is_ok()
        {
            return Ok(ClipboardResult {
                backend: ClipboardBackend::Command("powershell"),
                bytes: text.len(),
            });
        }
        if write_via_command("clip", &[], text).is_ok() {
            return Ok(ClipboardResult {
                backend: ClipboardBackend::Command("clip"),
                bytes: text.len(),
            });
        }
    }

    if io::stdout().is_terminal() {
        emit_osc52(text)?;
        return Ok(ClipboardResult {
            backend: ClipboardBackend::Osc52,
            bytes: text.len(),
        });
    }

    bail!("no clipboard backend available")
}

fn command_exists(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
        || Command::new("which")
            .arg(program)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
}

fn write_via_command(program: &str, args: &[&str], text: &str) -> Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to start clipboard command {program}"))?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        bail!("clipboard command {program} failed with {status}")
    }
}

fn emit_osc52(text: &str) -> Result<()> {
    use base64::Engine;

    let payload = base64::engine::general_purpose::STANDARD.encode(text);
    let mut stdout = io::stdout().lock();
    write!(stdout, "\u{1b}]52;c;{payload}\u{07}")?;
    stdout.flush()?;
    Ok(())
}

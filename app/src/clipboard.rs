//! Copying a short text (the site code) to the system clipboard from the
//! tray, where there is no window to copy from. The system's own tool reads
//! the text on stdin; nothing goes through a shell.

use std::io::Write;
use std::process::{Command, Stdio};

/// The clipboard tools to try, first that works wins.
fn tools() -> &'static [(&'static str, &'static [&'static str])] {
    if cfg!(target_os = "macos") {
        &[("/usr/bin/pbcopy", &[])]
    } else if cfg!(windows) {
        &[("clip.exe", &[])]
    } else {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    }
}

/// Copies `text` (ASCII: `clip.exe` reads the console code page).
pub fn copy(text: &str) -> Result<(), String> {
    if !text.is_ascii() {
        return Err("only plain ASCII is copied".into());
    }
    let mut last = String::from("no clipboard tool");
    for (tool, args) in tools() {
        let mut cmd = Command::new(tool);
        cmd.args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                last = format!("{tool}: {e}");
                continue;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        match child.wait() {
            Ok(s) if s.success() => return Ok(()),
            Ok(s) => last = format!("{tool}: {s}"),
            Err(e) => last = format!("{tool}: {e}"),
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_text_the_console_tools_would_garble() {
        assert!(copy("код").is_err());
    }
}

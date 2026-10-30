//! SIGTERM and SIGINT (macOS, Linux) end the app the way «Выйти» does: the
//! bridge stops and so does its private adb server on 5039. Without this a
//! plain `kill` skipped `RunEvent::Exit` and left that server running.
//! (Quit from the Dock or logging out on macOS come as Apple Events, which
//! already end in `RunEvent::Exit`.) A second signal exits at once.

use tauri::AppHandle;

#[cfg(unix)]
use crate::{lock, shared, tray};

/// SIGTERM and SIGINT, both registered at once when this is made, so no
/// signal falls between two registrations; `next` waits for either.
#[cfg(unix)]
pub struct Termination {
    term: tokio::signal::unix::Signal,
    int: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Termination {
    /// Must run inside a Tokio runtime (its signal driver).
    pub fn new() -> std::io::Result<Termination> {
        use tokio::signal::unix::{signal, SignalKind};
        Ok(Termination {
            term: signal(SignalKind::terminate())?,
            int: signal(SignalKind::interrupt())?,
        })
    }

    /// The next SIGTERM or SIGINT.
    pub async fn next(&mut self) {
        tokio::select! {
            _ = self.term.recv() => {}
            _ = self.int.recv() => {}
        }
    }
}

/// Quits on SIGTERM or SIGINT; a second one exits at once. The handlers are
/// set up on the Tauri runtime (Tokio's signal driver lives there, not in
/// `setup`), both before the first wait.
#[cfg(unix)]
pub fn quit_on_termination(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut signals = match Termination::new() {
            Ok(s) => s,
            Err(e) => {
                lock(&shared(&app)).push_log(format!("[bridge] no signal handling: {e}"));
                return;
            }
        };
        signals.next().await;
        lock(&shared(&app)).push_log("[bridge] stopping: the system asked the app to end".into());
        tray::quit(app);
        signals.next().await;
        std::process::exit(1);
    });
}

/// Windows: the app has no console to close; logging off ends it through
/// `RunEvent::Exit`, which stops the adb server too.
#[cfg(not(unix))]
pub fn quit_on_termination(_app: &AppHandle) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    fn raise(sig: &str) {
        let status = std::process::Command::new("kill")
            .args([sig, &std::process::id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
    }

    // One test, so the two signals never overlap with each other.
    #[tokio::test]
    async fn sigterm_and_sigint_are_caught_not_fatal() {
        let mut signals = Termination::new().unwrap();
        for sig in ["-TERM", "-INT"] {
            raise(sig);
            tokio::time::timeout(Duration::from_secs(5), signals.next())
                .await
                .unwrap_or_else(|_| panic!("{sig} was not seen"));
        }
    }
}

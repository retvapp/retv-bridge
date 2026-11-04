//! macOS: switching to Bridge with Cmd+Tab while no window is open shows the window. AppKit sends `Reopen` only for a
//! Dock click, so Bridge listens for `NSApplicationDidBecomeActiveNotification` too. The first seconds after start
//! are ignored, so «open at login» does not pop the window up.

#[cfg(target_os = "macos")]
pub fn reveal_on_activate(app: &tauri::AppHandle) {
    use block2::RcBlock;
    use objc2_foundation::{NSNotification, NSNotificationCenter, NSString};
    use std::ptr::NonNull;
    use std::time::{Duration, Instant};

    let started = Instant::now();
    let handle = app.clone();
    let block = RcBlock::new(move |_note: NonNull<NSNotification>| {
        if started.elapsed() < Duration::from_secs(3) {
            return;
        }
        let open =
            crate::window::main_window(&handle).is_some_and(|w| w.is_visible().unwrap_or(false));
        if !open {
            crate::window::reveal(&handle);
        }
    });
    let center = NSNotificationCenter::defaultCenter();
    let name = NSString::from_str("NSApplicationDidBecomeActiveNotification");
    // SAFETY: the block lives for the whole app (the observer is leaked on purpose: Bridge never stops
    // listening), and it only touches Tauri handles, which are Send + Sync.
    let observer = unsafe {
        center.addObserverForName_object_queue_usingBlock(Some(&name), None, None, &block)
    };
    std::mem::forget(observer);
}

#[cfg(not(target_os = "macos"))]
pub fn reveal_on_activate(_app: &tauri::AppHandle) {}

use crate::error::NetError;

/// Copies the room code to the system clipboard.
/// Best-effort: logs warning on failure (headless/CI environments may not have clipboard).
#[allow(clippy::result_large_err)]
pub fn copy_to_clipboard(text: &str) -> Result<(), NetError> {
    #[cfg(target_os = "linux")]
    {
        copy_linux(text)
    }
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        copy_via_arboard(text)
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
#[allow(clippy::result_large_err)]
fn copy_via_arboard(text: &str) -> Result<(), NetError> {
    match arboard::Clipboard::new() {
        Ok(mut clipboard) => clipboard
            .set_text(text)
            .map_err(|e| NetError::Clipboard(e.to_string())),
        Err(e) => {
            tracing::warn!("Clipboard unavailable (headless?): {}", e);
            Err(NetError::Clipboard(e.to_string()))
        }
    }
}

// wl-copy spawns its own daemon to own the selection, so the clipboard content
// persists after this process returns. arboard on Linux drops the selection
// owner as soon as the handle is dropped, making the copy a no-op unless a
// clipboard manager is running.
#[cfg(target_os = "linux")]
#[allow(clippy::result_large_err)]
fn copy_linux(text: &str) -> Result<(), NetError> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            tracing::warn!("wl-copy unavailable: {}", e);
            NetError::Clipboard(format!("wl-copy spawn failed: {}", e))
        })?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| NetError::Clipboard("wl-copy stdin unavailable".to_string()))?;
    stdin
        .write_all(text.as_bytes())
        .map_err(|e| NetError::Clipboard(format!("wl-copy write failed: {}", e)))?;
    drop(stdin);

    let status = child
        .wait()
        .map_err(|e| NetError::Clipboard(format!("wl-copy wait failed: {}", e)))?;
    if status.success() {
        Ok(())
    } else {
        Err(NetError::Clipboard(format!(
            "wl-copy exited with status {}",
            status
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_to_clipboard_does_not_panic() {
        // Clipboard may not be available in headless CI -- just ensure no panic
        let _ = copy_to_clipboard("test-room-code");
    }
}

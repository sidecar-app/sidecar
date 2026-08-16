use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};

use ::ipc::{HostToSidecar, SidecarToHost};

/// Shared collection of client writer handles for broadcasting events.
pub type ClientWriters = Arc<Mutex<Vec<BufWriter<UnixStream>>>>;

/// Check for and clean up stale socket files from previous crashes.
/// If a socket file exists and no process is listening, remove it.
/// If another sidecar is actively listening, return AddrInUse error.
pub fn cleanup_stale_socket(path: &str) -> std::io::Result<()> {
    if std::path::Path::new(path).exists() {
        match UnixStream::connect(path) {
            Ok(_stream) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "Socket is active -- another sidecar is already running",
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                tracing::info!("Removing stale socket: {}", path);
                std::fs::remove_file(path)?;
            }
            Err(e) => {
                tracing::warn!("Socket exists but connect failed ({}), removing", e);
                std::fs::remove_file(path)?;
            }
        }
    }
    Ok(())
}

/// Broadcast a SidecarToHost event to all connected clients.
/// Removes disconnected clients (broken pipe) from the list via retain_mut.
pub fn broadcast(writers: &ClientWriters, msg: &SidecarToHost) {
    if let Ok(json) = serde_json::to_string(msg) {
        let mut guard = writers.lock().unwrap();
        guard.retain_mut(|writer| {
            if writeln!(writer, "{}", json).is_ok() {
                writer.flush().is_ok()
            } else {
                tracing::debug!("Removing disconnected client writer");
                false
            }
        });
    }
}

/// Start the Unix socket server on the given path.
/// Spawns a background accept-loop thread. For each client:
///   - Sends initial StateChanged with current state
///   - Adds writer handle to shared ClientWriters for broadcast
///   - Spawns reader thread that forwards HostToSidecar messages to ipc_tx
///
/// Reader thread EOF does NOT send Shutdown (multi-client: one disconnect is not fatal).
pub fn start_server(
    socket_path: &str,
    ipc_tx: calloop::channel::Sender<HostToSidecar>,
    client_writers: ClientWriters,
    state_snapshot: Arc<dyn Fn() -> SidecarToHost + Send + Sync>,
) -> std::io::Result<()> {
    cleanup_stale_socket(socket_path)?;

    let listener = UnixListener::bind(socket_path)?;
    tracing::info!("IPC server listening on {}", socket_path);

    let writers = client_writers;
    std::thread::Builder::new()
        .name("ipc-accept".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        tracing::info!("New IPC client connected");
                        let reader_stream = match stream.try_clone() {
                            Ok(s) => s,
                            Err(e) => {
                                tracing::warn!("Failed to clone client stream: {}", e);
                                continue;
                            }
                        };

                        // Send initial state to new client before adding to broadcast list
                        let mut writer = BufWriter::new(stream);
                        let state_msg = state_snapshot();
                        if let Ok(json) = serde_json::to_string(&state_msg) {
                            let _ = writeln!(writer, "{}", json);
                            let _ = writer.flush();
                        }

                        writers.lock().unwrap().push(writer);

                        let reader_tx = ipc_tx.clone();
                        std::thread::Builder::new()
                            .name("ipc-client-reader".into())
                            .spawn(move || {
                                let reader = BufReader::new(reader_stream);
                                for line in reader.lines() {
                                    match line {
                                        Ok(line) if line.is_empty() => continue,
                                        Ok(line) => {
                                            match serde_json::from_str::<HostToSidecar>(&line) {
                                                Ok(msg) => {
                                                    if reader_tx.send(msg).is_err() {
                                                        tracing::info!("IPC channel closed, client reader exiting");
                                                        return;
                                                    }
                                                }
                                                Err(e) => {
                                                    tracing::warn!("Failed to parse client message: {} (line: {})", e, line);
                                                }
                                            }
                                        }
                                        Err(_) => {
                                            // Do NOT send Shutdown on disconnect: one client
                                            // leaving must not kill the multi-client sidecar.
                                            tracing::info!("IPC client disconnected");
                                            return;
                                        }
                                    }
                                }
                                tracing::info!("IPC client EOF");
                            })
                            .ok();
                    }
                    Err(e) => {
                        tracing::warn!("Accept error: {}", e);
                    }
                }
            }
        })?;

    Ok(())
}

/// Remove the socket file on clean exit.
pub fn cleanup_socket(path: &str) {
    if std::path::Path::new(path).exists() {
        if let Err(e) = std::fs::remove_file(path) {
            tracing::warn!("Failed to remove socket file {}: {}", path, e);
        } else {
            tracing::info!("Removed socket file: {}", path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cleanup_stale_socket_nonexistent() {
        // Should succeed when no socket file exists
        let result = cleanup_stale_socket("/tmp/nonexistent-test-socket.sock");
        assert!(result.is_ok());
    }

    #[test]
    fn test_cleanup_stale_socket_removes_stale_file() {
        let path = format!("/tmp/sidecar-test-stale-{}.sock", std::process::id());
        // Create a regular file (not a real socket) to simulate stale socket
        std::fs::write(&path, "stale").unwrap();
        assert!(std::path::Path::new(&path).exists());
        // cleanup_stale_socket should remove it (connect will fail)
        let result = cleanup_stale_socket(&path);
        assert!(result.is_ok());
        assert!(!std::path::Path::new(&path).exists());
    }

    #[test]
    fn test_broadcast_writes_to_all_clients() {
        // Create a Unix socket pair to simulate client connections
        let (s1a, s1b) = UnixStream::pair().unwrap();
        let (s2a, s2b) = UnixStream::pair().unwrap();

        let writers: ClientWriters =
            Arc::new(Mutex::new(vec![BufWriter::new(s1a), BufWriter::new(s2a)]));

        let msg = SidecarToHost::StateChanged {
            color: "#FF3B30".to_string(),
            brush_size: 0.003,
            draw_mode: false,
        };

        broadcast(&writers, &msg);

        // Read from the other end of each pair
        let mut reader1 = BufReader::new(s1b);
        let mut reader2 = BufReader::new(s2b);
        let mut line1 = String::new();
        let mut line2 = String::new();

        reader1.read_line(&mut line1).unwrap();
        reader2.read_line(&mut line2).unwrap();

        assert!(line1.contains("state_changed"));
        assert!(line1.contains("#FF3B30"));
        assert!(line2.contains("state_changed"));
        assert!(line2.contains("#FF3B30"));

        // Both writers should still be in the list
        assert_eq!(writers.lock().unwrap().len(), 2);
    }

    #[test]
    fn test_broadcast_removes_dead_clients() {
        let (s1a, s1b) = UnixStream::pair().unwrap();
        let (s2a, s2b) = UnixStream::pair().unwrap();

        // Drop s1b to simulate client 1 disconnecting
        drop(s1b);

        let writers: ClientWriters =
            Arc::new(Mutex::new(vec![BufWriter::new(s1a), BufWriter::new(s2a)]));

        let msg = SidecarToHost::Ready;
        // Need to broadcast a few times for the broken pipe to be detected
        broadcast(&writers, &msg);
        broadcast(&writers, &msg);

        // Dead client should have been removed
        let remaining = writers.lock().unwrap().len();
        assert!(
            remaining <= 1,
            "Expected at most 1 writer, got {}",
            remaining
        );
        drop(s2b);
    }
}

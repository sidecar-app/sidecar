use std::io::Write;
use std::os::unix::net::UnixStream;

/// Send an IPC command to the overlay sidecar via Unix socket.
/// This is the same protocol used by the old sidecar-ctl binary.
pub fn send(msg: ipc::HostToSidecar) {
    let path = ipc::socket_path();
    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sidecar: no active session ({})", e);
            std::process::exit(1);
        }
    };

    let json = serde_json::to_string(&msg).expect("serialize");
    if let Err(e) = writeln!(stream, "{}", json) {
        eprintln!("sidecar: write failed: {}", e);
        std::process::exit(1);
    }
}

// Protocol types are now in the shared `ipc` crate.
// Re-export for backward compatibility within overlay-sidecar.
#[allow(unused_imports)]
pub use ::ipc::{HostToSidecar, SidecarToHost, is_valid_hex_color};

//! macOS input injection backend (stub).
//!
//! Planned: drive the pointer/keyboard via Core Graphics `CGEvent`. Until then
//! `create()` returns [`InputError::Unsupported`] so the workspace still builds
//! and remote control degrades gracefully on macOS.

use crate::{Injector, InputError};

pub(crate) async fn spawn() -> Result<Injector, InputError> {
    Err(InputError::Unsupported)
}

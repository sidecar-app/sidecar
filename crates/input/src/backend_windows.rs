//! Windows input injection backend (stub).
//!
//! Planned: drive the pointer/keyboard via the Win32 `SendInput` API. Until
//! then `create()` returns [`InputError::Unsupported`] so the workspace still
//! builds and remote control degrades gracefully on Windows.

use crate::{Injector, InputError};

pub(crate) async fn spawn() -> Result<Injector, InputError> {
    Err(InputError::Unsupported)
}

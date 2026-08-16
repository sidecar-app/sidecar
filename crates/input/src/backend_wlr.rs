//! wlroots input injection via `zwlr_virtual_pointer_v1` + `zwp_virtual_keyboard_v1`.
//!
//! Used on wlroots-based compositors (Sway, Hyprland, river, ...) where the XDG
//! RemoteDesktop portal has no backend. These protocols are send-only and need
//! no consent dialog (the sidecar master switch remains the gate).
//!
//! The Wayland connection is `!Send`, so it lives entirely on a dedicated thread
//! driven by a calloop event loop (mirroring the overlay-sidecar): a calloop
//! channel delivers [`InputEvent`]s, the WaylandSource handles socket I/O. The
//! returned [`Injector`] only holds the channel sender (wrapped for `Sync`).

use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsFd, FromRawFd};
use std::time::{Duration, Instant};

use calloop::EventLoop;
use calloop::channel::{Event as ChannelEvent, Sender as CalloopSender};
use calloop_wayland_source::WaylandSource;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

use crate::{
    BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, Injector, InputError, InputEvent, MouseButton, keymap,
};

// Absolute-motion coordinate extent. Only the ratio (pos / extent) matters; the
// compositor maps it onto the bound output, so the value is arbitrary.
const MOTION_EXTENT: u32 = 0xffff;

pub(crate) async fn spawn() -> Result<Injector, InputError> {
    let (ready_tx, ready_rx) =
        tokio::sync::oneshot::channel::<Result<CalloopSender<InputEvent>, InputError>>();

    std::thread::Builder::new()
        .name("sidecar-input-wlr".into())
        .spawn(move || run_worker(ready_tx))
        .map_err(|e| InputError::StartFailed(format!("spawn wlr thread: {e}")))?;

    match ready_rx.await {
        Ok(Ok(tx)) => {
            // calloop's Sender is !Sync; a Mutex makes the closure Send + Sync.
            let tx = std::sync::Mutex::new(tx);
            Ok(Injector::from_fn(move |ev| {
                tx.lock().unwrap().send(ev).map_err(|_| InputError::Closed)
            }))
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err(InputError::StartFailed(
            "wlr input thread exited early".into(),
        )),
    }
}

fn run_worker(
    ready_tx: tokio::sync::oneshot::Sender<Result<CalloopSender<InputEvent>, InputError>>,
) {
    let (mut state, mut event_loop, tx) = match setup() {
        Ok(v) => v,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    if ready_tx.send(Ok(tx)).is_err() {
        return; // caller gave up
    }
    tracing::info!("wlroots virtual input ready");

    loop {
        if state.should_exit {
            break;
        }
        if let Err(e) = event_loop.dispatch(Some(Duration::from_millis(250)), &mut state) {
            tracing::error!(error = %e, "wlr input loop error");
            break;
        }
    }
    tracing::info!("wlroots virtual input stopped");
    // Devices drop here, releasing the virtual pointer/keyboard.
}

struct State {
    conn: Connection,
    pointer: ZwlrVirtualPointerV1,
    keyboard: ZwpVirtualKeyboardV1,
    /// Keeps the keymap memfd alive for the device's lifetime.
    _keymap_file: File,
    /// Currently-held modifier keycodes, for explicit `modifiers()` reporting.
    held_mods: HashSet<u32>,
    start: Instant,
    should_exit: bool,
}

impl State {
    fn time(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    fn handle(&mut self, ev: InputEvent) {
        let t = self.time();
        match ev {
            InputEvent::PointerMotionAbsolute { x, y } => {
                let px = (x.clamp(0.0, 1.0) * MOTION_EXTENT as f64) as u32;
                let py = (y.clamp(0.0, 1.0) * MOTION_EXTENT as f64) as u32;
                self.pointer
                    .motion_absolute(t, px, py, MOTION_EXTENT, MOTION_EXTENT);
                self.pointer.frame();
            }
            InputEvent::PointerButton { button, pressed } => {
                let code = match button {
                    MouseButton::Left => BTN_LEFT,
                    MouseButton::Right => BTN_RIGHT,
                    MouseButton::Middle => BTN_MIDDLE,
                };
                let st = if pressed {
                    wl_pointer::ButtonState::Pressed
                } else {
                    wl_pointer::ButtonState::Released
                };
                self.pointer.button(t, code, st);
                self.pointer.frame();
            }
            InputEvent::PointerAxis { dx, dy } => {
                if dy != 0.0 {
                    self.pointer.axis(t, wl_pointer::Axis::VerticalScroll, dy);
                }
                if dx != 0.0 {
                    self.pointer.axis(t, wl_pointer::Axis::HorizontalScroll, dx);
                }
                self.pointer.frame();
            }
            InputEvent::Key { code, pressed } => {
                if let Some(kc) = keymap::dom_code_to_evdev(&code) {
                    let kc = kc as u32;
                    // wl_keyboard/virtual-keyboard key state: 1 = pressed, 0 = released.
                    self.keyboard.key(t, kc, u32::from(pressed));
                    // The virtual-keyboard protocol does NOT derive modifier state
                    // from key() events, so report it explicitly (like wtype does);
                    // otherwise Shift/Ctrl/Alt/Super never apply to the next key.
                    if mod_bit(kc).is_some() {
                        if pressed {
                            self.held_mods.insert(kc);
                        } else {
                            self.held_mods.remove(&kc);
                        }
                        let depressed = self
                            .held_mods
                            .iter()
                            .filter_map(|&k| mod_bit(k))
                            .fold(0u32, |a, b| a | b);
                        self.keyboard.modifiers(depressed, 0, 0, 0);
                    }
                } else {
                    tracing::debug!(code = %code, "ignoring unmapped key");
                }
            }
        }
        let _ = self.conn.flush();
    }
}

fn setup() -> Result<(State, EventLoop<'static, State>, CalloopSender<InputEvent>), InputError> {
    let conn = Connection::connect_to_env()
        .map_err(|e| InputError::StartFailed(format!("wayland connect: {e}")))?;
    let (globals, mut event_queue) = registry_queue_init::<State>(&conn)
        .map_err(|e| InputError::StartFailed(format!("registry init: {e}")))?;
    let qh = event_queue.handle();

    let seat: wl_seat::WlSeat = globals
        .bind(&qh, 1..=8, ())
        .map_err(|e| InputError::StartFailed(format!("no wl_seat: {e}")))?;
    let output: Option<wl_output::WlOutput> = globals.bind(&qh, 1..=4, ()).ok();

    let pmgr: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=2, ()).map_err(|e| {
        InputError::StartFailed(format!(
            "compositor lacks zwlr_virtual_pointer_manager_v1: {e}"
        ))
    })?;
    let kmgr: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).map_err(|e| {
        InputError::StartFailed(format!(
            "compositor lacks zwp_virtual_keyboard_manager_v1: {e}"
        ))
    })?;

    // Bind the pointer to an output (v2) so absolute motion maps onto that
    // monitor; fall back to the layout-wide pointer on older managers.
    let pointer = if pmgr.version() >= 2 {
        pmgr.create_virtual_pointer_with_output(Some(&seat), output.as_ref(), &qh, ())
    } else {
        pmgr.create_virtual_pointer(Some(&seat), &qh, ())
    };

    let keyboard = kmgr.create_virtual_keyboard(&seat, &qh, ());

    // The virtual keyboard requires a keymap before any key events.
    let (keymap_file, keymap_size) = build_keymap_memfd()?;
    keyboard.keymap(
        wl_keyboard::KeymapFormat::XkbV1 as u32,
        keymap_file.as_fd(),
        keymap_size as u32,
    );

    let mut state = State {
        conn: conn.clone(),
        pointer,
        keyboard,
        _keymap_file: keymap_file,
        held_mods: HashSet::new(),
        start: Instant::now(),
        should_exit: false,
    };

    // Flush device creation + keymap to the compositor before reporting ready.
    event_queue
        .roundtrip(&mut state)
        .map_err(|e| InputError::StartFailed(format!("wayland roundtrip: {e}")))?;

    let event_loop =
        EventLoop::try_new().map_err(|e| InputError::StartFailed(format!("calloop init: {e}")))?;
    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .map_err(|e| InputError::StartFailed(format!("insert wayland source: {e}")))?;

    let (tx, channel) = calloop::channel::channel::<InputEvent>();
    event_loop
        .handle()
        .insert_source(channel, |event, _, state: &mut State| match event {
            ChannelEvent::Msg(input) => state.handle(input),
            ChannelEvent::Closed => state.should_exit = true,
        })
        .map_err(|e| InputError::StartFailed(format!("insert channel source: {e}")))?;

    Ok((state, event_loop, tx))
}

/// The xkb depressed-modifier bit for an evdev modifier keycode, if it is one.
/// (Shift=index0, Control=index2, Mod1/Alt=index3, Mod4/Super=index6.)
fn mod_bit(evdev: u32) -> Option<u32> {
    match evdev {
        42 | 54 => Some(1 << 0),   // Shift (left/right)
        29 | 97 => Some(1 << 2),   // Control (left/right)
        56 | 100 => Some(1 << 3),  // Alt (left/right)
        125 | 126 => Some(1 << 6), // Super/Meta (left/right)
        _ => None,
    }
}

/// Compile the default xkb keymap and write it to a memfd for the virtual
/// keyboard. Honors `XKB_DEFAULT_*` env vars (so the host's configured layout
/// is used when set); otherwise libxkbcommon defaults (US) apply.
fn build_keymap_memfd() -> Result<(File, usize), InputError> {
    use xkbcommon::xkb;

    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap =
        xkb::Keymap::new_from_names(&ctx, "", "", "", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)
            .ok_or_else(|| InputError::StartFailed("failed to compile xkb keymap".into()))?;
    let text = keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
    let size = text.len() + 1; // wl_keyboard keymap size includes the trailing NUL

    let fd = unsafe { libc::memfd_create(c"sidecar-keymap".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(InputError::StartFailed("memfd_create failed".into()));
    }
    // SAFETY: memfd_create returned a fresh owned fd.
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(text.as_bytes())
        .and_then(|_| file.write_all(&[0u8]))
        .map_err(|e| InputError::StartFailed(format!("write keymap: {e}")))?;

    Ok((file, size))
}

// Registry events are handled by registry_queue_init's GlobalList; we don't
// react to runtime global add/remove.
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

// We only send requests on these objects; ignore all their events.
delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ignore wl_output::WlOutput);
delegate_noop!(State: ignore ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ignore ZwlrVirtualPointerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardV1);

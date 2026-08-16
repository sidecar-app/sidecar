use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(not(target_os = "linux"))]
use tauri::webview::WebviewWindowBuilder;
use tauri::{AppHandle, Manager, WebviewUrl};

static DRAW_MODE: AtomicBool = AtomicBool::new(false);

// ── Linux sidecar types ──────────────────────────────────────────────

/// Handle to a running overlay sidecar process (Linux only).
/// Manages the child process and client connection to sidecar's Unix socket.
#[cfg(target_os = "linux")]
pub struct SidecarHandle {
    child: std::process::Child,
    writer: std::sync::Mutex<Option<std::io::BufWriter<std::os::unix::net::UnixStream>>>,
    _reader_thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(target_os = "linux")]
impl SidecarHandle {
    /// Send an annotation packet to the sidecar (newline-delimited JSON).
    pub fn send_annotation(&self, data: &str) {
        let msg = format!(
            r#"{{"cmd":"annotation","data":{}}}"#,
            serde_json::to_string(data).unwrap_or_default()
        );
        self.send_raw(&msg);
    }

    /// Send a draw/passthrough mode command to the sidecar.
    pub fn send_set_mode(&self, draw: bool) {
        let msg = format!(r#"{{"cmd":"set_mode","draw":{}}}"#, draw);
        self.send_raw(&msg);
    }

    /// Send a graceful shutdown command to the sidecar.
    pub fn send_shutdown(&self) {
        self.send_raw(r#"{"cmd":"shutdown"}"#);
    }

    pub(crate) fn send_raw(&self, msg: &str) {
        use std::io::Write;
        if let Ok(mut guard) = self.writer.lock()
            && let Some(ref mut writer) = *guard
        {
            let _ = writeln!(writer, "{}", msg);
            let _ = writer.flush();
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for SidecarHandle {
    fn drop(&mut self) {
        self.send_shutdown();
        // Poll for clean exit instead of fixed sleep (D-16)
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    }
}

#[cfg(target_os = "linux")]
static SIDECAR_HANDLE: std::sync::Mutex<Option<SidecarHandle>> = std::sync::Mutex::new(None);

// ── macOS focus helpers ─────────────────────────────────────────────
//
// All AppKit calls are dispatched to the main thread via `dispatch_async_f`
// (not `dispatch_sync_f`, which deadlocks when already on the main thread).
// Raw `objc_msgSend` is used with typed function pointers to avoid the
// ARM64 variadic-ABI mismatch (see commit e62dfcc).

#[cfg(target_os = "macos")]
mod macos_focus {
    use std::ffi::c_void;
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;
    use tauri::Manager;

    /// Newtype wrapper around a raw pointer to make it `Send`.
    struct SendPtr(*mut c_void);
    unsafe impl Send for SendPtr {}

    /// RAII guard for a retained ObjC object pointer (D-11).
    /// Automatically sends `release` on drop, preventing leaks on panic.
    struct ObjcRetained(*mut c_void);

    impl ObjcRetained {
        /// Take ownership of an already-retained pointer.
        unsafe fn from_retained(ptr: *mut c_void) -> Self {
            Self(ptr)
        }
    }

    impl Drop for ObjcRetained {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let send = void_send();
                    send(self.0, sel(b"release\0"));
                }
            }
        }
    }

    // ObjcRetained pointers can be sent between threads (same as SendPtr)
    unsafe impl Send for ObjcRetained {}

    /// Previously frontmost app (retained NSRunningApplication *).
    static PREV_APP: Mutex<Option<ObjcRetained>> = Mutex::new(None);
    /// Overlay NSWindow pointer while draw mode is active (NOT retained by our code).
    static DRAW_WINDOW: Mutex<Option<SendPtr>> = Mutex::new(None);
    /// Space-change notification observer (retained id).
    static SPACE_OBSERVER: Mutex<Option<ObjcRetained>> = Mutex::new(None);
    /// True if the user switched spaces while draw mode was active.
    /// When set, unfocus skips restoring the previous app to avoid
    /// an unwanted workspace switch.
    static SPACE_CHANGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    // ── ObjC FFI helpers ────────────────────────────────────────────

    type MsgVoid = extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;
    type MsgId = extern "C" fn(*mut c_void, *mut c_void, *mut c_void);
    type MsgBool = extern "C" fn(*mut c_void, *mut c_void, i8);
    type MsgULong = extern "C" fn(*mut c_void, *mut c_void, u64) -> i8;
    type MsgFourId = extern "C" fn(
        *mut c_void,
        *mut c_void,
        *mut c_void,
        *mut c_void,
        *mut c_void,
        *mut c_void,
    ) -> *mut c_void;

    unsafe extern "C" {
        static _dispatch_main_q: c_void;
        fn dispatch_async_f(
            queue: *const c_void,
            context: *mut c_void,
            work: extern "C" fn(*mut c_void),
        );
        fn dispatch_after_f(
            when: u64,
            queue: *const c_void,
            context: *mut c_void,
            work: extern "C" fn(*mut c_void),
        );
        fn dispatch_time(when: u64, delta: i64) -> u64;
        fn sel_registerName(name: *const std::ffi::c_char) -> *mut c_void;
        fn objc_msgSend();
        fn objc_getClass(name: *const std::ffi::c_char) -> *mut c_void;
        fn objc_allocateClassPair(
            superclass: *mut c_void,
            name: *const std::ffi::c_char,
            extra_bytes: usize,
        ) -> *mut c_void;
        fn objc_registerClassPair(cls: *mut c_void);
        fn class_addMethod(
            cls: *mut c_void,
            sel: *mut c_void,
            imp: *mut c_void,
            types: *const std::ffi::c_char,
        ) -> i8;
    }

    unsafe fn sel(name: &[u8]) -> *mut c_void {
        unsafe { sel_registerName(name.as_ptr().cast()) }
    }
    unsafe fn cls(name: &[u8]) -> *mut c_void {
        unsafe { objc_getClass(name.as_ptr().cast()) }
    }
    unsafe fn void_send() -> MsgVoid {
        unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) }
    }

    // ── Re-focus logic (shared by initial focus + space-change) ─────

    /// Make the overlay key window and activate the app process.
    /// Schedules a crosshair cursor set after a short delay so it fires
    /// AFTER the WebView has processed the React re-render (CSS cursor
    /// changes only take effect on the next mouse-move; the delayed
    /// NSCursor set bypasses that).
    /// MUST be called on the main thread.
    unsafe fn refocus_overlay(ns_window: *mut c_void) {
        unsafe {
            let send = void_send();
            let id_send: MsgId = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let bool_send: MsgBool = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());

            // makeKeyAndOrderFront:nil
            id_send(
                ns_window,
                sel(b"makeKeyAndOrderFront:\0"),
                std::ptr::null_mut(),
            );

            // [[NSApplication sharedApplication] activateIgnoringOtherApps:YES]
            let ns_app = send(cls(b"NSApplication\0"), sel(b"sharedApplication\0"));
            bool_send(ns_app, sel(b"activateIgnoringOtherApps:\0"), 1);

            // Set the crosshair cursor after 50ms, by which point React has re-rendered
            // with cursor:'crosshair' and the WebView has settled its tracking
            // areas after window activation.
            // DISPATCH_TIME_NOW = 0, NSEC_PER_MSEC = 1_000_000
            let when = dispatch_time(0, 50 * 1_000_000);
            dispatch_after_f(
                when,
                &_dispatch_main_q as *const c_void,
                std::ptr::null_mut(),
                set_crosshair_cursor,
            );
        }
    }

    extern "C" fn set_crosshair_cursor(_ctx: *mut c_void) {
        unsafe {
            let send = void_send();
            let crosshair = send(cls(b"NSCursor\0"), sel(b"crosshairCursor\0"));
            send(crosshair, sel(b"set\0"));
        }
    }

    extern "C" fn set_arrow_cursor(_ctx: *mut c_void) {
        unsafe {
            let send = void_send();
            let arrow = send(cls(b"NSCursor\0"), sel(b"arrowCursor\0"));
            send(arrow, sel(b"set\0"));
        }
    }

    // ── Space-change observer ───────────────────────────────────────

    /// ObjC method implementation for `spaceChanged:` selector.
    /// Called by NSNotificationCenter when the active space changes.
    extern "C" fn space_changed_imp(
        _self: *mut c_void,
        _cmd: *mut c_void,
        _notification: *mut c_void,
    ) {
        // Only re-focus if draw mode is still active
        if !super::DRAW_MODE.load(Ordering::SeqCst) {
            return;
        }
        SPACE_CHANGED.store(true, Ordering::SeqCst);
        let guard = DRAW_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(SendPtr(win)) = guard.as_ref() {
            unsafe {
                refocus_overlay(*win);
                // On space change React already has cursor:'crosshair',
                // so set it immediately (no 50ms delay needed).  The
                // delayed set from refocus_overlay acts as a safety net
                // in case the WebView re-evaluates tracking areas.
                set_crosshair_cursor(std::ptr::null_mut());
            };
        }
    }

    /// Register for NSWorkspaceActiveSpaceDidChangeNotification.
    /// Returns a retained observer object. MUST be called on main thread.
    unsafe fn register_space_observer() {
        unsafe {
            // Create a minimal ObjC class with a spaceChanged: method (once)
            static CLASS_REGISTERED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !CLASS_REGISTERED.swap(true, Ordering::SeqCst) {
                let superclass = cls(b"NSObject\0");
                let new_cls =
                    objc_allocateClassPair(superclass, c"SidecarSpaceObserver".as_ptr(), 0);
                if !new_cls.is_null() {
                    class_addMethod(
                        new_cls,
                        sel(b"spaceChanged:\0"),
                        space_changed_imp as *mut c_void,
                        c"v@:@".as_ptr(), // void, self, _cmd, NSNotification*
                    );
                    objc_registerClassPair(new_cls);
                }
            }

            let send = void_send();

            // Alloc + init observer instance
            let observer_cls = cls(b"SidecarSpaceObserver\0");
            let observer = send(observer_cls, sel(b"alloc\0"));
            let observer = send(observer, sel(b"init\0"));

            // Get workspace notification center:
            // [[NSWorkspace sharedWorkspace] notificationCenter]
            let workspace = send(cls(b"NSWorkspace\0"), sel(b"sharedWorkspace\0"));
            let nc = send(workspace, sel(b"notificationCenter\0"));

            // Create NSString for NSWorkspaceActiveSpaceDidChangeNotification
            type MsgCStr =
                extern "C" fn(*mut c_void, *mut c_void, *const std::ffi::c_char) -> *mut c_void;
            let cstr_send: MsgCStr = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let notif_name = send(cls(b"NSString\0"), sel(b"alloc\0"));
            let notif_name = cstr_send(
                notif_name,
                sel(b"initWithUTF8String:\0"),
                c"NSWorkspaceActiveSpaceDidChangeNotification".as_ptr(),
            );

            // [nc addObserver:observer selector:@selector(spaceChanged:)
            //     name:notifName object:nil]
            let add_obs: MsgFourId = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            add_obs(
                nc,
                sel(b"addObserver:selector:name:object:\0"),
                observer,
                sel(b"spaceChanged:\0"),
                notif_name,
                std::ptr::null_mut(), // object: nil
            );

            // Release the NSString (nc retains internally)
            send(notif_name, sel(b"release\0"));

            // Store observer for cleanup -- ObjcRetained handles release on drop
            let mut guard = SPACE_OBSERVER.lock().unwrap_or_else(|e| e.into_inner());
            *guard = Some(ObjcRetained::from_retained(observer));
        }
    }

    /// Unregister the space-change observer. MUST be called on main thread.
    unsafe fn unregister_space_observer() {
        unsafe {
            let send = void_send();
            let id_send: MsgId = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());

            let mut guard = SPACE_OBSERVER.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref retained) = *guard {
                // [[NSWorkspace sharedWorkspace] notificationCenter]
                let workspace = send(cls(b"NSWorkspace\0"), sel(b"sharedWorkspace\0"));
                let nc = send(workspace, sel(b"notificationCenter\0"));

                // [nc removeObserver:observer]
                id_send(nc, sel(b"removeObserver:\0"), retained.0);
            }
            // Drop via .take() triggers ObjcRetained::drop which sends release
            guard.take();
        }
    }

    // ── Public interface ────────────────────────────────────────────

    /// Focus the overlay, activate the app, save the previous frontmost app,
    /// and register for space-change notifications.
    pub fn focus(app: &tauri::AppHandle<tauri::Wry>) {
        if let Some(overlay_win) = app.get_webview_window("overlay")
            && let Ok(ns_window) = overlay_win.ns_window()
        {
            // Store ns_window for the space-change observer
            {
                let mut guard = DRAW_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
                *guard = Some(SendPtr(ns_window));
            }

            SPACE_CHANGED.store(false, Ordering::SeqCst);

            unsafe {
                extern "C" fn do_focus(ctx: *mut c_void) {
                    unsafe {
                        let send = void_send();

                        // Save frontmost app before we steal focus
                        let workspace = send(cls(b"NSWorkspace\0"), sel(b"sharedWorkspace\0"));
                        let front_app = send(workspace, sel(b"frontmostApplication\0"));
                        if !front_app.is_null() {
                            send(front_app, sel(b"retain\0"));
                            let mut guard = PREV_APP.lock().unwrap_or_else(|e| e.into_inner());
                            // Drop old value via .take() -- ObjcRetained::drop handles release
                            guard.take();
                            *guard = Some(ObjcRetained::from_retained(front_app));
                        }

                        refocus_overlay(ctx);
                        register_space_observer();
                    }
                }

                dispatch_async_f(&_dispatch_main_q as *const c_void, ns_window, do_focus);
            }
        }
    }

    /// Set NSWindowCollectionBehavior to stick to all spaces.
    /// Called once during overlay creation.
    pub fn set_collection_behavior(app: &tauri::AppHandle<tauri::Wry>) {
        if let Some(overlay_win) = app.get_webview_window("overlay")
            && let Ok(ns_window) = overlay_win.ns_window()
        {
            unsafe {
                extern "C" fn do_set_behavior(ctx: *mut c_void) {
                    unsafe {
                        type MsgSetBehavior = extern "C" fn(*mut c_void, *mut c_void, u64);
                        let msg_send: MsgSetBehavior =
                            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
                        // NSWindowCollectionBehaviorCanJoinAllSpaces (1 << 0)
                        // | NSWindowCollectionBehaviorFullScreenAuxiliary (1 << 8)
                        let behavior: u64 = (1 << 0) | (1 << 8);
                        msg_send(ctx, sel(b"setCollectionBehavior:\0"), behavior);
                    }
                }

                dispatch_async_f(
                    &_dispatch_main_q as *const c_void,
                    ns_window,
                    do_set_behavior,
                );
            }
        }
    }

    /// Unregister the space observer, restore the previously frontmost app.
    pub fn unfocus() {
        {
            let mut guard = DRAW_WINDOW.lock().unwrap_or_else(|e| e.into_inner());
            *guard = None;
        }

        unsafe {
            extern "C" fn do_unfocus(_ctx: *mut c_void) {
                unsafe {
                    unregister_space_observer();
                    set_arrow_cursor(std::ptr::null_mut());

                    let changed = SPACE_CHANGED.swap(false, Ordering::SeqCst);

                    let mut guard = PREV_APP.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(ref retained) = *guard
                        && !changed
                    {
                        // Only restore the previous app if the user
                        // stayed on the same space.  Restoring after a
                        // space switch would yank the user back to the
                        // original workspace.
                        let ulong_send: MsgULong =
                            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
                        ulong_send(retained.0, sel(b"activateWithOptions:\0"), 1 << 1);
                    }
                    // Drop via .take() triggers ObjcRetained::drop which sends release
                    guard.take();
                }
            }

            dispatch_async_f(
                &_dispatch_main_q as *const c_void,
                std::ptr::null_mut(),
                do_unfocus,
            );
        }
    }
}

// ── Public API ───────────────────────────────────────────────────────

/// Create the transparent overlay window positioned on the captured display.
/// Called from cmd_create_overlay when a session starts.
///
/// On Linux: spawns the overlay-sidecar process instead of creating a
/// Tauri window, communicating via Unix domain socket.
/// On other platforms: creates a standard Tauri transparent window.
pub fn create_overlay(app: &AppHandle, display_id: u32) -> Result<(), String> {
    if app.get_webview_window("overlay").is_some() {
        destroy_overlay(app)?;
    }

    // display_id is a monitor index; fall back to the primary monitor if out of range.
    let monitors = app.available_monitors().map_err(|e| e.to_string())?;

    let monitor = if (display_id as usize) < monitors.len() {
        &monitors[display_id as usize]
    } else {
        monitors.first().ok_or("No monitors found")?
    };

    let pos = monitor.position();
    let size = monitor.size();
    let scale = monitor.scale_factor();
    let logical_width = size.width as f64 / scale;
    let logical_height = size.height as f64 / scale;

    let url = WebviewUrl::App("index.html?window=overlay".into());

    #[cfg(not(target_os = "linux"))]
    {
        WebviewWindowBuilder::new(app, "overlay", url)
            .title("Sidecar Overlay")
            .position(pos.x as f64, pos.y as f64)
            .inner_size(logical_width, logical_height)
            .transparent(true)
            .decorations(false)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .build()
            .map_err(|e: tauri::Error| e.to_string())?;

        // Make overlay stick to all Mission Control Spaces and remain
        // visible during fullscreen apps on macOS.
        #[cfg(target_os = "macos")]
        macos_focus::set_collection_behavior(app);
    }

    #[cfg(target_os = "linux")]
    {
        let handle = create_linux_overlay(
            app,
            url,
            pos.x as f64,
            pos.y as f64,
            logical_width,
            logical_height,
            display_id,
        )?;
        *SIDECAR_HANDLE.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
    }

    // Start in passthrough mode
    DRAW_MODE.store(false, Ordering::SeqCst);
    if let Some(overlay) = app.get_webview_window("overlay") {
        let _ = overlay.set_ignore_cursor_events(true);
    }

    // Notify main window that overlay was created successfully
    use tauri::Emitter;
    if let Some(main_win) = app.get_webview_window("main") {
        let _ = main_win.emit("overlay-state", true);
    }

    Ok(())
}

/// Destroy the overlay window. Called when session stops or app exits.
pub fn destroy_overlay(app: &AppHandle) -> Result<(), String> {
    // On macOS, restore focus/cursor state if draw mode was active
    #[cfg(target_os = "macos")]
    if DRAW_MODE.load(Ordering::SeqCst) {
        macos_focus::unfocus();
    }

    // On Linux, clean up the sidecar process first
    #[cfg(target_os = "linux")]
    {
        let mut guard = SIDECAR_HANDLE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = guard.take() {
            drop(handle); // Drop triggers shutdown + kill + socket cleanup
        }
    }

    if let Some(overlay) = app.get_webview_window("overlay") {
        overlay.destroy().map_err(|e| e.to_string())?;
    }
    DRAW_MODE.store(false, Ordering::SeqCst);
    Ok(())
}

/// Toggle draw/passthrough mode on the overlay window.
/// In draw mode: overlay captures mouse input, cursor becomes crosshair.
/// In passthrough mode: overlay is click-through, mouse events pass to desktop.
pub fn toggle_draw_mode(app: &AppHandle) -> Result<bool, String> {
    let new_mode = !DRAW_MODE.load(Ordering::SeqCst);
    DRAW_MODE.store(new_mode, Ordering::SeqCst);

    #[cfg(target_os = "linux")]
    {
        if let Ok(guard) = SIDECAR_HANDLE.lock()
            && let Some(ref handle) = *guard
        {
            handle.send_set_mode(new_mode);
        }
    }

    if let Some(overlay) = app.get_webview_window("overlay") {
        overlay
            .set_ignore_cursor_events(!new_mode)
            .map_err(|e| e.to_string())?;

        // Focus the overlay when entering draw mode so the first click
        // registers as a draw stroke instead of a window-focus event.
        // Restore focus to the previous app when leaving draw mode.
        #[cfg(target_os = "macos")]
        if new_mode {
            macos_focus::focus(app);
        } else {
            macos_focus::unfocus();
        }

        use tauri::Emitter;
        overlay
            .emit("draw-mode-changed", new_mode)
            .map_err(|e| e.to_string())?;

        // Notify main window so HostPanel can update status
        if let Some(main_win) = app.get_webview_window("main") {
            main_win
                .emit("overlay-draw-mode", new_mode)
                .map_err(|e| e.to_string())?;
        }
    }

    // On Linux with sidecar (no overlay window), also notify main window
    #[cfg(target_os = "linux")]
    {
        if app.get_webview_window("overlay").is_none() {
            use tauri::Emitter;
            if let Some(main_win) = app.get_webview_window("main") {
                let _ = main_win.emit("overlay-draw-mode", new_mode);
            }
        }
    }

    Ok(new_mode)
}

/// Explicitly set draw mode (not toggle). Used by the React UI.
pub fn set_draw_mode(app: &AppHandle, draw_mode: bool) -> Result<(), String> {
    DRAW_MODE.store(draw_mode, Ordering::SeqCst);

    #[cfg(target_os = "linux")]
    {
        if let Ok(guard) = SIDECAR_HANDLE.lock()
            && let Some(ref handle) = *guard
        {
            handle.send_set_mode(draw_mode);
        }
    }

    if let Some(overlay) = app.get_webview_window("overlay") {
        overlay
            .set_ignore_cursor_events(!draw_mode)
            .map_err(|e| e.to_string())?;

        // Focus the overlay when entering draw mode so the first click
        // registers as a draw stroke instead of a window-focus event.
        // Restore focus to the previous app when leaving draw mode.
        #[cfg(target_os = "macos")]
        if draw_mode {
            macos_focus::focus(app);
        } else {
            macos_focus::unfocus();
        }

        use tauri::Emitter;
        overlay
            .emit("draw-mode-changed", draw_mode)
            .map_err(|e| e.to_string())?;

        if let Some(main_win) = app.get_webview_window("main") {
            main_win
                .emit("overlay-draw-mode", draw_mode)
                .map_err(|e| e.to_string())?;
        }
    }

    Ok(())
}

/// Forward an annotation to the sidecar (Linux only).
/// Called from lib.rs cmd_send_annotation and pipeline.rs viewer annotation
/// forwarding to push annotations to the native overlay for rendering.
#[cfg(target_os = "linux")]
pub fn forward_annotation_to_sidecar(data: &str) {
    if let Ok(guard) = SIDECAR_HANDLE.lock()
        && let Some(ref handle) = *guard
    {
        handle.send_annotation(data);
    }
}

/// Toggle the overlay's remote-control active indicator (Linux only).
/// Called from the pipeline bridge when the master-switch state changes.
#[cfg(target_os = "linux")]
pub fn set_remote_control_active_overlay(active: bool) {
    if let Ok(guard) = SIDECAR_HANDLE.lock()
        && let Some(ref handle) = *guard
    {
        handle.send_raw(&format!(
            r#"{{"cmd":"remote_control_active","active":{}}}"#,
            active
        ));
    }
}

// ── Linux sidecar spawn ──────────────────────────────────────────────

/// Linux overlay creation using overlay-sidecar for wlr-layer-shell compositors.
/// Spawns the sidecar binary, then connects as a client to its well-known socket.
#[cfg(target_os = "linux")]
fn create_linux_overlay(
    app: &AppHandle,
    _url: WebviewUrl, // unused -- sidecar does its own rendering
    _x: f64,
    _y: f64,
    _width: f64,
    _height: f64,
    display_id: u32,
) -> Result<SidecarHandle, String> {
    use std::io::{BufRead, BufReader, BufWriter};
    use std::os::unix::net::UnixStream;

    let socket_path = ::ipc::socket_path();

    let exe =
        std::env::current_exe().map_err(|e| format!("Failed to get current executable: {}", e))?;

    let child = std::process::Command::new(&exe)
        .arg("--overlay")
        .arg("--display")
        .arg(display_id.to_string())
        .env(
            "WAYLAND_DISPLAY",
            std::env::var("WAYLAND_DISPLAY").unwrap_or_default(),
        )
        .env(
            "XDG_RUNTIME_DIR",
            std::env::var("XDG_RUNTIME_DIR").unwrap_or_default(),
        )
        .spawn()
        .map_err(|e| format!("Failed to spawn overlay subprocess: {}", e))?;

    // Retry: the sidecar needs time to bind the socket after spawning.
    let stream = {
        let max_attempts = 50; // 50 * 100ms = 5 seconds
        let mut attempt = 0;
        loop {
            match UnixStream::connect(&socket_path) {
                Ok(s) => break s,
                Err(e) => {
                    attempt += 1;
                    if attempt >= max_attempts {
                        return Err(format!(
                            "Failed to connect to sidecar socket {} after {} attempts: {}",
                            socket_path, max_attempts, e
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
    };

    let reader_stream = stream
        .try_clone()
        .map_err(|e| format!("Failed to clone stream: {}", e))?;
    let writer = BufWriter::new(stream);

    // Reader thread: decode SidecarToHost messages and bridge them to Tauri.
    let app_clone = app.clone();
    let reader_thread = std::thread::spawn(move || {
        let reader = BufReader::new(reader_stream);
        for line in reader.lines() {
            match line {
                Ok(line) if line.is_empty() => continue,
                Ok(line) => {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                        match value.get("evt").and_then(|e| e.as_str()) {
                            Some("ready") => {
                                tracing::info!("Sidecar overlay ready");
                            }
                            Some("mode_changed") => {
                                if let Some(draw) = value.get("draw").and_then(|d| d.as_bool()) {
                                    DRAW_MODE.store(draw, Ordering::SeqCst);
                                    use tauri::Emitter;
                                    if let Some(main_win) = app_clone.get_webview_window("main") {
                                        let _ = main_win.emit("overlay-draw-mode", draw);
                                    }
                                }
                            }
                            Some("annotation") => {
                                if let Some(data) = value.get("data").and_then(|d| d.as_str()) {
                                    // Log cl packets for debugging relay issues
                                    if data.contains("\"t\":\"cl\"") {
                                        tracing::info!(
                                            "Sidecar annotation relay: cl packet received, forwarding to annotation_tx: {}",
                                            data
                                        );
                                    }
                                    let state = app_clone.state::<crate::AppState>();
                                    let tx = state
                                        .annotation_tx
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .clone();
                                    if let Some(tx) = tx {
                                        if tx.send(data.to_string()).is_err() {
                                            tracing::warn!(
                                                "Failed to forward sidecar annotation: annotation_tx receiver dropped"
                                            );
                                        }
                                    } else {
                                        tracing::warn!(
                                            "Sidecar annotation received but annotation_tx is None (no active session)"
                                        );
                                    }
                                }
                            }
                            Some("error") => {
                                let msg = value
                                    .get("msg")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("unknown");
                                tracing::error!("Sidecar error: {}", msg);
                                use tauri::Emitter;
                                if let Some(main_win) = app_clone.get_webview_window("main") {
                                    let _ = main_win.emit("overlay-state", false);
                                }
                            }
                            Some("state_changed") => {
                                if let Some(draw) = value.get("draw_mode").and_then(|d| d.as_bool())
                                {
                                    DRAW_MODE.store(draw, Ordering::SeqCst);
                                }
                                tracing::debug!(
                                    "Sidecar state: color={}, brush={}, draw={}",
                                    value.get("color").and_then(|c| c.as_str()).unwrap_or("?"),
                                    value
                                        .get("brush_size")
                                        .and_then(|b| b.as_f64())
                                        .unwrap_or(0.0),
                                    value
                                        .get("draw_mode")
                                        .and_then(|d| d.as_bool())
                                        .unwrap_or(false),
                                );
                            }
                            Some("set_resolution_request") => {
                                if let Some(height) = value.get("height").and_then(|h| h.as_u64()) {
                                    tracing::info!(
                                        "Sidecar resolution override request: height={}",
                                        height
                                    );
                                    let state = app_clone.state::<crate::AppState>();
                                    let tx = state
                                        .resolution_override_tx
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .clone();
                                    if let Some(tx) = tx {
                                        if tx.send(height as u32).is_err() {
                                            tracing::warn!(
                                                "Failed to forward resolution override: receiver dropped"
                                            );
                                        }
                                    } else {
                                        tracing::warn!(
                                            "Resolution override received but no active session"
                                        );
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Err(_) => {
                    tracing::info!("Sidecar socket disconnected");
                    break;
                }
            }
        }
    });

    Ok(SidecarHandle {
        child,
        writer: std::sync::Mutex::new(Some(writer)),
        _reader_thread: Some(reader_thread),
    })
}

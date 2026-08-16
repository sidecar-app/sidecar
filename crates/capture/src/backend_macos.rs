//! Native macOS capture backend built on ScreenCaptureKit via raw ObjC FFI.
//!
//! Uses ScreenCaptureKit (macOS 12.3+) for high-performance screen capture
//! with BGRA pixel output.

use crate::{CaptureError, CaptureOptions, Display, Frame};
use std::ffi::{c_char, c_void};
use std::sync::Once;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// ObjC runtime FFI (same pattern as overlay.rs)
// ---------------------------------------------------------------------------

type MsgVoid = extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;
type MsgId = extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void;
type MsgSetBool = extern "C" fn(*mut c_void, *mut c_void, i8);
type MsgSetU64 = extern "C" fn(*mut c_void, *mut c_void, u64);
type MsgGetUsize = extern "C" fn(*mut c_void, *mut c_void) -> usize;
type MsgGetI32 = extern "C" fn(*mut c_void, *mut c_void) -> i32;

unsafe extern "C" {
    fn objc_msgSend();
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_allocateClassPair(
        superclass: *mut c_void,
        name: *const c_char,
        extra_bytes: usize,
    ) -> *mut c_void;
    fn objc_registerClassPair(cls: *mut c_void);
    fn class_addMethod(
        cls: *mut c_void,
        sel: *mut c_void,
        imp: *mut c_void,
        types: *const c_char,
    ) -> i8;
    fn objc_setAssociatedObject(
        object: *mut c_void,
        key: *const c_void,
        value: *mut c_void,
        policy: usize,
    );
    fn objc_getAssociatedObject(object: *mut c_void, key: *const c_void) -> *mut c_void;
}

// CoreMedia / CoreVideo functions
#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMSampleBufferGetImageBuffer(buf: *mut c_void) -> *mut c_void;
    fn CMSampleBufferGetPresentationTimeStamp(buf: *mut c_void) -> CMTime;
    fn CVPixelBufferLockBaseAddress(pb: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pb: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(pb: *mut c_void) -> *mut u8;
    fn CVPixelBufferGetWidth(pb: *mut c_void) -> usize;
    fn CVPixelBufferGetHeight(pb: *mut c_void) -> usize;
    fn CVPixelBufferGetBytesPerRow(pb: *mut c_void) -> usize;
}

// Block runtime
unsafe extern "C" {
    static _NSConcreteStackBlock: c_void;
    fn _Block_copy(block: *const c_void) -> *mut c_void;
    fn _Block_release(block: *const c_void);
}

// CoreGraphics bindings for reading physical pixel dimensions from the display mode.
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGDisplayCopyDisplayMode(display: u32) -> *mut c_void;
    fn CGDisplayModeGetPixelWidth(mode: *mut c_void) -> usize;
    fn CGDisplayModeGetPixelHeight(mode: *mut c_void) -> usize;
    fn CGDisplayModeRelease(mode: *mut c_void);
}

/// Get physical backing-store pixel dimensions for a display.
/// Uses CGDisplayMode which correctly reports Retina pixel counts.
fn display_physical_size(display_id: u32) -> (u32, u32) {
    unsafe {
        let mode = CGDisplayCopyDisplayMode(display_id);
        if mode.is_null() {
            return (0, 0);
        }
        let w = CGDisplayModeGetPixelWidth(mode) as u32;
        let h = CGDisplayModeGetPixelHeight(mode) as u32;
        CGDisplayModeRelease(mode);
        (w, h)
    }
}

// GCD
unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *mut c_void) -> *mut c_void;
    fn dispatch_release(object: *mut c_void);
}

#[repr(C)]
#[derive(Copy, Clone)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

impl CMTime {
    fn to_nanos(self) -> u64 {
        if self.timescale <= 0 || self.flags & 1 == 0 {
            return 0;
        }
        ((self.value as i128 * 1_000_000_000) / self.timescale as i128) as u64
    }
}

// Constants
const KCVPIXELFORMATTYPE_32BGRA: u32 = 0x42475241; // 'BGRA'
const KSCSTREAMOUTPUTTYPE_SCREEN: i64 = 0;

// OBJC_ASSOCIATION_ASSIGN stores a raw pointer that the ObjC runtime won't retain or release.
const OBJC_ASSOCIATION_ASSIGN: usize = 0;

unsafe fn sel(name: &[u8]) -> *mut c_void {
    unsafe { sel_registerName(name.as_ptr().cast()) }
}
unsafe fn cls(name: &[u8]) -> *mut c_void {
    unsafe { objc_getClass(name.as_ptr().cast()) }
}
unsafe fn void_send() -> MsgVoid {
    unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) }
}

// ---------------------------------------------------------------------------
// Display enumeration
// ---------------------------------------------------------------------------

pub fn list_displays() -> Result<Vec<Display>, CaptureError> {
    // SCShareableContent.getShareableContentWithCompletionHandler: is async.
    // Block this thread on a condvar to wait for the result.
    let (tx, rx) = std_mpsc::sync_channel::<Result<Vec<Display>, CaptureError>>(1);

    unsafe {
        let tx_box = Box::new(tx);
        let ctx = Box::into_raw(tx_box) as *mut c_void;

        // SCShareableContent invokes the block as: ^(SCShareableContent *content, NSError *error)
        let block = create_shareable_content_block(ctx, shareable_content_handler);

        // [SCShareableContent getShareableContentExcludingDesktopWindows:NO
        //                     onScreenWindowsOnly:YES
        //                     completionHandler:block]
        type MsgGetContent = extern "C" fn(*mut c_void, *mut c_void, i8, i8, *mut c_void);
        let get_content: MsgGetContent =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        get_content(
            cls(b"SCShareableContent\0"),
            sel(b"getShareableContentExcludingDesktopWindows:onScreenWindowsOnly:completionHandler:\0"),
            0, // excludeDesktopWindows = NO
            1, // onScreenWindowsOnly = YES
            block,
        );

        _Block_release(block);
    }

    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(result) => result,
        Err(_) => Err(CaptureError::StartFailed(
            "Timed out waiting for display list".into(),
        )),
    }
}

/// ObjC block descriptor and layout for the completion handler.
/// Uses the C-ABI block layout: isa, flags, reserved, invoke, descriptor.
#[repr(C)]
struct Block<F> {
    isa: *mut c_void,
    flags: i32,
    reserved: i32,
    invoke: extern "C" fn(*mut Block<F>, *mut c_void, *mut c_void),
    descriptor: *const BlockDescriptor,
    context: *mut c_void,
}

#[repr(C)]
struct BlockDescriptor {
    reserved: u64,
    size: u64,
}

static BLOCK_DESCRIPTOR: BlockDescriptor = BlockDescriptor {
    reserved: 0,
    size: std::mem::size_of::<Block<()>>() as u64,
};

unsafe fn create_shareable_content_block(
    ctx: *mut c_void,
    invoke: extern "C" fn(*mut Block<()>, *mut c_void, *mut c_void),
) -> *mut c_void {
    let block = Box::new(Block {
        isa: &raw const _NSConcreteStackBlock as *mut c_void,
        flags: 0,
        reserved: 0,
        invoke,
        descriptor: &BLOCK_DESCRIPTOR,
        context: ctx,
    });
    // Copy the block to the heap so it survives after this function returns.
    unsafe { _Block_copy(Box::into_raw(block) as *const c_void) }
}

/// Extract the localizedDescription from an NSError object.
unsafe fn nserror_description(error: *mut c_void) -> String {
    if error.is_null() {
        return String::new();
    }
    unsafe {
        let send = void_send();
        let desc = send(error, sel(b"localizedDescription\0"));
        if desc.is_null() {
            return "unknown error".into();
        }
        let utf8: extern "C" fn(*mut c_void, *mut c_void) -> *const c_char =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let cstr = utf8(desc, sel(b"UTF8String\0"));
        if cstr.is_null() {
            return "unknown error".into();
        }
        std::ffi::CStr::from_ptr(cstr)
            .to_string_lossy()
            .into_owned()
    }
}

/// Completion handler for getShareableContent.
extern "C" fn shareable_content_handler(
    block: *mut Block<()>,
    content: *mut c_void,
    error: *mut c_void,
) {
    unsafe {
        let tx = Box::from_raw(
            (*block).context as *mut std_mpsc::SyncSender<Result<Vec<Display>, CaptureError>>,
        );

        if !error.is_null() || content.is_null() {
            let desc = nserror_description(error);
            tracing::error!("SCShareableContent error: {desc}");
            let _ = tx.send(Err(CaptureError::PermissionDenied));
            return;
        }

        let send = void_send();
        let get_u32: MsgGetI32 = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let get_usize: MsgGetUsize = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());

        // NSArray *displays = [content displays]
        let displays_arr = send(content, sel(b"displays\0"));
        if displays_arr.is_null() {
            let _ = tx.send(Err(CaptureError::NoDisplays));
            return;
        }

        let count = get_usize(displays_arr, sel(b"count\0"));
        if count == 0 {
            let _ = tx.send(Err(CaptureError::NoDisplays));
            return;
        }

        let mut displays = Vec::with_capacity(count);
        let get_at: MsgId = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());

        for i in 0..count {
            let sc_display = get_at(displays_arr, sel(b"objectAtIndex:\0"), i as *mut c_void);
            if sc_display.is_null() {
                continue;
            }

            let display_id = get_u32(sc_display, sel(b"displayID\0")) as u32;

            // Physical pixel dimensions via display mode (Retina-aware)
            let (width, height) = display_physical_size(display_id);

            // Logical dimensions from SCDisplay (points)
            let logical_w = get_u32(sc_display, sel(b"width\0")) as u32;

            let scale_factor = if logical_w > 0 {
                width as f64 / logical_w as f64
            } else {
                1.0
            };

            displays.push(Display {
                id: display_id,
                name: format!("Display {}", display_id),
                width,
                height,
                scale_factor,
                is_primary: i == 0,
            });
        }

        if displays.is_empty() {
            let _ = tx.send(Err(CaptureError::NoDisplays));
        } else {
            let _ = tx.send(Ok(displays));
        }
    }
}

// ---------------------------------------------------------------------------
// Stream output delegate
// ---------------------------------------------------------------------------

// Key for associated object storing the frame sender.
static ASSOC_KEY: u8 = 0;

/// Wrapper to store the frame sender as an ObjC associated object.
struct SenderWrapper {
    tx: std_mpsc::SyncSender<Frame>,
    display_id: u32,
    scale_factor: f64,
}

static DELEGATE_CLASS_INIT: Once = Once::new();
static mut DELEGATE_CLASS: *mut c_void = std::ptr::null_mut();

unsafe fn get_delegate_class() -> *mut c_void {
    DELEGATE_CLASS_INIT.call_once(|| unsafe {
        let superclass = cls(b"NSObject\0");
        let new_class = objc_allocateClassPair(superclass, c"SidecarSCKDelegate".as_ptr(), 0);
        assert!(!new_class.is_null(), "Failed to create SCK delegate class");

        // ObjC type encoding v@:@@q = (void, self, _cmd, SCStream*, CMSampleBuffer*, SCStreamOutputType)
        class_addMethod(
            new_class,
            sel(b"stream:didOutputSampleBuffer:ofType:\0"),
            stream_output_callback as *mut c_void,
            c"v@:@@q".as_ptr(),
        );

        objc_registerClassPair(new_class);
        DELEGATE_CLASS = new_class;
    });
    unsafe { DELEGATE_CLASS }
}

/// SCStreamOutput delegate callback: stream:didOutputSampleBuffer:ofType:
extern "C" fn stream_output_callback(
    this: *mut c_void,
    _cmd: *mut c_void,
    _stream: *mut c_void,
    sample_buffer: *mut c_void,
    output_type: i64,
) {
    if output_type != KSCSTREAMOUTPUTTYPE_SCREEN {
        return;
    }

    unsafe {
        let wrapper_ptr = objc_getAssociatedObject(this, &ASSOC_KEY as *const u8 as *const c_void);
        if wrapper_ptr.is_null() {
            return;
        }
        let wrapper = &*(wrapper_ptr as *const SenderWrapper);

        let pixel_buffer = CMSampleBufferGetImageBuffer(sample_buffer);
        if pixel_buffer.is_null() {
            return;
        }

        let pts = CMSampleBufferGetPresentationTimeStamp(sample_buffer);
        let timestamp_ns = pts.to_nanos();

        let lock_result = CVPixelBufferLockBaseAddress(pixel_buffer, 1); // kCVPixelBufferLock_ReadOnly
        if lock_result != 0 {
            return;
        }

        let base = CVPixelBufferGetBaseAddress(pixel_buffer);
        let width = CVPixelBufferGetWidth(pixel_buffer) as u32;
        let height = CVPixelBufferGetHeight(pixel_buffer) as u32;
        let stride = CVPixelBufferGetBytesPerRow(pixel_buffer) as u32;

        if base.is_null() || width == 0 || height == 0 {
            CVPixelBufferUnlockBaseAddress(pixel_buffer, 1);
            return;
        }

        let byte_count = (stride * height) as usize;
        let raw = std::slice::from_raw_parts(base, byte_count).to_vec();

        CVPixelBufferUnlockBaseAddress(pixel_buffer, 1);

        let (data, stride) = crate::strip_row_padding(raw, width, height, stride);

        let frame = Frame::new(
            data,
            width,
            height,
            stride,
            timestamp_ns,
            wrapper.display_id,
            wrapper.scale_factor,
        );

        // Non-blocking send so we drop the frame when the encoder can't keep up.
        let _ = wrapper.tx.try_send(frame);
    }
}

// ---------------------------------------------------------------------------
// Stream lifecycle
// ---------------------------------------------------------------------------

pub async fn spawn_capture_thread(
    opts: CaptureOptions,
) -> Result<
    (
        std_mpsc::Receiver<Frame>,
        std_mpsc::SyncSender<()>,
        Option<String>,
    ),
    CaptureError,
> {
    let (frame_tx, frame_rx) = std_mpsc::sync_channel::<Frame>(2);
    let (stop_tx, stop_rx) = std_mpsc::sync_channel::<()>(1);
    let (init_tx, init_rx) = std_mpsc::sync_channel::<Result<Option<String>, String>>(1);

    let fps = opts.fps;
    let target_display_id = opts.display_id;

    std::thread::Builder::new()
        .name("capture-sck".into())
        .spawn(move || {
            run_sck_capture(frame_tx, stop_rx, init_tx, fps, target_display_id);
        })
        .map_err(|e| CaptureError::StartFailed(format!("thread spawn failed: {e}")))?;

    match init_rx.recv() {
        Ok(Ok(warning)) => Ok((frame_rx, stop_tx, warning)),
        Ok(Err(msg)) => Err(CaptureError::StartFailed(msg)),
        Err(_) => Err(CaptureError::StartFailed(
            "capture thread died during init".into(),
        )),
    }
}

fn run_sck_capture(
    frame_tx: std_mpsc::SyncSender<Frame>,
    stop_rx: std_mpsc::Receiver<()>,
    init_tx: std_mpsc::SyncSender<Result<Option<String>, String>>,
    fps: u32,
    target_display_id: Option<u32>,
) {
    // Get shareable content, blocking on the channel for the async callback.
    let (content_tx, content_rx) =
        std_mpsc::sync_channel::<Result<(*mut c_void, u32, f64), String>>(1);

    unsafe {
        let ctx = Box::into_raw(Box::new((content_tx, target_display_id))) as *mut c_void;
        let block = create_shareable_content_block(ctx, content_completion_handler);

        type MsgGetContent = extern "C" fn(*mut c_void, *mut c_void, i8, i8, *mut c_void);
        let get_content: MsgGetContent =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        get_content(
            cls(b"SCShareableContent\0"),
            sel(b"getShareableContentExcludingDesktopWindows:onScreenWindowsOnly:completionHandler:\0"),
            0,
            1,
            block,
        );

        _Block_release(block);
    }

    let (sc_display, display_id, scale_factor) =
        match content_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                let _ = init_tx.send(Err(e));
                return;
            }
            Err(_) => {
                let _ = init_tx.send(Err("Timed out getting shareable content".into()));
                return;
            }
        };

    unsafe {
        let send = void_send();

        let config = send(cls(b"SCStreamConfiguration\0"), sel(b"alloc\0"));
        let config = send(config, sel(b"init\0"));

        // Physical pixel dimensions via display mode (Retina-aware).
        let (phys_w, phys_h) = display_physical_size(display_id);
        let width = phys_w as u64;
        let height = phys_h as u64;

        let set_u64: MsgSetU64 = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let set_bool: MsgSetBool = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());

        set_u64(config, sel(b"setWidth:\0"), width);
        set_u64(config, sel(b"setHeight:\0"), height);
        set_u64(
            config,
            sel(b"setPixelFormat:\0"),
            KCVPIXELFORMATTYPE_32BGRA as u64,
        );
        set_bool(config, sel(b"setShowsCursor:\0"), 1);

        // minimumFrameInterval = CMTimeMake(1, fps)
        // We set it via the setter which takes a CMTime struct.
        // On ARM64, small structs are passed in registers.
        type MsgSetCMTime = extern "C" fn(*mut c_void, *mut c_void, CMTime);
        let set_cmtime: MsgSetCMTime = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        set_cmtime(
            config,
            sel(b"setMinimumFrameInterval:\0"),
            CMTime {
                value: 1,
                timescale: fps as i32,
                flags: 1, // kCMTimeFlags_Valid
                epoch: 0,
            },
        );

        let empty_array = send(cls(b"NSArray\0"), sel(b"array\0"));
        type MsgInitFilter =
            extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> *mut c_void;
        let filter = send(cls(b"SCContentFilter\0"), sel(b"alloc\0"));
        let init_filter: MsgInitFilter =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let filter = init_filter(
            filter,
            sel(b"initWithDisplay:excludingWindows:\0"),
            sc_display,
            empty_array,
        );

        if filter.is_null() {
            let _ = init_tx.send(Err("Failed to create SCContentFilter".into()));
            send(config, sel(b"release\0"));
            return;
        }

        // [[SCStream alloc] initWithFilter:configuration:delegate:]
        let stream = send(cls(b"SCStream\0"), sel(b"alloc\0"));
        type MsgInitStream = extern "C" fn(
            *mut c_void,
            *mut c_void,
            *mut c_void,
            *mut c_void,
            *mut c_void,
        ) -> *mut c_void;
        let init_stream: MsgInitStream =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let stream = init_stream(
            stream,
            sel(b"initWithFilter:configuration:delegate:\0"),
            filter,
            config,
            std::ptr::null_mut(), // delegate (we use output handler instead)
        );

        if stream.is_null() {
            let _ = init_tx.send(Err("Failed to create SCStream".into()));
            send(filter, sel(b"release\0"));
            send(config, sel(b"release\0"));
            return;
        }

        let delegate_class = get_delegate_class();
        let delegate = send(delegate_class, sel(b"alloc\0"));
        let delegate = send(delegate, sel(b"init\0"));

        let wrapper = Box::new(SenderWrapper {
            tx: frame_tx,
            display_id,
            scale_factor,
        });
        let wrapper_ptr = Box::into_raw(wrapper) as *mut c_void;
        objc_setAssociatedObject(
            delegate,
            &ASSOC_KEY as *const u8 as *const c_void,
            wrapper_ptr,
            OBJC_ASSOCIATION_ASSIGN,
        );

        let queue = dispatch_queue_create(
            c"com.sidecar.capture".as_ptr(),
            std::ptr::null_mut(), // serial queue
        );

        // [stream addStreamOutput:delegate type:SCStreamOutputTypeScreen
        //         sampleHandlerQueue:queue error:&error]
        type MsgAddOutput = extern "C" fn(
            *mut c_void,
            *mut c_void,
            *mut c_void,
            i64,
            *mut c_void,
            *mut *mut c_void,
        ) -> i8;
        let add_output: MsgAddOutput = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let mut error: *mut c_void = std::ptr::null_mut();
        let ok = add_output(
            stream,
            sel(b"addStreamOutput:type:sampleHandlerQueue:error:\0"),
            delegate,
            KSCSTREAMOUTPUTTYPE_SCREEN,
            queue,
            &mut error,
        );

        if ok == 0 && !error.is_null() {
            let _ = init_tx.send(Err("Failed to add stream output".into()));
            send(delegate, sel(b"release\0"));
            send(stream, sel(b"release\0"));
            send(filter, sel(b"release\0"));
            send(config, sel(b"release\0"));
            dispatch_release(queue);
            return;
        }

        // Start capture asynchronously, blocking on the channel for the callback.
        let (start_tx, start_rx) = std_mpsc::sync_channel::<Result<(), String>>(1);
        let start_ctx = Box::into_raw(Box::new(start_tx)) as *mut c_void;
        let start_block = create_shareable_content_block(start_ctx, start_completion_handler);

        let start_msg: MsgId = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        start_msg(
            stream,
            sel(b"startCaptureWithCompletionHandler:\0"),
            start_block,
        );
        _Block_release(start_block);

        match start_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = init_tx.send(Err(format!("Failed to start capture: {e}")));
                send(delegate, sel(b"release\0"));
                send(stream, sel(b"release\0"));
                send(filter, sel(b"release\0"));
                send(config, sel(b"release\0"));
                dispatch_release(queue);
                return;
            }
            Err(_) => {
                let _ = init_tx.send(Err("Timed out starting capture".into()));
                send(delegate, sel(b"release\0"));
                send(stream, sel(b"release\0"));
                send(filter, sel(b"release\0"));
                send(config, sel(b"release\0"));
                dispatch_release(queue);
                return;
            }
        }

        let _ = init_tx.send(Ok(None));

        let _ = stop_rx.recv();

        let (stop_done_tx, stop_done_rx) = std_mpsc::sync_channel::<()>(1);
        let stop_ctx = Box::into_raw(Box::new(stop_done_tx)) as *mut c_void;
        let stop_block = create_shareable_content_block(stop_ctx, stop_completion_handler);

        let stop_msg: MsgId = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        stop_msg(
            stream,
            sel(b"stopCaptureWithCompletionHandler:\0"),
            stop_block,
        );
        _Block_release(stop_block);

        // Wait briefly for clean stop
        let _ = stop_done_rx.recv_timeout(Duration::from_secs(2));

        // Cleanup: reclaim the SenderWrapper
        let wrapper_ptr =
            objc_getAssociatedObject(delegate, &ASSOC_KEY as *const u8 as *const c_void);
        if !wrapper_ptr.is_null() {
            objc_setAssociatedObject(
                delegate,
                &ASSOC_KEY as *const u8 as *const c_void,
                std::ptr::null_mut(),
                OBJC_ASSOCIATION_ASSIGN,
            );
            drop(Box::from_raw(wrapper_ptr as *mut SenderWrapper));
        }

        send(delegate, sel(b"release\0"));
        send(stream, sel(b"release\0"));
        send(filter, sel(b"release\0"));
        send(config, sel(b"release\0"));
        dispatch_release(queue);
    }
}

/// Completion handler for getShareableContent that picks a specific display.
extern "C" fn content_completion_handler(
    block: *mut Block<()>,
    content: *mut c_void,
    error: *mut c_void,
) {
    unsafe {
        let (tx, target_id) = *Box::from_raw(
            (*block).context
                as *mut (
                    std_mpsc::SyncSender<Result<(*mut c_void, u32, f64), String>>,
                    Option<u32>,
                ),
        );

        if !error.is_null() || content.is_null() {
            let desc = nserror_description(error);
            tracing::error!("SCShareableContent error (capture): {desc}");
            let _ = tx.send(Err(format!("Screen capture failed: {desc}")));
            return;
        }

        let send = void_send();
        let get_i32: MsgGetI32 = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());

        let displays_arr = send(content, sel(b"displays\0"));
        if displays_arr.is_null() {
            let _ = tx.send(Err("No displays returned".into()));
            return;
        }

        let get_usize: MsgGetUsize = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let count = get_usize(displays_arr, sel(b"count\0"));
        if count == 0 {
            let _ = tx.send(Err("No displays found".into()));
            return;
        }

        let get_at: MsgId = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());

        let mut chosen: *mut c_void = std::ptr::null_mut();
        let mut chosen_id: u32 = 0;
        let mut chosen_scale: f64 = 1.0;

        for i in 0..count {
            let sc_display = get_at(displays_arr, sel(b"objectAtIndex:\0"), i as *mut c_void);
            if sc_display.is_null() {
                continue;
            }
            let did = get_i32(sc_display, sel(b"displayID\0")) as u32;
            let logical_w = get_i32(sc_display, sel(b"width\0")) as u32;
            let (physical_w, _) = display_physical_size(did);

            let scale = if logical_w > 0 {
                physical_w as f64 / logical_w as f64
            } else {
                1.0
            };

            if let Some(target) = target_id {
                if did == target {
                    chosen = sc_display;
                    chosen_id = did;
                    chosen_scale = scale;
                    break;
                }
            } else if chosen.is_null() {
                chosen = sc_display;
                chosen_id = did;
                chosen_scale = scale;
            }
        }

        if chosen.is_null() {
            let _ = tx.send(Err("Target display not found".into()));
            return;
        }

        // Retain the display object so it outlives this callback
        let retain: MsgVoid = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        retain(chosen, sel(b"retain\0"));

        let _ = tx.send(Ok((chosen, chosen_id, chosen_scale)));
    }
}

/// Completion handler for startCaptureWithCompletionHandler:
/// The block is called with (NSError *error).
extern "C" fn start_completion_handler(
    block: *mut Block<()>,
    error: *mut c_void,
    _unused: *mut c_void,
) {
    unsafe {
        let tx = Box::from_raw((*block).context as *mut std_mpsc::SyncSender<Result<(), String>>);
        if !error.is_null() {
            let desc = nserror_description(error);
            tracing::error!("startCapture error: {desc}");
            let _ = tx.send(Err(format!("startCapture failed: {desc}")));
        } else {
            let _ = tx.send(Ok(()));
        }
    }
}

/// Completion handler for stopCaptureWithCompletionHandler:
extern "C" fn stop_completion_handler(
    block: *mut Block<()>,
    _error: *mut c_void,
    _unused: *mut c_void,
) {
    unsafe {
        let tx = Box::from_raw((*block).context as *mut std_mpsc::SyncSender<()>);
        let _ = tx.send(());
    }
}

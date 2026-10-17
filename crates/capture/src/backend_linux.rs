//! Native Linux capture backend built on PipeWire via the XDG Desktop Portal.
//!
//! Uses ashpd for portal negotiation and the pipewire crate for
//! PipeWire stream handling.

use crate::{CaptureError, CaptureOptions, Display, Frame};
use pipewire as pw;
use pw::{
    context::ContextBox,
    main_loop::MainLoopBox,
    properties::properties,
    spa::{
        self,
        param::{
            ParamType,
            format::{FormatProperties, MediaSubtype, MediaType},
            video::VideoFormat,
        },
        pod::{Pod, Property},
        sys::{
            SPA_META_Header, SPA_PARAM_META_size, SPA_PARAM_META_type, spa_buffer, spa_meta_header,
        },
        utils::{Direction, SpaTypes},
    },
    stream::{Stream as StreamRef, StreamBox, StreamState},
};
use std::fs;
use std::mem::size_of;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// D-Bus portal state (D-08: replaces Box::leak/mem::forget)
// ---------------------------------------------------------------------------

/// D-Bus state that must outlive the PipeWire stream.
/// Dropping this closes the portal session and terminates the stream.
struct PortalState {
    _proxy: ashpd::desktop::screencast::Screencast,
    _session: ashpd::desktop::Session<ashpd::desktop::screencast::Screencast>,
}

// PortalState is only kept alive (not accessed) from the PipeWire thread,
// so Send is safe. ashpd::desktop::Session wraps a zbus::Connection which
// is Send, but the compiler can't see through the type alias.
unsafe impl Send for PortalState {}

// ---------------------------------------------------------------------------
// Per-session capture state (D-10: replaces global statics)
// ---------------------------------------------------------------------------

/// Per-session capture state shared between PipeWire thread and stop-watcher.
struct CaptureState {
    /// 0 = init, 1 = running, 2 = stopping
    state: AtomicU8,
    /// True if PipeWire stream entered error state.
    error: AtomicBool,
}

impl CaptureState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU8::new(0),
            error: AtomicBool::new(false),
        })
    }
}

// ---------------------------------------------------------------------------
// Display enumeration
// ---------------------------------------------------------------------------

pub fn list_displays() -> Result<Vec<Display>, CaptureError> {
    Ok(vec![Display {
        id: 0,
        name: "Default Display (selected via portal)".to_string(),
        width: 0,
        height: 0,
        scale_factor: 1.0,
        is_primary: true,
    }])
}

// ---------------------------------------------------------------------------
// Restore token management
// ---------------------------------------------------------------------------

fn xdg_cache_dir() -> Option<PathBuf> {
    std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".cache"))
        })
}

fn token_path() -> Option<PathBuf> {
    Some(xdg_cache_dir()?.join("sidecar").join("restore_token"))
}

fn load_restore_token() -> Option<String> {
    let path = token_path()?;
    let token = fs::read_to_string(&path).ok()?;
    if token.is_empty() { None } else { Some(token) }
}

fn save_restore_token(token: &str) {
    if let Some(path) = token_path() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&path, token);
    }
}

pub fn clear_restore_token() {
    if let Some(path) = token_path() {
        let _ = fs::remove_file(&path);
    }
}

// ---------------------------------------------------------------------------
// Monitor resolution checking (reads /sys/class/drm)
// ---------------------------------------------------------------------------

fn connected_monitors() -> impl Iterator<Item = fs::DirEntry> {
    fs::read_dir("/sys/class/drm")
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with("card") && name.contains('-') && !name.contains("Writeback")
        })
        .filter(|e| {
            fs::read_to_string(e.path().join("status"))
                .map(|s| s.trim() == "connected")
                .unwrap_or(false)
        })
}

fn count_connected_monitors() -> usize {
    connected_monitors().count()
}

pub fn connected_monitor_resolutions() -> Vec<(i32, i32)> {
    connected_monitors()
        .filter_map(|e| {
            let modes = fs::read_to_string(e.path().join("modes")).ok()?;
            let first_line = modes.lines().next()?;
            let (w, h) = first_line.split_once('x')?;
            Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
        })
        .collect()
}

fn check_stream_vs_monitors(stream_size: (i32, i32)) -> Option<String> {
    let monitors = connected_monitor_resolutions();
    if monitors.is_empty() {
        return None;
    }
    let (sw, sh) = stream_size;
    if monitors.iter().any(|&(mw, mh)| mw == sw && mh == sh) {
        None
    } else {
        Some(format!(
            "Selected source ({}x{}) does not match any monitor. \
             Window/region capture is not supported. Drawing coordinates will not match.",
            sw, sh
        ))
    }
}

// ---------------------------------------------------------------------------
// Format conversion (pixel format → BGRA)
// ---------------------------------------------------------------------------

use crate::{compute_stride_4bpp, strip_row_padding};

fn convert_to_bgra_frame(
    format: VideoFormat,
    data: Vec<u8>,
    width: u32,
    height: u32,
    timestamp_ns: u64,
) -> Option<Frame> {
    match format {
        VideoFormat::BGRx => {
            // BGRx is B,G,R,padding, the same layout as BGRA for encoding.
            let actual_stride = compute_stride_4bpp(data.len(), width, height);
            let (data, stride) = strip_row_padding(data, width, height, actual_stride);
            Some(Frame::new(
                data,
                width,
                height,
                stride,
                timestamp_ns,
                0,
                1.0,
            ))
        }
        VideoFormat::RGBx | VideoFormat::RGBA => {
            let actual_stride = compute_stride_4bpp(data.len(), width, height);
            let (mut data, stride) = strip_row_padding(data, width, height, actual_stride);
            for chunk in data.as_chunks_mut::<4>().0 {
                chunk.swap(0, 2);
                chunk[3] = 255;
            }
            Some(Frame::new(
                data,
                width,
                height,
                stride,
                timestamp_ns,
                0,
                1.0,
            ))
        }
        VideoFormat::xBGR => {
            let actual_stride = compute_stride_4bpp(data.len(), width, height);
            let (mut data, stride) = strip_row_padding(data, width, height, actual_stride);
            for chunk in data.as_chunks_mut::<4>().0 {
                let b = chunk[1];
                let g = chunk[2];
                let r = chunk[3];
                chunk[0] = b;
                chunk[1] = g;
                chunk[2] = r;
                chunk[3] = 255;
            }
            Some(Frame::new(
                data,
                width,
                height,
                stride,
                timestamp_ns,
                0,
                1.0,
            ))
        }
        VideoFormat::RGB => {
            let min_rgb_stride = (width as usize) * 3;
            let actual_rgb_stride = if height > 0 && data.len() > min_rgb_stride * (height as usize)
            {
                data.len() / height as usize
            } else {
                min_rgb_stride
            };
            let w = width as usize;
            let h = height as usize;
            let mut bgra = Vec::with_capacity(w * h * 4);
            for row in 0..h {
                let row_start = row * actual_rgb_stride;
                for col in 0..w {
                    let px = row_start + col * 3;
                    if px + 2 < data.len() {
                        bgra.push(data[px + 2]); // B
                        bgra.push(data[px + 1]); // G
                        bgra.push(data[px]); // R
                        bgra.push(255); // A
                    }
                }
            }
            Some(Frame::new(
                bgra,
                width,
                height,
                width * 4,
                timestamp_ns,
                0,
                1.0,
            ))
        }
        _ => {
            tracing::warn!(format = ?format, "unsupported frame format");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Portal negotiation (ashpd)
// ---------------------------------------------------------------------------

use ashpd::desktop::PersistMode;
use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use ashpd::enumflags2::BitFlags;

async fn negotiate_portal()
-> Result<(u32, Option<(i32, i32)>, Option<String>, PortalState), CaptureError> {
    let proxy = Screencast::new()
        .await
        .map_err(|e| CaptureError::StartFailed(format!("portal connect: {e}")))?;

    let session = proxy
        .create_session(Default::default())
        .await
        .map_err(|e| CaptureError::StartFailed(format!("create_session: {e}")))?;

    let restore_token = load_restore_token();
    let single_monitor = count_connected_monitors() == 1;
    let token_to_use = if single_monitor {
        restore_token.as_deref()
    } else {
        None
    };

    let mut select_opts = ashpd::desktop::screencast::SelectSourcesOptions::default()
        .set_cursor_mode(CursorMode::Embedded)
        .set_sources(BitFlags::from(SourceType::Monitor))
        .set_multiple(false)
        .set_persist_mode(PersistMode::ExplicitlyRevoked);
    if let Some(token) = token_to_use {
        select_opts = select_opts.set_restore_token(token);
    }

    proxy
        .select_sources(&session, select_opts)
        .await
        .map_err(|e| CaptureError::StartFailed(format!("select_sources: {e}")))?
        .response()
        .map_err(|e| {
            clear_restore_token();
            CaptureError::StartFailed(format!("select_sources response: {e}"))
        })?;

    let start_result = proxy
        .start(&session, None, Default::default())
        .await
        .map_err(|e| CaptureError::StartFailed(format!("start: {e}")))?
        .response();

    let streams = match start_result {
        Ok(s) => s,
        Err(e) => {
            clear_restore_token();
            return Err(CaptureError::StartFailed(format!("start response: {e}")));
        }
    };

    if let Some(token) = streams.restore_token() {
        save_restore_token(token);
    }

    let stream = streams
        .streams()
        .first()
        .ok_or_else(|| CaptureError::StartFailed("No streams returned".into()))?;

    let pw_node_id = stream.pipe_wire_node_id();
    let stream_size = stream.size();

    let portal_state = PortalState {
        _proxy: proxy,
        _session: session,
    };

    let warning = stream_size.and_then(check_stream_vs_monitors);
    if warning.is_some() {
        clear_restore_token();
    }

    Ok((pw_node_id, stream_size, warning, portal_state))
}

// ---------------------------------------------------------------------------
// PipeWire stream
// ---------------------------------------------------------------------------

/// State shared with PipeWire stream callbacks.
#[derive(Clone)]
struct CaptureCallbackData {
    frame_tx: std_mpsc::SyncSender<Frame>,
    format: spa::param::video::VideoInfoRaw,
    capture_state: Arc<CaptureState>,
}

fn param_changed_callback(
    _stream: &StreamRef,
    user_data: &mut CaptureCallbackData,
    id: u32,
    param: Option<&Pod>,
) {
    let Some(param) = param else { return };
    if id != ParamType::Format.as_raw() {
        return;
    }
    let (media_type, media_subtype) = match spa::param::format_utils::parse_format(param) {
        Ok(v) => v,
        Err(_) => return,
    };
    if media_type != MediaType::Video || media_subtype != MediaSubtype::Raw {
        return;
    }
    user_data
        .format
        .parse(param)
        .expect("Failed to parse format parameter");
}

fn state_changed_callback(
    _stream: &StreamRef,
    user_data: &mut CaptureCallbackData,
    _old: StreamState,
    new: StreamState,
) {
    if let StreamState::Error(e) = new {
        tracing::error!(error = %e, "PipeWire stream error");
        user_data.capture_state.error.store(true, Ordering::Relaxed);
    }
}

unsafe fn get_timestamp(buffer: *mut spa_buffer) -> i64 {
    unsafe {
        let n_metas = (*buffer).n_metas;
        if n_metas > 0 {
            let mut meta_ptr = (*buffer).metas;
            let metas_end = (*buffer).metas.wrapping_add(n_metas as usize);
            while meta_ptr != metas_end {
                if (*meta_ptr).type_ == SPA_META_Header {
                    let meta_header: &mut spa_meta_header =
                        &mut *((*meta_ptr).data as *mut spa_meta_header);
                    return meta_header.pts;
                }
                meta_ptr = meta_ptr.wrapping_add(1);
            }
            0
        } else {
            0
        }
    }
}

fn process_callback(stream: &StreamRef, user_data: &mut CaptureCallbackData) {
    let buffer = unsafe { stream.dequeue_raw_buffer() };
    if buffer.is_null() {
        tracing::warn!("out of PipeWire buffers");
        unsafe { stream.queue_raw_buffer(buffer) };
        return;
    }

    'process: {
        let spa_buf = unsafe { (*buffer).buffer };
        if spa_buf.is_null() {
            break 'process;
        }
        let timestamp = unsafe { get_timestamp(spa_buf) };

        let n_datas = unsafe { (*spa_buf).n_datas };
        if n_datas < 1 {
            break 'process;
        }

        let frame_size = user_data.format.size();
        let video_format = user_data.format.format();
        let width = frame_size.width;
        let height = frame_size.height;

        // Extract raw pixel data from the PipeWire buffer.
        // Handles both MemFd (fd >= 0, needs mmap) and MemPtr (data pointer).
        let frame_data: Vec<u8> = unsafe {
            let datas = (*spa_buf).datas;
            if datas.is_null() {
                break 'process;
            }
            let chunk = (*datas).chunk;
            if chunk.is_null() {
                break 'process;
            }
            let size = (*chunk).size as usize;
            let offset = (*chunk).offset as usize;
            let maxsize = (*datas).maxsize as usize;
            if size == 0 || size > maxsize {
                break 'process;
            }

            let fd = (*datas).fd;
            let data_ptr = (*datas).data;
            let mapoffset = (*datas).mapoffset as usize;

            // PipeWire 1.6.x MAP_BUFFERS doesn't reliably map MemFd into
            // the process address space. When we have a valid fd, manually
            // mmap it to guarantee access. Fall back to data_ptr for MemPtr
            // buffers (type=1) where no fd is available.
            if fd >= 0 {
                let map_size = maxsize + mapoffset;
                if mapoffset + offset + size > map_size {
                    break 'process;
                }
                let mapped = libc::mmap(
                    std::ptr::null_mut(),
                    map_size,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    fd as i32,
                    0,
                );
                if mapped == libc::MAP_FAILED {
                    break 'process;
                }
                let base = (mapped as *const u8).add(mapoffset + offset);
                let data = std::slice::from_raw_parts(base, size).to_vec();
                libc::munmap(mapped, map_size);
                data
            } else if !data_ptr.is_null() {
                if offset + size > maxsize {
                    break 'process;
                }
                let base = (data_ptr as *const u8).add(offset);
                std::slice::from_raw_parts(base, size).to_vec()
            } else {
                break 'process;
            }
        };

        let timestamp_ns = if timestamp > 0 { timestamp as u64 } else { 0 };

        if let Some(frame) =
            convert_to_bgra_frame(video_format, frame_data, width, height, timestamp_ns)
        {
            // Non-blocking send: drop frames when encoder can't keep up.
            // This prevents PipeWire's internal buffers from growing.
            let _ = user_data.frame_tx.try_send(frame);
        }
    }

    unsafe { stream.queue_raw_buffer(buffer) };
}

fn run_pipewire_stream(
    pw_node_id: u32,
    fps: u32,
    frame_tx: std_mpsc::SyncSender<Frame>,
    init_tx: std_mpsc::SyncSender<Result<(), String>>,
    capture_state: Arc<CaptureState>,
) {
    let mainloop = match MainLoopBox::new(None) {
        Ok(ml) => ml,
        Err(e) => {
            let _ = init_tx.send(Err(format!("PipeWire MainLoop: {e}")));
            return;
        }
    };
    let context = match ContextBox::new(mainloop.loop_(), None) {
        Ok(c) => c,
        Err(e) => {
            let _ = init_tx.send(Err(format!("PipeWire Context: {e}")));
            return;
        }
    };
    let core = match context.connect(None) {
        Ok(c) => c,
        Err(e) => {
            let _ = init_tx.send(Err(format!("PipeWire connect: {e}")));
            return;
        }
    };

    let user_data = CaptureCallbackData {
        frame_tx,
        format: Default::default(),
        capture_state: capture_state.clone(),
    };

    let stream = match StreamBox::new(
        &core,
        "sidecar-capture",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    ) {
        Ok(s) => s,
        Err(e) => {
            let _ = init_tx.send(Err(format!("PipeWire Stream: {e}")));
            return;
        }
    };

    let _listener = stream
        .add_local_listener_with_user_data(user_data)
        .state_changed(state_changed_callback)
        .param_changed(param_changed_callback)
        .process(process_callback)
        .register()
        .expect("Failed to register PipeWire stream listener");

    let format_obj = pw::spa::pod::object!(
        SpaTypes::ObjectParamFormat,
        ParamType::EnumFormat,
        pw::spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        pw::spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        pw::spa::pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::RGB,
            VideoFormat::RGBA,
            VideoFormat::RGBx,
            VideoFormat::BGRx,
        ),
        pw::spa::pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            pw::spa::utils::Rectangle {
                width: 128,
                height: 128,
            },
            pw::spa::utils::Rectangle {
                width: 1,
                height: 1,
            },
            pw::spa::utils::Rectangle {
                width: 4096,
                height: 4096,
            }
        ),
        pw::spa::pod::property!(
            FormatProperties::VideoMaxFramerate,
            Fraction,
            pw::spa::utils::Fraction { num: fps, denom: 1 }
        ),
    );

    // Request SPA_META_Header for timestamps
    let metas_obj = pw::spa::pod::object!(
        SpaTypes::ObjectParamMeta,
        ParamType::Meta,
        Property::new(
            SPA_PARAM_META_type,
            pw::spa::pod::Value::Id(pw::spa::utils::Id(SPA_META_Header))
        ),
        Property::new(
            SPA_PARAM_META_size,
            pw::spa::pod::Value::Int(size_of::<spa_meta_header>() as i32)
        ),
    );

    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(format_obj),
    )
    .expect("Failed to serialize format pod")
    .0
    .into_inner();

    let metas_values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(metas_obj),
    )
    .expect("Failed to serialize meta pod")
    .0
    .into_inner();

    let mut params = [
        Pod::from_bytes(&values).unwrap(),
        Pod::from_bytes(&metas_values).unwrap(),
    ];

    if let Err(e) = stream.connect(
        Direction::Input,
        Some(pw_node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    ) {
        let _ = init_tx.send(Err(format!("PipeWire stream connect: {e}")));
        return;
    }

    let _ = init_tx.send(Ok(()));

    while capture_state.state.load(Ordering::Relaxed) == 0 {
        std::thread::sleep(Duration::from_millis(10));
    }

    let pw_loop = mainloop.loop_();
    while capture_state.state.load(Ordering::Relaxed) == 1
        && !capture_state.error.load(Ordering::Relaxed)
    {
        pw_loop.iterate(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------
// Public API
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
    // Portal negotiation is async and shows the OS source-picker dialog.
    let (pw_node_id, _stream_size, warning, portal_state) = negotiate_portal().await?;

    let (frame_tx, frame_rx) = std_mpsc::sync_channel::<Frame>(2);
    let (stop_tx, stop_rx) = std_mpsc::sync_channel::<()>(1);
    let (init_tx, init_rx) = std_mpsc::sync_channel::<Result<(), String>>(1);

    let fps = opts.fps;

    // Per-session state (D-10: no more global statics)
    let capture_state = CaptureState::new();

    let cs_pw = capture_state.clone();
    std::thread::Builder::new()
        .name("capture-pipewire".into())
        .spawn(move || {
            // Keep portal state alive for the lifetime of the PipeWire stream.
            // Dropping it closes the D-Bus session and terminates the stream.
            let _portal_keep = portal_state;

            run_pipewire_stream(pw_node_id, fps, frame_tx, init_tx, cs_pw.clone());

            // Reset state so a later session starts clean.
            cs_pw.state.store(0, Ordering::Relaxed);
            cs_pw.error.store(false, Ordering::Relaxed);
        })
        .map_err(|e| CaptureError::StartFailed(format!("thread spawn failed: {e}")))?;

    match init_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(msg)) => return Err(CaptureError::StartFailed(msg)),
        Err(_) => {
            return Err(CaptureError::StartFailed(
                "capture thread died during init".into(),
            ));
        }
    }

    capture_state.state.store(1, Ordering::Relaxed);

    let cs_stop = capture_state.clone();
    std::thread::Builder::new()
        .name("capture-stop-watcher".into())
        .spawn(move || {
            let _ = stop_rx.recv();
            cs_stop.state.store(2, Ordering::Relaxed);
        })
        .map_err(|e| CaptureError::StartFailed(format!("stop watcher spawn failed: {e}")))?;

    Ok((frame_rx, stop_tx, warning))
}

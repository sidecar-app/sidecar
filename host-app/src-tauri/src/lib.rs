mod events;
mod overlay;
mod pipeline;

use std::sync::Mutex;

struct AppState {
    pipeline: Mutex<Option<pipeline::PipelineHandle>>,
    annotation_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>,
    /// Channel for CLI set-resolution commands -> ABR controller.
    /// Receives height values: 0 = clear override, >0 = set manual override.
    resolution_override_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<u32>>>,
    /// Channel for CLI set-tier commands -> pipeline.
    /// Bypasses ABR entirely with explicit encoder params. All-zero = clear override.
    encoder_params_override_tx:
        Mutex<Option<tokio::sync::mpsc::UnboundedSender<encode::EncoderParams>>>,
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

#[tauri::command]
fn cmd_check_screen_recording() -> bool {
    #[cfg(target_os = "macos")]
    {
        unsafe { CGPreflightScreenCaptureAccess() }
    }
    #[cfg(not(target_os = "macos"))]
    true
}

#[tauri::command]
fn cmd_request_screen_recording() -> bool {
    #[cfg(target_os = "macos")]
    {
        unsafe { CGRequestScreenCaptureAccess() }
    }
    #[cfg(not(target_os = "macos"))]
    true
}

#[tauri::command]
async fn cmd_start_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let existing = {
        let mut guard = state.pipeline.lock().unwrap_or_else(|e| e.into_inner());
        guard.take()
    };
    if let Some(handle) = existing {
        handle.stop().await;
    }

    let handle = pipeline::start_pipeline(app.clone())
        .await
        .map_err(|e| e.to_string())?;
    let room_code = handle.room_code().to_string();

    let annotation_tx = handle.annotation_tx();
    *state.pipeline.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
    *state
        .annotation_tx
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(annotation_tx);

    Ok(room_code)
}

#[tauri::command]
async fn cmd_stop_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let handle = {
        let mut guard = state.pipeline.lock().unwrap_or_else(|e| e.into_inner());
        guard.take()
    };

    if let Some(handle) = handle {
        handle.stop().await;
        *state
            .annotation_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        let _ = overlay::destroy_overlay(&app);
        {
            use tauri::Manager;
            if let Some(stats_win) = app.get_webview_window("stats") {
                let _ = stats_win.destroy();
            }
        }
        events::emit_session_ended(&app);
        Ok("Session stopped".to_string())
    } else {
        Ok("No session was running".to_string())
    }
}

#[tauri::command]
async fn cmd_send_annotation(
    state: tauri::State<'_, AppState>,
    data: String,
) -> Result<(), String> {
    let tx = {
        let guard = state
            .annotation_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.clone()
    };
    if let Some(tx) = tx {
        #[cfg(target_os = "linux")]
        overlay::forward_annotation_to_sidecar(&data);

        tx.send(data)
            .map_err(|e| format!("Failed to send annotation: {}", e))
    } else {
        Err("No active session".to_string())
    }
}

#[tauri::command]
async fn cmd_create_overlay(app: tauri::AppHandle, display_id: u32) -> Result<(), String> {
    overlay::create_overlay(&app, display_id)
}

#[tauri::command]
async fn cmd_destroy_overlay(app: tauri::AppHandle) -> Result<(), String> {
    overlay::destroy_overlay(&app)
}

#[tauri::command]
async fn cmd_set_overlay_draw_mode(app: tauri::AppHandle, draw_mode: bool) -> Result<(), String> {
    overlay::set_draw_mode(&app, draw_mode)
}

/// Set the remote-control master switch. Injection is gated by the pipeline;
/// enabling lazily negotiates the input portal (which prompts for permission).
#[tauri::command]
async fn cmd_set_remote_control(
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    let guard = state.pipeline.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(handle) = guard.as_ref() {
        handle.set_remote_control(enabled);
        Ok(())
    } else {
        Err("No active session".to_string())
    }
}

#[tauri::command]
async fn cmd_relaunch(app: tauri::AppHandle) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(&exe)
        .spawn()
        .map_err(|e| e.to_string())?;
    app.exit(0);
    Ok(())
}

#[tauri::command]
async fn cmd_open_stats_window(
    app: tauri::AppHandle,
    room_code: String,
    viewer_count: u32,
) -> Result<(), String> {
    use tauri::Manager;
    if let Some(win) = app.get_webview_window("stats") {
        win.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }

    let url = tauri::WebviewUrl::App(
        format!(
            "index.html?window=stats&room={}&viewers={}",
            room_code, viewer_count
        )
        .into(),
    );
    tauri::webview::WebviewWindowBuilder::new(&app, "stats", url)
        .title("Stats for Nerds")
        .inner_size(320.0, 520.0)
        .resizable(true)
        .decorations(true)
        .build()
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Set ABR tier override for testing. Tier 0 clears override (returns to auto ABR).
/// Tiers: 1=high(30fps/1080p), 2=mid(15fps/1080p), 3=low(5fps/1080p), 4=min(3fps/720p)
#[tauri::command]
async fn cmd_set_tier(state: tauri::State<'_, AppState>, tier: u32) -> Result<String, String> {
    let params = if tier == 0 {
        encode::EncoderParams {
            target_bitrate_kbps: 0,
            max_quantizer: 0,
            min_quantizer: 0,
            target_fps: 0,
            target_height: 0,
        }
    } else {
        encode::abr::tier_params(tier).ok_or_else(|| "Tier must be 0-4".to_string())?
    };
    let tx = {
        let guard = state
            .encoder_params_override_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.clone()
    };
    if let Some(tx) = tx {
        let desc = if tier == 0 {
            "ABR override cleared".to_string()
        } else {
            format!(
                "Tier {} set: {}fps, QP {}, {}p, {} kbps",
                tier,
                params.target_fps,
                params.max_quantizer,
                params.target_height,
                params.target_bitrate_kbps
            )
        };
        tx.send(params)
            .map_err(|e| format!("Failed to send: {}", e))?;
        Ok(desc)
    } else {
        Err("No active session".to_string())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    ::pipeline::fix_glibc_malloc();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tauri::Builder::default()
        .setup(|app| {
            use tauri::Manager;
            app.manage(AppState {
                pipeline: Mutex::new(None),
                annotation_tx: Mutex::new(None),
                resolution_override_tx: Mutex::new(None),
                encoder_params_override_tx: Mutex::new(None),
            });

            // Silent auto-update check on startup
            use tauri_plugin_updater::UpdaterExt;
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let updater = match handle.updater() {
                    Ok(u) => u,
                    Err(e) => {
                        tracing::debug!("Updater init failed: {}", e);
                        return;
                    }
                };
                match updater.check().await {
                    Ok(Some(update)) => {
                        tracing::info!("Update available: {}", update.version);
                        if let Err(e) = update.download_and_install(|_, _| {}, || {}).await {
                            tracing::warn!("Failed to install update: {}", e);
                        } else {
                            tracing::info!("Update installed; restarting");
                            handle.restart();
                        }
                    }
                    Ok(None) => tracing::debug!("No update available"),
                    Err(e) => tracing::debug!("Update check failed: {}", e),
                }
            });

            #[cfg(desktop)]
            {
                use tauri_plugin_global_shortcut::{
                    Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState,
                };
                let toggle = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyD);
                let app_handle = app.handle().clone();
                app.handle().plugin(
                    tauri_plugin_global_shortcut::Builder::new()
                        .with_handler(move |_app, shortcut, event| {
                            if shortcut == &toggle && event.state() == ShortcutState::Pressed {
                                let _ = overlay::toggle_draw_mode(&app_handle);
                            }
                        })
                        .build(),
                )?;
                app.global_shortcut().register(toggle)?;
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
            {
                if cfg!(target_os = "macos") {
                    // macOS: hide instead of close so the app can be
                    // reopened from the dock (standard macOS app behavior).
                    api.prevent_close();
                    let _ = window.hide();
                } else if cfg!(target_os = "windows") {
                    // Windows: there is no tray icon, so the red X must
                    // fully exit. Tear down any active session first so
                    // the pipeline's tokio tasks stop holding the process.
                    use tauri::Manager;
                    api.prevent_close();
                    let app = window.app_handle().clone();
                    tauri::async_runtime::spawn(async move {
                        let state = app.state::<AppState>();
                        let pipeline_handle = {
                            let mut guard =
                                state.pipeline.lock().unwrap_or_else(|e| e.into_inner());
                            guard.take()
                        };
                        if let Some(pipeline_handle) = pipeline_handle {
                            pipeline_handle.stop().await;
                        }
                        let _ = overlay::destroy_overlay(&app);
                        app.exit(0);
                    });
                }
            }
        })
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            cmd_check_screen_recording,
            cmd_request_screen_recording,
            cmd_start_session,
            cmd_stop_session,
            cmd_send_annotation,
            cmd_create_overlay,
            cmd_destroy_overlay,
            cmd_set_overlay_draw_mode,
            cmd_set_remote_control,
            cmd_set_tier,
            cmd_relaunch,
            cmd_open_stats_window,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| match event {
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => {
                // macOS: clicking the dock icon while the window is closed should reopen it
                use tauri::Manager;
                if let Some(win) = app_handle.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
            tauri::RunEvent::Exit => {
                // Ensure overlay is cleaned up when the app exits
                // (macOS: restore cursor/focus, Linux: kill sidecar process)
                let _ = overlay::destroy_overlay(app_handle);
            }
            _ => {}
        });
}

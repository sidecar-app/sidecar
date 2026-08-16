use tauri::AppHandle;
use tokio::sync::mpsc;
use tracing::info;

use crate::events;

pub use pipeline::PipelineHandle;

/// Start the pipeline and bridge events to the Tauri UI.
///
/// This is a thin adapter around `pipeline::start_pipeline()` that:
/// 1. Creates the event channel
/// 2. Starts the pipeline
/// 3. Stores override channels in AppState
/// 4. Spawns a bridge task that converts PipelineEvents into Tauri app.emit() calls
pub async fn start_pipeline(
    app: AppHandle,
) -> Result<PipelineHandle, Box<dyn std::error::Error + Send + Sync>> {
    #[allow(unused_mut)]
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let handle = pipeline::start_pipeline(event_tx).await?;

    {
        use tauri::Manager;
        let state = app.state::<crate::AppState>();
        *state
            .resolution_override_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(handle.resolution_override_tx.clone());
        *state
            .encoder_params_override_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(handle.encoder_params_override_tx.clone());
    }

    let app_clone = app.clone();
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                pipeline::PipelineEvent::ViewerJoined {
                    viewer_id,
                    viewer_count,
                } => {
                    events::emit_viewer_count(&app_clone, viewer_id, viewer_count, "joined");
                }
                pipeline::PipelineEvent::ViewerLeft {
                    viewer_id,
                    viewer_count,
                } => {
                    events::emit_viewer_count(&app_clone, viewer_id, viewer_count, "left");
                }
                pipeline::PipelineEvent::ViewerCountSync { viewer_count } => {
                    events::emit_viewer_count(&app_clone, String::new(), viewer_count, "sync");
                }
                pipeline::PipelineEvent::RoomExpired => {
                    events::emit_room_expired(&app_clone);
                }
                pipeline::PipelineEvent::LatencyUpdate {
                    rtt_ms,
                    packet_loss_pct,
                    status,
                    has_measured_rtt,
                } => {
                    events::emit_latency(
                        &app_clone,
                        rtt_ms,
                        packet_loss_pct,
                        has_measured_rtt,
                        &status,
                    );
                }
                pipeline::PipelineEvent::ResolutionUpdate {
                    source_width,
                    source_height,
                    output_width,
                    output_height,
                    target_bitrate_kbps,
                    max_quantizer,
                    target_fps,
                    bandwidth_kbps,
                    is_relay,
                } => {
                    events::emit_resolution(
                        &app_clone,
                        source_width,
                        source_height,
                        output_width,
                        output_height,
                        target_bitrate_kbps,
                        max_quantizer,
                        target_fps,
                        bandwidth_kbps,
                        is_relay,
                    );
                }
                pipeline::PipelineEvent::DataChannelState { state } => {
                    events::emit_data_channel_state(&app_clone, &state);
                }
                pipeline::PipelineEvent::ViewerAnnotation { data } => {
                    events::emit_viewer_annotation(&app_clone, &data);
                    #[cfg(target_os = "linux")]
                    crate::overlay::forward_annotation_to_sidecar(&data);
                }
                pipeline::PipelineEvent::Warning { message } => {
                    tracing::warn!("Pipeline warning: {message}");
                    events::emit_warning(&app_clone, &message);
                }
                pipeline::PipelineEvent::RemoteControlRequested { viewer_id } => {
                    events::emit_remote_control_requested(&app_clone, &viewer_id);
                }
                pipeline::PipelineEvent::RemoteControlActive {
                    enabled,
                    controllers,
                } => {
                    events::emit_remote_control_active(&app_clone, enabled, controllers);
                    #[cfg(target_os = "linux")]
                    crate::overlay::set_remote_control_active_overlay(enabled);
                }
            }
        }
        events::emit_session_ended(&app_clone);
        info!("Pipeline event bridge task ended");
    });

    Ok(handle)
}

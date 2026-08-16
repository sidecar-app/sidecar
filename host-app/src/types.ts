// Viewer URL base (env override for dev, production default)
export const VIEWER_BASE_URL = import.meta.env.VITE_VIEWER_URL ||
  "https://view.sidecar.rs";

// Tauri event payload types (match Rust event structs from events.rs)
export interface ViewerCountEvent {
  count: number;
  viewer_id: string;
  action: "joined" | "left";
}

export interface LatencyEvent {
  rtt_ms: number;
  packet_loss_pct: number;
  status: "connected" | "degraded" | "disconnected";
  has_measured_rtt: boolean;
}

// App mode for role routing
export type AppMode = "launch" | "host" | "permission-denied";

// Data channel connection state
export type DataChannelStatus = "pending" | "open" | "closed" | "error";

// Resolution change event payload (encode pipeline -> UI)
export interface ResolutionEvent {
  source_width: number;
  source_height: number;
  output_width: number;
  output_height: number;
  /** Target bitrate from ABR controller (kbps) */
  target_bitrate_kbps: number;
  /** Max quantizer from ABR controller (higher = lower quality) */
  max_quantizer: number;
  /** Target FPS from ABR controller */
  target_fps: number;
  /** Measured throughput from network stats (kbps) */
  bandwidth_kbps: number;
  /** Whether the connection is relayed through a TURN server */
  is_relay: boolean;
}

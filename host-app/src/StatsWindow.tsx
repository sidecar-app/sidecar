import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type { LatencyEvent, ResolutionEvent, ViewerCountEvent } from "./types";
import "./App.css";

export function StatsWindow() {
  const searchParams = new URLSearchParams(window.location.search);
  const [roomCode] = useState(() => searchParams.get("room") ?? "");
  const [viewerCount, setViewerCount] = useState(() => {
    const v = searchParams.get("viewers");
    return v ? parseInt(v, 10) || 0 : 0;
  });
  const [latency, setLatency] = useState<number | null>(null);
  const [packetLoss, setPacketLoss] = useState<number | null>(null);
  const [connectionStatus, setConnectionStatus] = useState<
    "connected" | "degraded" | "disconnected"
  >("connected");
  const [resolution, setResolution] = useState<ResolutionEvent | null>(null);

  useEffect(() => {
    const unlistenViewerCount = listen<ViewerCountEvent>(
      "viewer-count",
      (e) => {
        setViewerCount(e.payload.count);
      },
    );
    const unlistenLatency = listen<LatencyEvent>("latency-update", (e) => {
      setLatency(e.payload.has_measured_rtt ? e.payload.rtt_ms : null);
      setPacketLoss(
        e.payload.has_measured_rtt ? e.payload.packet_loss_pct : null,
      );
      setConnectionStatus(e.payload.status);
    });
    const unlistenResolution = listen<ResolutionEvent>(
      "resolution-update",
      (e) => {
        setResolution(e.payload);
      },
    );

    return () => {
      unlistenViewerCount.then((f) => f());
      unlistenLatency.then((f) => f());
      unlistenResolution.then((f) => f());
    };
  }, []);

  return (
    <div className="stats-window-container">
      <div className="stats-overlay-title">Stats for Nerds</div>

      <div className="stats-overlay-section">Encode Pipeline</div>
      <Row
        label="Source"
        value={resolution
          ? `${resolution.source_width}x${resolution.source_height}`
          : "N/A"}
      />
      <Row
        label="Output"
        value={resolution
          ? `${resolution.output_width}x${resolution.output_height}`
          : "N/A"}
      />
      <Row
        label="Target Bitrate"
        value={resolution ? `${resolution.target_bitrate_kbps} kbps` : "N/A"}
      />
      <Row
        label="Max Quantizer"
        value={resolution?.max_quantizer?.toString() ?? "N/A"}
      />
      <Row
        label="Target FPS"
        value={resolution?.target_fps?.toString() ?? "N/A"}
      />
      <Row
        label="Bandwidth"
        value={resolution ? `${resolution.bandwidth_kbps} kbps` : "N/A"}
      />
      <Row
        label="Relay"
        value={resolution ? (resolution.is_relay ? "TURN" : "Direct") : "N/A"}
      />

      <div className="stats-overlay-section">Network</div>
      <Row
        label="RTT"
        value={latency !== null ? `${Math.round(latency)}ms` : "N/A"}
      />
      <Row
        label="Packet Loss"
        value={packetLoss !== null ? `${packetLoss.toFixed(1)}%` : "N/A"}
      />
      <Row label="Status" value={connectionStatus} />

      <div className="stats-overlay-section">Session</div>
      <Row label="Viewers" value={String(viewerCount)} />
      <Row label="Room" value={roomCode} />
    </div>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="stats-overlay-row">
      <span className="stats-overlay-label">{label}</span>
      <span className="stats-overlay-value">{value}</span>
    </div>
  );
}

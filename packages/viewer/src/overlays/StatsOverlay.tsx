import { useCallback, useEffect, useRef, useState } from "react";

interface StatsOverlayProps {
  peerConnection: RTCPeerConnection | null;
  visible: boolean;
  onClose: () => void;
}

interface StatsData {
  // Video
  resolution: string;
  codec: string;
  fps: number;
  framesDecoded: number;
  framesDropped: number;
  // Network
  rttMs: number | null;
  jitterMs: number | null;
  packetsLost: number;
  packetsReceived: number;
  bitrateKbps: number;
  pliCount: number;
  nackCount: number;
  firCount: number;
  // Connection
  localCandidateType: string;
  remoteCandidateType: string;
  protocol: string;
  dtlsState: string;
}

const EMPTY_STATS: StatsData = {
  resolution: "-",
  codec: "-",
  fps: 0,
  framesDecoded: 0,
  framesDropped: 0,
  rttMs: null,
  jitterMs: null,
  packetsLost: 0,
  packetsReceived: 0,
  bitrateKbps: 0,
  pliCount: 0,
  nackCount: 0,
  firCount: 0,
  localCandidateType: "-",
  remoteCandidateType: "-",
  protocol: "-",
  dtlsState: "-",
};

export function StatsOverlay({
  peerConnection,
  visible,
  onClose,
}: StatsOverlayProps) {
  const [stats, setStats] = useState<StatsData>(EMPTY_STATS);
  const prevBytesReceived = useRef<number>(0);
  const prevFramesDecoded = useRef<number>(0);
  const prevTimestamp = useRef<number>(0);

  const pollStats = useCallback(async () => {
    if (!peerConnection) return;
    try {
      const report = await peerConnection.getStats();
      const next: Partial<StatsData> = {};

      let codecId: string | undefined;

      report.forEach((entry) => {
        // deno-lint-ignore no-explicit-any
        if (entry.type === "inbound-rtp" && (entry as any).kind === "video") {
          // deno-lint-ignore no-explicit-any
          const r = entry as any;
          const w: number = r.frameWidth ?? 0;
          const h: number = r.frameHeight ?? 0;
          next.resolution = w > 0 && h > 0 ? `${w}x${h}` : "-";
          next.framesDecoded = r.framesDecoded ?? 0;
          next.framesDropped = r.framesDropped ?? 0;
          next.packetsLost = r.packetsLost ?? 0;
          next.packetsReceived = r.packetsReceived ?? 0;
          next.pliCount = r.pliCount ?? 0;
          next.nackCount = r.nackCount ?? 0;
          next.firCount = r.firCount ?? 0;
          const jitter: number | undefined = r.jitter;
          next.jitterMs = jitter !== undefined
            ? Math.round(jitter * 1000)
            : null;
          codecId = r.codecId;

          // Compute fps and bitrate from deltas
          const now = performance.now();
          const elapsed = (now - prevTimestamp.current) / 1000;
          if (prevTimestamp.current > 0 && elapsed > 0) {
            const framesDelta = (r.framesDecoded ?? 0) -
              prevFramesDecoded.current;
            next.fps = Math.round(framesDelta / elapsed);

            const bytesDelta = (r.bytesReceived ?? 0) -
              prevBytesReceived.current;
            next.bitrateKbps = Math.round((bytesDelta * 8) / (elapsed * 1000));
          }
          prevBytesReceived.current = r.bytesReceived ?? 0;
          prevFramesDecoded.current = r.framesDecoded ?? 0;
          prevTimestamp.current = now;
        }

        // deno-lint-ignore no-explicit-any
        if (entry.type === "candidate-pair" && (entry as any).nominated) {
          // deno-lint-ignore no-explicit-any
          const cp = entry as any;
          const rtt: number | undefined = cp.currentRoundTripTime;
          next.rttMs = rtt !== undefined ? Math.round(rtt * 1000) : null;

          // Resolve candidate types
          const localId: string | undefined = cp.localCandidateId;
          const remoteId: string | undefined = cp.remoteCandidateId;
          if (localId) {
            // deno-lint-ignore no-explicit-any
            const lc = report.get(localId) as any;
            if (lc) {
              next.localCandidateType = lc.candidateType ?? "-";
              next.protocol = lc.protocol ?? "-";
            }
          }
          if (remoteId) {
            // deno-lint-ignore no-explicit-any
            const rc = report.get(remoteId) as any;
            if (rc) {
              next.remoteCandidateType = rc.candidateType ?? "-";
            }
          }
        }

        if (entry.type === "transport") {
          // deno-lint-ignore no-explicit-any
          next.dtlsState = (entry as any).dtlsState ?? "-";
        }
      });

      // Resolve codec name
      if (codecId) {
        // deno-lint-ignore no-explicit-any
        const codecEntry = report.get(codecId) as any;
        if (codecEntry) {
          next.codec = (codecEntry.mimeType ?? "-").replace("video/", "");
        }
      }

      setStats((prev) => ({ ...prev, ...next }));
    } catch {
      // peer connection may be closed
    }
  }, [peerConnection]);

  useEffect(() => {
    if (!visible) return;
    // Reset deltas on open
    prevBytesReceived.current = 0;
    prevFramesDecoded.current = 0;
    prevTimestamp.current = 0;
    setStats(EMPTY_STATS);

    // Poll immediately then every second
    pollStats();
    const id = setInterval(pollStats, 1000);
    return () => clearInterval(id);
  }, [visible, pollStats]);

  if (!visible) return null;

  return (
    <div className="stats-overlay">
      <div className="stats-overlay-title">
        <span>Stats for Nerds</span>
        <span className="stats-close" onClick={onClose}>
          x
        </span>
      </div>

      <div className="stats-overlay-section">Video</div>
      <Row label="Resolution" value={stats.resolution} />
      <Row label="Codec" value={stats.codec} />
      <Row label="FPS" value={String(stats.fps)} />
      <Row label="Frames Decoded" value={String(stats.framesDecoded)} />
      <Row label="Frames Dropped" value={String(stats.framesDropped)} />

      <div className="stats-overlay-section">Network</div>
      <Row
        label="RTT"
        value={stats.rttMs !== null ? `${stats.rttMs}ms` : "-"}
      />
      <Row
        label="Jitter"
        value={stats.jitterMs !== null ? `${stats.jitterMs}ms` : "-"}
      />
      <Row
        label="Packet Loss"
        value={stats.packetsReceived > 0
          ? `${
            ((stats.packetsLost / (stats.packetsLost + stats.packetsReceived)) *
              100).toFixed(1)
          }% (${stats.packetsLost})`
          : "-"}
      />
      <Row label="Bitrate" value={`${stats.bitrateKbps} kbps`} />
      <Row
        label="PLI / NACK / FIR"
        value={`${stats.pliCount} / ${stats.nackCount} / ${stats.firCount}`}
      />

      <div className="stats-overlay-section">Connection</div>
      <Row label="Local" value={stats.localCandidateType} />
      <Row label="Remote" value={stats.remoteCandidateType} />
      <Row label="Protocol" value={stats.protocol} />
      <Row label="DTLS" value={stats.dtlsState} />
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

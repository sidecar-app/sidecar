import { useCallback, useEffect, useRef, useState } from "react";

interface ConnectionOverlayProps {
  status: "connected" | "degraded" | "disconnected";
  latencyMs?: number;
  stallState?: "none" | "stalled" | "lost";
}

const STATUS_COLORS: Record<string, string> = {
  connected: "#22c55e",
  degraded: "#eab308",
  disconnected: "#ef4444",
};

const STATUS_LABELS: Record<string, string> = {
  connected: "Connected",
  degraded: "Degraded",
  disconnected: "Disconnected",
};

function getPillLabel(
  status: string,
  stallState: "none" | "stalled" | "lost" | undefined,
  latencyMs: number | undefined,
): string {
  if (stallState === "stalled" && status === "degraded") return "Video Stalled";
  if (stallState === "lost" && status === "disconnected") return "Stream Lost";
  if (status === "disconnected") return "Disconnected";
  if (latencyMs !== undefined) return `${Math.round(latencyMs)}ms`;
  return STATUS_LABELS[status] || status;
}

export function ConnectionOverlay({
  status,
  latencyMs,
  stallState,
}: ConnectionOverlayProps) {
  const stallActive = stallState !== undefined && stallState !== "none";
  const [recentlyActive, setRecentlyActive] = useState(true);
  const fadeTimeout = useRef<ReturnType<typeof setTimeout> | null>(null);

  const handleMouseEnter = useCallback(() => {
    if (fadeTimeout.current) {
      clearTimeout(fadeTimeout.current);
      fadeTimeout.current = null;
    }
    setRecentlyActive(true);
  }, []);

  const handleMouseLeave = useCallback(() => {
    fadeTimeout.current = setTimeout(() => {
      setRecentlyActive(false);
    }, 2000);
  }, []);

  // Clean up timeout on unmount
  useEffect(() => {
    return () => {
      if (fadeTimeout.current) {
        clearTimeout(fadeTimeout.current);
      }
    };
  }, []);

  // Start faded after initial 2s
  useEffect(() => {
    fadeTimeout.current = setTimeout(() => {
      setRecentlyActive(false);
    }, 2000);
    return () => {
      if (fadeTimeout.current) {
        clearTimeout(fadeTimeout.current);
      }
    };
  }, []);

  const dotColor = STATUS_COLORS[status] || STATUS_COLORS.disconnected;

  return (
    <div
      onMouseEnter={handleMouseEnter}
      onMouseLeave={handleMouseLeave}
      style={{
        position: "absolute",
        top: stallActive ? 46 : 8,
        left: 8,
        zIndex: 90,
        opacity: stallActive || recentlyActive ? 1 : 0.25,
        transition: "opacity 0.4s ease, top 0.3s ease",
        pointerEvents: "auto",
      }}
    >
      {/* Pill */}
      <div
        style={{
          display: "inline-flex",
          alignItems: "center",
          gap: 5,
          background: "rgba(0, 0, 0, 0.5)",
          borderRadius: 8,
          padding: "2px 8px 2px 6px",
          cursor: "default",
          userSelect: "none",
        }}
      >
        <span
          style={{
            width: 10,
            height: 10,
            borderRadius: "50%",
            backgroundColor: dotColor,
            display: "inline-block",
            flexShrink: 0,
          }}
        />
        <span style={{ fontSize: 13, color: "#a1a1aa", whiteSpace: "nowrap" }}>
          {getPillLabel(status, stallState, latencyMs)}
        </span>
      </div>
    </div>
  );
}

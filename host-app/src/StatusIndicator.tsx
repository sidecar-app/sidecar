import { useState } from "react";

interface StatusIndicatorProps {
  status: "connected" | "degraded" | "disconnected";
  latencyMs?: number;
  annotationStatus?: string;
  onClick?: () => void;
}

export function StatusIndicator(
  { status, latencyMs, annotationStatus, onClick }: StatusIndicatorProps,
) {
  const [hovered, setHovered] = useState(false);

  const colors = {
    connected: "#22c55e", // green-500
    degraded: "#eab308", // yellow-500
    disconnected: "#ef4444", // red-500
  };

  const annotationText = annotationStatus === "open"
    ? "Annotations ready"
    : annotationStatus === "connecting"
    ? "Annotations connecting..."
    : annotationStatus === "reconnecting"
    ? "Annotations reconnecting..."
    : annotationStatus === "closed"
    ? "Annotations unavailable"
    : null;

  return (
    <span style={{ display: "inline-flex", alignItems: "center", gap: "6px" }}>
      <span
        onClick={onClick}
        onMouseEnter={() =>
          setHovered(true)}
        onMouseLeave={() => setHovered(false)}
        title={onClick ? "Click for connection details" : undefined}
        style={{
          width: "10px",
          height: "10px",
          borderRadius: "50%",
          backgroundColor: colors[status],
          display: "inline-block",
          cursor: onClick ? "pointer" : "default",
          transform: onClick && hovered ? "scale(1.3)" : "scale(1)",
          transition: "transform 0.15s ease",
        }}
      />
      <span style={{ fontSize: "13px", color: "#a1a1aa" }}>
        {status === "disconnected"
          ? "Disconnected"
          : latencyMs !== undefined
          ? `${Math.round(latencyMs)}ms`
          : status}
        {annotationText && (
          <span
            style={{ marginLeft: "8px", fontSize: "12px", color: "#71717a" }}
          >
            &bull; {annotationText}
          </span>
        )}
      </span>
    </span>
  );
}

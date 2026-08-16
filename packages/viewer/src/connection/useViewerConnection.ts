import { useCallback, useEffect, useRef, useState } from "react";
import { WebRTCManager } from "./WebRTCManager";
import type { DataPacket } from "./protocol";

interface UseViewerConnectionOptions {
  roomCode: string;
  videoRef: React.RefObject<HTMLVideoElement | null>;
  updateVideoLayout: () => void;
  setVideoSize: (size: { w: number; h: number }) => void;
  onData: (packet: DataPacket) => void;
}

export function useViewerConnection(options: UseViewerConnectionOptions) {
  const [connected, setConnected] = useState(false);
  const [status, setStatus] = useState("Connecting...");
  const [hostLeft, setHostLeft] = useState(false);
  const [latency, setLatency] = useState<number | null>(null);
  const [connectionStatus, setConnectionStatus] = useState<
    "connected" | "degraded" | "disconnected"
  >("connected");
  const [stallState, setStallState] = useState<"none" | "stalled" | "lost">(
    "none",
  );
  const stallStateRef = useRef<"none" | "stalled" | "lost">("none");

  const managerRef = useRef<WebRTCManager | null>(null);
  const statsInterval = useRef<ReturnType<typeof setInterval> | null>(null);
  const prevFramesDecoded = useRef(0);
  const stallStart = useRef<number | null>(null);
  const stallCount = useRef(0);
  const lastKfRequest = useRef(0);
  const kfAttempts = useRef(0);
  const decoderResetAttempted = useRef(false);

  // Keep options in a ref to avoid stale closures in event handlers
  const optionsRef = useRef(options);
  optionsRef.current = options;

  const updateStallState = useCallback((state: "none" | "stalled" | "lost") => {
    stallStateRef.current = state;
    setStallState(state);
  }, []);

  const teardown = useCallback(() => {
    managerRef.current?.cleanup();
    managerRef.current = null;
    if (statsInterval.current) {
      clearInterval(statsInterval.current);
      statsInterval.current = null;
    }
  }, []);

  const startStatsPolling = useCallback(() => {
    statsInterval.current = setInterval(async () => {
      const pc = managerRef.current?.peerConnection;
      if (!pc) return;
      try {
        const stats = await pc.getStats();
        stats.forEach((report) => {
          if (report.type === "candidate-pair" && report.nominated) {
            // deno-lint-ignore no-explicit-any
            const rtt = (report as any).currentRoundTripTime;
            if (rtt !== undefined) {
              const rttMs = rtt * 1000;
              setLatency(rttMs);
              setConnectionStatus(rttMs > 100 ? "degraded" : "connected");
            }
          }
          // Stall detection via framesDecoded delta (D-01, D-10)
          if (
            report.type === "inbound-rtp" &&
            // deno-lint-ignore no-explicit-any
            (report as any).kind === "video"
          ) {
            // deno-lint-ignore no-explicit-any
            const decoded: number = (report as any).framesDecoded ?? 0;
            if (decoded === prevFramesDecoded.current && decoded > 0) {
              stallCount.current++;
              if (!stallStart.current) {
                stallStart.current = Date.now();
              }
              const elapsed = Date.now() - stallStart.current;

              // At 2s stall: try resetting the video decoder by re-attaching
              // the MediaStream. This forces WebKit to reinitialize its VP9
              // decoder and pick up the next keyframe without a full reconnect.
              if (stallCount.current === 2 && !decoderResetAttempted.current) {
                decoderResetAttempted.current = true;
                const video = optionsRef.current.videoRef.current;
                if (video && video.srcObject) {
                  console.log(
                    "[Viewer] Stall detected — resetting video decoder",
                  );
                  const stream = video.srcObject;
                  video.srcObject = null;
                  video.srcObject = stream;
                  video.play().catch(() => {});
                }
              }

              // After 3+ polls: request keyframe from host as backup
              if (
                stallCount.current >= 3 &&
                stallStateRef.current !== "lost" &&
                kfAttempts.current < 3
              ) {
                const now = Date.now();
                if (now - lastKfRequest.current >= 10000) {
                  lastKfRequest.current = now;
                  kfAttempts.current++;
                  try {
                    // deno-lint-ignore no-explicit-any
                    managerRef.current?.send({ t: "kf" } as any);
                  } catch {
                    /* data channel may not be open */
                  }
                }
              }

              if (elapsed >= 5000) {
                updateStallState("lost");
                setConnectionStatus("disconnected");
              } else if (elapsed >= 2000) {
                updateStallState("stalled");
                setConnectionStatus("degraded");
              }
            } else {
              // Frames flowing -- clear stall
              stallStart.current = null;
              stallCount.current = 0;
              kfAttempts.current = 0;
              decoderResetAttempted.current = false;
              if (stallStateRef.current !== "none") updateStallState("none");
            }
            prevFramesDecoded.current = decoded;
          }
        });
      } catch {
        /* pc may be closed */
      }
    }, 1000);
  }, [updateStallState]);

  const connect = useCallback(() => {
    const { videoRef } = optionsRef.current;
    const manager = new WebRTCManager();
    managerRef.current = manager;

    manager.onStatus = setStatus;
    manager.onHostLeft = () => setHostLeft(true);
    manager.onStream = (stream) => {
      setConnected(true);
      const video = videoRef.current;
      if (video) {
        video.srcObject = stream;
        video.onloadedmetadata = () => {
          video.play();
          optionsRef.current.updateVideoLayout();
          if (video) {
            optionsRef.current.setVideoSize({
              w: video.videoWidth,
              h: video.videoHeight,
            });
          }
        };
        // Handle mid-stream resolution changes (VP9 encoder reconfigure)
        video.onresize = () => {
          console.log(
            `[Viewer] Video resolution changed to ${video.videoWidth}x${video.videoHeight}`,
          );
          optionsRef.current.updateVideoLayout();
          optionsRef.current.setVideoSize({
            w: video.videoWidth,
            h: video.videoHeight,
          });
        };
      }
    };
    manager.onData = (packet: DataPacket) => optionsRef.current.onData(packet);

    manager.joinRoom(optionsRef.current.roomCode);
    startStatsPolling();
  }, [startStatsPolling]);

  // Auto-join on mount
  useEffect(() => {
    connect();
    return teardown;
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const handleReconnect = useCallback(() => {
    // Tear down current connection (D-04: fresh re-join, not ICE restart)
    teardown();
    // Reset stall state
    updateStallState("none");
    prevFramesDecoded.current = 0;
    stallStart.current = null;
    stallCount.current = 0;
    lastKfRequest.current = 0;
    kfAttempts.current = 0;
    decoderResetAttempted.current = false;
    setConnected(false);
    setStatus("Reconnecting...");
    connect();
  }, [teardown, connect, updateStallState]);

  return {
    connected,
    status,
    hostLeft,
    stallState,
    latency,
    connectionStatus,
    managerRef,
    handleReconnect,
  };
}

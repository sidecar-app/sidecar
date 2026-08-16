import type { DataPacket, ViewportPacket } from "./protocol";

type SignallingMessage =
  | { type: "join"; roomId: string }
  | { type: "join-success"; roomId: string; iceServers?: RTCIceServer[] }
  | { type: "error"; message: string }
  | { type: "offer"; sdp: RTCSessionDescriptionInit; from: string }
  | { type: "answer"; sdp: RTCSessionDescriptionInit; targetId: string }
  | { type: "candidate"; candidate: RTCIceCandidateInit; from: string }
  | { type: "host-left" };

export type PeerConnectionState =
  | "new"
  | "connecting"
  | "connected"
  | "disconnected"
  | "failed"
  | "closed";

export class WebRTCManager {
  private ws: WebSocket | null = null;
  private pc: RTCPeerConnection | null = null;
  private dataChannel: RTCDataChannel | null = null;
  private iceServers: RTCIceServer[] = [
    { urls: "stun:stun.l.google.com:19302" },
  ];
  private viewerId: string = "";
  private resizeHandler: (() => void) | null = null;
  private resizeTimer: ReturnType<typeof setTimeout> | null = null;

  public onStream: ((stream: MediaStream) => void) | null = null;
  public onData: ((data: DataPacket) => void) | null = null;
  public onStatus: ((status: string) => void) | null = null;
  public onHostLeft: (() => void) | null = null;
  public onConnectionStateChange:
    | ((state: PeerConnectionState) => void)
    | null = null;

  /** The hosted signaling server. VITE_SIGNALING_URL overrides it in dev builds. */
  private static readonly DEFAULT_SIGNALING_URL = "wss://ws.sidecar.rs";

  constructor(
    private signalingUrl: string = import.meta.env.VITE_SIGNALING_URL ||
      WebRTCManager.DEFAULT_SIGNALING_URL,
  ) {}

  get peerConnection(): RTCPeerConnection | null {
    return this.pc;
  }

  joinRoom(roomId: string) {
    this.viewerId = typeof crypto.randomUUID === "function"
      ? crypto.randomUUID()
      : Array.from(crypto.getRandomValues(new Uint8Array(16)))
        .map(
          (b, i) =>
            ([4, 6, 8, 10].includes(i) ? "-" : "") +
            b.toString(16).padStart(2, "0"),
        )
        .join("");
    this.ws = new WebSocket(this.signalingUrl);

    this.ws.onopen = () => {
      console.log("Connected to signaling server");
      this.onStatus?.("Connected to signaling");
      this.ws?.send(JSON.stringify({ type: "join", roomId }));
    };

    this.ws.onmessage = async (event) => {
      const msg = JSON.parse(event.data) as SignallingMessage;
      console.log("Received:", msg.type);

      switch (msg.type) {
        case "join-success":
          if (msg.iceServers && msg.iceServers.length > 0) {
            this.iceServers = msg.iceServers;
          }
          this.onStatus?.("Joined room, waiting for host...");
          break;
        case "error":
          this.onStatus?.(`Error: ${msg.message}`);
          break;
        case "offer":
          await this.handleOffer(msg.sdp, msg.from);
          break;
        case "candidate":
          // deno-lint-ignore no-explicit-any
          await this.handleCandidate(msg as any);
          break;
        case "host-left":
          this.onStatus?.("Host left the session");
          this.pc?.close();
          this.pc = null;
          this.onHostLeft?.();
          break;
      }
    };
  }

  send(packet: DataPacket) {
    if (this.dataChannel && this.dataChannel.readyState === "open") {
      this.dataChannel.send(JSON.stringify(packet));
    }
  }

  private async handleOffer(
    sdp: string | RTCSessionDescriptionInit,
    hostId: string,
  ) {
    this.onStatus?.("Received offer from host, connecting...");

    // Clean up existing peer connection if re-offer arrives
    if (this.pc) {
      console.log(
        "[WebRTC] Closing existing peer connection before handling new offer",
      );
      this.pc.close();
      this.pc = null;
      this.dataChannel = null;
    }

    this.pc = new RTCPeerConnection({
      iceServers: this.iceServers,
    });

    this.pc.ontrack = (event) => {
      console.log("Received remote stream");
      this.onStatus?.("Receiving video stream");

      // Minimize jitter buffer for low-latency playback.
      // jitterBufferTarget = 0 tells the browser to use the smallest
      // buffer it can sustain without underrun (Chrome 110+, Firefox 128+).
      const receiver = event.receiver;
      if ("jitterBufferTarget" in receiver) {
        // deno-lint-ignore no-explicit-any
        (receiver as any).jitterBufferTarget = 0;
      }

      this.onStream?.(event.streams[0]);
    };

    // Monitor connection state for disconnect/failure detection
    this.pc.onconnectionstatechange = () => {
      const state = this.pc?.connectionState as PeerConnectionState | undefined;
      if (state) {
        console.log(`[WebRTC] Connection state: ${state}`);
        this.onConnectionStateChange?.(state);
      }
    };

    this.pc.ondatachannel = (event) => {
      console.log("Received data channel");
      this.dataChannel = event.channel;

      this.dataChannel.onopen = () => {
        this.sendViewport();
      };

      this.dataChannel.onmessage = (e) => {
        try {
          const data = JSON.parse(e.data) as DataPacket;
          if (data.t === "cl") {
            console.log("[WebRTC] Received cl packet:", JSON.stringify(data));
          }
          this.onData?.(data);
        } catch (err) {
          console.warn(
            "[WebRTC] Failed to parse data channel message:",
            err,
            e.data,
          );
        }
      };
    };

    this.resizeHandler = () => {
      if (this.resizeTimer) clearTimeout(this.resizeTimer);
      this.resizeTimer = setTimeout(() => this.sendViewport(), 300);
    };
    window.addEventListener("resize", this.resizeHandler);

    this.pc.onicecandidate = (event) => {
      if (event.candidate) {
        this.ws?.send(
          JSON.stringify({
            type: "candidate",
            candidate: event.candidate.candidate,
            sdpMid: event.candidate.sdpMid,
            sdpMLineIndex: event.candidate.sdpMLineIndex,
            targetId: hostId,
          }),
        );
      }
    };

    // Host sends SDP as a raw string, not an RTCSessionDescriptionInit object
    const desc = typeof sdp === "string"
      ? { type: "offer" as RTCSdpType, sdp }
      : sdp;
    await this.pc.setRemoteDescription(desc);
    const answer = await this.pc.createAnswer();
    await this.pc.setLocalDescription(answer);

    // Test codec fallback: ?codec=vp9 strips all other codecs from the answer,
    // forcing the host to fall back to the specified codec.
    let answerSdp = answer.sdp ?? "";
    const forceCodec = new URLSearchParams(window.location.search)
      .get("codec")
      ?.toUpperCase();
    if (forceCodec) {
      console.log(
        `[WebRTC] codec=${forceCodec}: stripping other codecs from SDP answer`,
      );
      answerSdp = answerSdp
        .split("\n")
        .filter((line) => {
          const rtpmap = line.match(/^a=rtpmap:(\d+) (\w+)\//);
          if (!rtpmap) return true; // keep non-rtpmap lines
          return rtpmap[2].toUpperCase() === forceCodec;
        })
        .filter((line) => {
          // Remove fmtp/rtcp-fb for payload types not matching the forced codec
          const fb = line.match(/^a=(?:rtcp-fb|fmtp):(\d+) /);
          if (!fb) return true;
          // Check if this PT has an rtpmap for the forced codec
          const pt = fb[1];
          return answerSdp.includes(`a=rtpmap:${pt} ${forceCodec}/`);
        })
        .join("\n")
        .replace(/(m=video \d+ \S+)(.*)/, (_match, prefix, pts) => {
          // Keep only payload types that have an rtpmap for the forced codec
          const keepPts = [
            ...answerSdp.matchAll(
              new RegExp(`a=rtpmap:(\\d+) ${forceCodec}/`, "gi"),
            ),
          ].map((m) => m[1]);
          const filtered = pts
            .trim()
            .split(/\s+/)
            .filter((pt: string) => keepPts.includes(pt));
          return prefix + " " + filtered.join(" ");
        });
    }

    // Host expects sdp as a raw SDP string
    this.ws?.send(
      JSON.stringify({
        type: "answer",
        sdp: answerSdp,
        targetId: hostId,
      }),
    );
  }

  private async handleCandidate(msg: {
    candidate: string;
    sdpMid?: string;
    sdpMLineIndex?: number;
  }) {
    if (this.pc) {
      await this.pc.addIceCandidate(
        new RTCIceCandidate({
          candidate: msg.candidate,
          sdpMid: msg.sdpMid ?? null,
          sdpMLineIndex: msg.sdpMLineIndex ?? null,
        }),
      );
    }
  }

  private sendViewport() {
    if (this.dataChannel && this.dataChannel.readyState === "open") {
      const dpr = window.devicePixelRatio || 1;
      const packet: ViewportPacket = {
        t: "vp",
        w: Math.round(window.innerWidth * dpr),
        h: Math.round(window.innerHeight * dpr),
        id: this.viewerId,
      };
      this.dataChannel.send(JSON.stringify(packet));
    }
  }

  cleanup() {
    if (this.resizeTimer) {
      clearTimeout(this.resizeTimer);
      this.resizeTimer = null;
    }
    if (this.resizeHandler) {
      window.removeEventListener("resize", this.resizeHandler);
      this.resizeHandler = null;
    }
    this.ws?.close();
    this.pc?.close();
  }
}

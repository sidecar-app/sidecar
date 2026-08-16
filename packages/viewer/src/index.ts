export { ViewerScreen } from "./ViewerScreen";
export type { ViewerScreenProps } from "./ViewerScreen";
export { WebRTCManager } from "./connection/WebRTCManager";
export type { PeerConnectionState } from "./connection/WebRTCManager";
export { ConnectionOverlay } from "./overlays/ConnectionOverlay";
export { StatsOverlay } from "./overlays/StatsOverlay";
export { DrawSettingsPopover } from "./toolbar/DrawSettingsPopover";
export {
  ExitIcon,
  LinkIcon,
  PencilIcon,
  PointerIcon,
  StopIcon,
  TrashIcon,
  ZoomInIcon,
  ZoomOutIcon,
} from "./toolbar/icons";
export { useOutsideClick } from "./hooks/useOutsideClick";
export { useFadeLoop } from "./drawing/useFadeLoop";
export { useZoom } from "./hooks/useZoom";
export {
  BASE_OPACITY,
  FADE_DURATION_MS,
  HOLD_DELAY_MS,
} from "./drawing/fadeConstants";
export { COLOR_PALETTE, HOST_DEFAULT_COLOR } from "./toolbar/colorConstants";
export {
  DEFAULT_STROKE_WIDTH,
  STROKE_WIDTH_OPTIONS,
} from "./toolbar/drawConstants";
export { extractRoomCode } from "./extractRoomCode";
export type * from "./connection/protocol";

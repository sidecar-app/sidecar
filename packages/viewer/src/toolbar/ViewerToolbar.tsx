import {
  ExitIcon,
  KeyboardIcon,
  PencilIcon,
  PointerIcon,
  TrashIcon,
  ZoomInIcon,
  ZoomOutIcon,
} from "./icons";
import { DrawSettingsPopover } from "./DrawSettingsPopover";

interface ZoomState {
  canZoom: boolean;
  isZoomed: boolean;
  scale: number;
  maxScale: number;
  zoomLabel: string;
  resetZoom: () => void;
  stepZoom: (direction: 1 | -1) => void;
}

interface ViewerToolbarProps {
  drawMode: boolean;
  toggleDrawMode: () => void;
  remoteControlMode: boolean;
  toggleRemoteControlMode: () => void;
  showKeyboardButton: boolean;
  onShowKeyboard: () => void;
  myColor: string | null;
  setShowDrawSettings: React.Dispatch<React.SetStateAction<boolean>>;
  showDrawSettings: boolean;
  drawSettingsBtnRef: React.Ref<HTMLButtonElement>;
  drawSettingsPopoverRef: React.Ref<HTMLDivElement>;
  handleClearMine: () => void;
  zoom: ZoomState;
  onLeave: () => void;
  strokeWidth: number;
  onColorChange: (color: string) => void;
  onStrokeWidthChange: (width: number) => void;
}

export function ViewerToolbar({
  drawMode,
  toggleDrawMode,
  remoteControlMode,
  toggleRemoteControlMode,
  showKeyboardButton,
  onShowKeyboard,
  myColor,
  setShowDrawSettings,
  showDrawSettings,
  drawSettingsBtnRef,
  drawSettingsPopoverRef,
  handleClearMine,
  zoom,
  onLeave,
  strokeWidth,
  onColorChange,
  onStrokeWidthChange,
}: ViewerToolbarProps) {
  return (
    <div className="draw-toolbar">
      <button
        type="button"
        className={`toolbar-icon-btn ${drawMode ? "active" : ""}`}
        onClick={toggleDrawMode}
        disabled={!myColor}
        title="Toggle Draw Mode (D)"
      >
        <PencilIcon />
      </button>

      <div className={`draw-tools-group ${drawMode ? "visible" : ""}`}>
        <button
          type="button"
          className="toolbar-icon-btn draw-settings-btn"
          onClick={() => setShowDrawSettings((prev) => !prev)}
          title="Draw Settings"
          tabIndex={drawMode ? 0 : -1}
          ref={drawSettingsBtnRef}
        >
          <span
            className="settings-color-indicator"
            style={{ borderColor: myColor || "#fff" }}
          />
        </button>
        <button
          type="button"
          className="toolbar-icon-btn"
          onClick={handleClearMine}
          title="Clear My Annotations"
          tabIndex={drawMode ? 0 : -1}
        >
          <TrashIcon />
        </button>
      </div>

      <button
        type="button"
        className={`toolbar-icon-btn ${remoteControlMode ? "active" : ""}`}
        onClick={toggleRemoteControlMode}
        disabled={!myColor}
        title="Toggle Remote Control (R)"
      >
        <PointerIcon />
      </button>

      {zoom.canZoom &&
        (() => {
          const atMax = zoom.scale >= zoom.maxScale - 0.01;
          return (
            <button
              type="button"
              className={`toolbar-icon-btn zoom-btn ${
                zoom.isZoomed ? "zoomed" : ""
              }`}
              onClick={() => (atMax ? zoom.resetZoom() : zoom.stepZoom(1))}
              title={atMax
                ? `${zoom.zoomLabel}, click to reset. Pan with arrows or hjkl`
                : zoom.isZoomed
                ? `${zoom.zoomLabel}, click to zoom in (+). Pan with arrows or hjkl`
                : "Zoom in (+)"}
            >
              {atMax ? <ZoomOutIcon /> : <ZoomInIcon />}
            </button>
          );
        })()}

      {showKeyboardButton && (
        <button
          type="button"
          className="toolbar-icon-btn"
          onClick={onShowKeyboard}
          title="Show keyboard"
        >
          <KeyboardIcon />
        </button>
      )}

      <button
        type="button"
        className="toolbar-icon-btn leave-icon-btn"
        onClick={onLeave}
        title="Leave Session (Esc)"
      >
        <ExitIcon />
      </button>

      {showDrawSettings && (
        <DrawSettingsPopover
          ref={drawSettingsPopoverRef}
          myColor={myColor}
          strokeWidth={strokeWidth}
          onColorChange={onColorChange}
          onStrokeWidthChange={onStrokeWidthChange}
        />
      )}
    </div>
  );
}

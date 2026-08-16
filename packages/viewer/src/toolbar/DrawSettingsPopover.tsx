import { forwardRef } from "react";
import { COLOR_PALETTE } from "./colorConstants";
import { STROKE_WIDTH_OPTIONS } from "./drawConstants";
import "./toolbar.css";

interface DrawSettingsPopoverProps {
  myColor: string | null;
  strokeWidth: number;
  onColorChange: (color: string) => void;
  onStrokeWidthChange: (width: number) => void;
  className?: string;
}

export const DrawSettingsPopover = forwardRef<
  HTMLDivElement,
  DrawSettingsPopoverProps
>(function DrawSettingsPopover(
  { myColor, strokeWidth, onColorChange, onStrokeWidthChange, className },
  ref,
) {
  return (
    <div className={`draw-settings-popover ${className ?? ""}`} ref={ref}>
      <div className="popover-columns">
        <div className="popover-column">
          <div className="popover-section-label">Color</div>
          <div className="popover-color-col">
            {COLOR_PALETTE.map((c) => (
              <span
                key={c}
                className={`color-swatch ${c === myColor ? "active" : ""}`}
                style={{ backgroundColor: c }}
                onClick={() => onColorChange(c)}
              />
            ))}
          </div>
        </div>
        <div className="popover-column">
          <div className="popover-section-label">Size</div>
          <div className="popover-width-col">
            {STROKE_WIDTH_OPTIONS.map((w, i) => (
              <button
                type="button"
                key={w}
                className={`popover-width-btn ${
                  strokeWidth === w ? "active" : ""
                }`}
                onClick={() => onStrokeWidthChange(w)}
                title={["Pencil", "Marker"][i]}
              >
                <span
                  className="popover-width-dot"
                  style={{
                    width: [8, 14][i],
                    height: [8, 14][i],
                    backgroundColor: myColor || "#fff",
                  }}
                />
              </button>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
});

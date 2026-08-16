/** Preset annotation color palette — shared across host overlay, viewer, and server. */
export const COLOR_PALETTE = [
  "#FF3B30",
  "#FF9500",
  "#FFD60A",
  "#34C759",
  "#007AFF",
  "#AF52DE",
  "#00C7BE",
] as const;

/** Default host drawing color (last color in the palette). */
export const HOST_DEFAULT_COLOR = COLOR_PALETTE[COLOR_PALETTE.length - 1];

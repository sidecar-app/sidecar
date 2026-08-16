export interface LineData {
  id: string;
  senderId: string;
  points: number[];
  color: string;
  width: number;
  persist?: boolean;
  finished?: boolean;
}

export interface CursorData {
  id: string;
  x: number;
  y: number;
  color?: string;
  mode?: "d" | "c"; // "d" = draw, "c" = control; drives the cursor glyph
}

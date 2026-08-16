export function extractRoomCode(input: string): string {
  const trimmed = input.trim();
  try {
    const url = new URL(trimmed);
    const room = url.searchParams.get("room");
    if (room) return room;
  } catch {
    const qIdx = trimmed.indexOf("?");
    if (qIdx >= 0) {
      const room = new URLSearchParams(trimmed.slice(qIdx + 1)).get("room");
      if (room) return room;
    }
  }
  return trimmed;
}

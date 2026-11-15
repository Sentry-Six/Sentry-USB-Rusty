// Decides when the connection to the Pi cannot keep up with every camera.
//
// One minute of six cameras is about 265 MB, roughly 35 Mbit/s. A Pi that is
// busy archiving, or a weak Wi-Fi link, delivers less than that; playback
// then stops and starts. Rather than stutter on six pictures, the viewer
// drops to the front camera and says so.

export interface Stall {
  /** When playback resumed, in milliseconds on any steady clock. */
  at: number
  /** How long playback waited for data. */
  ms: number
}

export const STALL_WINDOW_MS = 30_000
export const STALL_BUDGET_MS = 6_000

/** Stalls still inside the window that counts. */
export function recentStalls(stalls: Stall[], now: number): Stall[] {
  return stalls.filter((stall) => now - stall.at <= STALL_WINDOW_MS)
}

/**
 * True when playback has spent too much of the last half minute waiting for
 * data. `waitingMs` is a stall still in progress.
 */
export function needsFewerCameras(stalls: Stall[], now: number, waitingMs = 0): boolean {
  const waited = recentStalls(stalls, now).reduce((sum, stall) => sum + stall.ms, 0) + Math.max(0, waitingMs)
  return waited >= STALL_BUDGET_MS
}

// Where playback is inside the current one-minute file, kept outside React
// state. It changes many times a second; only the few small pieces that show
// it (the clock, the seek bar, the telemetry cards) subscribe, so playing a
// clip no longer redraws the whole page on every tick.

import { useSyncExternalStore } from "react"

export interface PlaybackClock {
  /** Seconds into the current segment. */
  get(): number
  set(seconds: number): void
  subscribe(listener: () => void): () => void
}

export function createPlaybackClock(): PlaybackClock {
  let seconds = 0
  const listeners = new Set<() => void>()
  return {
    get: () => seconds,
    set(next) {
      if (next === seconds) return
      seconds = next
      listeners.forEach((listener) => listener())
    },
    subscribe(listener) {
      listeners.add(listener)
      return () => { listeners.delete(listener) }
    },
  }
}

/**
 * A value worked out from the clock. The component only redraws when that
 * value changes, so `select` should coarsen the time to what is shown: whole
 * seconds for a clock, a fraction of a second for a moving bar. It must
 * return a number, string or boolean.
 */
export function useClockValue<T extends number | string | boolean>(
  clock: PlaybackClock,
  select: (seconds: number) => T,
): T {
  return useSyncExternalStore(clock.subscribe, () => select(clock.get()))
}

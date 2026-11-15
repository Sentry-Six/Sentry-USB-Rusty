// Dragging the timeline asks for a new place many times a second, and every
// place in another minute is six files to load from the Pi. Sent as they
// come, those loads queue up in the browser's few connections, most of them
// for places already left behind, and the cameras end up stuck.
//
// The queue keeps only the newest place asked for, and lets it go once the
// cameras have reached the last place they were sent.

export interface SeekQueue {
  /** Ask for a place. It replaces any place not yet sent. */
  want(target: number): void
  /**
   * Send the wanted place if the cameras are free, or have been busy for
   * longer than the queue's patience. `force` sends it regardless: a click
   * should not wait behind whatever was loading before it.
   */
  pump(now: number, busy: boolean, send: (target: number) => void, force?: boolean): void
  /** True when nothing is waiting to be sent. */
  readonly idle: boolean
}

export function createSeekQueue(patienceMs: number): SeekQueue {
  let wanted: number | null = null
  let sentAt = -Infinity
  return {
    want(target) { wanted = target },
    pump(now, busy, send, force = false) {
      if (wanted === null) return
      if (!force && busy && now - sentAt <= patienceMs) return
      const target = wanted
      wanted = null
      sentAt = now
      send(target)
    },
    get idle() { return wanted === null },
  }
}

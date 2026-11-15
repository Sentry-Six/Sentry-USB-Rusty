// Clock time for dashcam clips.
//
// Tesla names every one-minute file `YYYY-MM-DD_HH-MM-SS-<camera>.mp4` after
// the moment it starts, in the car's local time and with no time zone. That is
// the time the driver saw on the car's screen, so it is the one to show. It is
// kept as "floating" wall-clock time (stored as if it were UTC and formatted
// as UTC): the browser's own time zone must never shift it, or a clip recorded
// on a trip would read hours off when watched at home.

const STAMP = /^(\d{4})-(\d{2})-(\d{2})_(\d{2})-(\d{2})-(\d{2})$/
const DAY = /^(\d{4})-(\d{2})-(\d{2})$/

/**
 * Wall-clock milliseconds for a Tesla timestamp (`2025-02-22_17-58-00`), or
 * null when the text is not one. The result is only meaningful to the
 * formatters in this file.
 */
export function parseClipStamp(stamp: string): number | null {
  const match = STAMP.exec(stamp)
  if (!match) return null
  const [y, mo, d, h, mi, s] = match.slice(1).map(Number)
  const ms = Date.UTC(y, mo - 1, d, h, mi, s)
  // Date.UTC rolls impossible values over (month 13, 25:00); reject those.
  const back = new Date(ms)
  if (
    back.getUTCFullYear() !== y || back.getUTCMonth() !== mo - 1 || back.getUTCDate() !== d ||
    back.getUTCHours() !== h || back.getUTCMinutes() !== mi || back.getUTCSeconds() !== s
  ) return null
  return ms
}

/**
 * The clock time `offsetSec` into the segment that starts at `segmentStamp`.
 *
 * Each segment is anchored to its own file name, never to the first segment
 * plus elapsed playback: segments are not exactly 60 seconds long, and a day
 * of Recent clips has gaps wherever the car was parked or asleep.
 */
export function clipMomentMs(segmentStamp: string, offsetSec: number): number | null {
  const start = parseClipStamp(segmentStamp)
  if (start === null) return null
  const offset = Number.isFinite(offsetSec) && offsetSec > 0 ? offsetSec : 0
  // Positions worked out by adding and subtracting segment lengths land a
  // hair under a whole second (52.99999999 for 53); that is still second 53.
  return start + Math.floor(offset + 0.001) * 1000
}

const formatters = new Map<string, Intl.DateTimeFormat>()

function formatter(kind: string, locale: string | undefined, options: Intl.DateTimeFormatOptions) {
  const key = `${kind}|${locale ?? ""}`
  let found = formatters.get(key)
  if (!found) {
    // `undefined` follows the browser's language and its 12- or 24-hour clock.
    found = new Intl.DateTimeFormat(locale, { ...options, timeZone: "UTC" })
    formatters.set(key, found)
  }
  return found
}

/** `Mon, Nov 9, 2026` */
export function formatClipDay(ms: number, locale?: string): string {
  return formatter("day", locale, { weekday: "short", month: "short", day: "numeric", year: "numeric" }).format(ms)
}

/** `1:45:09 AM`, or `01:45:09` where the browser uses a 24-hour clock. */
export function formatClipClock(ms: number, locale?: string): string {
  return formatter("clock", locale, { hour: "numeric", minute: "2-digit", second: "2-digit" }).format(ms)
}

/**
 * The date in figures, in the order the browser's language uses:
 * `11/09/2026`, `09/11/2026` or `2026-11-09`. (Sentry Studio offers the
 * same three as its Date Format setting.)
 */
export function formatClipDate(ms: number, locale?: string): string {
  return formatter("date", locale, { year: "numeric", month: "2-digit", day: "2-digit" }).format(ms)
}

/** `2026-11-09T01:45:09`, for a `<time dateTime>` attribute. */
export function clipMomentIso(ms: number): string {
  return new Date(ms).toISOString().slice(0, 19)
}

/**
 * Label for a row in the clip list. Events are named for their moment
 * (`2025-02-22_17-58-00` → `Feb 22, 5:58 PM`); Recent clips are grouped by
 * day (`2025-02-22` → `Sat, Feb 22`). Anything else is shown as it is.
 */
export function formatClipListLabel(name: string, locale?: string): string {
  const moment = parseClipStamp(name)
  if (moment !== null) {
    const day = formatter("list-day", locale, { month: "short", day: "numeric" }).format(moment)
    const time = formatter("list-time", locale, { hour: "numeric", minute: "2-digit" }).format(moment)
    return `${day}, ${time}`
  }
  const day = DAY.exec(name)
  if (day) {
    const [y, mo, d] = day.slice(1).map(Number)
    return formatter("list-weekday", locale, { weekday: "short", month: "short", day: "numeric" })
      .format(Date.UTC(y, mo - 1, d))
  }
  return name
}

const EVENT_TIME = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})/

/**
 * Wall-clock milliseconds for the `timestamp` in a clip's event.json
 * (`2026-11-09T14:29:23`, the car's local time), comparable with
 * `parseClipStamp`. Null when the text is not one.
 */
export function parseEventTime(timestamp: string | undefined): number | null {
  const match = EVENT_TIME.exec(timestamp ?? "")
  if (!match) return null
  const [, y, mo, d, h, mi, s] = match
  return parseClipStamp(`${y}-${mo}-${d}_${h}-${mi}-${s}`)
}

export const DEFAULT_SEGMENT_SECONDS = 60

/**
 * How long each segment probably is, from the file names alone, for use
 * until its video has been read. The car starts the next file as one
 * closes, so the time to the next file is right to about a second; before
 * a gap, or at the end, a segment is taken as a full minute. (The rule
 * Sentry Studio uses.)
 */
export function estimateSegmentSeconds(segmentStamps: string[]): number[] {
  const starts = segmentStamps.map(parseClipStamp)
  return starts.map((start, i) => {
    const next = starts[i + 1]
    if (start === null || next === null || next === undefined) return DEFAULT_SEGMENT_SECONDS
    const gap = (next - start) / 1000
    return gap >= 50 && gap <= 62 ? gap : DEFAULT_SEGMENT_SECONDS
  })
}

/** Elapsed time for the player: `3:19`, or `4:25:58` past an hour. */
export function formatElapsed(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "0:00"
  const h = Math.floor(seconds / 3600)
  const m = Math.floor((seconds % 3600) / 60)
  const sec = Math.floor(seconds % 60).toString().padStart(2, "0")
  // A day of Recent clips runs for hours; 4:25:58 reads better than 265:58.
  return h > 0 ? `${h}:${m.toString().padStart(2, "0")}:${sec}` : `${m}:${sec}`
}

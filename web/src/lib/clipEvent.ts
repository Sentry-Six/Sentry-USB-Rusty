// What a Sentry or Saved clip's event.json says about the event.

import { parseClipStamp } from "@/lib/clipTime"

/**
 * The camera that triggered an event. Tesla writes it as a number; several
 * numbers are front-facing cameras that are saved as the one "front" file.
 * (The table Sentry Studio uses.)
 */
const EVENT_CAMERAS: Record<string, string> = {
  "0": "front",
  "1": "front",
  "2": "front",
  "3": "left_pillar",
  "4": "right_pillar",
  "5": "left_repeater",
  "6": "right_repeater",
  "7": "back",
}

/** Camera name for event.json's `camera`, or null when it names none we know. */
export function eventCamera(index: string | number | undefined | null): string | null {
  if (index === undefined || index === null) return null
  return EVENT_CAMERAS[String(index).trim()] ?? null
}

export interface EventPosition {
  /** Index of the segment the event happened in. */
  segment: number
  /** Seconds into that segment. */
  offset: number
  /** Seconds from the start of the whole clip. */
  global: number
}

// The event can be stamped a moment after the last file closes.
const END_SLACK_SECONDS = 5

/**
 * Where on the clip's timeline an event happened, or null when it falls
 * outside the footage (before it, in a gap, or well after it ends).
 *
 * Found from the event's own segment, not as time since the first file:
 * segments are not exactly a minute long, and the timeline skips any gaps.
 */
export function eventPosition(
  eventMs: number | null,
  segmentStamps: string[],
  segmentSeconds: number[],
): EventPosition | null {
  if (eventMs === null) return null
  let before = 0
  let found: EventPosition | null = null
  for (let i = 0; i < segmentStamps.length; i++) {
    const start = parseClipStamp(segmentStamps[i])
    const length = segmentSeconds[i] ?? 0
    if (start !== null && eventMs >= start) {
      const offset = (eventMs - start) / 1000
      if (offset <= length) found = { segment: i, offset, global: before + offset }
      else if (offset <= length + END_SLACK_SECONDS) found = { segment: i, offset: length, global: before + length }
      else found = null
    }
    before += length
  }
  return found
}

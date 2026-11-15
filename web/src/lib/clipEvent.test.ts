import assert from "node:assert/strict"
import test from "node:test"
import { eventCamera, eventPosition } from "./clipEvent.ts"
import { estimateSegmentSeconds, parseEventTime } from "./clipTime.ts"

test("event camera numbers map to the cameras Sentry Studio uses", () => {
  assert.equal(eventCamera("0"), "front")
  assert.equal(eventCamera("1"), "front")
  assert.equal(eventCamera("2"), "front")
  assert.equal(eventCamera("3"), "left_pillar")
  assert.equal(eventCamera("4"), "right_pillar")
  assert.equal(eventCamera("5"), "left_repeater")
  assert.equal(eventCamera("6"), "right_repeater")
  assert.equal(eventCamera("7"), "back")
  // Written as a number, or with stray spaces.
  assert.equal(eventCamera(0), "front")
  assert.equal(eventCamera(" 7 "), "back")
  for (const unknown of ["9", "", "front", undefined, null]) assert.equal(eventCamera(unknown), null)
})

test("an event's time is read as the car's wall-clock time", () => {
  const ms = parseEventTime("2026-11-09T14:29:23")
  assert.equal(new Date(ms!).toISOString(), "2026-11-09T14:29:23.000Z")
  for (const bad of [undefined, "", "2026-11-09", "yesterday", "2026-13-40T99:00:00"]) assert.equal(parseEventTime(bad), null)
})

test("segment lengths are estimated from the gap to the next file", () => {
  assert.deepEqual(
    estimateSegmentSeconds(["2026-11-09_14-19-41", "2026-11-09_14-20-41", "2026-11-09_14-21-42", "2026-11-09_14-22-42"]),
    [60, 61, 60, 60],
  )
  // Before a gap (the car slept), a file that ran short, and the last file: a full minute.
  assert.deepEqual(
    estimateSegmentSeconds(["2026-11-12_08-14-47", "2026-11-12_08-18-49", "2026-11-12_08-19-20", "2026-11-12_08-20-13"]),
    [60, 60, 53, 60],
  )
  assert.deepEqual(estimateSegmentSeconds(["first", "second"]), [60, 60])
  assert.deepEqual(estimateSegmentSeconds([]), [])
})

const stamps = ["2026-11-09_14-27-43", "2026-11-09_14-28-44", "2026-11-09_14-29-44"]

test("an event is placed in its own segment on the timeline", () => {
  const lengths = [60.2, 59.9, 37]
  // 14:29:23 is 39 seconds into the file that starts 14:28:44.
  assert.deepEqual(eventPosition(parseEventTime("2026-11-09T14:29:23"), stamps, lengths), { segment: 1, offset: 39, global: 60.2 + 39 })
  assert.deepEqual(eventPosition(parseEventTime("2026-11-09T14:27:43"), stamps, lengths), { segment: 0, offset: 0, global: 0 })
  assert.deepEqual(eventPosition(parseEventTime("2026-11-09T14:30:10"), stamps, lengths), { segment: 2, offset: 26, global: 60.2 + 59.9 + 26 })
})

test("an event just after the footage ends sits at the end; one outside it has no place", () => {
  const lengths = [60, 60, 37]
  assert.deepEqual(eventPosition(parseEventTime("2026-11-09T14:30:24"), stamps, lengths), { segment: 2, offset: 37, global: 157 })
  assert.equal(eventPosition(parseEventTime("2026-11-09T14:31:30"), stamps, lengths), null)
  assert.equal(eventPosition(parseEventTime("2026-11-09T14:20:00"), stamps, lengths), null)
  assert.equal(eventPosition(null, stamps, lengths), null)
  assert.equal(eventPosition(parseEventTime("2026-11-09T14:29:23"), [], []), null)
})

test("an event in a gap between files has no place", () => {
  const gappy = ["2026-11-12_08-14-47", "2026-11-12_08-18-49"]
  assert.equal(eventPosition(parseEventTime("2026-11-12T08:16:30"), gappy, [60, 60]), null)
  assert.deepEqual(eventPosition(parseEventTime("2026-11-12T08:18:59"), gappy, [60, 60]), { segment: 1, offset: 10, global: 70 })
})

import assert from "node:assert/strict"
import test from "node:test"
import {
  clipMomentIso,
  clipMomentMs,
  formatClipClock,
  formatClipDate,
  formatClipDay,
  formatClipListLabel,
  parseClipStamp,
} from "./clipTime.ts"

// Intl separates the time from AM/PM with a narrow no-break space.
const NARROW_SPACES = new RegExp(`[${String.fromCharCode(0x202f, 0xa0)}]`, "g")
const plain = (text: string) => text.replace(NARROW_SPACES, " ")

test("a Tesla timestamp is read as the car's wall-clock time", () => {
  const ms = parseClipStamp("2026-11-09_14-19-41")
  assert.notEqual(ms, null)
  assert.equal(clipMomentIso(ms!), "2026-11-09T14:19:41")
})

test("text that is not a Tesla timestamp is rejected", () => {
  for (const bad of ["", "first", "2026-11-09", "2026-11-09_14-19", "2026-13-09_14-19-41", "2026-02-30_10-00-00", "2026-11-09_25-00-00", "2026-11-09_14-19-41-front"]) {
    assert.equal(parseClipStamp(bad), null, bad)
    assert.equal(clipMomentMs(bad, 5), null, bad)
  }
})

test("the moment on screen is the segment's own start plus the time into it", () => {
  assert.equal(clipMomentIso(clipMomentMs("2026-11-09_14-19-41", 0)!), "2026-11-09T14:19:41")
  assert.equal(clipMomentIso(clipMomentMs("2026-11-09_14-19-41", 19.9)!), "2026-11-09T14:20:00")
  assert.equal(clipMomentIso(clipMomentMs("2026-11-09_14-19-41", 59.96)!), "2026-11-09T14:20:40")
  // The next file starts 61 seconds after the last one, not 60.
  assert.equal(clipMomentIso(clipMomentMs("2026-11-09_14-20-41", 0)!), "2026-11-09T14:20:41")
  assert.equal(clipMomentIso(clipMomentMs("2026-11-09_14-21-42", 0.5)!), "2026-11-09T14:21:42")
})

test("a position a hair under a whole second counts as that second", () => {
  // A position found by adding up file lengths and subtracting them again
  // can come back as 52.99999999999994 instead of 53.
  assert.equal(clipMomentIso(clipMomentMs("2026-11-14_09-36-06", 52.99999999999994)!), "2026-11-14T09:36:59")
  assert.equal(clipMomentIso(clipMomentMs("2026-11-14_09-36-06", 52.99)!), "2026-11-14T09:36:58")
})

test("a gap between Recent segments shows the later segment's real time", () => {
  // The car slept between these two files of the same day.
  assert.equal(clipMomentIso(clipMomentMs("2026-11-12_08-19-50", 59)!), "2026-11-12T08:20:49")
  assert.equal(clipMomentIso(clipMomentMs("2026-11-12_09-02-07", 3)!), "2026-11-12T09:02:10")
})

test("time into a segment carries over midnight and the end of a year", () => {
  assert.equal(clipMomentIso(clipMomentMs("2026-12-31_23-59-30", 45)!), "2027-01-01T00:00:15")
})

test("odd playback positions never move the clock backwards", () => {
  for (const offset of [-3, Number.NaN, Number.POSITIVE_INFINITY]) {
    assert.equal(clipMomentIso(clipMomentMs("2026-11-09_14-19-41", offset)!), "2026-11-09T14:19:41")
  }
})

test("the browser's time zone does not shift the car's time", () => {
  const original = process.env.TZ
  try {
    for (const zone of ["Pacific/Auckland", "America/Edmonton", "UTC"]) {
      process.env.TZ = zone
      const ms = clipMomentMs("2026-11-09_01-45-09", 30)!
      assert.equal(plain(formatClipClock(ms, "en-US")), "1:45:39 AM", zone)
      assert.equal(formatClipDay(ms, "en-US"), "Mon, Nov 9, 2026", zone)
    }
  } finally {
    if (original === undefined) delete process.env.TZ
    else process.env.TZ = original
  }
})

test("the clock follows the language's 12- or 24-hour habit", () => {
  const ms = clipMomentMs("2026-11-09_14-19-41", 0)!
  assert.equal(plain(formatClipClock(ms, "en-US")), "2:19:41 PM")
  assert.equal(formatClipClock(ms, "en-GB"), "14:19:41")
  assert.equal(formatClipClock(ms, "de-DE"), "14:19:41")
  assert.equal(formatClipDay(ms, "en-GB"), "Mon, 9 Nov 2026")
})

test("the date in figures follows the language's order", () => {
  const ms = clipMomentMs("2026-11-09_14-19-41", 0)!
  assert.equal(formatClipDate(ms, "en-US"), "11/09/2026")
  assert.equal(formatClipDate(ms, "en-GB"), "09/11/2026")
  assert.equal(formatClipDate(ms, "en-CA"), "2026-11-09")
})

test("clip list labels: events by moment, Recent by day, anything else as is", () => {
  assert.equal(plain(formatClipListLabel("2025-02-22_17-58-00", "en-US")), "Feb 22, 5:58 PM")
  assert.equal(formatClipListLabel("2025-02-22_17-58-00", "en-GB"), "22 Feb, 17:58")
  assert.equal(formatClipListLabel("2025-02-22", "en-US"), "Sat, Feb 22")
  assert.equal(formatClipListLabel("first"), "first")
})

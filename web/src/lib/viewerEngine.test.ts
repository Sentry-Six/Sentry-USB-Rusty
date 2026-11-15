import assert from "node:assert/strict"
import test from "node:test"
import { fitCameraGrid } from "./cameraGrid.ts"
import { createPlaybackClock } from "./playbackClock.ts"
import { needsFewerCameras, recentStalls } from "./streamHealth.ts"

const HW4 = 2896 / 1876

test("six cameras sit three across on a wide screen and two across on an upright phone", () => {
  const desktop = fitCameraGrid({ width: 1400, height: 760 }, 6, HW4, 4)
  assert.deepEqual([desktop.cols, desktop.rows], [3, 2])
  const phone = fitCameraGrid({ width: 343, height: 520 }, 6, HW4, 4)
  assert.deepEqual([phone.cols, phone.rows], [2, 3])
})

test("a desktop window keeps three across even when two across would be slightly bigger", () => {
  // 1192 x 828: two across gives 423 px pictures, three across 395 px.
  const fit = fitCameraGrid({ width: 1192, height: 828 }, 6, HW4, 3)
  assert.deepEqual([fit.cols, fit.rows], [3, 2])
  assert.ok(fit.width > 1180)
})

test("the grid fits inside the space and keeps each camera's true shape", () => {
  for (const area of [{ width: 1400, height: 760 }, { width: 343, height: 520 }, { width: 900, height: 300 }, { width: 300, height: 900 }]) {
    for (const count of [1, 4, 6]) {
      const fit = fitCameraGrid(area, count, HW4, 4)
      assert.ok(fit.width <= area.width && fit.height <= area.height, JSON.stringify({ area, count, fit }))
      const tileWidth = (fit.width - 4 * (fit.cols - 1)) / fit.cols
      const tileHeight = (fit.height - 4 * (fit.rows - 1)) / fit.rows
      assert.ok(Math.abs(tileWidth / tileHeight - HW4) < 0.02, JSON.stringify({ area, count, fit }))
      assert.ok(fit.cols * fit.rows >= count)
    }
  }
})

test("the grid uses the space well: wide screens are limited by height, phones by width", () => {
  const wide = fitCameraGrid({ width: 1400, height: 600 }, 6, HW4, 4)
  assert.ok(wide.height > 590)
  const phone = fitCameraGrid({ width: 343, height: 700 }, 6, HW4, 4)
  assert.ok(phone.width > 335)
})

test("four cameras (older cars) make a two by two grid; one camera fills the space", () => {
  const four = fitCameraGrid({ width: 1200, height: 700 }, 4, 4 / 3, 4)
  assert.deepEqual([four.cols, four.rows], [2, 2])
  const one = fitCameraGrid({ width: 1200, height: 700 }, 1, HW4, 4)
  assert.deepEqual([one.cols, one.rows], [1, 1])
  assert.ok(one.height > 690)
})

test("an unmeasured space still names a layout", () => {
  assert.deepEqual(fitCameraGrid({ width: 0, height: 0 }, 6, HW4, 4), { cols: 3, rows: 2, width: 0, height: 0 })
  assert.deepEqual(fitCameraGrid({ width: 0, height: 0 }, 4, HW4, 4), { cols: 3, rows: 2, width: 0, height: 0 })
  assert.deepEqual(fitCameraGrid({ width: 40, height: 0 }, 1, HW4, 4), { cols: 1, rows: 1, width: 0, height: 0 })
})

test("the playback clock tells its listeners only when the time changes", () => {
  const clock = createPlaybackClock()
  let calls = 0
  const stop = clock.subscribe(() => { calls++ })
  clock.set(1.5)
  clock.set(1.5)
  clock.set(2)
  assert.equal(calls, 2)
  assert.equal(clock.get(), 2)
  stop()
  clock.set(3)
  assert.equal(calls, 2)
})

test("a few short pauses are fine; waiting for much of half a minute is not", () => {
  const now = 100_000
  assert.equal(needsFewerCameras([], now), false)
  assert.equal(needsFewerCameras([{ at: now - 5000, ms: 800 }, { at: now - 1000, ms: 1200 }], now), false)
  assert.equal(needsFewerCameras([{ at: now - 20_000, ms: 2500 }, { at: now - 9000, ms: 2000 }, { at: now, ms: 1600 }], now), true)
  // A stall still going on counts too.
  assert.equal(needsFewerCameras([{ at: now - 4000, ms: 2000 }], now, 4500), true)
})

test("old stalls stop counting", () => {
  const now = 100_000
  const stalls = [{ at: now - 45_000, ms: 9000 }, { at: now - 2000, ms: 500 }]
  assert.deepEqual(recentStalls(stalls, now), [{ at: now - 2000, ms: 500 }])
  assert.equal(needsFewerCameras(stalls, now), false)
})

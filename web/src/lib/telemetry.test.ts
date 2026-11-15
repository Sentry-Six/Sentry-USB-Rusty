import assert from "node:assert/strict"
import test from "node:test"
import { accelPercent } from "./telemetry.ts"

test("a pedal position written as a percentage is used as it is", () => {
  // Values a real drive produced: a light press, not a floored pedal.
  assert.equal(accelPercent(20.4), 20.4)
  assert.equal(accelPercent(3.5), 3.5)
  assert.equal(accelPercent(100), 100)
  assert.equal(accelPercent(140), 100)
})

test("a pedal position written as a share is turned into a percentage", () => {
  assert.equal(accelPercent(0.25), 25)
  assert.equal(accelPercent(1), 100)
})

test("no press, or no reading, is zero", () => {
  for (const none of [0, -0.3, undefined, null, Number.NaN]) assert.equal(accelPercent(none), 0)
})

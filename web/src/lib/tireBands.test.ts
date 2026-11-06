import assert from "node:assert/strict"
import test from "node:test"

import {
  DEFAULT_TIRE_BANDS,
  classifyTirePsi,
  formatTireBands,
  isDefaultTireBands,
  parseTireBands,
  tireBandsValid,
  tireChartDomainPsi,
} from "./tireBands.ts"

test("unset or malformed bands fall back to the defaults", () => {
  for (const raw of [undefined, null, "", "36,45", "28,36,45,x", "50,45,36,28", "28,36,36,50", "1,36,45,50", "28,36,45,500"]) {
    assert.deepEqual(parseTireBands(raw), DEFAULT_TIRE_BANDS, String(raw))
  }
})

test("saved bands round-trip, including decimals", () => {
  const cybertruck = { unsafeLow: 40, optimalLow: 46, optimalHigh: 55, unsafeHigh: 66 }
  assert.deepEqual(parseTireBands(formatTireBands(cybertruck)), cybertruck)
  assert.deepEqual(parseTireBands(" 28.5, 36 ,45.2,50 "), { unsafeLow: 28.5, optimalLow: 36, optimalHigh: 45.2, unsafeHigh: 50 })
  assert.equal(formatTireBands({ unsafeLow: 28.04, optimalLow: 36, optimalHigh: 45, unsafeHigh: 50.06 }), "28,36,45,50.1")
  assert.ok(isDefaultTireBands(parseTireBands("28,36,45,50")))
})

test("validity needs four strictly ascending values in a plausible range", () => {
  assert.ok(tireBandsValid(DEFAULT_TIRE_BANDS))
  assert.ok(!tireBandsValid({ ...DEFAULT_TIRE_BANDS, optimalHigh: 36 }))
  assert.ok(!tireBandsValid({ ...DEFAULT_TIRE_BANDS, unsafeHigh: Number.NaN }))
})

test("classification follows the bands, with the edges inside the optimal range", () => {
  const b = DEFAULT_TIRE_BANDS
  assert.equal(classifyTirePsi(42, b), "optimal")
  assert.equal(classifyTirePsi(36, b), "optimal")
  assert.equal(classifyTirePsi(45, b), "optimal")
  assert.equal(classifyTirePsi(47, b), "check")
  assert.equal(classifyTirePsi(30, b), "check")
  assert.equal(classifyTirePsi(27, b), "unsafe")
  assert.equal(classifyTirePsi(51, b), "unsafe")
  // A Cybertruck at its 50 PSI spec is fine with matching bands.
  assert.equal(classifyTirePsi(50, { unsafeLow: 40, optimalLow: 46, optimalHigh: 55, unsafeHigh: 66 }), "optimal")
})

test("the chart range follows the bands", () => {
  assert.deepEqual(tireChartDomainPsi(DEFAULT_TIRE_BANDS), [20, 55])
  assert.deepEqual(tireChartDomainPsi({ unsafeLow: 40, optimalLow: 46, optimalHigh: 55, unsafeHigh: 66 }), [32, 71])
})

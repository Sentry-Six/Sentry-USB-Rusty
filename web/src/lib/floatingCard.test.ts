import assert from "node:assert/strict"
import test from "node:test"
import {
  clampPoint,
  fromPlacement,
  loadPlacement,
  parsePlacement,
  savePlacement,
  toPlacement,
} from "./floatingCard.ts"

const desktop = { width: 960, height: 540 }
const phone = { width: 340, height: 600 }
const card = { width: 240, height: 56 }

test("a card cannot be dragged out of its area", () => {
  assert.deepEqual(clampPoint({ x: -40, y: -5 }, desktop, card), { x: 0, y: 0 })
  assert.deepEqual(clampPoint({ x: 5000, y: 5000 }, desktop, card), { x: 720, y: 484 })
  assert.deepEqual(clampPoint({ x: 100, y: 200 }, desktop, card), { x: 100, y: 200 })
})

test("a card wider than its area stays against the left edge", () => {
  assert.deepEqual(clampPoint({ x: 30, y: 10 }, { width: 200, height: 40 }, card), { x: 0, y: 0 })
})

test("a place survives a change of screen size", () => {
  const corner = toPlacement({ x: 720, y: 484 }, desktop, card)
  assert.deepEqual(corner, { x: 1, y: 1 })
  // Bottom right on a desktop is still bottom right on a phone.
  assert.deepEqual(fromPlacement(corner, phone, card), { x: 100, y: 544 })

  const middle = toPlacement({ x: 360, y: 121 }, desktop, card)
  const onPhone = fromPlacement(middle, phone, card)
  assert.deepEqual(onPhone, { x: 50, y: 136 })
  assert.deepEqual(clampPoint(onPhone, phone, card), onPhone)
})

test("a place round-trips at the same size", () => {
  for (const point of [{ x: 0, y: 0 }, { x: 13, y: 401 }, { x: 720, y: 484 }]) {
    assert.deepEqual(fromPlacement(toPlacement(point, desktop, card), desktop, card), point)
  }
})

test("an area with no room to move puts the card in the middle of nothing", () => {
  assert.deepEqual(toPlacement({ x: 0, y: 0 }, card, card), { x: 0.5, y: 0.5 })
  assert.deepEqual(fromPlacement({ x: 0.5, y: 0.5 }, card, card), { x: 0, y: 0 })
})

test("saved text that is not a place is ignored", () => {
  for (const bad of [null, undefined, "", "nope", "null", "[]", "{}", '{"x":"1","y":0}', '{"x":null,"y":0}', '{"x":1e999,"y":0}']) {
    assert.equal(parsePlacement(bad), null, String(bad))
  }
  assert.deepEqual(parsePlacement('{"x":0.25,"y":1}'), { x: 0.25, y: 1 })
  // Out-of-range numbers are pulled back inside rather than thrown away.
  assert.deepEqual(parsePlacement('{"x":-3,"y":7}'), { x: 0, y: 1 })
})

function withStorage(storage: unknown, run: () => void) {
  const original = Object.getOwnPropertyDescriptor(globalThis, "localStorage")
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: storage })
  try { run() } finally {
    if (original) Object.defineProperty(globalThis, "localStorage", original)
    else Reflect.deleteProperty(globalThis, "localStorage")
  }
}

test("a place is remembered and can be forgotten", () => {
  const items = new Map<string, string>()
  const storage = {
    getItem: (key: string) => items.get(key) ?? null,
    setItem: (key: string, value: string) => { items.set(key, value) },
    removeItem: (key: string) => { items.delete(key) },
  }
  withStorage(storage, () => {
    assert.equal(loadPlacement("card"), null)
    savePlacement("card", { x: 1 / 3, y: 1 })
    assert.equal(items.get("card"), '{"x":0.3333,"y":1}')
    assert.deepEqual(loadPlacement("card"), { x: 0.3333, y: 1 })
    savePlacement("card", null)
    assert.equal(loadPlacement("card"), null)
  })
})

test("blocked storage does not break the card", () => {
  const blocked = {
    getItem: () => { throw new Error("blocked") },
    setItem: () => { throw new Error("blocked") },
    removeItem: () => { throw new Error("blocked") },
  }
  withStorage(blocked, () => {
    assert.equal(loadPlacement("card"), null)
    savePlacement("card", { x: 0, y: 0 })
    savePlacement("card", null)
  })
})

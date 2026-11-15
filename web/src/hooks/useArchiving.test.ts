import assert from "node:assert/strict"
import test from "node:test"
import { roughWait } from "./useArchiving.ts"

test("a wait is given in plain, rounded words", () => {
  assert.equal(roughWait(20), "about 1 minute")
  assert.equal(roughWait(95), "about 2 minutes")
  assert.equal(roughWait(45 * 60), "about 45 minutes")
  assert.equal(roughWait(60 * 60), "about 1 hour")
  assert.equal(roughWait(4275), "about 1 hour 10 minutes")
  assert.equal(roughWait(118 * 60), "about 2 hours")
  assert.equal(roughWait(3 * 3600 + 34 * 60), "about 3 hours 30 minutes")
})

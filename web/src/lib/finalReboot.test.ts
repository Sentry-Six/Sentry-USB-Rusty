import assert from 'node:assert/strict'
import test from 'node:test'
import { createFinalRebootWatch } from './finalReboot.ts'

function serve(replies: Array<object | Error>) {
  globalThis.fetch = async () => {
    const reply = replies.shift()
    if (reply instanceof Error) throw reply
    return Response.json(reply)
  }
}

test('a new boot ID ends the wait even when no poll saw the Pi offline', async () => {
  const original = globalThis.fetch
  try {
    serve([{ boot_id: 'a' }, { boot_id: 'a' }, { boot_id: 'b' }])
    const rebooted = createFinalRebootWatch({ bootIdBefore: 'a' })
    assert.equal(await rebooted(), false)
    assert.equal(await rebooted(), false)
    assert.equal(await rebooted(), true)
  } finally { globalThis.fetch = original }
})

test('a timed-out poll alone does not end the wait while the boot ID is unchanged', async () => {
  const original = globalThis.fetch
  try {
    serve([new Error('timeout'), { boot_id: 'a' }, { boot_id: 'b' }])
    const rebooted = createFinalRebootWatch({ bootIdBefore: 'a' })
    assert.equal(await rebooted(), false)
    assert.equal(await rebooted(), false, 'slow reply before the reboot is not a reboot')
    assert.equal(await rebooted(), true)
  } finally { globalThis.fetch = original }
})

test('without a starting boot ID the first answer is the baseline', async () => {
  const original = globalThis.fetch
  try {
    serve([{ boot_id: 'a' }, { boot_id: 'b' }])
    const rebooted = createFinalRebootWatch()
    assert.equal(await rebooted(), false)
    assert.equal(await rebooted(), true)
  } finally { globalThis.fetch = original }
})

test('servers without a boot ID fall back to seeing the Pi go offline', async () => {
  const original = globalThis.fetch
  try {
    serve([{ setup_finished: true }, new Error('offline'), { setup_finished: true }])
    const rebooted = createFinalRebootWatch()
    assert.equal(await rebooted(), false)
    assert.equal(await rebooted(), false)
    assert.equal(await rebooted(), true)
  } finally { globalThis.fetch = original }
})

test('a missed reboot still ends the wait once the time limit has passed', async () => {
  const original = globalThis.fetch
  let clock = 0
  try {
    serve([{ boot_id: 'b' }, new Error('offline'), { boot_id: 'b' }])
    const rebooted = createFinalRebootWatch({ maxWaitMs: 1000, now: () => clock })
    assert.equal(await rebooted(), false)
    clock = 2000
    assert.equal(await rebooted(), false, 'no answer is never a finish')
    assert.equal(await rebooted(), true)
  } finally { globalThis.fetch = original }
})

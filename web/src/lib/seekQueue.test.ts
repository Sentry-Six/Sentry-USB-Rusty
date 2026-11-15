import assert from 'node:assert/strict'
import test from 'node:test'
import { createSeekQueue } from './seekQueue.ts'

test('a drag sends one place at a time, and only the newest', () => {
  const queue = createSeekQueue(1500)
  const sent: number[] = []
  const send = (target: number) => sent.push(target)

  // The press goes out at once, whatever was loading.
  queue.want(100)
  queue.pump(0, true, send, true)
  assert.deepEqual(sent, [100])
  assert.ok(queue.idle)

  // Thirty movements while the cameras load: nothing more is sent.
  for (let i = 1; i <= 30; i++) {
    queue.want(100 + i * 60)
    queue.pump(i * 33, true, send)
  }
  assert.deepEqual(sent, [100])
  assert.ok(!queue.idle)

  // The cameras arrive: the newest place goes, the ones between are skipped.
  queue.pump(1000, false, send)
  assert.deepEqual(sent, [100, 1900])
  assert.ok(queue.idle)

  // Nothing wanted, nothing sent.
  queue.pump(1100, false, send)
  assert.deepEqual(sent, [100, 1900])
})

test('a load that never finishes does not hold the timeline for ever', () => {
  const queue = createSeekQueue(1500)
  const sent: number[] = []
  const send = (target: number) => sent.push(target)
  queue.want(10)
  queue.pump(0, false, send)
  queue.want(20)
  queue.pump(1500, true, send)
  assert.deepEqual(sent, [10])
  queue.pump(1501, true, send)
  assert.deepEqual(sent, [10, 20])
})

import assert from 'node:assert/strict'
import test from 'node:test'
import { boundLogWindow } from './logBuffer.ts'

test('live trimming moves older cursor by UTF-8 bytes without losing the adjacent page', () => {
  const lines = Array.from({ length: 2500 }, (_, index) => `${index} 🚗 café\n`)
  const initialStart = 127
  const window = boundLogWindow(lines.join(''), initialStart)
  assert.equal(window.content, lines.slice(500).join(''))
  assert.equal(window.start, initialStart + new TextEncoder().encode(lines.slice(0, 500).join('')).length)
  const older = boundLogWindow(lines.slice(0, 500).join('') + window.content, initialStart, 'oldest')
  assert.equal(older.content, lines.slice(0, 2000).join(''))
  assert.equal(older.start, initialStart)
})
test('byte limit never slices a UTF-8 codepoint and advances only a dropped prefix', () => {
  const content = '🚗'.repeat(10) + 'a'
  const newest = boundLogWindow(content, 100, 'newest', 2000, 10)
  assert.equal(newest.content, '🚗🚗a')
  assert.equal(newest.start, 132)
  const oldest = boundLogWindow(content, 100, 'oldest', 2000, 10)
  assert.equal(oldest.content, '🚗🚗')
  assert.equal(oldest.start, 100)
})

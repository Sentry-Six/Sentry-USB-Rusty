import assert from 'node:assert/strict'
import test from 'node:test'
import { parseLine } from './logParser.ts'

test('explicit archive logger levels control classification and presentation', () => {
  for (const [prefix, message, expected] of [
    [' INFO', 'RecentClips filter: scanned 8, reused 283; reading front.mp4', 'info'],
    ['INFO', 'RecentClips filter complete: keeping 243 candidates, skipping 2046', 'info'],
    ['WARN', 'RecentClips filter: cannot read front.mp4; retrying later', 'warning'],
    ['ERROR', 'RecentClips filter: database unavailable', 'error'],
    ['DEBUG', 'RecentClips filter: no SEI gear data; skipping this recording', 'debug'],
    ['TRACE', 'RecentClips filter: reading frame', 'debug'],
  ]) {
    const raw = `Sun  8 Nov 12:10:53 MST 2026: ${prefix} ${message}`
    const parsed = parseLine(raw)
    assert.equal(parsed.level, expected)
    assert.equal(parsed.message, message)
    assert.equal(parsed.time, '12:10:53')
    assert.equal(parsed.raw, raw)
  }
})

test('legacy shell logs retain keyword classification', () => {
  for (const [message, level] of [
    ['Checking RecentClips driving metadata...', 'info'],
    ['Finished checking RecentClips driving metadata.', 'success'],
    ['Archive transfer failed', 'error'],
    ['Archive unreachable; retrying', 'warning'],
    ['[away-mode] Starting archive', 'warning'],
    ['Some message mentioning INFO in the middle', 'default'],
  ]) assert.equal(parseLine(`Sun  8 Nov 12:10:53 MST 2026: ${message}`).level, level)
})

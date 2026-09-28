import assert from 'node:assert/strict'
import test from 'node:test'
import { normalizeLegacyStorage, needsLegacyStorageProbe, type HealthReport } from './healthReport.ts'
const total = 1024 ** 4
const reserve = { mounted: true, total_bytes: total, available_bytes: total * .054 }
function report(detail = '5.4% free'): HealthReport {
  return { summary: '1 problem found', categories: [{ name: 'Storage', items: [
    { name: 'Backingfiles free space', status: 'warn', detail },
    { name: 'Automatic storage cleanup', status: 'pass' }, { name: 'Clip index capacity', status: 'pass' },
  ] }] }
}
test('legacy percentage-only warning is informational when independently measured reserve is available', () => {
  const original = report()
  const normalized = normalizeLegacyStorage(original, reserve)
  assert.equal(normalized.categories[0].items[0].status, 'info')
  assert.equal(normalized.categories[0].items[0].detail, '5.4% free')
  assert.ok(normalized.categories[0].items[0].explanation?.includes('Original device check: warn'))
  assert.equal(normalized.summary, 'No actionable issues reported')
  assert.equal(original.categories[0].items[0].status, 'warn')
})
test('full, under-reserve, unmounted, malformed and unavailable readings retain the original warning', () => {
  for (const probe of [null, {}, { ...reserve, mounted: false }, { ...reserve, available_bytes: 0 }, { ...reserve, available_bytes: 1024 ** 3 }, { ...reserve, total_bytes: NaN }]) {
    const original = report()
    assert.equal(normalizeLegacyStorage(original, probe), original)
  }
  for (const detail of ['0.0% free', 'read-only filesystem', '5.4% free; cleanup failed']) {
    const original = report(detail)
    assert.equal(normalizeLegacyStorage(original, reserve), original)
  }
})
test('real storage faults and modern storage results always retain authority', () => {
  for (const name of ['Automatic storage cleanup', 'Clip index capacity', 'Mount read-only']) {
    const original = report()
    original.categories[0].items.push({ name, status: 'fail', detail: 'actual fault' })
    assert.equal(normalizeLegacyStorage(original, reserve), original)
  }
  const modern = report()
  modern.categories[0].items.push({ name: 'Recording storage', status: 'warn', detail: 'Cleanup cannot restore space' })
  assert.equal(needsLegacyStorageProbe(modern), false)
  assert.equal(normalizeLegacyStorage(modern, reserve), modern)
})
test('normalizing legacy storage preserves other actionable findings and summary', () => {
  const original = report()
  original.categories.push({ name: 'Hardware', items: [{ name: 'Temperature', status: 'fail', detail: 'overheating' }] })
  const normalized = normalizeLegacyStorage(original, reserve)
  assert.equal(normalized.summary, '1 issue needs attention')
  assert.equal(normalized.categories[1].items[0].status, 'fail')
})

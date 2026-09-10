import test from 'node:test'
import assert from 'node:assert/strict'
import { applyRateChanges, loadRates, parseRates, saveRateChanges } from './charging-rate-editor.ts'

const schedule = { label: '  Night  ', start: '22:00', end: '06:00', days: ['1', '2'], startMonth: '1', endMonth: '12', rate: '0.12', extension: { keep: true } }
test('no-op preserves legacy strings, empty plans and schedule representations', () => {
  const doc = { charging_default_rate: '0.20', charging_tag_rates: JSON.stringify({ Home: { flat: '0.3', schedules: [schedule], future: true }, Empty: {}, Work: '0.4' }) }
  assert.deepEqual(applyRateChanges(doc, parseRates(doc)), doc)
  const next = parseRates(doc)
  next.currency = 'CAD'
  const changed = applyRateChanges(doc, next)
  assert.deepEqual(changed, { ...doc, charging_currency: 'CAD' })
})
test('flat edits preserve schedules and unrelated plans including unreadable schedules', () => {
  const doc = { charging_tag_rates: { Home: { flat: '0.3', schedules: [schedule, { unknown: true }], future: true }, Work: '0.4' } }
  const next = parseRates(doc)
  next.tags.Home.flat = 0.5
  const result = applyRateChanges(doc, next)
  assert.deepEqual(result, { charging_tag_rates: { ...doc.charging_tag_rates, Home: { ...doc.charging_tag_rates.Home, flat: 0.5 } } })
  next.tags.Home.schedules = []
  assert.throws(() => applyRateChanges(doc, next), /could not be read/)
})
test('schedule changes preserve unknown fields and move their original identity', () => {
  const doc = { charging_tag_rates: { Home: { flat: 0.2, schedules: [schedule, { ...schedule, label: 'Other', extension: 2 }] } } }
  const next = parseRates(doc)
  next.tags.Home.schedules.reverse()
  next.tags.Home.schedules[1].rate = 0.15
  const result = applyRateChanges(doc, next) as typeof doc
  assert.deepEqual(result.charging_tag_rates.Home.schedules, [doc.charging_tag_rates.Home.schedules[1], { ...schedule, rate: 0.15 }])
  next.tags.Home.schedules.push(next.tags.Home.schedules[0])
  assert.throws(() => applyRateChanges(doc, next), /schedule list changed/)
})
test('clearing visible values retains extensions and untouched empty plans', () => {
  const doc = { charging_tag_rates: { Home: { flat: 0.2, schedules: [schedule], extension: true }, Empty: {} } }
  const next = parseRates(doc)
  next.tags.Home = { flat: null, schedules: [] }
  assert.deepEqual(applyRateChanges(doc, next), { charging_tag_rates: { Home: { flat: null, schedules: [], extension: true }, Empty: {} } })
})
test('failed reads never become defaults or an editable empty document', async () => {
  await assert.rejects(loadRates(async () => new Response('{}', { status: 500 })), /could not be loaded/)
  await assert.rejects(loadRates(async () => Response.json({})), /could not be read/)
  await assert.rejects(loadRates(async () => Response.json({ document: { charging_tag_rates: 'broken' } })))
})
test('save sends a single conditional request and surfaces failed/unknown outcomes', async () => {
  const doc = { charging_currency: 'CAD', charging_default_rate: 0.1 }
  const next = { ...parseRates(doc), defaultRate: 0.2 }
  let calls = 0
  const saved = await saveRateChanges(doc, next, async (url, options) => {
    calls++
    assert.equal(url, '/api/charging/rates')
    assert.equal(options?.method, 'PUT')
    const body = JSON.parse(String(options?.body))
    assert.deepEqual(body.expected, doc)
    assert.deepEqual(body.document, { ...doc, charging_default_rate: 0.2 })
    return Response.json({ document: body.document })
  })
  assert.equal(calls, 1)
  assert.equal(saved.charging_default_rate, 0.2)
  await assert.rejects(saveRateChanges(doc, next, async () => new Response('', { status: 409 })), /changed elsewhere/)
  await assert.rejects(saveRateChanges(doc, next, async () => new Response('', { status: 500 })), /could not be confirmed/)
  await assert.rejects(saveRateChanges(doc, next, async () => Response.json({ document: doc })), /could not be confirmed/)
  await assert.rejects(saveRateChanges(doc, next, async () => { throw new Error('offline') }), /offline/)
})

test('tag names are data, including names used by object prototypes', () => {
  const document = {}
  const next = parseRates(document)
  next.tags = Object.fromEntries(['__proto__', 'constructor', 'toString'].map(tag => [tag, { flat: 0.2, schedules: [] }]))
  assert.deepEqual(applyRateChanges(document, next), { charging_tag_rates: Object.fromEntries(Object.keys(next.tags).map(tag => [tag, { flat: 0.2 }])) })
})
test('confirmation checks raw extensions while ignoring object key order', async () => {
  const doc = { charging_tag_rates: { Home: { flat: 0.1, future: true } }, charging_currency: 'CAD' }
  const rates = parseRates(doc)
  assert.deepEqual(await saveRateChanges(doc, rates, async () => Response.json({ document: { charging_currency: 'CAD', charging_tag_rates: { Home: { future: true, flat: 0.1 } } } })), { charging_currency: 'CAD', charging_tag_rates: { Home: { future: true, flat: 0.1 } } })
  await assert.rejects(saveRateChanges(doc, rates, async () => Response.json({ document: { charging_currency: 'CAD', charging_tag_rates: { Home: { flat: 0.1 } } } })), /could not be confirmed/)
})

test('legacy minute-count times remain editable without changing their stored representation', () => {
  const doc = { charging_tag_rates: { Home: { schedules: [{ ...schedule, start: 1320, end: "360" }] } } }
  const next = parseRates(doc)
  assert.equal(next.tags.Home.schedules[0].start, '22:00')
  assert.equal(next.tags.Home.schedules[0].end, '06:00')
  next.tags.Home.schedules[0].rate = 0.15
  assert.deepEqual(applyRateChanges(doc, next), { charging_tag_rates: { Home: { schedules: [{ ...schedule, start: 1320, end: "360", rate: 0.15 }] } } })
})

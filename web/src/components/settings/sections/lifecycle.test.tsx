import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, StrictMode } from 'react'
import { Window } from 'happy-dom'

async function setup() {
  const win = new Window({ url: 'http://localhost/' })
  class TestSocket { static OPEN = 1; readyState = 1; close() {}; send() {} }
  const values = { window: win, document: win.document, navigator: win.navigator, localStorage: win.localStorage, WebSocket: TestSocket, IS_REACT_ACT_ENVIRONMENT: true }
  const descriptors = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const oldFetch = globalThis.fetch
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div')
  win.document.body.append(container)
  const root = createRoot(container)
  return { win, root, container, async cleanup() {
    await act(async () => root.unmount())
    globalThis.fetch = oldFetch
    for (const [key, descriptor] of descriptors) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  } }
}

test('speed test aborts StrictMode stale responses and the active stream on Stop; restarting owns a new request', async () => {
  const dom = await setup()
  const { SpeedTestModal } = await import('./SpeedTestModal.tsx')
  const reads: { signal: AbortSignal; resolve: (response: Response) => void }[] = []
  globalThis.fetch = (_input, init) => new Promise(resolve => {
    assert.equal(init?.method, undefined)
    reads.push({ signal: init?.signal as AbortSignal, resolve })
  })
  const button = (label: string) => [...dom.win.document.querySelectorAll('button')].find(item => item.textContent === label)!
  let staleCanceled = 0
  let activeCanceled = 0
  try {
    await act(async () => dom.root.render(createElement(StrictMode, {}, createElement(SpeedTestModal, { onClose() {} }))))
    assert.equal(reads.length, 2)
    assert.equal(reads[0].signal.aborted, true)
    await act(async () => reads[0].resolve(new Response(new ReadableStream({ cancel() { staleCanceled++ } }))))
    assert.equal(staleCanceled, 1)
    await act(async () => reads[1].resolve(new Response(new ReadableStream({ cancel() { activeCanceled++ } }))))
    assert.match(dom.win.document.body.textContent, /Measuring throughput/)
    await act(async () => button('Stop').click())
    assert.equal(activeCanceled, 1)
    assert.equal(reads[1].signal.aborted, true)
    assert.equal(reads.length, 2, 'an aborted loop must not start another transfer')
    assert.match(dom.win.document.body.textContent, /Test stopped/)
    await act(async () => button('Run again').click())
    assert.equal(reads.length, 3)
    assert.equal(reads[2].signal.aborted, false)
    await act(async () => reads[2].resolve(new Response('', { status: 503 })))
    assert.match(dom.win.document.body.textContent, /Speed test failed/)
    assert.ok(button('Run again'))
  } finally { await dom.cleanup() }
})

test('BLE read failures stay retryable, stale mount reads cannot disable pairing, and failed samples retain the last data', async () => {
  const dom = await setup()
  const { BlePairButton } = await import('./BlePairButton.tsx')
  const pending: { url: string; signal: AbortSignal; resolve: (response: Response) => void }[] = []
  const writes: string[] = []
  const timestamp = Math.floor(Date.now() / 1000)
  globalThis.fetch = (input, init) => {
    const url = String(input)
    if (init?.method) writes.push(url)
    if (['/api/system/ble-enabled', '/api/system/ble-status?quick=true', '/api/system/ble-adapters', '/api/system/ble-latest-sample'].includes(url)) {
      return new Promise(resolve => pending.push({ url, signal: init?.signal as AbortSignal, resolve }))
    }
    if (url === '/api/system/ble-connected') return Promise.resolve(Response.json({ last_success_ts: timestamp, sample_count_10min: 1, radio_owner: null, archiving: false }))
    if (url === '/api/system/clock-status') return Promise.resolve(Response.json({ synced: true, show_warning: false }))
    return Promise.resolve(Response.json({}))
  }
  const reads = (url: string) => pending.filter(request => request.url === url)
  const button = (label: string) => [...dom.container.querySelectorAll('button')].find(item => item.textContent.trim() === label)!
  const adapters = { current: 'hci1', default: 'hci0', available: [{ id: 'hci0', source: 'onboard', address: null }, { id: 'hci1', source: 'external', address: null }] }
  try {
    await act(async () => dom.root.render(createElement(StrictMode, {}, createElement(BlePairButton))))
    assert.equal(reads('/api/system/ble-enabled').length, 2)
    assert.equal(reads('/api/system/ble-enabled')[0].signal.aborted, true)
    await act(async () => {
      reads('/api/system/ble-enabled')[1].resolve(new Response('', { status: 503 }))
      reads('/api/system/ble-status?quick=true')[1].resolve(new Response('', { status: 503 }))
      reads('/api/system/ble-adapters')[1].resolve(Response.json(adapters))
    })
    assert.match(dom.container.textContent, /Could not load BLE status/)
    assert.equal(button('Retry').disabled, false, 'a missing VIN after a failed read must not block Retry')
    await act(async () => button('Retry').click())
    await act(async () => {
      reads('/api/system/ble-enabled')[2].resolve(Response.json({ enabled: true }))
      reads('/api/system/ble-status?quick=true')[2].resolve(Response.json({ status: 'paired', vin: '5YJ3E1EA1KF000001', binaries_installed: true }))
      reads('/api/system/ble-enabled')[0].resolve(Response.json({ enabled: false }))
      reads('/api/system/ble-status?quick=true')[0].resolve(Response.json({ status: 'not_paired' }))
      reads('/api/system/ble-adapters')[0].resolve(Response.json({ ...adapters, current: 'hci0' }))
    })
    assert.equal(button('Re-pair').disabled, false)
    assert.doesNotMatch(dom.container.textContent, /BLE is disabled/)
    const external = [...dom.container.querySelectorAll('button')].find(item => item.textContent.includes('USB Bluetooth dongle'))!
    assert.equal(external.disabled, true, 'stale adapter read must not replace the current selection')
    await act(async () => button('Show output').click())
    await act(async () => reads('/api/system/ble-latest-sample')[0].resolve(Response.json({ ts: timestamp, seconds_ago: 1, battery_pct: 74 })))
    assert.match(dom.container.textContent, /74%/)
    await act(async () => button('Refresh').click())
    await act(async () => reads('/api/system/ble-latest-sample')[1].resolve(new Response('', { status: 503 })))
    assert.match(dom.container.querySelector('[role="alert"]')!.textContent, /Could not refresh live data/)
    assert.match(dom.container.textContent, /74%/)
    await act(async () => button('Refresh').click())
    await act(async () => button('Hide output').click())
    assert.equal(reads('/api/system/ble-latest-sample')[2].signal.aborted, true)
    assert.deepEqual(writes, [], 'reading status or opening live output must never pair, install, or force a car poll')
  } finally {
    const { wsClient } = await import('../../../lib/ws.ts')
    wsClient.disconnect()
    await dom.cleanup()
  }
})

test('update installation starts only on explicit action and a failed preflight closes progress while keeping the error visible', async () => {
  const dom = await setup()
  const { UpdateSection } = await import('./UpdateSection.tsx')
  const reads: { signal: AbortSignal; resolve: (response: Response) => void }[] = []
  let resolveInternet: (response: Response) => void = () => {}
  let started = 0
  const writes: string[] = []
  globalThis.fetch = (input, init) => {
    const url = String(input)
    if (init?.method) writes.push(url)
    if (url === '/api/system/update-status') return new Promise(resolve => reads.push({ signal: init?.signal as AbortSignal, resolve }))
    if (url === '/api/system/check-internet') return new Promise(resolve => { resolveInternet = resolve })
    if (url.startsWith('/api/config/preference')) return Promise.resolve(Response.json({ value: null }))
    if (url === '/api/system/version') return Promise.resolve(Response.json({ version: 'v3.22.7', boot_id: 'boot-before-update' }))
    return Promise.resolve(Response.json({}))
  }
  try {
    await act(async () => dom.root.render(createElement(StrictMode, {}, createElement(UpdateSection, { onInstallStart() { started++ } }))))
    assert.equal(reads.length, 2)
    assert.equal(reads[0].signal.aborted, true)
    await act(async () => reads[1].resolve(Response.json({ stable: { available: true, version: 'v3.23.0', release_url: '', release_notes: '' } })))
    await act(async () => reads[0].resolve(Response.json({ stable: { available: true, version: 'v1.0.0', release_url: '', release_notes: '' } })))
    assert.match(dom.container.textContent, /v3.23.0/)
    assert.doesNotMatch(dom.container.textContent, /v1.0.0/)
    assert.equal(started, 0)
    assert.deepEqual(writes, [])
    const install = [...dom.container.querySelectorAll('button')].find(item => item.textContent.trim() === 'Install')!
    await act(async () => install.click())
    assert.equal(started, 1)
    assert.match(dom.win.document.querySelector('[role="dialog"]')!.textContent, /Installing Update/)
    await act(async () => resolveInternet(Response.json({ connected: false })))
    assert.equal(dom.win.document.querySelector('[role="dialog"]'), null)
    assert.match(dom.container.textContent, /No internet connection/)
    assert.deepEqual(writes, [], 'a failed preflight must not POST the update')
  } finally {
    const { wsClient } = await import('../../../lib/ws.ts')
    wsClient.disconnect()
    await dom.cleanup()
  }
})

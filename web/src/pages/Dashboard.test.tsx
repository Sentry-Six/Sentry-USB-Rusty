import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { MemoryRouter } from 'react-router-dom'
import { Window } from 'happy-dom'

test('dashboard keeps its last data on failure, reads server ETA, and never loads the drive list', async () => {
  const win = new Window({ url: 'http://localhost/' })
  const descriptors = ['window', 'document', 'navigator', 'localStorage', 'WebSocket', 'IS_REACT_ACT_ENVIRONMENT'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  class TestSocket { static OPEN = 1; readyState = 1; close() {}; send() {} }
  for (const [key, value] of Object.entries({ window: win, document: win.document, navigator: win.navigator, localStorage: win.localStorage, WebSocket: TestSocket, IS_REACT_ACT_ENVIRONMENT: true })) {
    Object.defineProperty(globalThis, key, { configurable: true, value })
  }
  win.localStorage.setItem('cloud-bar-dismissed', '1')
  const oldFetch = globalThis.fetch
  const requested: string[] = []
  let rejectStatus = false
  const fixtures: Record<string, unknown> = {
    '/api/status': { cpu_temp: '55000', num_snapshots: '86', snapshot_oldest: '', snapshot_newest: '', total_space: '1000000', free_space: '45000', uptime: '60', drives_active: 'yes', udc_state: 'configured', wifi_ssid: 'Test network', wifi_strength: '60/70', wifi_ip: '192.0.2.1', ether_ip: '', ether_speed: '', fan_speed: '2000', wifi_rx_bps: 0, wifi_tx_bps: 0, wifi_rate_state: 'live', storage_health: { state: 'healthy', message: 'Storage managed automatically' } },
    '/api/drives/stats': { processed_count: 100000, drives_count: 985, total_distance_km: 20000, total_distance_mi: 12000, fsd_percent: 65, latest_drive_end: null },
    '/api/drives/status': { running: false, phase: 'archiving', current: 300, total: 3200, eta_seconds: 6200, eta_state: 'ready', sampled_at: Date.now() / 1000, archive_cycle: { id: 'fixture-cycle', cancelling: false } },
    '/api/telemetry/tire-history?days=30': { points: [] },
    '/api/status/storage': { cam_size: 100000, music_size: 0, lightshow_size: 0, boombox_size: 0, snapshots_size: 855000, total_space: 1000000, free_space: 45000 },
  }
  globalThis.fetch = async (url) => {
    const path = String(url)
    requested.push(path)
    if (rejectStatus && path === '/api/status') return new Response('', { status: 503 })
    return Response.json(fixtures[path] ?? {})
  }
  const { createRoot } = await import('react-dom/client')
  const { default: Dashboard } = await import('./Dashboard.tsx')
  const container = win.document.createElement('div')
  win.document.body.append(container)
  const root = createRoot(container)
  try {
    await act(async () => root.render(createElement(MemoryRouter, null, createElement(Dashboard))))
    assert.match(container.textContent, /Archiving footage/)
    assert.match(container.textContent, /About 1.7 h remaining/)
    assert.match(container.textContent, /0 Mbps/)
    assert.match(container.textContent, /985/)
    assert.ok(container.querySelector('[role="progressbar"]'))
    assert.equal(requested.includes('/api/drives'), false)
    rejectStatus = true
    await act(async () => win.document.dispatchEvent(new win.Event('visibilitychange')))
    assert.match(container.textContent, /Reconnecting · showing the last update/)
    assert.match(container.textContent, /985/)
    assert.match(container.textContent, /Archiving footage/)
  } finally {
    await act(async () => root.unmount())
    globalThis.fetch = oldFetch
    for (const [key, descriptor] of descriptors) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor)
      else Reflect.deleteProperty(globalThis, key)
    }
    win.close()
  }
})

test('optional keep-awake menu is keyboard operable and automatic Away Mode has no invented countdown', async () => {
  const win = new Window({ url: 'http://localhost/' })
  const descriptors = ['window', 'document', 'navigator', 'localStorage', 'WebSocket', 'IS_REACT_ACT_ENVIRONMENT'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  class TestSocket { static OPEN = 1; readyState = 1; close() {}; send() {} }
  for (const [key, value] of Object.entries({ window: win, document: win.document, navigator: win.navigator, localStorage: win.localStorage, WebSocket: TestSocket, IS_REACT_ACT_ENVIRONMENT: true })) Object.defineProperty(globalThis, key, { configurable: true, value })
  win.localStorage.setItem('cloud-bar-dismissed', '1')
  const oldFetch = globalThis.fetch
  const writes: string[] = []
  globalThis.fetch = async (input, init) => {
    const path = String(input)
    if (init?.method) writes.push(path)
    if (path.includes('keep_awake_webui_mode')) return Response.json({ value: 'manual' })
    if (path === '/api/keep-awake/status') return Response.json({ state: 'idle', mode: 'manual' })
    if (path === '/api/away-mode/status') return Response.json({ state: 'active', mode: 'auto', ap_on: true, ap_ssid: 'Sentry AP' })
    if (path === '/api/status') return Response.json({ cpu_temp: '55000', num_snapshots: '86', snapshot_oldest: '', snapshot_newest: '', total_space: '1000000', free_space: '45000', uptime: '60', drives_active: 'yes', udc_state: 'configured', wifi_ssid: 'Test', wifi_strength: '60/70', wifi_ip: '192.0.2.1', ether_ip: '', ether_speed: '', fan_speed: '2000' })
    if (path === '/api/drives/stats') return Response.json({ processed_count: 100, drives_count: 10, total_distance_km: 200, total_distance_mi: 120, fsd_percent: 65 })
    return Response.json({})
  }
  const { createRoot } = await import('react-dom/client')
  const { StrictMode } = await import('react')
  const { KeepAwakeProvider } = await import('../hooks/useKeepAwake.tsx')
  const { AwayModeProvider } = await import('../hooks/useAwayMode.tsx')
  const { default: Dashboard } = await import('./Dashboard.tsx')
  const container = win.document.createElement('div'); win.document.body.append(container)
  const root = createRoot(container)
  try {
    await act(async () => root.render(createElement(StrictMode, {}, createElement(MemoryRouter, {}, createElement(AwayModeProvider, { children: createElement(KeepAwakeProvider, { children: createElement(Dashboard) }) })))))
    assert.match(container.textContent, /Automatic/)
    assert.match(container.textContent, /Active while away/)
    assert.doesNotMatch(container.textContent, /0h\s*0m\s*remaining/)
    assert.equal(container.querySelector('[aria-label="Away mode duration"]'), null)
    const start = container.querySelector<HTMLButtonElement>('[aria-label="Start web app keep-awake"]')!
    assert.ok(start)
    start.focus()
    await act(async () => start.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true, cancelable: true })))
    assert.equal(start.getAttribute('aria-expanded'), 'true')
    const menu = container.querySelector('[role="menu"]')!
    const choices = menu.querySelectorAll('button')
    assert.equal(win.document.activeElement, choices[0])
    await act(async () => choices[0].dispatchEvent(new win.KeyboardEvent('keydown', { key: 'End', bubbles: true, cancelable: true })))
    assert.equal(win.document.activeElement, choices[choices.length - 1])
    await act(async () => choices[choices.length - 1].dispatchEvent(new win.KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })))
    assert.equal(container.querySelector('[role="menu"]'), null)
    assert.equal(win.document.activeElement, start)
    assert.deepEqual(writes, [], 'Opening/dismissing duration choices must not start keep-awake')
  } finally {
    await act(async () => root.unmount()); globalThis.fetch = oldFetch
    for (const [key, descriptor] of descriptors) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
})

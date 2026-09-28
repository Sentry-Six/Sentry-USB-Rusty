import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, StrictMode } from 'react'
import { Window } from 'happy-dom'
import { HealthCheckModal } from './HealthCheckModal.tsx'

test('health separates failed, recovering and unknown checks from all-check details', async () => {
  const win = new Window({ url: 'http://localhost/' })
  const descriptors = ['window', 'document', 'navigator', 'IS_REACT_ACT_ENVIRONMENT'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries({ window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true })) Object.defineProperty(globalThis, key, { configurable: true, value })
  const oldFetch = globalThis.fetch
  globalThis.fetch = async () => Response.json({ summary: '1 problem found', categories: [{ name: 'Storage', items: [
    { name: 'Writes', status: 'fail', detail: 'Read-only filesystem' },
    { name: 'Cleanup', status: 'recovering' }, { name: 'Probe', status: 'unknown' },
    { name: 'Optional disk', status: 'not_applicable' }, { name: 'Index', status: 'pass' },
    { name: 'Backingfiles free space', status: 'info', detail: '5.4% free' },
  ] }] })
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div'); win.document.body.append(container)
  const root = createRoot(container)
  try {
    await act(async () => root.render(createElement(StrictMode, {}, createElement(HealthCheckModal, { onClose: () => {} }))))
    const attention = win.document.querySelector('[aria-label="Needs attention"]')!
    assert.ok(attention.textContent.includes('Writes'))
    assert.ok(!attention.textContent.includes('Probe'))
    assert.ok(!attention.textContent.includes('Cleanup'))
    const monitoring = win.document.querySelector('[aria-label="Device status"]')!
    assert.ok(monitoring.textContent.includes('Recovering'))
    assert.ok(monitoring.textContent.includes('Not measured'))
    assert.ok(!monitoring.textContent.includes('Backingfiles free space'))
    const details = win.document.querySelector('details')!
    assert.equal(details.open, false)
    assert.ok(details.textContent.includes('Not applicable'))
    assert.ok(details.textContent.includes('Index'))
    assert.ok(details.textContent.includes('Backingfiles free space'))
    assert.equal(win.document.querySelectorAll('li').length, 9)
  } finally {
    await act(async () => root.unmount()); globalThis.fetch = oldFetch
    for (const [key, descriptor] of descriptors) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
})

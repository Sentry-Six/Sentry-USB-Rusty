import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, StrictMode } from 'react'
import { Window } from 'happy-dom'
import { AutoUpdateToggle } from './AutoUpdateToggle.tsx'

test('auto install is opt-in and a failed save never appears enabled', async () => {
  const win = new Window({ url: 'http://localhost/' })
  const descriptors = ['window', 'document', 'navigator', 'IS_REACT_ACT_ENVIRONMENT'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries({ window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true })) {
    Object.defineProperty(globalThis, key, { configurable: true, value })
  }
  const oldFetch = globalThis.fetch
  let fail = true
  const writes: unknown[] = []
  globalThis.fetch = async (_url, options) => {
    if (options?.method === 'PUT') {
      writes.push(JSON.parse(String(options.body)))
      return fail ? new Response('', { status: 500 }) : Response.json({ success: true })
    }
    return Response.json({ value: null })
  }
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div')
  win.document.body.append(container)
  const root = createRoot(container)
  try {
    await act(async () => root.render(createElement(StrictMode, {}, createElement(AutoUpdateToggle))))
    const toggle = container.querySelector('input')!
    assert.equal(toggle.checked, false)
    assert.ok(!container.textContent.includes('Park and Sentry Mode off'))
    const help = container.querySelector('button[aria-label="About automatic updates"]')!
    help.focus()
    await act(async () => help.click())
    assert.ok(win.document.querySelector('[role="dialog"]')?.textContent.includes('Park and Sentry Mode off'))
    assert.ok(win.document.querySelector('[role="dialog"]')?.textContent.includes('briefly disconnects the USB drive'))
    assert.equal(writes.length, 0, 'Info must not change the setting')
    await act(async () => win.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'Escape', bubbles: true })))
    assert.equal(win.document.activeElement, help)
    await act(async () => toggle.click())
    assert.equal(toggle.checked, false)
    assert.ok(container.querySelector('[role="alert"]'))
    fail = false
    await act(async () => toggle.click())
    assert.equal(toggle.checked, true)
    assert.deepEqual(writes[1], { key: 'auto_install_stable_updates', value: 'enabled' })
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

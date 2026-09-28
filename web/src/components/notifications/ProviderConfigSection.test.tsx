import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, StrictMode } from 'react'
import { Window } from 'happy-dom'
import { ProviderConfigSection } from './ProviderConfigSection.tsx'

async function mount(run: (container: HTMLElement, win: Window) => Promise<void>) {
  const win = new Window({ url: 'http://localhost/notifications?tab=delivery' })
  const descriptors = ['window', 'document', 'navigator', 'IS_REACT_ACT_ENVIRONMENT'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries({ window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true })) Object.defineProperty(globalThis, key, { configurable: true, value })
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div'); win.document.body.append(container)
  const root = createRoot(container)
  try { await act(async () => root.render(createElement(StrictMode, {}, createElement(ProviderConfigSection)))); await run(container as unknown as HTMLElement, win) }
  finally {
    await act(async () => root.unmount())
    for (const [key, descriptor] of descriptors) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
}

test('StrictMode HTML200 missing endpoint uses whitelisted legacy values read-only and ignores aborted older reads', async () => {
  const originalFetch = globalThis.fetch
  let providerRequests = 0
  let resolveStale: (response: Response) => void = () => {}
  const methods: string[] = []
  globalThis.fetch = async (input, init) => {
    methods.push(init?.method ?? 'GET')
    if (String(input).includes('/providers')) {
      providerRequests++
      if (providerRequests === 1) return new Promise<Response>(resolve => { resolveStale = resolve })
      return new Response('<!doctype html><html>app</html>', { status: 200, headers: { 'content-type': 'text/html' } })
    }
    assert.equal(String(input), '/api/setup/config')
    return Response.json({
      NOTIFICATION_TITLE: { value: 'Legacy title', active: true },
      TELEGRAM_CHAT_ID: { value: '123', active: true }, TELEGRAM_BOT_TOKEN: { value: 'saved token', active: true },
      WEB_PASSWORD: { value: 'must never enter provider form', active: true },
      SNAPSHOT_INTERVAL: { value: '480', active: true },
    })
  }
  try { await mount(async container => {
    assert.ok(container.textContent?.includes('View only on this device version'))
    assert.equal(container.querySelector('[role="alert"]'), null)
    const fields = [...container.querySelectorAll<HTMLInputElement>('input')]
    assert.ok(fields.some(input => input.value === 'Legacy title' && input.readOnly))
    assert.ok(fields.some(input => input.value === '123' && input.readOnly))
    assert.ok(!fields.some(input => input.value === 'must never enter provider form' || input.value === '480'))
    assert.ok(!container.textContent?.includes('Save delivery settings'))
    await act(async () => resolveStale(Response.json({ values: { NOTIFICATION_TITLE: 'Stale response' } })))
    assert.ok([...container.querySelectorAll<HTMLInputElement>('input')].some(input => input.value === 'Legacy title'))
    assert.deepEqual(new Set(methods), new Set(['GET']))
  }) } finally { globalThis.fetch = originalFetch }
})

test('current JSON endpoint stays editable and a server failure does not become empty defaults', async () => {
  const originalFetch = globalThis.fetch
  let fail = false
  const calls: string[] = []
  globalThis.fetch = async input => {
    calls.push(String(input))
    return fail ? new Response('', { status: 500 }) : Response.json({ values: { NOTIFICATION_TITLE: 'Current title' } })
  }
  try {
    await mount(async container => {
      const title = container.querySelector<HTMLInputElement>('input')!
      assert.equal(title.value, 'Current title'); assert.equal(title.readOnly, false)
      assert.ok(container.textContent?.includes('Save delivery settings'))
    })
    fail = true
    await mount(async container => {
      assert.ok(container.querySelector('[role="alert"]'))
      assert.equal(container.querySelector('input'), null)
      assert.ok(!container.textContent?.includes('Save delivery settings'))
    })
    assert.ok(calls.every(url => url === '/api/notifications/providers'))
  } finally { globalThis.fetch = originalFetch }
})

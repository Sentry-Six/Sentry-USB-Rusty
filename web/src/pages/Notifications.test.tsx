import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { Window } from 'happy-dom'
import Notifications from './Notifications.tsx'

async function mount(run: (container: HTMLElement, win: Window) => Promise<void>) {
  const win = new Window({ url: 'http://localhost/notifications' })
  const descriptors = ['window', 'document', 'navigator', 'IS_REACT_ACT_ENVIRONMENT'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries({ window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true })) Object.defineProperty(globalThis, key, { configurable: true, value })
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div')
  win.document.body.append(container)
  const root = createRoot(container)
  try { await act(async () => root.render(createElement(Notifications))); await run(container as unknown as HTMLElement, win) }
  finally {
    await act(async () => root.unmount())
    for (const [key, descriptor] of descriptors) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
}
const settings = Object.fromEntries(['archive_start','archive_complete','archive_error','temperature','keep_awake_failure','update','drives','rtc_battery','music_sync','keep_accessory','storage_repair'].map(key => [key, false]))

test('a failed preference read never presents invented switches; retry preserves unrelated choices', async () => {
  const originalFetch = globalThis.fetch
  let failed = true
  let saved: Record<string, boolean> | undefined
  globalThis.fetch = async (input, init) => {
    if (String(input).includes('/history')) return Response.json({ events: [], total: 0 })
    if (init?.method === 'PUT') { saved = JSON.parse(String(init.body)); return Response.json({}) }
    return failed ? new Response('', { status: 500 }) : Response.json(settings)
  }
  try { await mount(async container => {
    await act(async () => [...container.querySelectorAll('button')].find(button => button.textContent === 'Events')!.click())
    assert.ok(container.querySelector('[role="alert"]'))
    assert.equal(container.querySelectorAll('[role="switch"]').length, 0)
    assert.equal(saved, undefined)
    failed = false
    await act(async () => [...container.querySelectorAll('button')].find(button => button.textContent === 'Retry')!.click())
    const toggle = container.querySelector<HTMLButtonElement>('[aria-label="Archive Started"]')!
    assert.equal(toggle.getAttribute('aria-checked'), 'false')
    await act(async () => toggle.click())
    assert.deepEqual(saved, { ...settings, archive_start: true })
  }) } finally { globalThis.fetch = originalFetch }
})

test('one request per filter change and stale history cannot replace the selected filter', async () => {
  const originalFetch = globalThis.fetch
  let resolveOld: (value: Response) => void = () => {}
  const requests: string[] = []
  globalThis.fetch = async input => {
    const url = String(input); requests.push(url)
    if (url.includes('type=archive_error')) return Response.json({ events: [{ id: 'new', ts: 1, type: 'archive_error', title: 'New', message: 'Filtered result', providers: [] }], total: 1 })
    return new Promise<Response>(resolve => { resolveOld = resolve })
  }
  try { await mount(async (container) => {
    const filter = container.querySelector<HTMLButtonElement>('[role="combobox"][aria-label="Notification type"]')!
    await act(async () => filter.click())
    await act(async () => container.ownerDocument.querySelector<HTMLElement>('[role="option"][data-value="archive_error"]')!.click())
    assert.equal(requests.length, 2)
    await act(async () => resolveOld(Response.json({ events: [{ id: 'old', ts: 1, type: 'archive_start', title: 'Old', message: 'Stale result', providers: [] }], total: 1 })))
    assert.ok(container.textContent!.includes('Filtered result'))
    assert.ok(!container.textContent!.includes('Stale result'))
  }) } finally { globalThis.fetch = originalFetch }
})

import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { MemoryRouter } from 'react-router-dom'
import { Window } from 'happy-dom'
import Logs from './Logs.tsx'

async function withDiagnosticsPage(
  capture: () => Promise<Response>,
  check: (container: HTMLElement, requests: { url: string; method?: string }[], downloads: { filename: string; blob: Blob }[]) => Promise<void>,
) {
  const win = new Window({ url: 'http://localhost/logs?tab=diagnostics' })
  const values = { window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true }
  const descriptors = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const oldFetch = globalThis.fetch
  const oldCreate = URL.createObjectURL
  const oldRevoke = URL.revokeObjectURL
  const requests: { url: string; method?: string }[] = []
  const downloads: { filename: string; blob: Blob }[] = []
  let savedBlob: Blob
  URL.createObjectURL = blob => { savedBlob = blob as Blob; return 'blob:diagnostics-test' }
  URL.revokeObjectURL = () => {}
  win.HTMLAnchorElement.prototype.click = function () { downloads.push({ filename: this.download, blob: savedBlob }) }
  globalThis.fetch = async (input, init) => {
    const url = String(input); requests.push({ url, method: init?.method })
    if (url === '/api/diagnostics/download') return capture()
    if (url === '/api/diagnostics') return new Response('Old cached report\n')
    return Response.json({ content: 'Archive log\n', cursor: 'identity', before: 0, reset: true, has_more: false })
  }
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div'); win.document.body.append(container)
  const root = createRoot(container)
  try {
    await act(async () => root.render(createElement(MemoryRouter, { initialEntries: ['/logs?tab=diagnostics'] }, createElement(Logs))))
    await check(container as unknown as HTMLElement, requests, downloads)
  } finally {
    await act(async () => root.unmount())
    globalThis.fetch = oldFetch; URL.createObjectURL = oldCreate; URL.revokeObjectURL = oldRevoke
    for (const [key, descriptor] of descriptors) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
}

test('diagnostics download captures fresh evidence once and uses the server filename', async () => {
  let finish!: (response: Response) => void
  const response = new Promise<Response>(resolve => { finish = resolve })
  await withDiagnosticsPage(() => response, async (container, requests, downloads) => {
    assert.ok(container.textContent?.includes('Old cached report'))
    const button = [...container.querySelectorAll('button')].find(button => button.textContent === 'Capture & download')!
    await act(async () => button.click())
    assert.equal(button.disabled, true)
    assert.equal(button.textContent, 'Capturing…')
    await act(async () => button.click())
    assert.equal(requests.filter(r => r.url === '/api/diagnostics/download').length, 1)
    await act(async () => {
      finish(new Response('Fresh USB evidence\n', { headers: { 'Content-Type': 'text/plain; charset=utf-8', 'Content-Disposition': 'attachment; filename="sentryusb-diagnostics-now.txt"' } }))
    })
    assert.equal(requests.find(r => r.url === '/api/diagnostics/download')?.method, 'POST')
    assert.equal(downloads.length, 1)
    assert.equal(downloads[0].filename, 'sentryusb-diagnostics-now.txt')
    assert.equal(await downloads[0].blob.text(), 'Fresh USB evidence\n')
    assert.ok(container.textContent?.includes('Fresh USB evidence'))
    assert.equal(button.disabled, false)
    assert.equal(requests.filter(r => r.url === '/api/diagnostics').length, 1, 'Download must not re-fetch a cached report')
  })
})

for (const [label, response] of [
  ['capture failure', () => new Response('Failed', { status: 500 })],
  ['older server SPA fallback', () => new Response('<html>fallback</html>', { headers: { 'Content-Type': 'text/html' } })],
] as const) test(`diagnostics ${label} shows an error instead of downloading stale evidence`, async () => {
  await withDiagnosticsPage(async () => response(), async (container, _requests, downloads) => {
    const button = [...container.querySelectorAll('button')].find(button => button.textContent === 'Capture & download')!
    await act(async () => button.click())
    assert.equal(downloads.length, 0)
    assert.match(container.querySelector('[role="alert"]')?.textContent || '', /Could not capture diagnostics/)
    assert.equal(button.disabled, false)
  })
})

test('pausing stops tail requests and older paging uses the identity and retained byte offset', async () => {
  const win = new Window({ url: 'http://localhost/logs' })
  const descriptors = ['window', 'document', 'navigator', 'IS_REACT_ACT_ENVIRONMENT'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries({ window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true })) Object.defineProperty(globalThis, key, { configurable: true, value })
  const oldFetch = globalThis.fetch
  const requests: string[] = []
  const text = Array.from({ length: 2200 }, (_, i) => `${i} 🚗\n`).join('')
  const prefix = Array.from({ length: 200 }, (_, i) => `${i} 🚗\n`).join('')
  globalThis.fetch = async input => {
    const url = String(input); requests.push(url)
    return url.includes('/page?')
      ? Response.json({ content: 'older entry\n', before: 1 })
      : Response.json({ content: text, cursor: 'identity', before: 100, reset: true, has_more: false })
  }
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div'); win.document.body.append(container)
  const root = createRoot(container)
  try {
    await act(async () => root.render(createElement(MemoryRouter, {}, createElement(Logs))))
    assert.equal(requests.length, 1)
    await act(async () => [...container.querySelectorAll('button')].find(button => button.textContent === 'Pause live')!.click())
    assert.equal(requests.length, 1, 'Pause must not fetch a new tail')
    await act(async () => [...container.querySelectorAll('button')].find(button => button.textContent === 'Load older')!.click())
    assert.equal(requests.length, 2, 'Older fetch must not restart live tailing')
    const page = new URL(requests[1], 'http://localhost')
    assert.equal(page.searchParams.get('cursor'), 'identity')
    assert.equal(page.searchParams.get('before'), String(100 + new TextEncoder().encode(prefix).length))
    assert.ok(container.textContent.includes('older entry'))
  } finally {
    await act(async () => root.unmount()); globalThis.fetch = oldFetch
    for (const [key, descriptor] of descriptors) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
})

test('StrictMode reads legacy logs when an unknown tail route returns HTML with status 200', async () => {
  const { StrictMode } = await import('react')
  const win = new Window({ url: 'http://localhost/logs' })
  const values = { window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true }
  const descriptors = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const oldFetch = globalThis.fetch
  const requests: string[] = []
  globalThis.fetch = async input => {
    const url = String(input); requests.push(url)
    return url.includes('/tail?')
      ? new Response('<!doctype html><html>SPA fallback</html>', { headers: { 'content-type': 'text/html; charset=utf-8' } })
      : new Response('Actual archive log content\n', { headers: { 'content-type': 'text/plain' } })
  }
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div'); win.document.body.append(container)
  const root = createRoot(container)
  try {
    await act(async () => root.render(createElement(StrictMode, null, createElement(MemoryRouter, null, createElement(Logs)))))
    assert.ok(container.textContent.includes('Actual archive log content'))
    assert.ok(!container.querySelector('[role="alert"]'))
    assert.ok(!container.textContent.includes('SPA fallback'))
    assert.ok(requests.includes('/api/logs/archiveloop'))
  } finally {
    await act(async () => root.unmount()); globalThis.fetch = oldFetch
    for (const [key, descriptor] of descriptors) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
})

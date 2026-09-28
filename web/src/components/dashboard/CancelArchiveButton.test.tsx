import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { Window } from 'happy-dom'
import { CancelArchiveButton } from './CancelArchiveButton.tsx'

test('cancel targets the displayed cycle, shows failures, and never offers resume', async () => {
  const win = new Window({ url: 'http://localhost/' })
  const descriptors = ['window', 'document', 'navigator', 'IS_REACT_ACT_ENVIRONMENT'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries({ window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true })) {
    Object.defineProperty(globalThis, key, { configurable: true, value })
  }
  const oldFetch = globalThis.fetch
  let fail = true
  const requests: { url: string; options?: RequestInit }[] = []
  globalThis.fetch = async (url, options) => {
    requests.push({ url: String(url), options })
    return fail ? new Response('', { status: 500 }) : Response.json({ success: true }, { status: 202 })
  }
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div')
  win.document.body.append(container)
  const root = createRoot(container)
  try {
    await act(async () => root.render(createElement(CancelArchiveButton, { cycle: { id: '123:visit', cancelling: false } })))
    const button = container.querySelector('button')!
    assert.equal(button.textContent, 'Cancel Archive')
    await act(async () => button.click())
    assert.ok(container.querySelector('[role="alert"]'))
    assert.equal(button.disabled, false)
    fail = false
    await act(async () => button.click())
    assert.equal(requests[1].url, '/api/system/cancel-archive')
    assert.equal(requests[1].options?.method, 'POST')
    assert.deepEqual(JSON.parse(String(requests[1].options?.body)), { cycle_id: '123:visit' })
    assert.equal(button.disabled, true)
    assert.equal(button.textContent, 'Cancelling…')
    assert.doesNotMatch(container.textContent, /resume/i)
    await act(async () => root.render(createElement(CancelArchiveButton, { cycle: null })))
    assert.equal(container.querySelector('button'), null)
    await act(async () => root.render(createElement(CancelArchiveButton, { cycle: null, unavailable: true })))
    const unavailable = container.querySelector('button')!
    assert.equal(unavailable.textContent, 'Cancel Archive')
    assert.equal(unavailable.disabled, true)
    const before = requests.length
    await act(async () => unavailable.click())
    assert.equal(requests.length, before, 'missing cycle IDs must never trigger a cancellation request')
    await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="About archive controls unavailable"]')!.click())
    assert.match(win.document.querySelector('[role="dialog"]')!.textContent, /isn’t reporting cancellation support/)
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

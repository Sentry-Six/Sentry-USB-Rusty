import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { Window } from 'happy-dom'
import { NotificationHistoryItem } from './NotificationHistoryItem.tsx'

test('history preserves full legacy/new details, delivery failures and dismissal', async () => {
  const win = new Window({ url: 'http://localhost/' })
  const keys = ['window', 'document', 'navigator', 'IS_REACT_ACT_ENVIRONMENT']
  const descriptors = keys.map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries({ window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true })) {
    Object.defineProperty(globalThis, key, { configurable: true, value })
  }
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div')
  win.document.body.append(container)
  const root = createRoot(container)
  let dismissed = ''
  const legacy = {
    id: 'old', ts: 1727376000, type: 'archive_error', title: 'SentryUSB',
    message: 'exit code 23\n<script>secret()</script>',
    providers: ['telegram'], results: { telegram: 'error: HTTP 400 chat not found' },
  }
  try {
    await act(async () => root.render(createElement(NotificationHistoryItem, {
      event: legacy, onDismiss: id => { dismissed = id },
    })))
    assert.ok(container.textContent.includes('exit code 23'))
    assert.ok(container.textContent.includes('HTTP 400 chat not found'))
    assert.equal(container.querySelector('script'), null)
    assert.ok(container.querySelector('time')?.getAttribute('dateTime'))
    const event = { ...legacy, summary: 'Archive interrupted.', provider_errors: { telegram: 'HTTP 429 retry later' } }
    await act(async () => root.render(createElement(NotificationHistoryItem, { event, onDismiss: id => { dismissed = id } })))
    assert.equal(container.querySelector('[data-notification-summary]')?.textContent, 'Archive interrupted.')
    const details = container.querySelector('details')!
    assert.ok(details)
    assert.equal(details.open, false)
    await act(async () => container.querySelector('summary')!.click())
    assert.equal(details.open, true)
    assert.ok(details.textContent.includes('exit code 23'))
    assert.ok(details.textContent.includes('HTTP 429 retry later'))
    await act(async () => container.querySelector('button[aria-label="Dismiss notification"]')!.click())
    assert.equal(dismissed, 'old')
  } finally {
    await act(async () => root.unmount())
    for (const [key, descriptor] of descriptors) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor)
      else Reflect.deleteProperty(globalThis, key)
    }
    win.close()
  }
})

test('unreadable history is an error, not empty history, and can be retried', async () => {
  const win = new Window({ url: 'http://localhost/' })
  const descriptors = ['window', 'document', 'navigator', 'IS_REACT_ACT_ENVIRONMENT'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries({ window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true })) {
    Object.defineProperty(globalThis, key, { configurable: true, value })
  }
  const oldFetch = globalThis.fetch
  let failed = true
  globalThis.fetch = async url => String(url).includes('/history')
    ? failed ? new Response('', { status: 500 }) : Response.json({ events: [], total: 0, limit: 50, offset: 0 })
    : Response.json({})
  const { default: Notifications } = await import('../../pages/Notifications.tsx')
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div')
  win.document.body.append(container)
  const root = createRoot(container)
  try {
    await act(async () => root.render(createElement(Notifications)))
    assert.ok(container.querySelector('[role="alert"]'))
    assert.ok(!container.textContent.includes('No notifications yet'))
    const clear = [...container.querySelectorAll('button')].find(button => button.textContent === 'Clear All')!
    assert.equal(clear.disabled, false)
    failed = false
    const retry = [...container.querySelectorAll('button')].find(button => button.textContent === 'Retry')!
    await act(async () => retry.click())
    assert.equal(container.querySelector('[role="alert"]'), null)
    assert.ok(container.textContent.includes('No notifications yet'))
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

import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { Window } from 'happy-dom'

type Event = { status: string; line?: string; error?: string; message?: string }
type Card = {
  container: HTMLElement
  emit: (event: Event) => void
  requests: RequestInit[]
}

async function withCard(repair: () => Promise<Response>, check: (card: Card) => Promise<void>) {
  const win = new Window({ url: 'http://localhost/settings' })
  const values = { window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true }
  const descriptors = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const { wsClient } = await import('@/lib/ws')
  const { StorageRepairCard } = await import('./StorageRepairCard.tsx')
  const { createRoot } = await import('react-dom/client')
  const oldFetch = globalThis.fetch
  const oldSubscribe = wsClient.subscribe
  const handlers = new Set<(event: unknown) => void>()
  const requests: RequestInit[] = []
  wsClient.subscribe = (_type, handler) => {
    handlers.add(handler)
    return () => { handlers.delete(handler) }
  }
  globalThis.fetch = async (input, init) => {
    const url = String(input)
    if (url === '/api/storage/health') return Response.json({
      state: 'corrupt', external: true, device: '/dev/fixture', fstype: 'xfs',
      mounted: false, mountpoint: '/backingfiles', cam_disk_present: false,
      dmesg_errors: [], last_repair_log: null,
    })
    if (url.startsWith('/api/config/preference?')) return Response.json({ value: false })
    assert.equal(url, '/api/storage/repair')
    requests.push(init || {})
    return repair()
  }
  const container = win.document.createElement('div')
  win.document.body.append(container)
  const root = createRoot(container)
  try {
    await act(async () => root.render(createElement(StorageRepairCard)))
    await check({ container: container as unknown as HTMLElement, requests, emit: event => handlers.forEach(handler => handler(event)) })
  } finally {
    await act(async () => root.unmount())
    globalThis.fetch = oldFetch
    wsClient.subscribe = oldSubscribe
    for (const [key, descriptor] of descriptors) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor)
      else Reflect.deleteProperty(globalThis, key)
    }
    win.close()
  }
}

function button(container: HTMLElement, label = 'Repair storage') {
  const result = [...container.querySelectorAll('button')].find(node => node.textContent?.trim() === label)
  assert.ok(result, `missing button: ${label}`)
  return result
}

async function previousLog(card: Card) {
  await act(async () => {
    card.emit({ status: 'running', line: 'Existing repair evidence' })
    card.emit({ status: 'error', error: 'Previous attempt stopped' })
  })
}

for (const status of [409, 500]) test(`rejected repair HTTP ${status} preserves existing and live logs`, async () => {
  let finish!: (response: Response) => void
  const response = new Promise<Response>(resolve => { finish = resolve })
  await withCard(() => response, async card => {
    await previousLog(card)
    await act(async () => button(card.container).click())
    assert.ok(card.container.textContent?.includes('Existing repair evidence'), 'pending request erased the log')
    await act(async () => card.emit({ status: 'running', line: 'Current repair continues' }))
    await act(async () => finish(Response.json({ error: 'Storage repair is already in progress.' }, { status })))
    assert.ok(card.container.textContent?.includes('Existing repair evidence'))
    assert.ok(card.container.textContent?.includes('Current repair continues'))
    assert.ok(card.container.textContent?.includes('already in progress'))
    assert.ok(card.container.textContent?.includes('working'), 'rejection replaced the live repair phase')
  })
})

test('network failure preserves the prior repair log', async () => {
  let fail!: (error: Error) => void
  const response = new Promise<Response>((_resolve, reject) => { fail = reject })
  await withCard(() => response, async card => {
    await previousLog(card)
    await act(async () => button(card.container).click())
    await act(async () => fail(new Error('offline')))
    assert.ok(card.container.textContent?.includes('Existing repair evidence'))
    assert.ok(card.container.textContent?.includes('Could not reach the device'))
  })
})

for (const terminal of ['needs_force', 'reboot_required']) test(`accepted repair keeps early progress and ${terminal} received before HTTP completion`, async () => {
  let finish!: (response: Response) => void
  const response = new Promise<Response>(resolve => { finish = resolve })
  await withCard(() => response, async card => {
    await previousLog(card)
    await act(async () => button(card.container).click())
    assert.ok(card.container.textContent?.includes('Existing repair evidence'), 'old log cleared before acceptance')
    await act(async () => {
      card.emit({ status: 'running', line: 'New repair first line' })
      card.emit({ status: terminal, message: 'Early terminal result' })
    })
    await act(async () => finish(Response.json({ status: 'started' })))
    assert.ok(!card.container.textContent?.includes('Existing repair evidence'))
    assert.ok(!card.container.textContent?.includes('Previous attempt stopped'))
    assert.ok(card.container.textContent?.includes('New repair first line'))
    assert.ok(card.container.textContent?.includes('Early terminal result'))
    assert.ok(!card.container.textContent?.includes('working'), 'acceptance overwrote a terminal event')
    assert.deepEqual(JSON.parse(String(card.requests[0].body)), { confirm_destructive: false })
  })
})

test('two immediate clicks submit only one repair request', async () => {
  let finish!: (response: Response) => void
  const response = new Promise<Response>(resolve => { finish = resolve })
  await withCard(() => response, async card => {
    const start = button(card.container)
    await act(async () => { start.click(); start.click() })
    assert.equal(card.requests.length, 1)
    await act(async () => finish(Response.json({ status: 'started' })))
  })
})

test('rejected force request preserves the log and confirmation context', async () => {
  await withCard(async () => Response.json({ error: 'Already in progress' }, { status: 409 }), async card => {
    await act(async () => {
      card.emit({ status: 'running', line: 'Evidence before force confirmation' })
      card.emit({ status: 'needs_force', message: 'Force repair may lose recent metadata.' })
    })
    await act(async () => button(card.container, 'Force repair (destroys XFS log)').click())
    assert.deepEqual(JSON.parse(String(card.requests[0].body)), { confirm_destructive: true })
    assert.ok(card.container.textContent?.includes('Evidence before force confirmation'))
    assert.ok(card.container.textContent?.includes('Force repair may lose recent metadata.'))
    assert.ok(card.container.textContent?.includes('Already in progress'))
  })
})

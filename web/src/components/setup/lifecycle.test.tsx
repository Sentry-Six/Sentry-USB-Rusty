import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, StrictMode } from 'react'
import { Window } from 'happy-dom'

async function setup() {
  const win = new Window({ url: 'http://localhost/' })
  const values = { window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true }
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

test('storage discovery ignores StrictMode stale reads and keeps the configured drive on failure without saving', async () => {
  const dom = await setup()
  const { StorageStep } = await import('./steps/StorageStep.tsx')
  const reads: { signal: AbortSignal; resolve: (response: Response) => void }[] = []
  const writes: unknown[] = []
  globalThis.fetch = (_input, init) => new Promise(resolve => {
    assert.equal(init?.method, undefined)
    reads.push({ signal: init?.signal as AbortSignal, resolve })
  })
  try {
    await act(async () => dom.root.render(createElement(StrictMode, {}, createElement(StorageStep, {
      data: { DATA_DRIVE: '/dev/sda', CAM_SIZE: '40G' }, onChange: (...args) => writes.push(args), onBatchChange: (...args) => writes.push(args),
    }))))
    assert.equal(reads.length, 2)
    assert.equal(reads[0].signal.aborted, true)
    await act(async () => reads[1].resolve(Response.json([{ path: '/dev/sda', name: 'Current SSD', size_gb: '1000', model: 'Test' }])))
    await act(async () => reads[0].resolve(Response.json([{ path: '/dev/sda', name: 'Stale SSD', size_gb: '1000', model: 'Test' }])))
    assert.match(dom.container.textContent, /Current SSD/)
    assert.doesNotMatch(dom.container.textContent, /Stale SSD/)
    const refresh = [...dom.container.querySelectorAll('button')].find(button => button.textContent === 'Refresh')!
    await act(async () => refresh.click())
    await act(async () => reads[2].resolve(new Response('', { status: 503 })))
    assert.match(dom.container.querySelector('[role="alert"]')!.textContent, /Could not read storage devices/)
    assert.match(dom.container.textContent, /Current SSD/)
    assert.equal(refresh.disabled, false)
    assert.deepEqual(writes, [])
  } finally { await dom.cleanup() }
})

test('backup discovery starts only on request and a failed read remains retryable instead of claiming no backups', async () => {
  const dom = await setup()
  const { WelcomeStep } = await import('./steps/WelcomeStep.tsx')
  const reads: { signal: AbortSignal; resolve: (response: Response) => void }[] = []
  const writes: unknown[] = []
  globalThis.fetch = (_input, init) => new Promise(resolve => {
    assert.equal(init?.method, undefined)
    reads.push({ signal: init?.signal as AbortSignal, resolve })
  })
  const button = (label: string) => [...dom.container.querySelectorAll('button')].find(item => item.textContent.trim() === label)!
  try {
    await act(async () => dom.root.render(createElement(StrictMode, {}, createElement(WelcomeStep, {
      data: {}, onChange: (...args) => writes.push(args), onBatchChange: (...args) => writes.push(args),
    }))))
    assert.equal(reads.length, 0)
    await act(async () => button('Restore from backup').click())
    await act(async () => dom.container.querySelector<HTMLButtonElement>('[aria-label="Close backup list"]')!.click())
    assert.equal(reads[0].signal.aborted, true)
    await act(async () => button('Restore from backup').click())
    await act(async () => reads[1].resolve(new Response('', { status: 503 })))
    await act(async () => reads[0].resolve(Response.json([])))
    assert.match(dom.container.querySelector('[role="alert"]')!.textContent, /Could not load backups/)
    assert.doesNotMatch(dom.container.textContent, /No backups found/)
    assert.ok(button('Upload backup file from your computer'))
    await act(async () => button('Retry').click())
    assert.match(dom.container.textContent, /Scanning for backups/)
    await act(async () => reads[2].resolve(Response.json([])))
    assert.equal(dom.container.querySelector('[role="alert"]'), null)
    assert.match(dom.container.textContent, /No backups found/)
    assert.deepEqual(writes, [])
  } finally { await dom.cleanup() }
})

test('size inputs derive changed props without saving and preserve the focused draft until blur', async () => {
  const dom = await setup()
  const { SizeInput } = await import('./SizeInput.tsx')
  const writes: unknown[] = []
  const render = (value: string) => dom.root.render(createElement(StrictMode, {}, createElement(SizeInput, {
    label: 'Music', field: 'MUSIC_SIZE', data: { MUSIC_SIZE: value }, defaultVal: '', hint: '', onChange: (...args) => writes.push(args),
  })))
  try {
    await act(async () => render('128M'))
    const input = dom.container.querySelector('input')!
    assert.equal(input.value, '128')
    assert.match(dom.container.querySelector('[aria-label="Music units"]')!.textContent, /MB/)
    await act(async () => render('2G'))
    assert.equal(input.value, '2')
    assert.match(dom.container.querySelector('[aria-label="Music units"]')!.textContent, /GB/)
    await act(async () => input.focus())
    await act(async () => {
      Object.getOwnPropertyDescriptor(dom.win.HTMLInputElement.prototype, 'value')!.set!.call(input, '7')
      input.dispatchEvent(new dom.win.Event('input', { bubbles: true }))
    })
    await act(async () => render('3G'))
    assert.equal(input.value, '7', 'external refresh must not erase the active draft')
    assert.deepEqual(writes, [], 'render, focus and typing must not commit the setting')
    await act(async () => input.blur())
    assert.deepEqual(writes, [['MUSIC_SIZE', '7G']])
    await act(async () => render('64M'))
    await act(async () => render(''))
    assert.match(dom.container.querySelector('[aria-label="Music units"]')!.textContent, /MB/, 'clearing an optional size retains the chosen units')
  } finally { await dom.cleanup() }
})

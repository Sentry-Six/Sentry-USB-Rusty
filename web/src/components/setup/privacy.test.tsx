import assert from 'node:assert/strict'
import test from 'node:test'
import { register } from 'node:module'
import { act, createElement, StrictMode } from 'react'
import { Window } from 'happy-dom'

// Setup imports a map used by another step; CSS rendering is covered in browser checks.
register(`data:text/javascript,${encodeURIComponent("export async function load(url, context, nextLoad) { return url.endsWith('.css') ? { format: 'module', source: '', shortCircuit: true } : nextLoad(url, context) }")}`, import.meta.url)

async function setup() {
  const win = new Window({ url: 'http://localhost/' })
  const values = {
    window: win, document: win.document, navigator: win.navigator, sessionStorage: win.sessionStorage,
    HTMLElement: win.HTMLElement, requestAnimationFrame: (callback: FrameRequestCallback) => setTimeout(() => callback(0), 0),
    IS_REACT_ACT_ENVIRONMENT: true,
  }
  const descriptors = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const oldFetch = globalThis.fetch
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div')
  win.document.body.append(container)
  const root = createRoot(container)
  const button = (label: string) => [...win.document.querySelectorAll('button')].find(item => item.textContent.trim() === label)!
  return { win, root, button, async cleanup() {
    await act(async () => root.unmount())
    globalThis.fetch = oldFetch
    for (const [key, descriptor] of descriptors) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  } }
}

for (const choice of [true, false]) test(`new setup requires an explicit ${choice ? 'yes' : 'no'} privacy choice to save before continuing`, async () => {
  const dom = await setup()
  const { SetupWizard } = await import('./SetupWizard.tsx')
  const writes: { value: boolean }[] = []
  let finishSave: ((response: Response) => void) | undefined
  globalThis.fetch = async (input, init) => {
    const url = String(input)
    if (init?.method === 'PUT') {
      assert.equal(url, '/api/config/preference')
      writes.push(JSON.parse(String(init.body)))
      return new Promise(resolve => { finishSave = resolve })
    }
    if (url === '/api/setup/status') return Response.json({ setup_finished: false })
    return Response.json(url.includes('/api/config/preference') ? { value: null } : {})
  }
  try {
    await act(async () => dom.root.render(createElement(StrictMode, {}, createElement(SetupWizard, { onClose() {}, initialStepId: 'privacy' }))))
    const yes = dom.button('Yes, count me')
    const no = dom.button('No thanks')
    assert.equal(yes.getAttribute('aria-pressed'), 'false')
    assert.equal(no.getAttribute('aria-pressed'), 'false')
    assert.equal(yes.className, no.className, 'unselected choices receive equal emphasis')
    assert.equal(writes.length, 0, 'mounting must never opt a device in or out')
    assert.equal(dom.button('Next').disabled, true)
    assert.equal(dom.win.document.querySelector('details')!.open, false)
    assert.ok(yes.compareDocumentPosition(dom.win.document.querySelector('details')!) & dom.win.Node.DOCUMENT_POSITION_FOLLOWING, 'choices precede expanded disclosure')
    await act(async () => dom.win.document.querySelector<HTMLButtonElement>('[aria-label="Step 11: Review"]')!.click())
    assert.ok(dom.button('Yes, count me'), 'header navigation must not skip the choice')
    await act(async () => (choice ? yes : no).click())
    assert.deepEqual(writes, [{ key: 'analytics_opt_in', value: choice }])
    assert.equal(dom.button('Next').disabled, true, 'pending writes cannot count as a saved choice')
    await act(async () => finishSave!(new Response('', { status: 500 })))
    assert.equal(dom.button('Next').disabled, true)
    assert.match(dom.win.document.querySelector('[role="alert"]')!.textContent, /Couldn't save preference/)
    assert.equal(yes.getAttribute('aria-pressed'), 'false')
    assert.equal(no.getAttribute('aria-pressed'), 'false')
    await act(async () => (choice ? yes : no).click())
    await act(async () => finishSave!(Response.json({ ok: true })))
    assert.equal(dom.button('Next').disabled, false)
    await act(async () => dom.button('Next').click())
    assert.match(dom.win.document.body.textContent, /Step 3 of 11 · Network/)
  } finally { await dom.cleanup() }
})

test('Apply cannot bypass a missing new-install privacy choice', async () => {
  const dom = await setup()
  const { SetupWizard } = await import('./SetupWizard.tsx')
  const writes: string[] = []
  globalThis.fetch = async (input, init) => {
    if (init?.method) writes.push(String(input))
    if (String(input) === '/api/setup/status') return Response.json({ setup_finished: false })
    return Response.json(String(input).includes('/api/config/preference') ? { value: null } : {})
  }
  try {
    await act(async () => dom.root.render(createElement(SetupWizard, { onClose() {}, initialStepId: 'review', initialData: { ARCHIVE_SYSTEM: 'none' } })))
    await act(async () => {
      dom.button('Apply & Run Setup').click()
      await new Promise(resolve => setTimeout(resolve, 5))
    })
    assert.match(dom.win.document.body.textContent, /Step 2 of 11 · Privacy/)
    assert.deepEqual(writes, [])
    await act(async () => dom.button('No thanks').click())
    assert.equal(dom.button('Next').disabled, false)
    assert.doesNotMatch(dom.win.document.body.textContent, /Choose Yes, count me or No thanks to continue/)
    assert.deepEqual(writes, ['/api/config/preference'])
  } finally { await dom.cleanup() }
})

for (const choice of [true, false]) test(`saved ${choice ? 'opt-in' : 'opt-out'} is preserved when setup is reopened`, async () => {
  const dom = await setup()
  const { SetupWizard } = await import('./SetupWizard.tsx')
  globalThis.fetch = async (input, init) => {
    assert.equal(init?.method, undefined, 'loading a preference must not write it')
    if (String(input) === '/api/setup/status') return Response.json({ setup_finished: false })
    return Response.json(String(input).includes('analytics_opt_in') ? { value: choice } : {})
  }
  try {
    await act(async () => dom.root.render(createElement(SetupWizard, { onClose() {}, initialStepId: 'privacy' })))
    assert.equal(dom.button(choice ? 'Yes, count me' : 'No thanks').getAttribute('aria-pressed'), 'true')
    assert.equal(dom.button('Next').disabled, false)
  } finally { await dom.cleanup() }
})

test('completed installations can continue without a recorded analytics preference', async () => {
  const dom = await setup()
  const { SetupWizard } = await import('./SetupWizard.tsx')
  globalThis.fetch = async (input, init) => {
    assert.equal(init?.method, undefined)
    if (String(input) === '/api/setup/status') return Response.json({ setup_finished: true })
    return Response.json(String(input).includes('/api/config/preference') ? { value: null } : {})
  }
  try {
    await act(async () => dom.root.render(createElement(SetupWizard, { onClose() {}, initialStepId: 'privacy' })))
    assert.equal(dom.button('Next').disabled, false)
    assert.equal(dom.button('Yes, count me').getAttribute('aria-pressed'), 'false')
    assert.equal(dom.button('No thanks').getAttribute('aria-pressed'), 'false')
    await act(async () => dom.button('Next').click())
    assert.match(dom.win.document.body.textContent, /Step 3 of 11 · Network/)
  } finally { await dom.cleanup() }
})

test('privacy read failures are retryable and cannot silently become a saved opt-out', async () => {
  const dom = await setup()
  const { PrivacyStep } = await import('./steps/PrivacyStep.tsx')
  const changes: unknown[] = []
  let loaded = false
  globalThis.fetch = async (_input, init) => {
    assert.equal(init?.method, undefined)
    if (!loaded) return new Response('', { status: 503 })
    return Response.json({ value: false })
  }
  try {
    await act(async () => dom.root.render(createElement(PrivacyStep, {
      data: {}, setupAlreadyFinished: false, onChange: (...args) => changes.push(args), onBatchChange() {},
    })))
    assert.match(dom.win.document.querySelector('[role="alert"]')!.textContent, /Couldn't load/)
    assert.equal(dom.button('Yes, count me').getAttribute('aria-pressed'), 'false')
    assert.equal(dom.button('No thanks').getAttribute('aria-pressed'), 'false')
    assert.deepEqual(changes, [])
    loaded = true
    await act(async () => dom.button('Reload saved choice').click())
    assert.equal(dom.win.document.querySelector('[role="alert"]'), null)
    assert.deepEqual(changes, [['_analytics_choice', 'false']])
    assert.equal(dom.button('No thanks').getAttribute('aria-pressed'), 'true')
  } finally { await dom.cleanup() }
})

test('an obsolete StrictMode privacy response cannot override the current saved choice', async () => {
  const dom = await setup()
  const { PrivacyStep } = await import('./steps/PrivacyStep.tsx')
  const reads: { signal: AbortSignal; resolve: (response: Response) => void }[] = []
  const changes: unknown[] = []
  globalThis.fetch = (_input, init) => new Promise(resolve => {
    reads.push({ signal: init?.signal as AbortSignal, resolve })
  })
  try {
    await act(async () => dom.root.render(createElement(StrictMode, {}, createElement(PrivacyStep, {
      data: {}, setupAlreadyFinished: false, onChange: (...args) => changes.push(args), onBatchChange() {},
    }))))
    assert.equal(reads.length, 2)
    assert.equal(reads[0].signal.aborted, true)
    await act(async () => reads[1].resolve(Response.json({ value: false })))
    await act(async () => reads[0].resolve(Response.json({ value: true })))
    assert.deepEqual(changes, [['_analytics_choice', 'false']])
    assert.equal(dom.button('No thanks').getAttribute('aria-pressed'), 'true')
  } finally { await dom.cleanup() }
})

test('a stalled privacy read times out so either explicit choice remains available', async (context) => {
  const dom = await setup()
  const { PrivacyStep } = await import('./steps/PrivacyStep.tsx')
  let signal: AbortSignal | undefined
  globalThis.fetch = (_input, init) => new Promise((_resolve, reject) => {
    signal = init?.signal as AbortSignal
    signal.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError')), { once: true })
  })
  const schedule = globalThis.setTimeout
  let expire: (() => void) | undefined
  context.mock.method(globalThis, 'setTimeout', (callback: () => void, delay?: number) => {
    if (delay === 10_000) expire = callback
    return schedule(callback, delay)
  })
  try {
    await act(async () => dom.root.render(createElement(PrivacyStep, {
      data: {}, setupAlreadyFinished: false, onChange() {}, onBatchChange() {},
    })))
    assert.equal(dom.button('Yes, count me').disabled, true)
    await act(async () => expire!())
    assert.equal(signal?.aborted, true)
    assert.equal(dom.button('Yes, count me').disabled, false)
    assert.equal(dom.button('No thanks').disabled, false)
    assert.match(dom.win.document.querySelector('[role="alert"]')!.textContent, /Couldn't load/)
  } finally {
    context.mock.restoreAll()
    await dom.cleanup()
  }
})

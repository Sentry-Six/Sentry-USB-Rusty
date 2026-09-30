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

interface ApiOptions {
  finished?: boolean
  preference?: { value: unknown; is_set?: boolean }
  status?: () => Promise<Response>
  read?: () => Promise<Response>
  write?: (body: Record<string, unknown>) => Promise<Response>
}
function mockApi(options: ApiOptions = {}) {
  const writes: Record<string, unknown>[] = []
  globalThis.fetch = async (input, init) => {
    const url = String(input)
    if (init?.method === 'PUT') {
      assert.equal(url, '/api/config/preference')
      const body = JSON.parse(String(init.body))
      writes.push(body)
      return options.write ? options.write(body) : Response.json({ success: true, value: body.value, is_set: true })
    }
    assert.equal(init?.method, undefined)
    if (url === '/api/setup/status') return options.status ? options.status() : Response.json({ setup_finished: options.finished ?? false })
    if (url.includes('analytics_opt_in')) return options.read ? options.read() : Response.json(options.preference ?? { value: null, is_set: false })
    return Response.json({})
  }
  return writes
}
async function wizard(dom: Awaited<ReturnType<typeof setup>>, props: Record<string, unknown> = {}, strict = false) {
  const { SetupWizard } = await import('./SetupWizard.tsx')
  const element = createElement(SetupWizard, { onClose() {}, initialStepId: 'privacy', ...props })
  await act(async () => dom.root.render(strict ? createElement(StrictMode, {}, element) : element))
}
function step(dom: Awaited<ReturnType<typeof setup>>, label: string) {
  return dom.win.document.querySelector<HTMLButtonElement>(`[aria-label="${label}"]`)!
}

for (const header of [false, true]) test(`new default saves only on forward ${header ? 'step-header' : 'Next'} navigation, once`, async () => {
  const dom = await setup()
  let finishSave: ((response: Response) => void) | undefined
  const writes = mockApi({ write: () => new Promise(resolve => { finishSave = resolve }) })
  try {
    await wizard(dom, {}, true)
    const yes = dom.button('Yes, count me')
    const no = dom.button('No thanks')
    assert.equal(yes.getAttribute('aria-pressed'), 'true')
    assert.equal(no.getAttribute('aria-pressed'), 'false')
    assert.equal(yes.disabled, false)
    assert.equal(no.disabled, false)
    assert.ok(yes.className.includes('flex-1') && no.className.includes('flex-1'))
    assert.equal(writes.length, 0, 'mounting and reading must never enable reporting')
    assert.equal(dom.win.document.querySelector('details')!.open, false)
    assert.ok(yes.compareDocumentPosition(dom.win.document.querySelector('details')!) & dom.win.Node.DOCUMENT_POSITION_FOLLOWING)
    assert.match(dom.win.document.body.textContent, /On for new installations when you continue/)
    await act(async () => (header ? step(dom, 'Step 3: Network') : dom.button('Next')).click())
    assert.deepEqual(writes, [{ key: 'analytics_opt_in', value: true, only_if_unset: true }])
    assert.match(dom.win.document.body.textContent, /Step 2 of 11 · Privacy/)
    assert.equal(dom.button('Next').disabled, true)
    assert.equal(dom.button('Cancel').disabled, true)
    await act(async () => step(dom, 'Step 3: Network').click())
    assert.equal(writes.length, 1)
    await act(async () => finishSave!(Response.json({ success: true, value: true, is_set: true, applied: true })))
    assert.match(dom.win.document.body.textContent, /Step 3 of 11 · Network/)
  } finally { await dom.cleanup() }
})

for (const choice of [false, true]) test(`explicit ${choice ? 'Yes' : 'No'} stays a draft until Continue`, async () => {
  const dom = await setup()
  const writes = mockApi()
  try {
    await wizard(dom)
    await act(async () => dom.button(choice ? 'Yes, count me' : 'No thanks').click())
    assert.equal(writes.length, 0)
    assert.match(dom.win.document.body.textContent, /Your choice is saved when you continue/)
    await act(async () => dom.button('Next').click())
    assert.deepEqual(writes, [{ key: 'analytics_opt_in', value: choice }])
    assert.match(dom.win.document.body.textContent, /Step 3 of 11 · Network/)
  } finally { await dom.cleanup() }
})

test('Back and Cancel do not activate reporting or discard an explicit No draft', async () => {
  const dom = await setup()
  const writes = mockApi()
  let closed = false
  try {
    await wizard(dom, { onClose() { closed = true } })
    await act(async () => dom.button('No thanks').click())
    await act(async () => dom.button('Back').click())
    assert.match(dom.win.document.body.textContent, /Step 1 of 11 · Welcome/)
    await act(async () => dom.button('Next').click())
    assert.equal(dom.button('No thanks').getAttribute('aria-pressed'), 'true')
    await act(async () => dom.button('Yes, count me').click())
    await act(async () => dom.button('Cancel').click())
    assert.equal(closed, true)
    assert.deepEqual(writes, [])
  } finally { await dom.cleanup() }
})

for (const initialStepId of ['welcome', 'review']) test(`${initialStepId === 'review' ? 'Apply' : 'a header jump'} cannot skip the new-install Privacy notice`, async () => {
  const dom = await setup()
  const writes = mockApi()
  try {
    await wizard(dom, { initialStepId, initialData: { ARCHIVE_SYSTEM: 'none', _analytics_choice: 'true' } })
    await act(async () => {
      (initialStepId === 'review' ? dom.button('Apply & Run Setup') : step(dom, 'Step 11: Review')).click()
      await new Promise(resolve => setTimeout(resolve, 5))
    })
    assert.match(dom.win.document.body.textContent, /Step 2 of 11 · Privacy/)
    assert.deepEqual(writes, [])
  } finally { await dom.cleanup() }
})

for (const finished of [false, true]) for (const choice of [false, true]) test(`existing ${choice ? 'Yes' : 'No'} is preserved on ${finished ? 'completed' : 'unfinished'} setup`, async () => {
  const dom = await setup()
  const writes = mockApi({ finished, preference: { value: choice, is_set: true } })
  try {
    await wizard(dom)
    assert.equal(dom.button(choice ? 'Yes, count me' : 'No thanks').getAttribute('aria-pressed'), 'true')
    await act(async () => dom.button('Next').click())
    assert.match(dom.win.document.body.textContent, /Step 3 of 11 · Network/)
    assert.deepEqual(writes, [])
  } finally { await dom.cleanup() }
})

test('completed installations with no preference remain off without a write', async () => {
  const dom = await setup()
  const writes = mockApi({ finished: true })
  try {
    await wizard(dom)
    assert.equal(dom.button('Yes, count me').getAttribute('aria-pressed'), 'false')
    assert.equal(dom.button('No thanks').getAttribute('aria-pressed'), 'false')
    await act(async () => dom.button('Next').click())
    assert.match(dom.win.document.body.textContent, /Step 3 of 11 · Network/)
    assert.deepEqual(writes, [])
  } finally { await dom.cleanup() }
})

test('unresolved setup status never briefly selects the new-install default for an existing device', async () => {
  const dom = await setup()
  let finishStatus: ((response: Response) => void) | undefined
  const writes = mockApi({ status: () => new Promise(resolve => { finishStatus = resolve }) })
  try {
    await wizard(dom)
    assert.equal(dom.button('Yes, count me').getAttribute('aria-pressed'), 'false')
    assert.equal(dom.button('Next').disabled, true)
    await act(async () => step(dom, 'Step 3: Network').click())
    assert.deepEqual(writes, [])
    await act(async () => finishStatus!(Response.json({ setup_finished: true })))
    assert.equal(dom.button('Yes, count me').getAttribute('aria-pressed'), 'false')
    await act(async () => dom.button('Next').click())
    assert.deepEqual(writes, [])
    assert.match(dom.win.document.body.textContent, /Step 3 of 11 · Network/)
  } finally { await dom.cleanup() }
})

for (const invalid of [false, true]) test(`${invalid ? 'invalid' : 'failed'} setup status cannot turn on the default and can be retried`, async () => {
  const dom = await setup()
  let healthy = false
  const writes = mockApi({ status: async () => healthy ? Response.json({ setup_finished: false }) : invalid ? Response.json({}) : new Response('', { status: 503 }) })
  try {
    await wizard(dom)
    assert.equal(dom.button('Yes, count me').getAttribute('aria-pressed'), 'false')
    assert.equal(dom.button('Next').disabled, true)
    assert.match(dom.win.document.body.textContent, /Couldn't confirm this device/)
    healthy = true
    await act(async () => dom.button('Retry setup status').click())
    assert.equal(dom.button('Yes, count me').getAttribute('aria-pressed'), 'true')
    assert.deepEqual(writes, [])
  } finally { await dom.cleanup() }
})

test('failed privacy reads do not select or save the default', async () => {
  const dom = await setup()
  let healthy = false
  const writes = mockApi({ read: async () => healthy ? Response.json({ value: null, is_set: false }) : new Response('', { status: 503 }) })
  try {
    await wizard(dom)
    assert.equal(dom.button('Yes, count me').getAttribute('aria-pressed'), 'false')
    assert.equal(dom.button('Next').disabled, true)
    await act(async () => step(dom, 'Step 3: Network').click())
    assert.deepEqual(writes, [])
    healthy = true
    await act(async () => dom.button('Reload saved choice').click())
    assert.equal(dom.button('Yes, count me').getAttribute('aria-pressed'), 'true')
    assert.deepEqual(writes, [])
  } finally { await dom.cleanup() }
})

for (const is_set of [undefined, true]) test(`${is_set ? 'present null' : 'legacy missing is_set'} preference cannot be treated as unset`, async () => {
  const dom = await setup()
  const writes = mockApi({ preference: { value: null, is_set } })
  try {
    await wizard(dom)
    assert.equal(dom.button('Yes, count me').getAttribute('aria-pressed'), 'false')
    await act(async () => dom.button('Next').click())
    assert.match(dom.win.document.body.textContent, /Step 2 of 11 · Privacy/)
    assert.deepEqual(writes, [])
    await act(async () => dom.button('No thanks').click())
    await act(async () => dom.button('Next').click())
    assert.deepEqual(writes, [{ key: 'analytics_opt_in', value: false }])
  } finally { await dom.cleanup() }
})

for (const restored of ['false', 'true']) for (const choice of [false, true]) test(`restored string ${restored} is never enabled implicitly and can be explicitly repaired to ${choice}`, async () => {
  const dom = await setup()
  const writes = mockApi({ finished: true, preference: { value: restored, is_set: true } })
  try {
    await wizard(dom)
    assert.equal(dom.button('Yes, count me').getAttribute('aria-pressed'), 'false')
    assert.equal(dom.button('No thanks').getAttribute('aria-pressed'), 'false')
    assert.equal(dom.button('Yes, count me').disabled, false)
    assert.equal(dom.button('No thanks').disabled, false)
    await act(async () => dom.button('Next').click())
    assert.match(dom.win.document.body.textContent, /Step 2 of 11 · Privacy/)
    assert.deepEqual(writes, [])
    await act(async () => dom.button(choice ? 'Yes, count me' : 'No thanks').click())
    assert.deepEqual(writes, [])
    await act(async () => dom.button('Next').click())
    assert.deepEqual(writes, [{ key: 'analytics_opt_in', value: choice }])
    assert.match(dom.win.document.body.textContent, /Step 3 of 11 · Network/)
  } finally { await dom.cleanup() }
})

test('a concurrent saved No wins over accepting the displayed default', async () => {
  const dom = await setup()
  let value: boolean | null = null
  const writes = mockApi({
    read: async () => Response.json({ value, is_set: value !== null }),
    write: async () => { value = false; return Response.json({ success: true, value: false, is_set: true, applied: false }) },
  })
  try {
    await wizard(dom)
    await act(async () => dom.button('Next').click())
    assert.match(dom.win.document.body.textContent, /Step 3 of 11 · Network/)
    await act(async () => dom.button('Back').click())
    assert.equal(dom.button('No thanks').getAttribute('aria-pressed'), 'true')
    assert.deepEqual(writes, [{ key: 'analytics_opt_in', value: true, only_if_unset: true }])
  } finally { await dom.cleanup() }
})

for (const conditional of [false, true]) test(`${conditional ? 'default' : 'explicit No'} save failure stays on Privacy and retries without assuming success`, async () => {
  const dom = await setup()
  let healthy = false
  const writes = mockApi({ write: async body => healthy ? Response.json({ success: true, value: body.value, is_set: true }) : new Response('', { status: 500 }) })
  try {
    await wizard(dom)
    if (!conditional) await act(async () => dom.button('No thanks').click())
    await act(async () => dom.button('Next').click())
    assert.match(dom.win.document.body.textContent, /Step 2 of 11 · Privacy/)
    assert.match(dom.win.document.body.textContent, /Couldn't save preference/)
    assert.doesNotMatch(dom.win.document.querySelector('[role="status"]')!.textContent, /Saved:/)
    healthy = true
    await act(async () => dom.button('Next').click())
    assert.equal(writes.length, 2)
    assert.deepEqual(writes[0], writes[1])
    assert.match(dom.win.document.body.textContent, /Step 3 of 11 · Network/)
  } finally { await dom.cleanup() }
})

for (const response of [() => new Response('', { status: 409 }), () => Response.json({ success: true, value: null, is_set: true }), () => Response.json({ success: true }), () => Response.json({ success: false, value: true })]) test('an unconfirmed or no-longer-eligible default cannot advance', async () => {
  const dom = await setup()
  mockApi({ write: async () => response() })
  try {
    await wizard(dom)
    await act(async () => dom.button('Next').click())
    assert.match(dom.win.document.body.textContent, /Step 2 of 11 · Privacy/)
    assert.match(dom.win.document.body.textContent, /Couldn't save preference/)
  } finally { await dom.cleanup() }
})

test('an obsolete StrictMode privacy response cannot override a saved No', async () => {
  const dom = await setup()
  const { PrivacyStep } = await import('./steps/PrivacyStep.tsx')
  const reads: { signal: AbortSignal; resolve: (response: Response) => void }[] = []
  globalThis.fetch = (_input, init) => new Promise(resolve => { reads.push({ signal: init?.signal as AbortSignal, resolve }) })
  try {
    await act(async () => dom.root.render(createElement(StrictMode, {}, createElement(PrivacyStep, {
      data: {}, setupAlreadyFinished: false, setupStatusKnown: true, onChange() {}, onBatchChange() {},
    }))))
    assert.equal(reads.length, 2)
    assert.equal(reads[0].signal.aborted, true)
    await act(async () => reads[1].resolve(Response.json({ value: false, is_set: true })))
    await act(async () => reads[0].resolve(Response.json({ value: null, is_set: false })))
    assert.equal(dom.button('No thanks').getAttribute('aria-pressed'), 'true')
  } finally { await dom.cleanup() }
})

test('a stalled privacy read times out without selecting or enabling the default', async (context) => {
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
      data: {}, setupAlreadyFinished: false, setupStatusKnown: true, onChange() {}, onBatchChange() {},
    })))
    assert.equal(dom.button('Yes, count me').getAttribute('aria-pressed'), 'false')
    await act(async () => expire!())
    assert.equal(signal?.aborted, true)
    assert.equal(dom.button('Yes, count me').disabled, true)
    assert.equal(dom.button('Yes, count me').getAttribute('aria-pressed'), 'false')
    assert.match(dom.win.document.querySelector('[role="alert"]')!.textContent, /Couldn't load/)
  } finally { context.mock.restoreAll(); await dom.cleanup() }
})


test('a stalled privacy save restores navigation after a bounded timeout', async (context) => {
  const dom = await setup()
  const schedule = globalThis.setTimeout
  let expire: (() => void) | undefined
  context.mock.method(globalThis, 'setTimeout', (callback: () => void, delay?: number) => {
    if (delay === 10_000) expire = callback
    return schedule(callback, delay)
  })
  let aborted = false
  globalThis.fetch = async (input, init) => {
    if (init?.method === 'PUT') return new Promise((_resolve, reject) => {
      init.signal!.addEventListener('abort', () => { aborted = true; reject(new DOMException('Aborted', 'AbortError')) }, { once: true })
    })
    if (String(input) === '/api/setup/status') return Response.json({ setup_finished: false })
    return Response.json(String(input).includes('analytics_opt_in') ? { value: null, is_set: false } : {})
  }
  try {
    await wizard(dom)
    await act(async () => dom.button('Next').click())
    assert.equal(dom.button('Cancel').disabled, true)
    await act(async () => expire!())
    assert.equal(aborted, true)
    assert.match(dom.win.document.body.textContent, /Step 2 of 11 · Privacy/)
    assert.match(dom.win.document.body.textContent, /request timed out/)
    assert.equal(dom.button('Next').disabled, false)
    assert.equal(dom.button('Cancel').disabled, false)
  } finally { context.mock.restoreAll(); await dom.cleanup() }
})

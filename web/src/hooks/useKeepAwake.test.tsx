import assert from 'node:assert/strict'
import test from 'node:test'
import React, { act, createElement } from 'react'
import { Window } from 'happy-dom'
import { KeepAwakeProvider, useKeepAwake } from './useKeepAwake.tsx'

function Controls() {
  const state = useKeepAwake()
  return <>
    <p data-mode>{state.mode}</p><p data-status>{state.status.state}</p><p role="alert">{state.error}</p>
    <button onClick={() => void state.updateMode('')}>Off</button>
    <button onClick={() => void state.stop()}>Stop</button>
    <button onClick={() => void state.start(30)}>Start</button>
  </>
}
test('failed keep-awake mutations preserve server-confirmed mode and status', async () => {
  const win = new Window({ url: 'http://localhost/' })
  const descriptors = ['window', 'document', 'navigator', 'IS_REACT_ACT_ENVIRONMENT'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries({ window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true })) Object.defineProperty(globalThis, key, { configurable: true, value })
  const oldFetch = globalThis.fetch
  let failStop = true
  globalThis.fetch = async (input, init) => {
    if (init?.method === 'DELETE') return new Response('', { status: failStop ? 500 : 200 })
    if (init?.method) return new Response('', { status: 500 })
    return String(input).includes('/preference') ? Response.json({ value: 'manual' }) : Response.json({ state: 'active', mode: 'manual' })
  }
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div')
  win.document.body.append(container)
  const root = createRoot(container)
  const click = async (label: string) => act(async () => [...container.querySelectorAll('button')].find(button => button.textContent === label)!.click())
  try {
    await act(async () => root.render(createElement(KeepAwakeProvider, { children: createElement(Controls) })))
    assert.equal(container.querySelector('[data-status]')?.textContent, 'active')
    await click('Off')
    assert.equal(container.querySelector('[data-mode]')?.textContent, 'manual')
    assert.ok(container.querySelector('[role="alert"]')?.textContent.includes('500'))
    await click('Stop')
    assert.equal(container.querySelector('[data-status]')?.textContent, 'active')
    failStop = false
    await click('Stop')
    assert.equal(container.querySelector('[data-status]')?.textContent, 'idle')
    await click('Start')
    assert.equal(container.querySelector('[data-status]')?.textContent, 'idle')
  } finally {
    await act(async () => root.unmount()); globalThis.fetch = oldFetch
    for (const [key, descriptor] of descriptors) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
})

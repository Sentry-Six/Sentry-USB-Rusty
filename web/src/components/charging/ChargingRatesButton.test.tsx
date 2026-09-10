import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { Window } from 'happy-dom'
import { ChargingRatesButton } from './ChargingRatesButton.tsx'

test('failed save stays open with an error; reopening reloads the rate snapshot', async () => {
  const testWindow = new Window({ url: 'http://localhost/' })
  const descriptors = ['window', 'document', 'navigator', 'IS_REACT_ACT_ENVIRONMENT'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries({ window: testWindow, document: testWindow.document, navigator: testWindow.navigator, IS_REACT_ACT_ENVIRONMENT: true })) {
    Object.defineProperty(globalThis, key, { configurable: true, value })
  }
  const oldFetch = globalThis.fetch
  let reads = 0, writes = 0, saved = 0
  globalThis.fetch = async (_url, options) => {
    if (options?.method === 'PUT') { writes++; return new Response('', { status: 500 }) }
    reads++
    return Response.json({ document: { charging_currency: 'CAD', charging_default_rate: 0.2 } })
  }
  const { createRoot } = await import('react-dom/client')
  const container = testWindow.document.createElement('div')
  testWindow.document.body.append(container)
  const root = createRoot(container)
  const button = (text: string) => {
    const found = Array.from(container.querySelectorAll('button')).find(button => button.textContent?.trim() === text)
    assert.ok(found, `missing button ${text}`)
    return found
  }
  try {
    await act(async () => root.render(createElement(ChargingRatesButton, { tags: [], onSaved: () => saved++ })))
    await act(async () => button('Rates').click())
    assert.equal(reads, 2)
    await act(async () => button('Save rates').click())
    assert.equal(writes, 1)
    assert.equal(saved, 0)
    assert.match(container.querySelector('[role="alert"]')?.textContent ?? '', /could not be confirmed/)
    assert.ok(container.querySelector('fieldset'))
    await act(async () => button('Cancel').click())
    await act(async () => button('Rates').click())
    assert.equal(reads, 3)
    assert.equal(container.querySelector('[role="alert"]'), null)
  } finally {
    await act(async () => root.unmount())
    globalThis.fetch = oldFetch
    for (const [key, descriptor] of descriptors) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor)
      else Reflect.deleteProperty(globalThis, key)
    }
    testWindow.close()
  }
})

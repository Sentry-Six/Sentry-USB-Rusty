import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, StrictMode } from 'react'
import { Window } from 'happy-dom'
import { PrefCard } from './PrefCard.tsx'
import { InfoButton } from '../ui/InfoButton.tsx'

test('unavailable controls stay inert while feature help and the setup action remain accessible', async () => {
  const win = new Window({ url: 'http://localhost/settings' })
  const descriptors = ['window', 'document', 'navigator', 'IS_REACT_ACT_ENVIRONMENT'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries({ window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true })) Object.defineProperty(globalThis, key, { configurable: true, value })
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div'); win.document.body.append(container)
  const root = createRoot(container)
  let setup = 0
  try {
    await act(async () => root.render(createElement(StrictMode, {}, createElement(PrefCard, {
      icon: null, title: 'Keep Accessory', disabled: { reason: 'Not configured', cta: { label: 'Open Setup Wizard', onClick: () => { setup++ } } },
      help: createElement(InfoButton, { title: 'Accessory power', children: 'Help content' }), children: createElement('input', { 'aria-label': 'Unavailable value' }),
    }))))
    const help = container.querySelector<HTMLButtonElement>('[aria-label="About accessory power"]')!
    const cta = [...container.querySelectorAll('button')].find(button => button.textContent === 'Open Setup Wizard')!
    assert.equal(help.closest('[aria-disabled="true"], [inert]'), null)
    assert.equal(cta.closest('[aria-disabled="true"], [inert]'), null)
    assert.ok(container.querySelector('input')?.closest('[inert][hidden]'))
    await act(async () => cta.click())
    assert.equal(setup, 1)
    await act(async () => help.click())
    assert.ok(win.document.querySelector('[role="dialog"]')?.textContent.includes('Help content'))
  } finally {
    await act(async () => root.unmount())
    for (const [key, descriptor] of descriptors) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
})

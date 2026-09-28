import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { Window } from 'happy-dom'
import { Modal } from './Modal.tsx'

test('modal contains keyboard focus, labels itself, restores inert state and focus', async () => {
  const win = new Window({ url: 'http://localhost/' })
  const descriptors = ['window', 'document', 'navigator', 'IS_REACT_ACT_ENVIRONMENT'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries({ window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true })) Object.defineProperty(globalThis, key, { configurable: true, value })
  const { createRoot } = await import('react-dom/client')
  const app = win.document.createElement('div')
  const opener = win.document.createElement('button')
  const alreadyInert = win.document.createElement('div')
  alreadyInert.inert = true
  app.append(opener)
  win.document.body.append(app, alreadyInert)
  opener.focus()
  const root = createRoot(app.appendChild(win.document.createElement('div')))
  let closed = 0
  try {
    await act(async () => root.render(createElement(Modal, { title: 'Update status', onClose: () => { closed++ }, children: createElement('button', null, 'Last action') })))
    const dialog = win.document.querySelector('[role="dialog"]')!
    assert.equal(win.document.getElementById(dialog.getAttribute('aria-labelledby')!)?.textContent, 'Update status')
    assert.equal(app.inert, true)
    const buttons = dialog.querySelectorAll('button')
    assert.equal(win.document.activeElement, buttons[0])
    buttons[1].focus()
    win.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true }))
    assert.equal(win.document.activeElement, buttons[0])
    win.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'Tab', shiftKey: true, bubbles: true, cancelable: true }))
    assert.equal(win.document.activeElement, buttons[1])
    await act(async () => win.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'Escape', bubbles: true })))
    assert.equal(closed, 1)
    await act(async () => root.render(null))
    assert.equal(app.inert, false)
    assert.equal(alreadyInert.inert, true)
    assert.equal(win.document.activeElement, opener)
    assert.equal(win.document.body.style.overflow, '')
  } finally {
    await act(async () => root.unmount())
    for (const [key, descriptor] of descriptors) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
})

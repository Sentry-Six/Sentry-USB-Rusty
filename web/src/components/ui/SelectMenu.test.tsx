import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, StrictMode } from 'react'
import { Window } from 'happy-dom'
import { SelectMenu } from './SelectMenu.tsx'

async function mount(run: (container: HTMLElement, win: Window, choices: string[], render: (options: { value: string; label: string; group?: string }[]) => Promise<void>) => Promise<void>) {
  const win = new Window({ url: 'http://localhost/files' })
  const descriptors = ['window', 'document', 'navigator', 'IS_REACT_ACT_ENVIRONMENT'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries({ window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true })) Object.defineProperty(globalThis, key, { configurable: true, value })
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div'); win.document.body.append(container)
  const root = createRoot(container)
  const choices: string[] = []
  const render = async (options: { value: string; label: string; group?: string }[]) => act(async () => root.render(createElement(StrictMode, {}, createElement(SelectMenu, { label: 'File location', value: 'alpha', options, onChange: value => choices.push(value) }))))
  try { await run(container as unknown as HTMLElement, win, choices, render) }
  finally {
    await act(async () => root.unmount())
    for (const [key, descriptor] of descriptors) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
}
const options = [
  { value: 'alpha', label: 'Alpha', group: 'Media' }, { value: 'beta', label: 'Beta', group: 'Media' },
  { value: 'saved', label: 'Saved clips', group: 'Media' }, { value: 'system', label: 'System files', group: 'Advanced' },
]
async function press(trigger: HTMLElement, win: Window, key: string) {
  await act(async () => trigger.dispatchEvent(new win.KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }) as unknown as KeyboardEvent))
}
function active(container: HTMLElement) {
  const id = container.querySelector('[role="combobox"]')!.getAttribute('aria-activedescendant')
  return id ? container.ownerDocument.getElementById(id)?.getAttribute('data-value') : null
}

test('keyboard boundaries scroll active option into view, keep DOM focus, and commit only on Enter', async () => mount(async (container, win, choices, render) => {
  const scrolled: string[] = []
  win.HTMLElement.prototype.scrollIntoView = function() { scrolled.push(this.id) }
  await render(options)
  const trigger = container.querySelector<HTMLElement>('[role="combobox"]')!
  trigger.focus()
  await press(trigger, win, 'End')
  assert.equal(active(container), 'system')
  assert.equal(scrolled.at(-1), trigger.getAttribute('aria-activedescendant'))
  assert.equal(container.ownerDocument.activeElement, trigger)
  assert.deepEqual(choices, [])
  await press(trigger, win, 'Escape')
  assert.equal(trigger.getAttribute('aria-expanded'), 'false')
  assert.deepEqual(choices, [])
  await press(trigger, win, 'Home')
  assert.equal(active(container), 'alpha')
  await press(trigger, win, 'ArrowDown')
  await press(trigger, win, 'Enter')
  assert.deepEqual(choices, ['beta'])
  assert.equal(trigger.getAttribute('aria-expanded'), 'false')
  assert.equal(container.ownerDocument.activeElement, trigger)
}))

test('type-ahead supports prefixes and cycling, while Tab closes without changing selection', async () => mount(async (container, win, choices, render) => {
  await render(options)
  const trigger = container.querySelector<HTMLElement>('[role="combobox"]')!
  trigger.focus()
  await press(trigger, win, 's')
  assert.equal(active(container), 'saved')
  await press(trigger, win, 's')
  assert.equal(active(container), 'system')
  await press(trigger, win, 'Escape')
  await press(trigger, win, 'ArrowDown')
  await press(trigger, win, 's')
  await press(trigger, win, 'a')
  assert.equal(active(container), 'saved')
  assert.ok(container.ownerDocument.querySelector('[role="group"][aria-label="Advanced"]'))
  await press(trigger, win, 'Tab')
  assert.equal(trigger.getAttribute('aria-expanded'), 'false')
  assert.deepEqual(choices, [])
}))

test('empty menus have no invalid active descendant and losing focus closes the popup', async () => mount(async (container, win, choices, render) => {
  await render([])
  let trigger = container.querySelector<HTMLButtonElement>('[role="combobox"]')!
  assert.equal(trigger.disabled, true)
  await press(trigger, win, 'ArrowDown')
  assert.equal(trigger.getAttribute('aria-activedescendant'), null)
  assert.equal(container.querySelector('[role="listbox"]'), null)
  await render(options)
  trigger = container.querySelector<HTMLButtonElement>('[role="combobox"]')!
  trigger.focus()
  await press(trigger, win, 'ArrowDown')
  const outside = win.document.createElement('button'); win.document.body.append(outside)
  await act(async () => outside.focus())
  assert.equal(trigger.getAttribute('aria-expanded'), 'false')
  assert.deepEqual(choices, [])
}))

test('searchable portals stay inside the modal and Tab follows the original trigger order', async () => {
  const win = new Window({ url: 'http://localhost/settings' })
  const descriptors = ['window', 'document', 'navigator', 'IS_REACT_ACT_ENVIRONMENT'].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries({ window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true })) Object.defineProperty(globalThis, key, { configurable: true, value })
  const { createRoot } = await import('react-dom/client')
  const { Modal } = await import('./Modal.tsx')
  const container = win.document.createElement('div'); win.document.body.append(container)
  const root = createRoot(container)
  const choices: string[] = []; let closes = 0
  try {
    await act(async () => root.render(createElement(StrictMode, {}, createElement(Modal, {
      title: 'Settings', onClose: () => { closes++ }, children: [
        createElement('button', { key: 'before' }, 'Previous control'),
        createElement('div', { key: 'clip', style: { overflow: 'hidden' } }, createElement(SelectMenu, { label: 'Currency', value: 'alpha', options, searchable: true, onChange: value => choices.push(value) })),
        createElement('button', { key: 'after' }, 'Next control'),
      ],
    }))))
    const trigger = win.document.querySelector<HTMLButtonElement>('[aria-label="Currency"][data-select-trigger]')!
    trigger.focus()
    await act(async () => trigger.click())
    const popup = win.document.querySelector('[data-select-menu-popup]')!
    assert.equal(popup.parentElement?.getAttribute('role'), 'dialog', 'Portal must escape clipped card but remain inside focus/inert boundary')
    assert.equal(popup.closest('[inert]'), null)
    const search = popup.querySelector<HTMLInputElement>('input')!
    assert.equal(win.document.activeElement, search)
    await act(async () => search.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true })))
    assert.equal(win.document.activeElement?.textContent, 'Next control')
    assert.equal(win.document.querySelector('[data-select-menu-popup]'), null)
    await act(async () => trigger.click())
    await act(async () => win.document.querySelector('input')!.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'Tab', shiftKey: true, bubbles: true, cancelable: true })))
    assert.equal(win.document.activeElement?.textContent, 'Previous control')
    await act(async () => trigger.click())
    await act(async () => win.document.querySelector('input')!.dispatchEvent(new win.KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })))
    assert.equal(closes, 0)
    assert.equal(win.document.activeElement, trigger)
    await act(async () => trigger.click())
    await act(async () => win.document.querySelector<HTMLElement>('[role="option"][data-value="beta"]')!.click())
    assert.deepEqual(choices, ['beta'])
    assert.equal(closes, 0)
    assert.equal(win.document.activeElement, trigger)
  } finally {
    await act(async () => root.unmount())
    for (const [key, descriptor] of descriptors) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
})

test('disabled options are skipped and an inherited disabled fieldset blocks a portaled choice', async () => mount(async (container, win, choices, render) => {
  await render(options.map(option => option.value === 'beta' ? { ...option, disabled: true } : option))
  const trigger = container.querySelector<HTMLButtonElement>('[data-select-trigger]')!
  trigger.focus()
  await press(trigger, win, 'ArrowDown')
  await press(trigger, win, 'ArrowDown')
  assert.equal(active(container), 'saved')
  await act(async () => container.ownerDocument.querySelector<HTMLElement>('[data-value="beta"]')!.click())
  assert.deepEqual(choices, [])
  const fieldset = win.document.createElement('fieldset')
  container.parentElement!.insertBefore(fieldset, container as unknown as Node)
  fieldset.append(container as unknown as Node)
  fieldset.disabled = true
  await act(async () => win.document.querySelector<HTMLElement>('[data-value="saved"]')!.click())
  assert.deepEqual(choices, [])
  assert.equal(win.document.querySelector('[data-select-menu-popup]'), null)
}))

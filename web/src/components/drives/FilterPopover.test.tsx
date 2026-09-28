import assert from "node:assert/strict"
import test from "node:test"
import { act, createElement } from "react"
import { Window } from "happy-dom"
import { FilterPopover } from "./FilterPopover.tsx"

test("choosing a portaled tag menu does not dismiss the enclosing drive filter", async () => {
  const win = new Window({ url: "http://localhost/drives" })
  const saved = ["window", "document", "navigator", "IS_REACT_ACT_ENVIRONMENT"].map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries({ window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true })) Object.defineProperty(globalThis, key, { configurable: true, value })
  const { createRoot } = await import("react-dom/client")
  const container = win.document.createElement("div"); win.document.body.append(container)
  const root = createRoot(container)
  let selected: { tag?: string } | null = null
  const button = (text: string) => {
    const found = [...container.querySelectorAll("button")].find((button) => button.textContent?.trim() === text)
    assert.ok(found, `missing ${text}`)
    return found
  }
  try {
    await act(async () => root.render(createElement(FilterPopover, { drives: [], tags: ["Home", "Work"], filters: {}, metric: true, onChange: (value) => { selected = value } })))
    await act(async () => button("Filter").click())
    await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Tag"]')!.click())
    const option = win.document.querySelector<HTMLElement>('[role="option"][data-value="Work"]')!
    assert.ok(option)
    assert.equal(container.contains(option), false)
    await act(async () => {
      option.dispatchEvent(new win.MouseEvent("mousedown", { bubbles: true, cancelable: true }))
      option.click()
    })
    assert.equal(selected, null)
    await act(async () => button("Apply filters").click())
    assert.deepEqual(selected, { tag: "Work" })
  } finally {
    await act(async () => root.unmount())
    for (const [key, descriptor] of saved) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
})

import assert from "node:assert/strict"
import test from "node:test"
import { act, createElement, useState } from "react"
import { Window } from "happy-dom"
import { DateRangeCalendar } from "./DateRangeCalendar.tsx"
import { DatePopover } from "./DatePopover.tsx"
import type { DateRange } from "../../lib/date-range.ts"

async function environment(run: (container: HTMLElement, root: import("react-dom/client").Root, win: Window) => Promise<void>) {
  const win = new Window({ url: "http://localhost/drives" })
  const saved = ["window", "document", "navigator", "IS_REACT_ACT_ENVIRONMENT"].map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries({ window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true })) Object.defineProperty(globalThis, key, { configurable: true, value })
  const { createRoot } = await import("react-dom/client")
  const container = win.document.createElement("div"); win.document.body.append(container)
  const root = createRoot(container as unknown as HTMLElement)
  try { await run(container as unknown as HTMLElement, root, win) }
  finally {
    await act(async () => root.unmount())
    for (const [key, descriptor] of saved) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
}
function day(container: HTMLElement, value: string) {
  const found = container.querySelector<HTMLButtonElement>(`[data-date="${value}"]`)
  assert.ok(found, `missing calendar date ${value}`)
  return found
}

test("calendar keyboard moves across leap-month boundaries without committing selection", async () => environment(async (container, root, win) => {
  let changed = false
  await act(async () => root.render(createElement(DateRangeCalendar, { start: "2024-01-31", end: "2024-01-31", onChange: () => { changed = true } })))
  await act(async () => day(container, "2024-01-31").focus())
  await act(async () => win.document.activeElement!.dispatchEvent(new win.KeyboardEvent("keydown", { key: "PageDown", bubbles: true, cancelable: true })))
  assert.equal(win.document.activeElement?.getAttribute("data-date"), "2024-02-29")
  await act(async () => win.document.activeElement!.dispatchEvent(new win.KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true, cancelable: true })))
  assert.equal(win.document.activeElement?.getAttribute("data-date"), "2024-03-01")
  await act(async () => win.document.activeElement!.dispatchEvent(new win.KeyboardEvent("keydown", { key: "PageUp", shiftKey: true, bubbles: true, cancelable: true })))
  assert.equal(win.document.activeElement?.getAttribute("data-date"), "2023-03-01")
  assert.equal(changed, false)
  assert.equal(container.querySelectorAll('[data-date][tabindex="0"]').length, 1)
  assert.equal(container.querySelectorAll('th[scope="col"]').length, 7)
}))

test("calendar selects both range directions and same-day ranges with highlight", async () => environment(async (container, root) => {
  let selected: [string, string] = ["2024-02-01", "2024-02-02"]
  function Probe() {
    const [range, setRange] = useState(selected)
    return createElement(DateRangeCalendar, { start: range[0], end: range[1], onChange: (start, end) => { selected = [start, end]; setRange(selected) } })
  }
  await act(async () => root.render(createElement(Probe)))
  await act(async () => day(container, "2024-02-28").click())
  assert.deepEqual(selected, ["2024-02-28", ""])
  await act(async () => day(container, "2024-03-01").click())
  assert.deepEqual(selected, ["2024-02-28", "2024-03-01"])
  assert.equal(day(container, "2024-02-29").closest("td")?.getAttribute("aria-selected"), "true")
  await act(async () => day(container, "2024-03-02").click())
  await act(async () => day(container, "2024-02-29").click())
  assert.deepEqual(selected, ["2024-02-29", "2024-03-02"])
  await act(async () => day(container, "2024-02-29").click())
  await act(async () => day(container, "2024-02-29").click())
  assert.deepEqual(selected, ["2024-02-29", "2024-02-29"])
}))

test("date popover uses themed calendar, retains presets and applies a custom range", async () => environment(async (container, root, win) => {
  let chosen: DateRange | null = null
  function Probe() {
    const [range, setRange] = useState<DateRange>({ kind: "custom", start: "2024-02-28", end: "2024-03-01" })
    return createElement(DatePopover, { range, onChange: (next) => { chosen = next; setRange(next) } })
  }
  await act(async () => root.render(createElement(Probe)))
  const trigger = container.querySelector<HTMLButtonElement>('[aria-haspopup="dialog"]')!
  await act(async () => trigger.click())
  assert.equal(container.querySelector('input[type="date"]'), null)
  assert.equal(container.querySelector<HTMLInputElement>('[aria-label="Start date"]')?.type, "text")
  assert.ok(container.querySelector('[role="grid"]'))
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Calendar month"]')!.click())
  const month = win.document.querySelector<HTMLElement>('[role="option"][data-value="2"]')!
  await act(async () => month.click())
  assert.ok(container.querySelector('[role="dialog"][aria-label="Choose date range"]'))
  await act(async () => day(container, "2024-03-05").click())
  await act(async () => day(container, "2024-03-07").click())
  const apply = [...container.querySelectorAll("button")].find((button) => button.textContent === "Apply range")!
  assert.equal(apply.disabled, false)
  await act(async () => apply.click())
  assert.deepEqual(chosen, { kind: "custom", start: "2024-03-05", end: "2024-03-07" })
  assert.equal(win.document.activeElement, trigger)
  await act(async () => trigger.click())
  const preset = [...container.querySelectorAll("button")].find((button) => button.textContent === "Last 7 days")!
  await act(async () => preset.click())
  assert.deepEqual(chosen, { kind: "preset", preset: "last7" })
}))

test("manual date edits and calendar clicks share the same range selection phase", async () => environment(async (container, root, win) => {
  let chosen: DateRange | null = null
  await act(async () => root.render(createElement(DatePopover, { range: { kind: "preset", preset: "all" }, onChange: (next) => { chosen = next } })))
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-haspopup="dialog"]')!.click())
  const edit = async (label: string, value: string) => {
    const input = container.querySelector<HTMLInputElement>(`[aria-label="${label}"]`)!
    await act(async () => {
      Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!.call(input, value)
      input.dispatchEvent(new win.Event("input", { bubbles: true }))
    })
  }
  await edit("Start date", "2024-02-27")
  await act(async () => day(container, "2024-02-29").click())
  assert.equal(container.querySelector<HTMLInputElement>('[aria-label="Start date"]')!.value, "2024-02-27")
  assert.equal(container.querySelector<HTMLInputElement>('[aria-label="End date"]')!.value, "2024-02-29")
  await act(async () => day(container, "2024-02-20").click())
  await edit("End date", "2024-02-22")
  await act(async () => day(container, "2024-02-23").click())
  assert.equal(container.querySelector<HTMLInputElement>('[aria-label="Start date"]')!.value, "2024-02-23")
  assert.equal(container.querySelector<HTMLInputElement>('[aria-label="End date"]')!.value, "")
  await act(async () => day(container, "2024-02-24").click())
  await act(async () => [...container.querySelectorAll("button")].find((button) => button.textContent === "Apply range")!.click())
  assert.deepEqual(chosen, { kind: "custom", start: "2024-02-23", end: "2024-02-24" })
}))

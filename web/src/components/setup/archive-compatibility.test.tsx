import assert from "node:assert/strict"
import test from "node:test"
import { act, createElement } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { Window } from "happy-dom"
import { ArchiveStep } from "./steps/ArchiveStep.tsx"
import { archiveSpeedError } from "./archiveSpeed.ts"
import { ReviewStep } from "./steps/ReviewStep.tsx"

test("Unraid compatibility is opt-in and only shown for SMB", () => {
  const win = new Window()
  try {
    for (const system of ["cifs", "nfs", "rsync", "rclone", "none"]) {
      for (const value of [undefined, "false", "true", "yes"]) {
        const data: Record<string, string> = { ARCHIVE_SYSTEM: system }
        if (value !== undefined) data.CIFS_NOSERVERINO = value
        win.document.body.innerHTML = renderToStaticMarkup(createElement(ArchiveStep, {
          data, onChange() {}, onBatchChange() {},
        }))
        const label = [...win.document.querySelectorAll("label")].find(node => node.textContent.includes("Unraid SMB compatibility"))
        if (system === "cifs") {
          assert.ok(label)
          assert.equal(label.querySelector("input")!.checked, value === "true" || value === "yes")
        } else {
          assert.equal(label, undefined)
        }
      }
    }
  } finally { win.close() }
})

test("toggling compatibility sends the setting to Test Connection and supports disabling it", async () => {
  const win = new Window({ url: "http://localhost/" })
  const globals = { window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true }
  const descriptors = Object.keys(globals).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(globals)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const oldFetch = globalThis.fetch
  const { createRoot } = await import("react-dom/client")
  const container = win.document.createElement("div")
  win.document.body.append(container)
  const root = createRoot(container)
  const requests: Record<string, string>[] = []
  globalThis.fetch = async (url, options) => {
    assert.equal(url, "/api/setup/test-archive")
    requests.push(JSON.parse(options!.body as string))
    return Response.json({ success: true })
  }
  let data: Record<string, string> = {
    ARCHIVE_SYSTEM: "cifs", ARCHIVE_SERVER: "nas.local", SHARE_NAME: "TeslaCam",
    SHARE_USER: "test", SHARE_PASSWORD: "test", DRIVE_MAP_UNIT: "mi",
  }
  const render = () => root.render(createElement(ArchiveStep, {
    data, onBatchChange() {}, onChange(key, value) { data = { ...data, [key]: value }; render() },
  }))
  const checkbox = () => [...container.querySelectorAll("label")]
    .find(node => node.textContent.includes("Unraid SMB compatibility"))!.querySelector("input")!
  const testButton = () => [...container.querySelectorAll("button")].find(node => node.textContent.trim() === "Test Connection")!
  try {
    await act(async () => render())
    assert.equal(data.CIFS_NOSERVERINO, undefined, "rendering does not opt existing users in")
    const drivingCheckbox = [...container.querySelectorAll("label")]
      .find(node => node.textContent.includes("Recent Clips: only while driving"))!.querySelector("input")!
    await act(async () => drivingCheckbox.click())
    assert.equal(data.ARCHIVE_RECENTCLIPS_DRIVING_ONLY, "true")
    const speedInput = () => container.querySelector('input[type="number"]')!
    assert.equal(Number(speedInput().getAttribute("value")), 1)
    await act(async () => {
      Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!.call(speedInput(), "10")
      speedInput().dispatchEvent(new win.Event("input", { bubbles: true }))
    })
    assert.ok(Math.abs(Number(data.ARCHIVE_RECENTCLIPS_MIN_SPEED_MPS) - 4.4704) < 0.000001)
    data = { ...data, DRIVE_MAP_UNIT: "km" }
    await act(async () => render())
    assert.match(container.textContent, /Speed threshold \(km\/h\)/)
    assert.equal(Number((speedInput() as HTMLInputElement).value), 16)
    await act(async () => {
      Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!.call(speedInput(), "36")
      speedInput().dispatchEvent(new win.Event("input", { bubbles: true }))
    })
    assert.equal(Number(data.ARCHIVE_RECENTCLIPS_MIN_SPEED_MPS), 10)
    await act(async () => speedInput().focus())
    await act(async () => {
      Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!.call(speedInput(), "36.7")
      speedInput().dispatchEvent(new win.Event("input", { bubbles: true }))
    })
    assert.equal((speedInput() as HTMLInputElement).value, "37", "rounds fractional input to a whole speed")
    assert.equal(Number(data.ARCHIVE_RECENTCLIPS_MIN_SPEED_MPS), 37 / 3.6)
    await act(async () => speedInput().blur())
    assert.equal((speedInput() as HTMLInputElement).value, "37", "formats the stored value after editing")
    await act(async () => checkbox().click())
    assert.equal(data.CIFS_NOSERVERINO, "true")
    await act(async () => testButton().click())
    assert.equal(requests[0].CIFS_NOSERVERINO, "true")
    assert.equal(requests[0].ARCHIVE_RECENTCLIPS_DRIVING_ONLY, "true")
    assert.match(container.textContent, /Connection successful/)
    await act(async () => checkbox().click())
    assert.equal(data.CIFS_NOSERVERINO, "false")
    assert.doesNotMatch(container.textContent, /Connection successful/, "changing mount options invalidates the old test result")
    await act(async () => testButton().click())
    assert.equal(requests[1].CIFS_NOSERVERINO, "false")
    assert.equal(requests[1].SHARE_NAME, "TeslaCam")
    assert.match(renderToStaticMarkup(createElement(ReviewStep, {
      data: { ...data, CIFS_NOSERVERINO: "true" }, onChange() {}, onBatchChange() {},
    })), /CIFS_NOSERVERINO.*Enabled/)
  } finally {
    await act(async () => root.unmount())
    globalThis.fetch = oldFetch
    for (const [key, descriptor] of descriptors) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor)
      else Reflect.deleteProperty(globalThis, key)
    }
    win.close()
  }
})

test("driving-only Recent Clips is opt-in, requires Recent Clips, and appears in review", () => {
  const win = new Window()
  try {
    for (const system of ["cifs", "nfs", "rsync", "rclone", "none"]) {
      for (const recent of ["true", "false"]) {
        for (const driving of [undefined, "true", "false"]) {
          const data: Record<string, string> = { ARCHIVE_SYSTEM: system, ARCHIVE_RECENTCLIPS: recent }
          if (driving !== undefined) data.ARCHIVE_RECENTCLIPS_DRIVING_ONLY = driving
          win.document.body.innerHTML = renderToStaticMarkup(createElement(ArchiveStep, {
            data, onChange() {}, onBatchChange() {},
          }))
          const label = [...win.document.querySelectorAll("label")]
            .find(node => node.textContent.includes("Recent Clips: only while driving"))
          if (system === "none") { assert.equal(label, undefined); continue }
          assert.ok(label)
          const input = label.querySelector("input")!
          assert.equal(input.checked, driving === "true")
          assert.equal(input.disabled, recent === "false")
        }
      }
    }
    assert.match(renderToStaticMarkup(createElement(ReviewStep, {
      data: { ARCHIVE_RECENTCLIPS_DRIVING_ONLY: "true" }, onChange() {}, onBatchChange() {},
    })), /ARCHIVE_RECENTCLIPS_DRIVING_ONLY.*true/)
  } finally { win.close() }
})

test("archive speed rejects invalid values and review uses preferred units", () => {
  for (const value of [undefined, "0", "0.1", "10"]) assert.equal(archiveSpeedError(value), null)
  for (const value of ["", " ", "-1", "NaN", "Infinity", "1e99", "no"])
    assert.ok(archiveSpeedError(value))
  for (const [unit, expected] of [["mi", "22 mph"], ["km", "36 km/h"]]) {
    const html = renderToStaticMarkup(createElement(ReviewStep, {
      data: { DRIVE_MAP_UNIT: unit, ARCHIVE_RECENTCLIPS_MIN_SPEED_MPS: "10" },
      onChange() {}, onBatchChange() {},
    }))
    assert.ok(html.includes(expected))
  }
})

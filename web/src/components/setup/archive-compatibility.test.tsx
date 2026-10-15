import assert from "node:assert/strict"
import test from "node:test"
import { act, createElement } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { Window } from "happy-dom"
import { ArchiveStep } from "./steps/ArchiveStep.tsx"
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
    SHARE_USER: "test", SHARE_PASSWORD: "test",
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
    await act(async () => checkbox().click())
    assert.equal(data.CIFS_NOSERVERINO, "true")
    await act(async () => testButton().click())
    assert.equal(requests[0].CIFS_NOSERVERINO, "true")
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

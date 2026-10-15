import assert from "node:assert/strict"
import test from "node:test"
import { register } from "node:module"
import { act, createElement, StrictMode } from "react"
import { Window } from "happy-dom"

register(`data:text/javascript,${encodeURIComponent("export async function load(url, context, nextLoad) { return url.endsWith('.css') ? { format: 'module', source: '', shortCircuit: true } : nextLoad(url, context) }")}`, import.meta.url)

async function environment(run: (dom: {
  container: HTMLElement
  render: () => Promise<void>
  click: (label: string) => Promise<void>
  wizardOpens: () => number
}) => Promise<void>) {
  const win = new Window({ url: "http://localhost/settings" })
  const values = { window: win, document: win.document, navigator: win.navigator, localStorage: win.localStorage, Element: win.Element, HTMLElement: win.HTMLElement, IS_REACT_ACT_ENVIRONMENT: true }
  const descriptors = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const oldFetch = globalThis.fetch
  const { createRoot } = await import("react-dom/client")
  const { AwayModeProvider } = await import("../../../hooks/useAwayMode.tsx")
  const { AwayModeControl } = await import("./AwayModeControl.tsx")
  const container = win.document.createElement("div")
  win.document.body.append(container)
  const root = createRoot(container)
  let wizardOpens = 0
  try {
    await run({
      container: container as unknown as HTMLElement,
      render: () => act(async () => root.render(createElement(StrictMode, {}, createElement(AwayModeProvider, {
        children: createElement(AwayModeControl, { onOpenWizard: () => { wizardOpens++ } }),
      })))),
      click: (label) => act(async () => {
        const button = [...container.querySelectorAll("button")].find(item => item.textContent?.trim() === label && !item.closest("[hidden]"))
        assert.ok(button, `expected a visible ${label} button`)
        button.click()
      }),
      wizardOpens: () => wizardOpens,
    })
  } finally {
    await act(async () => root.unmount())
    globalThis.fetch = oldFetch
    for (const [key, descriptor] of descriptors) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
}

const ready = { state: "idle", mode: "manual", ap_configured: true, ap_ssid: "Road WiFi", ap_ip: "192.168.66.1" }

test("only an unconfigured AP offers the setup wizard", async () => environment(async (dom) => {
  globalThis.fetch = async input => Response.json(String(input).endsWith("/away-mode/status") ? { ...ready, ap_configured: false } : {})
  await dom.render()
  assert.match(dom.container.textContent!, /Enable the WiFi Access Point in the Setup Wizard/)
  await dom.click("Open Setup Wizard")
  assert.equal(dom.wizardOpens(), 1)
}))

test("AP detection failures show the real problem and Retry recovers without configuring anything", async () => environment(async (dom) => {
  const writes: string[] = []
  let healthy = false
  globalThis.fetch = async (input, init) => {
    if (init?.method) writes.push(String(input))
    return Response.json(String(input).endsWith("/away-mode/status")
      ? healthy ? ready : { ...ready, ap_configured: false, ap_error: "NetworkManager is not running" }
      : {})
  }
  await dom.render()
  assert.match(dom.container.textContent!, /NetworkManager is not running/)
  assert.doesNotMatch(dom.container.textContent!, /Enable the WiFi Access Point in the Setup Wizard|Open Setup Wizard/)
  assert.ok(dom.container.querySelector("[data-disabled='true']"))
  healthy = true
  await dom.click("Retry")
  assert.equal(dom.container.querySelector("[data-disabled='true']"), null)
  assert.doesNotMatch(dom.container.textContent!, /NetworkManager is not running/)
  assert.deepEqual(writes, [])
}))

test("failed HTTP status reads stay retryable and a stale failure cannot replace a recovered status", async () => environment(async (dom) => {
  let resolveStale!: (response: Response) => void
  let reads = 0
  globalThis.fetch = async input => {
    if (!String(input).endsWith("/away-mode/status")) return Response.json({})
    reads++
    if (reads === 1) return new Promise(resolve => { resolveStale = resolve })
    if (reads === 2) return Response.json({ error: "Device service unavailable" }, { status: 503 })
    return Response.json(ready)
  }
  await dom.render()
  assert.match(dom.container.textContent!, /Device service unavailable/)
  assert.doesNotMatch(dom.container.textContent!, /Open Setup Wizard/)
  await dom.click("Retry")
  await act(async () => resolveStale(Response.json({ error: "Old lookup failure" }, { status: 500 })))
  assert.equal(dom.container.querySelector("[data-disabled='true']"), null)
  assert.doesNotMatch(dom.container.textContent!, /Old lookup failure|Device service unavailable/)
}))

test("failed manual and automatic activation preserve the confirmed state and display the backend error", async () => environment(async (dom) => {
  globalThis.fetch = async (input, init) => {
    if (init?.method) return Response.json({ error: "AP profile lookup timed out" }, { status: 503 })
    return Response.json(String(input).endsWith("/away-mode/status") ? ready : {})
  }
  await dom.render()
  await dom.click("Enable")
  await dom.click("Enable Away Mode")
  assert.match(dom.container.querySelector("[role='alert']")!.textContent!, /AP profile lookup timed out/)
  assert.equal(localStorage.getItem("sentryusb_away_mode"), null)
  assert.doesNotMatch(dom.container.textContent!, /remaining/)
  await dom.click("Automatic")
  assert.match(dom.container.querySelector("[role='alert']")!.textContent!, /Could not change Away Mode: AP profile lookup timed out/)
  assert.doesNotMatch(dom.container.textContent!, /The hotspot turns on by itself/)
}))

test("a hotspot that is still starting has no Active badge and can be canceled", async () => environment(async (dom) => {
  let canceled = 0
  globalThis.fetch = async (input, init) => {
    if (init?.method === 'DELETE') { canceled++; return Response.json({ ok: true }) }
    return Response.json(String(input).endsWith('/away-mode/status') ? { ...ready, ap_starting: true, ap_on: false } : {})
  }
  await dom.render()
  assert.match(dom.container.textContent!, /Starting the hotspot/)
  assert.doesNotMatch(dom.container.textContent!, /Active/)
  await dom.click('Cancel startup')
  assert.equal(canceled, 1)
  assert.doesNotMatch(dom.container.textContent!, /Starting the hotspot/)
}))

test("an automatic hotspot failure remains off, explains the failure, and keeps mode controls available", async () => environment(async (dom) => {
  globalThis.fetch = async input => Response.json(String(input).endsWith('/away-mode/status')
    ? { ...ready, mode: 'auto', is_home: false, ap_on: false, ap_start_error: 'Armbian hotspot failed to become ready' }
    : {})
  await dom.render()
  assert.match(dom.container.querySelector('[role="alert"]')!.textContent!, /failed to become ready/)
  assert.match(dom.container.textContent!, /Automatic mode will retry/)
  assert.doesNotMatch(dom.container.textContent!, /Active|Open Setup Wizard/)
  assert.equal(dom.container.querySelector('[data-disabled="true"]'), null)
}))

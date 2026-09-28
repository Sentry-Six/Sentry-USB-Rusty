import assert from "node:assert/strict"
import test from "node:test"
import { act, createElement, StrictMode } from "react"
import { Window } from "happy-dom"
import { AuthProvider, useAuth } from "./useAuth.tsx"
import { KeepAwakeProvider, useKeepAwake } from "./useKeepAwake.tsx"
import { useTelemetry } from "./useTelemetry.ts"
import { useChargingRates } from "./useChargingRates.ts"
import { useChargingHistory } from "./useChargingHistory.ts"

async function environment(run: (container: HTMLElement, root: import("react-dom/client").Root) => Promise<void>) {
  const browser = new Window({ url: "http://localhost/" })
  const previous = new Map<string, PropertyDescriptor | undefined>()
  for (const [key, value] of Object.entries({ window: browser, document: browser.document, navigator: browser.navigator, IS_REACT_ACT_ENVIRONMENT: true })) {
    previous.set(key, Object.getOwnPropertyDescriptor(globalThis, key))
    Object.defineProperty(globalThis, key, { configurable: true, writable: true, value })
  }
  const oldFetch = globalThis.fetch
  const { createRoot } = await import("react-dom/client")
  const container = browser.document.createElement("div")
  browser.document.body.append(container)
  const root = createRoot(container as unknown as HTMLElement)
  try { await run(container as unknown as HTMLElement, root) }
  finally {
    await act(async () => root.unmount())
    globalThis.fetch = oldFetch
    browser.close()
    for (const [key, value] of previous) { if (value) Object.defineProperty(globalThis, key, value); else Reflect.deleteProperty(globalThis, key) }
  }
}
function deferred() {
  let resolve!: (value: Response) => void
  const promise = new Promise<Response>((done) => { resolve = done })
  return { promise, resolve }
}
const json = (body: unknown) => Response.json(body)

test("abandoned auth checks cannot undo login or logout under StrictMode", async () => environment(async (container, root) => {
  const pending: (ReturnType<typeof deferred> & { signal: AbortSignal | null | undefined })[] = []
  globalThis.fetch = async (url, options) => {
    if (String(url).endsWith("/check")) {
      const request = { ...deferred(), signal: options?.signal }
      pending.push(request)
      return request.promise
    }
    return json({})
  }
  function Probe() {
    const auth = useAuth()
    return createElement("div", null,
      createElement("output", null, auth.state),
      createElement("button", { onClick: () => void auth.login("user", "password") }, "Login"),
      createElement("button", { onClick: () => void auth.logout() }, "Logout"))
  }
  await act(async () => root.render(createElement(StrictMode, null, createElement(AuthProvider, { children: createElement(Probe) }))))
  assert.equal(pending.length, 2)
  assert.equal(pending[0].signal?.aborted, true)
  await act(async () => container.querySelectorAll("button")[0].click())
  assert.equal(container.querySelector("output")?.textContent, "authenticated")
  assert.equal(pending[1].signal?.aborted, true)
  await act(async () => pending[0].resolve(json({ auth_required: true, authenticated: false })))
  assert.equal(container.querySelector("output")?.textContent, "authenticated")
  await act(async () => container.querySelectorAll("button")[1].click())
  await act(async () => pending[1].resolve(json({ auth_required: true, authenticated: true })))
  assert.equal(container.querySelector("output")?.textContent, "unauthenticated")
}))

test("a stale preference read cannot enable automatic keep-awake after a manual choice", async () => environment(async (container, root) => {
  const preference = deferred()
  let preferenceSignal: AbortSignal | null | undefined
  let heartbeats = 0
  globalThis.fetch = async (url, options) => {
    if (String(url).includes("/heartbeat")) heartbeats++
    if (String(url).includes("/preference?") && !options?.method) {
      preferenceSignal = options?.signal
      return preference.promise
    }
    return json({ state: "idle", mode: "manual" })
  }
  function Probe() {
    const awake = useKeepAwake()
    return createElement("div", null, createElement("output", null, awake.mode),
      createElement("button", { onClick: () => void awake.updateMode("manual") }, "Manual"))
  }
  await act(async () => root.render(createElement(KeepAwakeProvider, { children: createElement(Probe) })))
  await act(async () => container.querySelector("button")!.click())
  assert.equal(preferenceSignal?.aborted, true)
  await act(async () => preference.resolve(json({ value: "auto" })))
  assert.equal(container.querySelector("output")?.textContent, "manual")
  assert.equal(heartbeats, 0)
}))

test("telemetry follows the selected clip immediately and ignores canceled responses", async () => environment(async (container, root) => {
  const requests: (ReturnType<typeof deferred> & { signal: AbortSignal | null | undefined })[] = []
  globalThis.fetch = async (_url, options) => {
    const request = { ...deferred(), signal: options?.signal }
    requests.push(request)
    return request.promise
  }
  function Probe({ clip }: { clip: string | null }) {
    const telemetry = useTelemetry(clip, clip ? "front.mp4" : null)
    return createElement("output", null, `${telemetry.loading}:${telemetry.telemetry?.duration_sec ?? "none"}`)
  }
  await act(async () => root.render(createElement(Probe, { clip: "A" })))
  await act(async () => root.render(createElement(Probe, { clip: "B" })))
  assert.equal(requests[0].signal?.aborted, true)
  await act(async () => requests[0].resolve(json({ frames: [], duration_sec: 10, has_gps: false, has_autopilot: false })))
  assert.equal(container.textContent, "true:none")
  await act(async () => requests[1].resolve(json({ frames: [], duration_sec: 20, has_gps: false, has_autopilot: false })))
  assert.equal(container.textContent, "false:20")
  await act(async () => root.render(createElement(Probe, { clip: null })))
  assert.equal(container.textContent, "false:none")
  await act(async () => root.render(createElement(Probe, { clip: "B" })))
  assert.equal(container.textContent, "false:20")
  assert.equal(requests.length, 2)
  await act(async () => root.render(createElement(Probe, { clip: "A" })))
  await act(async () => requests[2].resolve(new Response("Unavailable", { status: 503 })))
  assert.equal(container.textContent, "false:none")
  await act(async () => root.render(createElement(Probe, { clip: "B" })))
  await act(async () => root.render(createElement(Probe, { clip: "A" })))
  assert.equal(container.textContent, "true:none")
  assert.equal(requests.length, 4)
  await act(async () => requests[3].resolve(json({ frames: [], duration_sec: 10, has_gps: false, has_autopilot: false })))
  assert.equal(container.textContent, "false:10")
}))

test("rate refresh cancels older reads and retains the latest returned currency", async () => environment(async (container, root) => {
  const requests: (ReturnType<typeof deferred> & { signal: AbortSignal | null | undefined })[] = []
  globalThis.fetch = async (_url, options) => {
    const request = { ...deferred(), signal: options?.signal }
    requests.push(request)
    return request.promise
  }
  function Probe() {
    const rates = useChargingRates()
    return createElement("div", null, createElement("output", null, `${rates.loading}:${rates.rates.currency}`),
      createElement("button", { onClick: () => void rates.refresh() }, "Refresh"))
  }
  await act(async () => root.render(createElement(StrictMode, null, createElement(Probe))))
  await act(async () => container.querySelector("button")!.click())
  assert.equal(requests.length, 3)
  assert.equal(requests[0].signal?.aborted, true)
  assert.equal(requests[1].signal?.aborted, true)
  await act(async () => requests[2].resolve(json({ document: { charging_currency: "CAD", charging_default_rate: 0.2 } })))
  assert.equal(container.querySelector("output")?.textContent, "false:CAD")
  await act(async () => {
    requests[0].resolve(json({ document: { charging_currency: "USD" } }))
    requests[1].resolve(json({ document: { charging_currency: "EUR" } }))
  })
  assert.equal(container.querySelector("output")?.textContent, "false:CAD")
}))

test("cached optional charging data does not mark pending history as loaded on remount", async () => environment(async (container, root) => {
  const requests: (ReturnType<typeof deferred> & { signal: AbortSignal | null | undefined })[] = []
  globalThis.fetch = async (url, options) => {
    if (/^\/api\/charging(?:\?|$)/.test(String(url))) {
      const request = { ...deferred(), signal: options?.signal }
      requests.push(request)
      return request.promise
    }
    return String(url).endsWith("/tags") ? json(["Home"]) : json({ charging: false })
  }
  function Probe() {
    const history = useChargingHistory()
    return createElement("output", null, `${history.loading}:${history.sessions.length}:${history.tags.join(",")}`)
  }
  await act(async () => root.render(createElement(Probe)))
  assert.equal(container.textContent, "true:0:Home")
  await act(async () => root.render(null))
  assert.equal(requests[0].signal?.aborted, true)
  await act(async () => root.render(createElement(Probe)))
  assert.equal(container.textContent, "true:0:Home")
  await act(async () => requests[0].resolve(json({ sessions: [{ id: 1, tags: [] }] })))
  assert.equal(container.textContent, "true:0:Home")
  await act(async () => requests[1].resolve(json({ sessions: [{ id: 2, tags: [] }] })))
  assert.equal(container.textContent, "false:1:Home")
}))

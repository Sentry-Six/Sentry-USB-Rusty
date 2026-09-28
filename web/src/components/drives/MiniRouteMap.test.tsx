import assert from "node:assert/strict"
import { register } from "node:module"
import test from "node:test"
import { act, createElement, StrictMode } from "react"
import { Window } from "happy-dom"

const points: [number, number][] = [[51, -114], [51.01, -114.01]]

test("route thumbnails show loading, recover from failed tiles, and clear stale map work", async context => {
  const win = new Window({ url: "http://localhost/drives" })
  const observers: IntersectionObserverCallback[] = []
  const values = {
    window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true,
    IntersectionObserver: class {
      constructor(callback: IntersectionObserverCallback) { observers.push(callback) }
      observe() {}
      disconnect() {}
    },
  }
  const originals = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  register(`data:text/javascript,${encodeURIComponent(`
    export async function load(url, options, nextLoad) {
      return url.endsWith(".css") ? { format: "module", source: "", shortCircuit: true } : nextLoad(url, options)
    }
  `)}`, import.meta.url)
  const { default: L } = await import("leaflet")
  const { MiniRouteMap } = await import("./MiniRouteMap.tsx")
  const { createRoot } = await import("react-dom/client")
  const layers: InstanceType<typeof L.Evented>[] = []
  const removed: boolean[] = []
  context.mock.method(L, "map", () => {
    const index = removed.push(false) - 1
    return { attributionControl: { setPrefix() {} }, fitBounds() {}, remove() { removed[index] = true } }
  })
  context.mock.method(L, "tileLayer", () => {
    const events = new L.Evented()
    layers.push(events)
    return Object.assign(events, { addTo() {} })
  })
  context.mock.method(L, "polyline", () => ({ addTo() {} }))
  context.mock.method(L, "circleMarker", () => ({ addTo() {} }))
  const timeouts: (() => void)[] = []
  const realSetTimeout = globalThis.setTimeout
  context.mock.method(globalThis, "setTimeout", (...args: Parameters<typeof setTimeout>) => {
    if (args[1] !== 15_000) return realSetTimeout(...args)
    timeouts.push(() => args[0](...args.slice(2)))
    return 0 as unknown as ReturnType<typeof setTimeout>
  })
  const container = win.document.createElement("div")
  win.document.body.append(container)
  const root = createRoot(container)
  const render = async (status: "loading" | "ready" | "unavailable", route = points) => {
    await act(async () => root.render(createElement(StrictMode, {}, createElement(MiniRouteMap, { points: route, status }))))
  }
  const busy = () => container.querySelector('[role="img"]')?.getAttribute("aria-busy")
  try {
    await render("loading")
    assert.equal(busy(), "true")
    assert.ok(container.querySelector(".animate-spin"))
    assert.equal(layers.length, 0)
    await act(async () => {
      observers.at(-1)!([{ isIntersecting: true } as IntersectionObserverEntry], {} as IntersectionObserver)
    })
    assert.equal(layers.length, 0, "route data must arrive before starting the map")
    await render("ready")
    assert.equal(layers.length, 1)
    assert.equal(busy(), "true", "spinner remains until initial tiles settle")
    await act(async () => { layers[0].fire("load") })
    assert.equal(busy(), "false")
    assert.equal(container.querySelector(".animate-spin"), null)

    await render("loading")
    assert.equal(removed[0], true)
    await render("ready")
    assert.equal(busy(), "true", "refreshing the same route resets the tile spinner")
    await act(async () => { layers[0].fire("load") })
    assert.equal(busy(), "true", "removed maps cannot settle their replacement")
    await act(async () => { layers[1].fire("tileerror") })
    assert.equal(busy(), "false", "tile failure keeps the route visible without spinning forever")

    await render("ready", [[52, -113], [52.01, -113.01]])
    assert.equal(busy(), "true")
    await act(async () => { timeouts.at(-1)!() })
    assert.equal(busy(), "false", "a stalled tile server has a bounded wait")
    await render("ready", [])
    assert.equal(busy(), "false")
    assert.match(container.textContent ?? "", /No route/)
    await render("unavailable", [])
    assert.match(container.textContent ?? "", /Map unavailable/)
  } finally {
    await act(async () => root.unmount())
    assert.ok(removed.every(Boolean), "all initialized maps are removed")
    context.mock.restoreAll()
    for (const [key, descriptor] of originals) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor)
      else Reflect.deleteProperty(globalThis, key)
    }
    await win.happyDOM.close()
  }
})

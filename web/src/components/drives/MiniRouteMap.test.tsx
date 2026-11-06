import assert from "node:assert/strict"
import { register } from "node:module"
import test from "node:test"
import { act, createElement, StrictMode } from "react"
import { Window } from "happy-dom"

const points: [number, number][] = [[51, -114], [51.01, -114.01]]

test("route thumbnails show loading, recover from failed snapshots, and clear stale map work", async context => {
  const win = new Window({ url: "http://localhost/drives" })
  // Take the WebGL2 path; the snapshot renderer is replaced below.
  win.HTMLCanvasElement.prototype.getContext = ((type: string) => type === "webgl2" ? {} : null) as typeof win.HTMLCanvasElement.prototype.getContext
  const snapshots: ((url: string | null) => void)[] = []
  const observers: IntersectionObserverCallback[] = []
  const values = {
    window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true,
    __renderMapSnapshot: () => new Promise<string | null>(resolve => { snapshots.push(resolve) }),
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
      if (url.endsWith(".css")) return { format: "module", source: "", shortCircuit: true }
      if (url.endsWith("/lib/mapSnapshot.ts")) {
        return { format: "module", source: "export const renderMapSnapshot = view => globalThis.__renderMapSnapshot(view)", shortCircuit: true }
      }
      return nextLoad(url, options)
    }
  `)}`, import.meta.url)
  const { default: L } = await import("leaflet")
  const { MiniRouteMap } = await import("./MiniRouteMap.tsx")
  const { createRoot } = await import("react-dom/client")
  const overlays: InstanceType<typeof L.Evented>[] = []
  const removed: boolean[] = []
  context.mock.method(L, "map", () => {
    const index = removed.push(false) - 1
    return {
      attributionControl: { setPrefix() {}, addAttribution() {} },
      fitBounds() {},
      getContainer: () => ({ style: {} }),
      getSize: () => ({ x: 128, y: 80 }),
      getCenter: () => ({ lat: 51, lng: -114 }),
      getZoom: () => 14,
      getBounds: () => ({}),
      removeLayer() {},
      remove() { removed[index] = true },
    }
  })
  context.mock.method(L, "imageOverlay", () => {
    const events = new L.Evented()
    overlays.push(events)
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
  // The snapshot module loads with a dynamic import, so requests arrive a few ticks later.
  const snapshotRequested = async (count: number) => {
    for (let i = 0; i < 100 && snapshots.length < count; i++) await new Promise(resolve => realSetTimeout(resolve, 5))
    assert.equal(snapshots.length, count)
  }
  const resolveSnapshot = async (index: number, url: string | null) => {
    await act(async () => {
      snapshots[index](url)
      await new Promise(resolve => realSetTimeout(resolve, 0))
    })
  }
  try {
    await render("loading")
    assert.equal(busy(), "true")
    assert.ok(container.querySelector(".animate-spin"))
    assert.equal(removed.length, 0)
    await act(async () => {
      observers.at(-1)!([{ isIntersecting: true } as IntersectionObserverEntry], {} as IntersectionObserver)
    })
    assert.equal(removed.length, 0, "route data must arrive before starting the map")
    await render("ready")
    await snapshotRequested(1)
    assert.equal(busy(), "true", "spinner remains until the basemap image settles")
    await resolveSnapshot(0, "blob:basemap")
    assert.equal(overlays.length, 1)
    assert.equal(busy(), "true", "spinner remains until the basemap image loads")
    await act(async () => { overlays[0].fire("load") })
    assert.equal(busy(), "false")
    assert.equal(container.querySelector(".animate-spin"), null)

    await render("loading")
    assert.equal(removed[0], true)
    await render("ready")
    await snapshotRequested(2)
    assert.equal(busy(), "true", "refreshing the same route resets the spinner")
    await render("loading")
    await render("ready")
    await snapshotRequested(3)
    await resolveSnapshot(1, "blob:stale")
    assert.equal(overlays.length, 1, "removed maps add no basemap")
    assert.equal(busy(), "true", "removed maps cannot settle their replacement")
    await resolveSnapshot(2, null)
    assert.equal(busy(), "false", "a failed snapshot keeps the route visible without spinning forever")

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

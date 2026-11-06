import assert from "node:assert/strict"
import { register } from "node:module"
import test from "node:test"
import { act, createElement, StrictMode } from "react"
import { Window } from "happy-dom"

// Without WebGL2, maps show the style's background colour, routes and pins
// on top and a short notice, and request no tiles.

register(`data:text/javascript,${encodeURIComponent("export async function load(url, context, nextLoad) { return url.endsWith('.css') ? { format: 'module', source: '', shortCircuit: true } : nextLoad(url, context) }")}`, import.meta.url)

const NOTICE = "Map unavailable: this browser can't draw maps (WebGL is turned off)."

async function environment(run: (env: { win: Window; requests: string[]; errors: unknown[][] }) => Promise<void>) {
  const win = new Window({ url: "http://localhost/settings", width: 800, height: 600 })
  // Force the no-WebGL2 branch; other canvas contexts are left alone.
  const proto = win.HTMLCanvasElement.prototype
  const getContext = proto.getContext
  proto.getContext = function (this: typeof proto, type: string, ...rest: unknown[]) {
    return /^(webgl2?|experimental-webgl)$/.test(type) ? null : (getContext as (...args: unknown[]) => unknown).call(this, type, ...rest)
  } as typeof proto.getContext
  const requests: string[] = []
  const errors: unknown[][] = []
  const values = {
    window: win, document: win.document, navigator: win.navigator, localStorage: win.localStorage,
    Element: win.Element, HTMLElement: win.HTMLElement, IS_REACT_ACT_ENVIRONMENT: true,
    IntersectionObserver: class {
      constructor(private callback: IntersectionObserverCallback) {}
      observe() { this.callback([{ isIntersecting: true } as IntersectionObserverEntry], this as unknown as IntersectionObserver) }
      disconnect() {}
    },
    fetch: async (input: RequestInfo | URL) => { requests.push(String(input)); throw new Error("no network in tests") },
  }
  const descriptors = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, writable: true, value })
  const consoleError = console.error
  console.error = (...args: unknown[]) => { errors.push(args) }
  try {
    await run({ win, requests, errors })
  } finally {
    console.error = consoleError
    for (const [key, descriptor] of descriptors) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    await win.happyDOM.close()
  }
}

// Image and script loads would show as elements; tiles would be <img> in the tile pane.
function assertNoTiles(root: ParentNode) {
  assert.equal(root.querySelectorAll(".leaflet-tile, .leaflet-tile-container, img").length, 0)
  assert.doesNotMatch((root as Element).innerHTML ?? "", /openstreetmap\.org\/\d|tile\.openstreetmap/)
}

test("interactive maps without WebGL2 keep the background, draw routes and show a notice", async () => environment(async ({ win, requests }) => {
  const { default: L } = await import("leaflet")
  const { attachBasemap, MAP_BACKGROUNDS, supportsWebGL } = await import("./mapTiles.ts")
  assert.equal(supportsWebGL(), false)

  const el = win.document.createElement("div")
  win.document.body.append(el)
  const map = L.map(el as unknown as HTMLElement, { preferCanvas: false }).setView([51, -114], 12)
  const basemap = attachBasemap(map, "dark")
  const route = L.polyline([[51, -114], [51.01, -114.01]], { color: "#34d399" }).addTo(map)
  L.circleMarker([51, -114], { radius: 3 }).addTo(map)

  assert.ok(map.hasLayer(route), "routes still draw with no basemap layer")
  assert.equal(el.querySelectorAll(".leaflet-overlay-pane path").length, 2)
  assert.equal(el.querySelector(".map-webgl-notice")?.textContent, NOTICE)
  assert.equal(el.querySelectorAll(".map-webgl-notice").length, 1)
  const bg = (style: string) => { const probe = win.document.createElement("div"); probe.style.background = style; return probe.style.background }
  assert.equal(el.style.background, bg(MAP_BACKGROUNDS.dark))
  const layers: L.Layer[] = []
  map.eachLayer(layer => { layers.push(layer) })
  assert.ok(!layers.some(layer => layer instanceof L.GridLayer), "no tile layer")
  assert.equal(layers.filter(layer => !(layer instanceof L.Renderer)).length, 2, "only the route and marker are on the map")

  basemap.setStyle("streets")
  assert.equal(el.style.background, bg(MAP_BACKGROUNDS.streets))
  basemap.remove()
  assert.equal(el.querySelector(".map-webgl-notice"), null)
  map.remove()
  assertNoTiles(el)
  assert.deepEqual(requests, [])
}))

test("thumbnails without WebGL2 settle at once with a short notice", async () => environment(async ({ win, requests }) => {
  const { default: L } = await import("leaflet")
  const { attachStaticBasemap, MAP_BACKGROUNDS } = await import("./mapTiles.ts")
  const el = win.document.createElement("div")
  el.className = "map-thumbnail"
  win.document.body.append(el)
  const map = L.map(el as unknown as HTMLElement).setView([51, -114], 12)
  let settled = 0
  const detach = attachStaticBasemap(map, "dark", () => { settled++ })
  await Promise.resolve()
  assert.equal(settled, 1)
  const notice = el.querySelector(".map-webgl-notice")
  assert.equal(notice?.textContent, "Map needs WebGL")
  assert.equal(notice?.getAttribute("title"), NOTICE)
  assert.notEqual(el.style.background, "")
  assert.ok(MAP_BACKGROUNDS.dark)
  detach()
  assert.equal(el.querySelector(".map-webgl-notice"), null)
  // A detached thumbnail never settles late.
  const detachEarly = attachStaticBasemap(map, "dark", () => { settled++ })
  detachEarly()
  await Promise.resolve()
  assert.equal(settled, 1)
  map.remove()
  assertNoTiles(el)
  assert.deepEqual(requests, [])
}))

test("route thumbnails without WebGL2 stop spinning and keep the route", async () => environment(async ({ win, requests, errors }) => {
  const { MiniRouteMap } = await import("../components/drives/MiniRouteMap.tsx")
  const { MiniPinMap } = await import("../components/charging/MiniPinMap.tsx")
  const { createRoot } = await import("react-dom/client")
  const container = win.document.createElement("div")
  win.document.body.append(container)
  const root = createRoot(container as unknown as HTMLElement)
  try {
    await act(async () => root.render(createElement(StrictMode, {},
      createElement(MiniRouteMap, { points: [[51, -114], [51.01, -114.01]], status: "ready" }),
      createElement(MiniPinMap, { lat: 51, lon: -114 }),
    )))
    const route = container.querySelector('[aria-label="Route thumbnail"]')
    assert.ok(route, "route thumbnail renders")
    assert.equal(route.getAttribute("aria-busy"), "false")
    assert.equal(container.querySelector(".animate-spin"), null)
    assert.ok(route.querySelector(".leaflet-overlay-pane path"), "route is drawn")
    assert.equal(route.querySelector(".map-webgl-notice")?.textContent, "Map needs WebGL")
    const pin = container.querySelector('[aria-label="Charge location"]')
    assert.ok(pin?.querySelector(".leaflet-overlay-pane path"), "charge pin is drawn")
    assert.equal(pin?.querySelector(".map-webgl-notice")?.textContent, "Map needs WebGL")
    assertNoTiles(container)
  } finally {
    await act(async () => root.unmount())
  }
  assert.deepEqual(requests, [])
  assert.deepEqual(errors, [])
}))

test("the home-location picker works without WebGL2", async () => environment(async ({ win, requests, errors }) => {
  const { HomeGeofencePicker } = await import("../components/settings/HomeGeofencePicker.tsx")
  const { createRoot } = await import("react-dom/client")
  type Values = { homeLat: number | null; homeLon: number | null; radiusM: number }
  let values: Values = { homeLat: null, homeLon: null, radiusM: 120 }
  const changes: Partial<Values>[] = []
  const container = win.document.createElement("div")
  win.document.body.append(container)
  const root = createRoot(container as unknown as HTMLElement)
  const render = () => act(async () => root.render(createElement(StrictMode, {}, createElement(HomeGeofencePicker, {
    values,
    onChange: (patch) => { changes.push(patch); values = { ...values, ...patch } },
    onUseCurrentLocation: async () => ({ lat: 30.22214, lon: -97.61833 }),
  }))))
  const input = (selector: string) => container.querySelector(selector) as unknown as HTMLInputElement
  const type = async (el: HTMLInputElement, text: string) => act(async () => {
    const setter = Object.getOwnPropertyDescriptor(win.HTMLInputElement.prototype, "value")!.set!
    setter.call(el, text)
    el.dispatchEvent(new win.Event("input", { bubbles: true }) as unknown as Event)
    el.focus()
    el.blur()
  })
  const click = (label: string) => act(async () => {
    const button = [...container.querySelectorAll("button")].find(b => b.textContent?.trim() === label)
    assert.ok(button, `expected a ${label} button`)
    ;(button as unknown as HTMLButtonElement).click()
  })
  try {
    await render()
    assert.equal(container.querySelector(".map-webgl-notice")?.textContent, NOTICE)
    assert.match(container.textContent ?? "", /No home set/)

    // Typed coordinates still reach the owner and move the pin.
    const coords = input('input[type="text"]')
    assert.equal(coords.disabled, false)
    await type(coords, "51.04473, -114.07193")
    const typed = changes.at(-1)!
    assert.equal(typed.homeLat, 51.04473)
    assert.ok(Math.abs(typed.homeLon! + 114.07193) < 1e-9)
    await render()
    assert.doesNotMatch(container.textContent ?? "", /No home set/)
    assert.equal(container.querySelectorAll(".leaflet-marker-icon").length, 1)
    assert.ok(container.querySelector(".leaflet-overlay-pane path"), "radius circle is drawn")

    await type(coords, "not a place")
    assert.match(container.textContent ?? "", /Couldn't parse that/)

    // "Use current location" fills the home from the car's fix.
    await click("Use current location")
    assert.deepEqual(changes.at(-1), { homeLat: 30.22214, homeLon: -97.61833 })
    await render()
    assert.equal(coords.value, "30.22214, -97.61833")

    // Radius presets and typed radii still work.
    await click("200m")
    assert.deepEqual(changes.at(-1), { radiusM: 200 })
    await render()
    await type(input('input[type="number"]'), "350")
    assert.deepEqual(changes.at(-1), { radiusM: 350 })
    await render()
    assertNoTiles(container)
  } finally {
    await act(async () => root.unmount())
  }
  assert.deepEqual(requests, [])
  assert.deepEqual(errors, [])
}))

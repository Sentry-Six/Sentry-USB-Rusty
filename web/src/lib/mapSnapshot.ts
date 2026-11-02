import type { Map as MapLibreMap } from "maplibre-gl"
import { MAP_STYLE_URLS, keepMercator, loadMapLibre, mapStyle, normalizeMapStyle, prefetchMapStyles, supportsWebGL, type MapStyle } from "@/lib/mapTiles"

// One offscreen MapLibre map renders basemap images for list thumbnails.
// Browsers cap live WebGL contexts at a few dozen, so a map per row would
// drop contexts in long lists. Jobs run one at a time and results are cached
// as object URLs.
const MAX_CACHED = 240
const RENDER_TIMEOUT_MS = 8000

export interface SnapshotView {
  style: MapStyle
  width: number
  height: number
  center: [number, number] // [lng, lat]
  zoom: number // MapLibre zoom (512px tiles)
}

const cache = new Map<string, Promise<string | null>>()
let renderer: { map: MapLibreMap; container: HTMLDivElement; style: MapStyle } | null = null
let queue: Promise<unknown> = Promise.resolve()

export function snapshotKey({ style, width, height, center, zoom }: SnapshotView) {
  return [normalizeMapStyle(style), Math.round(width), Math.round(height),
    center[0].toFixed(6), center[1].toFixed(6), zoom.toFixed(3)].join("|")
}

async function getRenderer() {
  if (renderer) return renderer
  const maplibregl = await loadMapLibre()
  const container = document.createElement("div")
  container.setAttribute("aria-hidden", "true")
  container.style.cssText = "position:fixed;left:-10000px;top:0;width:256px;height:256px;pointer-events:none;"
  document.body.appendChild(container)
  const map = new maplibregl.Map({
    container,
    style: MAP_STYLE_URLS.dark,
    interactive: false,
    attributionControl: false,
    fadeDuration: 0,
    pixelRatio: Math.min(2, window.devicePixelRatio || 1),
    canvasContextAttributes: { preserveDrawingBuffer: true },
  })
  keepMercator(map)
  prefetchMapStyles(map)
  renderer = { map, container, style: "dark" }
  return renderer
}

function waitForIdle(map: MapLibreMap) {
  return new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("map_snapshot_timeout")), RENDER_TIMEOUT_MS)
    map.once("idle", () => { clearTimeout(timer); resolve() })
    map.triggerRepaint()
  })
}

async function render(view: SnapshotView) {
  const r = await getRenderer()
  r.container.style.width = `${Math.round(view.width)}px`
  r.container.style.height = `${Math.round(view.height)}px`
  r.map.resize()
  if (r.style !== view.style) {
    r.style = view.style
    r.map.setStyle(mapStyle(view.style))
  }
  r.map.jumpTo({ center: view.center, zoom: view.zoom })
  await waitForIdle(r.map)
  const blob = await new Promise<Blob | null>(resolve => r.map.getCanvas().toBlob(resolve, "image/png"))
  if (!blob) throw new Error("map_snapshot_unavailable")
  return URL.createObjectURL(blob)
}

/**
 * Basemap image for a view, as an object URL, or null when WebGL or the map
 * server is unavailable (callers keep their plain background).
 */
export function renderMapSnapshot(view: SnapshotView): Promise<string | null> {
  if (!supportsWebGL() || !(view.width > 0 && view.height > 0)) return Promise.resolve(null)
  const job = { ...view, style: normalizeMapStyle(view.style) }
  const key = snapshotKey(job)
  const hit = cache.get(key)
  if (hit) {
    cache.delete(key); cache.set(key, hit)
    return hit
  }
  const promise: Promise<string | null> = queue.then(() => render(job)).catch((err) => {
    console.warn("[map] Map snapshot failed:", err)
    return null
  })
  queue = promise
  cache.set(key, promise)
  while (cache.size > MAX_CACHED) {
    const [oldKey, oldPromise] = cache.entries().next().value!
    cache.delete(oldKey)
    oldPromise.then(url => { if (url) URL.revokeObjectURL(url) })
  }
  promise.then(url => { if (!url && cache.get(key) === promise) cache.delete(key) })
  return promise
}

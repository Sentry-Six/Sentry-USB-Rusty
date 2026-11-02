import L from "leaflet"
import type { Map as MapLibreMap } from "maplibre-gl"
import { cachedStyle, prefetchStyles } from "@/lib/mapStyleCache"

// Basemaps: OpenStreetMap vector tiles from the SentryUSB VersaTiles server,
// drawn by MapLibre GL. Interactive maps add the style as a Leaflet layer
// (maplibre-gl-leaflet) under the existing Leaflet routes and markers; list
// thumbnails show an image from one shared offscreen renderer
// (mapSnapshot.ts) so long lists do not open a WebGL context per row.
//
// Browsers without WebGL2 (which MapLibre requires) show the style's
// background colour with the routes and markers on top, and a short notice
// that the map needs WebGL. They request no tiles.
export const TILE_ORIGIN = "https://tiles.sentryusb.com"

export type MapStyle = "dark" | "streets"

export const MAP_STYLE_URLS: Record<MapStyle, string> = {
  streets: `${TILE_ORIGIN}/assets/styles/colorful/style.json`,
  dark: `${TILE_ORIGIN}/assets/styles/sentry-dark/style.json`,
}

const OSM_COPYRIGHT = '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a>'
const VERSATILES = '<a href="https://versatiles.org/">VersaTiles</a>'
// The styles also draw ESA WorldCover land cover (CC BY 4.0), GEBCO ocean
// depth and Mapzen terrain shading. These strings replace the styles' own
// source credits, so they name them too.
const ESA = '<a href="https://esa-worldcover.org/en/data-access">ESA WorldCover</a>'
const GEBCO = '<a href="https://www.gebco.net/">GEBCO</a>'
const MAPZEN = '<a href="https://github.com/tilezen/joerd/blob/master/docs/attribution.md">Mapzen</a>'
const CC_BY = '<a href="https://creativecommons.org/licenses/by/4.0/">CC BY 4.0</a>'
export const MAP_ATTRIBUTION = `${VERSATILES} | ${OSM_COPYRIGHT} contributors · &copy; ${ESA} (${CC_BY}) · ${GEBCO} · ${MAPZEN}`
export const COMPACT_MAP_ATTRIBUTION = `${VERSATILES} | ${OSM_COPYRIGHT} · ${ESA} · ${GEBCO} · ${MAPZEN}`
// List thumbnails are too narrow for more than the OpenStreetMap credit the data license requires.
export const THUMBNAIL_MAP_ATTRIBUTION = OSM_COPYRIGHT

// Painted under the map so routes stay readable while a style loads, when
// the map server is unreachable (for example a Pi without internet access)
// and when the browser has no WebGL2.
export const MAP_BACKGROUNDS: Record<MapStyle, string> = { streets: "#f8f4f0", dark: "#141c21" }

export const NO_WEBGL_NOTICE = "Map unavailable: this browser can't draw maps (WebGL is turned off)."
const NO_WEBGL_SHORT_NOTICE = "Map needs WebGL"

// Older builds offered "satellite"; it resolves to the dark default.
export function normalizeMapStyle(saved: unknown): MapStyle {
  return saved === "streets" ? "streets" : "dark"
}

let webglSupport: boolean | undefined
export function supportsWebGL(): boolean {
  if (webglSupport !== undefined) return webglSupport
  try {
    webglSupport = !!document.createElement("canvas").getContext("webgl2")
  } catch {
    webglSupport = false
  }
  return webglSupport
}

type Libraries = {
  maplibregl: typeof import("maplibre-gl")
  maplibreGL: typeof import("@maplibre/maplibre-gl-leaflet").maplibreGL
}
let libraries: Promise<Libraries> | null = null
function loadLibraries(): Promise<Libraries> {
  libraries ??= Promise.all([
    import("maplibre-gl"),
    import("@maplibre/maplibre-gl-leaflet"),
    // The worker is a separate module file; Vite emits it as an asset.
    import("maplibre-gl/dist/maplibre-gl-worker.mjs?url"),
    import("maplibre-gl/dist/maplibre-gl.css"),
  ]).then(([maplibregl, plugin, worker]) => {
    maplibregl.setWorkerUrl(worker.default)
    return { maplibregl, maplibreGL: plugin.maplibreGL }
  })
  libraries.catch(() => { libraries = null })
  return libraries
}
export function loadMapLibre() {
  return loadLibraries().then(({ maplibregl }) => maplibregl)
}

// The VersaTiles styles ask for a globe at low zoom. Leaflet layers and
// thumbnail routes are Web Mercator, so every map here stays Mercator.
export function keepMercator(glMap: MapLibreMap) {
  glMap.on("style.load", () => glMap.setProjection({ type: "mercator" }))
}

// Fetch both styles once a map has loaded, so the first switch is instant.
export function prefetchMapStyles(glMap: MapLibreMap) {
  glMap.once("load", () => prefetchStyles(Object.values(MAP_STYLE_URLS)))
}

export function mapStyle(style: MapStyle) {
  return cachedStyle(MAP_STYLE_URLS[style])
}

export interface Basemap {
  setStyle(style: MapStyle): void
  remove(): void
}

// Small corner note shown in place of the basemap when WebGL2 is missing.
// Thumbnails get a shorter line, with the full text as a tooltip.
function addNoWebGLNotice(map: L.Map, { compact = false } = {}): L.Control {
  const control = new L.Control({ position: "bottomleft" })
  control.onAdd = () => {
    const el = L.DomUtil.create("div", "map-webgl-notice")
    el.textContent = compact ? NO_WEBGL_SHORT_NOTICE : NO_WEBGL_NOTICE
    if (compact) el.title = NO_WEBGL_NOTICE
    el.setAttribute("role", "note")
    return el
  }
  control.addTo(map)
  return control
}

/**
 * Add the basemap to an interactive Leaflet map. The style loads
 * asynchronously; a map removed before then is left untouched.
 */
export function attachBasemap(
  map: L.Map,
  style: MapStyle,
  { attribution = MAP_ATTRIBUTION }: { attribution?: string } = {},
): Basemap {
  let current = normalizeMapStyle(style)
  let removed = false
  let glLayer: L.Layer | null = null
  let notice: L.Control | null = null
  const container = map.getContainer()
  const paint = () => { container.style.background = MAP_BACKGROUNDS[current] }
  paint()
  map.once("unload", () => { removed = true })

  if (supportsWebGL()) {
    loadLibraries().then(({ maplibreGL }) => {
      if (removed) return
      const layer = maplibreGL({
        style: MAP_STYLE_URLS[current],
        interactive: false,
        // Leaflet's attribution control shows this in place of the style's
        // source credits.
        attributionControl: { customAttribution: attribution },
      } as Parameters<typeof maplibreGL>[0])
      layer.addTo(map)
      keepMercator(layer.getMaplibreMap())
      prefetchMapStyles(layer.getMaplibreMap())
      glLayer = layer
    }).catch((err) => { console.warn("[map] Vector basemap unavailable; routes draw on a plain background:", err) })
  } else {
    // No basemap: the background colour stays, routes and markers draw on top.
    notice = addNoWebGLNotice(map)
  }

  return {
    setStyle(next) {
      const style = normalizeMapStyle(next)
      if (style === current) return
      current = style
      paint()
      const gl = glLayer as (L.Layer & { getMaplibreMap(): MapLibreMap }) | null
      gl?.getMaplibreMap()?.setStyle(mapStyle(current))
    },
    remove() {
      removed = true
      if (glLayer) map.removeLayer(glLayer)
      notice?.remove()
      glLayer = null
      notice = null
    },
  }
}

/**
 * Basemap for a static thumbnail whose view is already set: an image of the
 * current view from the shared renderer, or the style's background colour
 * and a short notice without WebGL. `onSettled` fires once the basemap has
 * loaded or failed. Returns a cleanup function.
 */
export function attachStaticBasemap(map: L.Map, style: MapStyle, onSettled: () => void): () => void {
  let active = true
  const settle = () => { if (active) onSettled() }
  map.getContainer().style.background = MAP_BACKGROUNDS[style]
  if (!supportsWebGL()) {
    const notice = addNoWebGLNotice(map, { compact: true })
    queueMicrotask(settle)
    return () => { active = false; notice.remove() }
  }
  map.attributionControl?.addAttribution(THUMBNAIL_MAP_ATTRIBUTION)
  let overlay: L.ImageOverlay | null = null
  const size = map.getSize()
  const center = map.getCenter()
  import("./mapSnapshot").then(({ renderMapSnapshot }) => renderMapSnapshot({
    style, width: size.x, height: size.y, center: [center.lng, center.lat],
    // Leaflet zooms count 256px tiles; MapLibre's count 512px tiles.
    zoom: map.getZoom() - 1,
  })).then(url => {
    if (!active) return
    if (!url) return settle()
    overlay = L.imageOverlay(url, map.getBounds(), { pane: "tilePane", interactive: false })
    overlay.once("load error", settle)
    overlay.addTo(map)
  }).catch(settle)
  return () => {
    active = false
    if (overlay) map.removeLayer(overlay)
  }
}

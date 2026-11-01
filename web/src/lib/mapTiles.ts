import L from "leaflet"
import type { Map as MapLibreMap } from "maplibre-gl"

// Basemaps: OpenStreetMap vector tiles from the SentryUSB VersaTiles server,
// drawn by MapLibre GL. Interactive maps add the style as a Leaflet layer
// (maplibre-gl-leaflet) under the existing Leaflet routes and markers; list
// thumbnails show an image from one shared offscreen renderer
// (mapSnapshot.ts) so long lists do not open a WebGL context per row.
//
// Browsers without WebGL use OpenStreetMap raster tiles instead, so maps
// such as the Keep Accessory picker stay usable.
export const TILE_ORIGIN = "https://tiles.sentryusb.com"

export type MapStyle = "dark" | "streets"

export const MAP_STYLE_URLS: Record<MapStyle, string> = {
  streets: `${TILE_ORIGIN}/assets/styles/colorful/style.json`,
  dark: `${TILE_ORIGIN}/assets/styles/colorful-dark/style.json`,
}

const OSM_COPYRIGHT = '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a>'
const VERSATILES = '<a href="https://versatiles.org/">VersaTiles</a>'
export const MAP_ATTRIBUTION = `${VERSATILES} | ${OSM_COPYRIGHT} contributors`
export const COMPACT_MAP_ATTRIBUTION = `${VERSATILES} | ${OSM_COPYRIGHT}`

// Painted under the map so routes stay readable while a style loads or when
// the map server is unreachable (for example a Pi without internet access).
export const MAP_BACKGROUNDS: Record<MapStyle, string> = { streets: "#f8f4f0", dark: "#282725" }

export const OSM_RASTER = {
  url: "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
  maxZoom: 19,
  referrerPolicy: "strict-origin-when-cross-origin",
} as const

// Older builds offered "satellite"; it resolves to the dark default.
export function normalizeMapStyle(saved: unknown): MapStyle {
  return saved === "streets" ? "streets" : "dark"
}

let webglSupport: boolean | undefined
export function supportsWebGL(): boolean {
  if (webglSupport !== undefined) return webglSupport
  try {
    const canvas = document.createElement("canvas")
    webglSupport = !!(canvas.getContext("webgl2") || canvas.getContext("webgl"))
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
    import("maplibre-gl/dist/maplibre-gl.css"),
  ]).then(([maplibre, plugin]) => ({
    maplibregl: (maplibre as unknown as { default?: typeof import("maplibre-gl") }).default ?? maplibre,
    maplibreGL: plugin.maplibreGL,
  }))
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

export interface Basemap {
  setStyle(style: MapStyle): void
  remove(): void
}

function osmRaster(map: L.Map, attribution: string, options: L.TileLayerOptions = {}) {
  const layer = L.tileLayer(OSM_RASTER.url, {
    maxZoom: OSM_RASTER.maxZoom,
    referrerPolicy: OSM_RASTER.referrerPolicy,
    attribution: attribution.replace(VERSATILES + " | ", ""),
    ...options,
  })
  layer.addTo(map)
  return layer
}

/**
 * Add the basemap to an interactive Leaflet map. The style loads
 * asynchronously; a map removed before then is left untouched.
 */
export function attachBasemap(
  map: L.Map,
  style: MapStyle,
  { attribution = MAP_ATTRIBUTION, minZoom }: { attribution?: string; minZoom?: number } = {},
): Basemap {
  let current = normalizeMapStyle(style)
  let removed = false
  let glLayer: L.Layer | null = null
  let rasterLayer: L.TileLayer | null = null
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
      glLayer = layer
    }).catch(() => { /* plain background; routes still draw */ })
  } else {
    rasterLayer = osmRaster(map, attribution, { ...(minZoom ? { minZoom } : {}), className: current === "dark" ? "map-dark-tiles" : "" })
  }

  return {
    setStyle(next) {
      const style = normalizeMapStyle(next)
      if (style === current) return
      current = style
      paint()
      const gl = glLayer as (L.Layer & { getMaplibreMap(): MapLibreMap }) | null
      gl?.getMaplibreMap()?.setStyle(MAP_STYLE_URLS[current])
      rasterLayer?.getContainer()?.classList.toggle("map-dark-tiles", current === "dark")
    },
    remove() {
      removed = true
      for (const layer of [glLayer, rasterLayer]) if (layer) map.removeLayer(layer)
      glLayer = rasterLayer = null
    },
  }
}

/**
 * Basemap for a static thumbnail whose view is already set: an image of the
 * current view from the shared renderer, or OpenStreetMap raster tiles
 * without WebGL. `onSettled` fires once the basemap has loaded or failed.
 * Returns a cleanup function.
 */
export function attachStaticBasemap(map: L.Map, style: MapStyle, onSettled: () => void): () => void {
  let active = true
  const settle = () => { if (active) onSettled() }
  if (!supportsWebGL()) {
    const layer = osmRaster(map, COMPACT_MAP_ATTRIBUTION, { maxZoom: 18, minZoom: 3, className: style === "dark" ? "map-dark-tiles" : "" })
    layer.on("load tileerror", settle)
    return () => { active = false; layer.off("load tileerror", settle) }
  }
  map.getContainer().style.background = MAP_BACKGROUNDS[style]
  map.attributionControl?.addAttribution(COMPACT_MAP_ATTRIBUTION)
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

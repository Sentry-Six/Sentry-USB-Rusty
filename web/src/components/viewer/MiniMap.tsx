import { useEffect, useId, useRef, useState, memo } from "react"
import L from "leaflet"
import "leaflet/dist/leaflet.css"
import { attachBasemap, COMPACT_MAP_ATTRIBUTION } from "@/lib/mapTiles"
import { AddIcon, CloseFullscreenIcon, LocationOnIcon, OpenInFullIcon } from "@/components/icons"
import type { ClipTelemetry, TelemetryFrame } from "@/lib/api"
import { useFloatingCard } from "@/hooks/useFloatingCard"
import type { Point, Size } from "@/lib/floatingCard"
import { cn } from "@/lib/utils"

// The map panel from Sentry Studio, drawn the same way: a rounded glass
// square with no title bar, centred on the car and following it, the car as
// Studio's red heading arrow, the route as a thick line in Studio's colours,
// and the map credits as one small line at the bottom.

const PLACE_KEY = "viewer-map-card"
const OPEN_KEY = "viewer-map-open"

// Studio's map settings.
const DEFAULT_ZOOM = 16
const MIN_ZOOM = 11
const MAX_ZOOM = 19
const ROUTE_ASSISTED = "#22c55e"
const ROUTE_MANUAL = "#3b82f6"
// The same size the Drives map draws its arrow.
const ARROW_PX = 18
// The map is re-centred once the car is this far from the middle.
const FOLLOW_SLACK_PX = 2
// Heading is taken across about half a second of driving (36 frames a second).
const HEADING_SPAN = 9

// Leaflet's own look, restyled for this panel only. The credit line is
// Studio's: 8 px, white at 75%, on a soft dark pill, centred at the bottom.
// The arrow's size has to be set here, with a selector that outranks
// Leaflet's own rule for images in the marker pane (width: auto): without
// it the image ignores its width attribute and draws at its full 68 px.
const MAP_CSS = `
.viewer-map .leaflet-container{background:#141c21;font:inherit}
.viewer-map .leaflet-bottom.leaflet-right{left:0;right:0;display:flex;justify-content:center;padding:0 12px 4px}
.viewer-map .leaflet-control-attribution{float:none;margin:0;padding:1px 5px;border-radius:6px;background:rgba(0,0,0,.45);color:rgba(255,255,255,.75);font-size:8px;line-height:1.3;text-align:center}
.viewer-map .leaflet-control-attribution a{color:inherit;text-decoration:none}
.viewer-map .leaflet-control-attribution a:hover{text-decoration:underline}
.viewer-map .viewer-map-car{pointer-events:none}
.viewer-map .leaflet-container .leaflet-marker-pane .viewer-map-car img{display:block;width:${ARROW_PX}px;height:${ARROW_PX}px;max-width:none;transition:transform 150ms linear;filter:drop-shadow(0 1px 2px rgba(0,0,0,.6))}
`

// Until it is moved, the panel sits in the top right corner of the cameras.
function defaultPoint(area: HTMLElement, card: Size): Point {
  return { x: area.clientWidth - card.width - 8, y: 8 }
}

// Open or folded away is remembered. With no choice yet, phones start folded:
// an open map covers most of a camera there.
function startsOpen(): boolean {
  try {
    const saved = localStorage.getItem(OPEN_KEY)
    if (saved !== null) return saved === "1"
  } catch { /* not remembered */ }
  return typeof window === "undefined" || window.innerWidth >= 768
}

const hasGps = (frame: TelemetryFrame) => frame.lat !== 0 || frame.lng !== 0

// Compass bearing from one point to another, in degrees clockwise from north.
function bearing(from: TelemetryFrame, to: TelemetryFrame): number | null {
  const rad = Math.PI / 180
  const dLng = (to.lng - from.lng) * rad
  const y = Math.sin(dLng) * Math.cos(to.lat * rad)
  const x = Math.cos(from.lat * rad) * Math.sin(to.lat * rad) - Math.sin(from.lat * rad) * Math.cos(to.lat * rad) * Math.cos(dLng)
  // Under about a metre apart the direction is GPS noise: keep the last one.
  if (Math.hypot(to.lat - from.lat, (to.lng - from.lng) * Math.cos(from.lat * rad)) < 0.00001) return null
  return (Math.atan2(y, x) / rad + 360) % 360
}

function drawRoute(layer: L.LayerGroup, frames: TelemetryFrame[]) {
  // One line per stretch of the same driving state, so each takes its colour.
  let stretch: L.LatLng[] = []
  let assisted = frames[0].autopilot > 0
  const add = (points: L.LatLng[], isAssisted: boolean) => {
    if (points.length < 2) return
    L.polyline(points, {
      color: isAssisted ? ROUTE_ASSISTED : ROUTE_MANUAL,
      weight: 4,
      opacity: 1,
      lineCap: "round",
      lineJoin: "round",
      interactive: false,
    }).addTo(layer)
  }
  for (const frame of frames) {
    const point = L.latLng(frame.lat, frame.lng)
    if (frame.autopilot > 0 !== assisted) {
      // Share the point where the state changes so the lines meet.
      stretch.push(point)
      add(stretch, assisted)
      stretch = [point]
      assisted = frame.autopilot > 0
    } else {
      stretch.push(point)
    }
  }
  add(stretch, assisted)
}

interface MiniMapProps {
  telemetry: ClipTelemetry
  currentFrame: TelemetryFrame | null
  /** True while the route is held over from the minute before and the next minute's is loading. */
  stale?: boolean
}

const mapButton = "flex h-6 w-6 items-center justify-center rounded-full border border-white/10 bg-black/55 text-slate-200 backdrop-blur-md transition-colors hover:bg-black/75"

export default memo(function MiniMap({ telemetry, currentFrame, stale = false }: MiniMapProps) {
  const { ref: cardRef, cardProps, dragging } = useFloatingCard({ storageKey: PLACE_KEY, defaultPoint })
  const hintId = useId()
  const mapRef = useRef<HTMLDivElement>(null)
  const mapInstance = useRef<L.Map | null>(null)
  const routeLayer = useRef<L.LayerGroup | null>(null)
  const car = useRef<L.Marker | null>(null)
  const heading = useRef(0)
  const zoom = useRef(DEFAULT_ZOOM)
  const lastMove = useRef(0)
  const [open, setOpen] = useState(startsOpen)

  const toggle = () => {
    const next = !open
    setOpen(next)
    try { localStorage.setItem(OPEN_KEY, next ? "1" : "0") } catch { /* not remembered */ }
  }

  const zoomBy = (step: number) => {
    const map = mapInstance.current
    if (!map) return
    zoom.current = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, zoom.current + step))
    map.setZoom(zoom.current, { animate: false })
  }

  // One map for as long as the panel is open. It is kept across the minutes
  // of a clip: building a new map each minute flashed, and tearing one down
  // while its style was still loading threw errors. The map itself takes no
  // mouse or touch: it follows the car, and dragging it moves the panel.
  useEffect(() => {
    if (!open || !mapRef.current) return
    const map = L.map(mapRef.current, {
      zoomControl: false,
      attributionControl: true,
      dragging: false,
      scrollWheelZoom: false,
      doubleClickZoom: false,
      touchZoom: false,
      boxZoom: false,
      keyboard: false,
      zoomSnap: 0.5,
    }).setView([0, 0], zoom.current)
    map.attributionControl.setPrefix(false)
    attachBasemap(map, "dark", { attribution: COMPACT_MAP_ATTRIBUTION })
    mapInstance.current = map
    routeLayer.current = L.layerGroup().addTo(map)
    return () => {
      map.remove()
      mapInstance.current = null
      routeLayer.current = null
      car.current = null
    }
  }, [open])

  // Draw this minute's route and put the car at its start. Runs after the
  // effect above, so the map exists.
  useEffect(() => {
    const map = mapInstance.current
    const layer = routeLayer.current
    if (!open || !map || !layer) return
    layer.clearLayers()
    car.current = null
    const gpsFrames = telemetry.frames.filter(hasGps)
    if (gpsFrames.length === 0) return
    if (gpsFrames.length > 1) drawRoute(layer, gpsFrames)
    const start: L.LatLngTuple = [gpsFrames[0].lat, gpsFrames[0].lng]
    car.current = L.marker(start, {
      interactive: false,
      keyboard: false,
      icon: L.divIcon({
        className: "viewer-map-car",
        html: `<img src="/arrow.png" alt="" width="${ARROW_PX}" height="${ARROW_PX}" draggable="false" style="transform:rotate(${heading.current}deg)"/>`,
        iconSize: [ARROW_PX, ARROW_PX],
        iconAnchor: [ARROW_PX / 2, ARROW_PX / 2],
      }),
    }).addTo(layer)
    map.setView(start, zoom.current, { animate: false })
  }, [open, telemetry])

  // Move the car, turn it to its heading and keep the map centred on it,
  // at most about ten times a second.
  useEffect(() => {
    const map = mapInstance.current
    const marker = car.current
    if (!map || !marker || !currentFrame || !hasGps(currentFrame)) return
    const now = performance.now()
    if (now - lastMove.current < 100) return
    lastMove.current = now

    const at = telemetry.frames.indexOf(currentFrame)
    if (at >= 0) {
      const before = telemetry.frames[Math.max(0, at - HEADING_SPAN)]
      const after = telemetry.frames[Math.min(telemetry.frames.length - 1, at + HEADING_SPAN)]
      const turned = hasGps(before) && hasGps(after) ? bearing(before, after) : null
      if (turned !== null) heading.current = turned
    }
    const position: L.LatLngTuple = [currentFrame.lat, currentFrame.lng]
    marker.setLatLng(position)
    const arrow = marker.getElement()?.querySelector("img")
    if (arrow) arrow.style.transform = `rotate(${heading.current}deg)`

    const size = map.getSize()
    const point = map.latLngToContainerPoint(position)
    if (Math.abs(point.x - size.x / 2) > FOLLOW_SLACK_PX || Math.abs(point.y - size.y / 2) > FOLLOW_SLACK_PX) {
      map.setView(position, zoom.current, { animate: false })
    }
  }, [currentFrame, telemetry])

  const shared = {
    ref: cardRef,
    ...cardProps,
    role: "group",
    "aria-label": "Map",
    "aria-describedby": hintId,
    "aria-busy": stale,
  } as const
  const hint = (
    <span id={hintId} className="sr-only">
      Drag to move. With the keyboard, the arrow keys move this panel, Shift moves it further, and Home puts it back.
    </span>
  )

  if (!open) {
    return (
      <div
        {...shared}
        title="Drag to move"
        className={cn(
          "z-20 flex select-none items-center gap-1.5 rounded-lg border border-white/10 bg-black/60 py-1 pl-2.5 pr-1 text-xs font-medium text-slate-200 backdrop-blur-md",
          "outline-none focus-visible:ring-2 focus-visible:ring-blue-400",
          dragging && "border-white/25",
        )}
      >
        {hint}
        <LocationOnIcon className="h-3.5 w-3.5 text-blue-400" aria-hidden="true" />
        Map
        <button
          onClick={toggle}
          aria-expanded={false}
          aria-label="Show map"
          title="Show map"
          className="rounded-md p-1 text-slate-400 transition-colors hover:bg-white/10 hover:text-slate-100"
        >
          <OpenInFullIcon className="h-3.5 w-3.5" />
        </button>
      </div>
    )
  }

  return (
    <div
      {...shared}
      title="Green: assisted driving. Blue: manual. Drag to move."
      className={cn(
        "viewer-map group z-20 h-[220px] w-[220px] max-w-full select-none overflow-hidden rounded-3xl border border-white/[0.08] bg-[#141c21]",
        "outline-none focus-visible:ring-2 focus-visible:ring-blue-400",
        dragging && "border-white/25",
      )}
      style={{ ...cardProps.style, boxShadow: "0 12px 34px rgba(0,0,0,0.4)" }}
    >
      <style href="viewer-map" precedence="default">{MAP_CSS}</style>
      {hint}
      {/* The map library adds its own classes to the map's element, and
          React would wipe them if that element's className ever changed, so
          the dimming goes on a wrapper. */}
      <div className={cn("h-full w-full transition-opacity", stale && "opacity-60")}>
        <div ref={mapRef} className="h-full w-full" />
      </div>
      {/* Catches the mouse and touch above the map, so dragging anywhere on
          the panel moves it. It sits under the map's credit line (z 1000),
          whose links stay clickable. */}
      <div className="absolute inset-0 z-[800]" />
      <div className="absolute right-2 top-2 z-[1100] flex flex-col gap-1 opacity-0 transition-opacity focus-within:opacity-100 group-hover:opacity-100 [@media(hover:none)]:opacity-100">
        <button onClick={toggle} aria-expanded={true} aria-label="Hide map" title="Hide map" className={mapButton}>
          <CloseFullscreenIcon className="h-3 w-3" />
        </button>
        <button onClick={() => zoomBy(1)} aria-label="Zoom in" title="Zoom in" className={mapButton}>
          <AddIcon className="h-3.5 w-3.5" />
        </button>
        <button onClick={() => zoomBy(-1)} aria-label="Zoom out" title="Zoom out" className={mapButton}>
          <span aria-hidden="true" className="block h-px w-2.5 bg-current" />
        </button>
      </div>
    </div>
  )
})

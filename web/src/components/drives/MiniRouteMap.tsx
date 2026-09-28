import { useEffect, useRef, useState } from "react"
import L from "leaflet"
import "leaflet/dist/leaflet.css"
import { COMPACT_MAP_ATTRIBUTION, MAP_TILES } from "@/lib/mapTiles"
import { ProgressActivityIcon } from "@/components/icons"

interface MiniRouteMapProps {
  points: [number, number][]
  source?: string
  status: "loading" | "ready" | "unavailable"
}

export function MiniRouteMap({ points, source, status }: MiniRouteMapProps) {
  const frameRef = useRef<HTMLDivElement>(null)
  const containerRef = useRef<HTMLDivElement>(null)
  const mapRef = useRef<L.Map | null>(null)
  const [visible, setVisible] = useState(false)
  const [tiles, setTiles] = useState({ points, source, status, loaded: false })
  if (tiles.points !== points || tiles.source !== source || tiles.status !== status) {
    setTiles({ points, source, status, loaded: false })
  }
  const loading = status === "loading" || (status === "ready" && points.length > 0 && !tiles.loaded)

  useEffect(() => {
    const el = frameRef.current
    if (!el) return
    const io = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (e.isIntersecting) {
            setVisible(true)
            io.disconnect()
            break
          }
        }
      },
      { rootMargin: "200px" },
    )
    io.observe(el)
    return () => io.disconnect()
  }, [])

  useEffect(() => {
    if (!visible || status !== "ready") return
    const el = containerRef.current
    if (!el || mapRef.current) return
    if (points.length === 0) return

    const stroke = source === "tessie" ? "#a78bfa" : "#34d399"

    const map = L.map(el, {
      attributionControl: true,
      zoomControl: false,
      dragging: false,
      scrollWheelZoom: false,
      doubleClickZoom: false,
      touchZoom: false,
      keyboard: false,
      boxZoom: false,
    })
    mapRef.current = map
    map.attributionControl.setPrefix(false)

    let active = true
    let finished = false
    const finishLoading = () => {
      if (!active || finished) return
      finished = true
      setTiles({ points, source, status, loaded: true })
      clearTimeout(timeout)
    }
    // A stalled tile server must not hide an available route behind a permanent spinner.
    const timeout = setTimeout(finishLoading, 15_000)
    const tileLayer = L.tileLayer(MAP_TILES.dark.url, {
      ...MAP_TILES.dark,
      attribution: COMPACT_MAP_ATTRIBUTION,
      maxZoom: 18,
      minZoom: 3,
    })
    tileLayer.on("load tileerror", finishLoading)
    tileLayer.addTo(map)

    const latLngs = points.map(([lat, lng]) => L.latLng(lat, lng))
    L.polyline(latLngs, {
      color: stroke,
      weight: 2.5,
      opacity: 0.95,
      smoothFactor: 1.5,
    }).addTo(map)

    if (latLngs.length >= 2) {
      L.circleMarker(latLngs[0], {
        radius: 3,
        color: "#94a3b8",
        weight: 1.5,
        fillColor: "#94a3b8",
        fillOpacity: 1,
      }).addTo(map)
      L.circleMarker(latLngs[latLngs.length - 1], {
        radius: 3,
        color: stroke,
        weight: 1.5,
        fillColor: stroke,
        fillOpacity: 1,
      }).addTo(map)
    }

    map.fitBounds(L.latLngBounds(latLngs), { padding: [8, 8], maxZoom: 15 })

    return () => {
      active = false
      clearTimeout(timeout)
      tileLayer.off("load tileerror", finishLoading)
      map.remove()
      mapRef.current = null
    }
  }, [visible, points, source, status])

  return (
    <div
      ref={frameRef}
      className="map-thumbnail relative isolate h-20 w-32 shrink-0 overflow-hidden rounded-lg bg-slate-900/60 ring-1 ring-inset ring-white/5"
      role="img"
      aria-label={loading ? "Loading route map" : status === "unavailable" ? "Route map unavailable" : points.length === 0 ? "No recorded route" : "Route thumbnail"}
      aria-busy={loading}
    >
      <div ref={containerRef} className="absolute inset-0" />
      {loading ? (
        <div className="pointer-events-none absolute inset-0 z-[500] flex items-center justify-center bg-slate-950/30" aria-hidden="true">
          <ProgressActivityIcon className="h-5 w-5 animate-spin text-emerald-400" />
        </div>
      ) : (status === "unavailable" || points.length === 0) ? (
        <div className="absolute inset-0 flex items-center justify-center px-2 text-center text-xs text-muted-foreground">
          {status === "unavailable" ? "Map unavailable" : "No route"}
        </div>
      ) : null}
    </div>
  )
}

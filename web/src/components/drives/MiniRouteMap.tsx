import { useEffect, useRef, useState } from "react"
import L from "leaflet"
import "leaflet/dist/leaflet.css"
import { COMPACT_MAP_ATTRIBUTION, MAP_TILES } from "@/lib/mapTiles"

interface MiniRouteMapProps {
  points: [number, number][]
  source?: string
}

export function MiniRouteMap({ points, source }: MiniRouteMapProps) {
  const containerRef = useRef<HTMLDivElement>(null)
  const mapRef = useRef<L.Map | null>(null)
  const [visible, setVisible] = useState(false)

  useEffect(() => {
    const el = containerRef.current
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
    if (!visible) return
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

    L.tileLayer(MAP_TILES.dark.url, {
      ...MAP_TILES.dark,
      attribution: COMPACT_MAP_ATTRIBUTION,
      maxZoom: 18,
      minZoom: 3,
    }).addTo(map)

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
      map.remove()
      mapRef.current = null
    }
  }, [visible, points, source])

  return (
    <div
      ref={containerRef}
      className="map-thumbnail relative h-20 w-32 shrink-0 overflow-hidden rounded-lg bg-slate-900/60 ring-1 ring-inset ring-white/5"
      role="img"
      aria-label="Route thumbnail"
    />
  )
}

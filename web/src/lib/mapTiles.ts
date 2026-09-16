import type { TileLayerOptions } from "leaflet"

const streets = {
  url: "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
  attribution: '&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> contributors',
  maxZoom: 19,
  referrerPolicy: "strict-origin-when-cross-origin",
} satisfies TileLayerOptions & { url: string }

// Match Sentry Cloud: dark styling applies only to tiles, preserving overlay colors.
export const MAP_TILES = {
  streets,
  dark: { ...streets, className: "map-dark-tiles" },
  // Hybrid imagery includes labels without a separate tile overlay.
  satellite: {
    url: "https://mt1.google.com/vt/lyrs=y&x={x}&y={y}&z={z}",
    attribution: "&copy; Google",
    maxZoom: 20,
  },
} satisfies Record<string, TileLayerOptions & { url: string }>

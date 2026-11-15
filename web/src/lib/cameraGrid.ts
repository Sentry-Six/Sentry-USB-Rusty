// Lays the cameras out as the largest grid of true-shape pictures that fits
// the space, the way Sentry Studio's player does: no black bands inside a
// camera, three across on a wide screen, two across on a phone held upright.

import type { Size } from "@/lib/floatingCard"

export interface GridFit {
  cols: number
  rows: number
  /** Size of the whole grid in pixels; 0 when the space has not been measured. */
  width: number
  height: number
}

// A narrower grid must show clearly bigger pictures to be chosen over a wider
// one: on a desktop the cameras sit three across with the front camera in
// the middle, as in the car, even when two across would be a few pixels larger.
const NARROWER_MUST_GAIN = 1.2

export function fitCameraGrid(area: Size, count: number, aspect: number, gap: number): GridFit {
  const tiles = Math.max(1, count)
  const widest = Math.min(tiles, 3)
  const unmeasured = { cols: widest, rows: Math.ceil(tiles / widest), width: 0, height: 0 }
  if (area.width <= 0 || area.height <= 0 || !(aspect > 0)) return unmeasured
  // Keep the grid roughly square in its count (3x2 or 2x3, never 1x6 or 6x1).
  const shapes: number[] = []
  for (let cols = tiles; cols >= 1; cols--) {
    if (Math.abs(cols - Math.ceil(tiles / cols)) <= 1) shapes.push(cols)
  }
  // Grids with no empty cell (2x2 for four cameras) beat ones with holes.
  const full = shapes.filter((cols) => tiles % cols === 0)
  let best: (GridFit & { tile: number }) | null = null
  for (const cols of full.length > 0 ? full : shapes) {
    const rows = Math.ceil(tiles / cols)
    const byWidth = (area.width - gap * (cols - 1)) / cols
    const byHeight = ((area.height - gap * (rows - 1)) / rows) * aspect
    const tile = Math.floor(Math.min(byWidth, byHeight))
    if (tile <= 0) continue
    if (!best || tile > best.tile * NARROWER_MUST_GAIN) {
      best = {
        cols,
        rows,
        tile,
        width: tile * cols + gap * (cols - 1),
        height: Math.floor(tile / aspect) * rows + gap * (rows - 1),
      }
    }
  }
  return best ? { cols: best.cols, rows: best.rows, width: best.width, height: best.height } : unmeasured
}

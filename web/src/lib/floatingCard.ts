// Geometry and storage for a card the user can drag around an area.
//
// A card's place is remembered as how far along its free travel it sits on
// each axis: 0 is flush with the left or top edge, 1 flush with the right or
// bottom. Stored that way, a card parked in a corner stays in that corner
// and the card is always fully inside the area, whatever size the area is
// next time: a phone, a desktop window, or fullscreen.

export interface Size { width: number; height: number }
export interface Point { x: number; y: number }
/** Share of the free travel on each axis, 0 to 1. */
export type Placement = Point

const clamp = (value: number, min: number, max: number) => Math.max(min, Math.min(value, max))

/** Keep a card's top-left corner where the whole card stays inside the area. */
export function clampPoint(point: Point, area: Size, card: Size): Point {
  return {
    x: clamp(point.x, 0, Math.max(0, area.width - card.width)),
    y: clamp(point.y, 0, Math.max(0, area.height - card.height)),
  }
}

export function toPlacement(point: Point, area: Size, card: Size): Placement {
  const share = (at: number, travel: number) => travel > 0 ? clamp(at / travel, 0, 1) : 0.5
  return {
    x: share(point.x, area.width - card.width),
    y: share(point.y, area.height - card.height),
  }
}

export function fromPlacement(placement: Placement, area: Size, card: Size): Point {
  return {
    x: Math.round(clamp(placement.x, 0, 1) * Math.max(0, area.width - card.width)),
    y: Math.round(clamp(placement.y, 0, 1) * Math.max(0, area.height - card.height)),
  }
}

/** A saved placement, or null when the text is missing or not one. */
export function parsePlacement(raw: string | null | undefined): Placement | null {
  if (!raw) return null
  try {
    const value: unknown = JSON.parse(raw)
    if (typeof value !== "object" || value === null) return null
    const { x, y } = value as Record<string, unknown>
    if (typeof x !== "number" || typeof y !== "number") return null
    if (!Number.isFinite(x) || !Number.isFinite(y)) return null
    return { x: clamp(x, 0, 1), y: clamp(y, 0, 1) }
  } catch {
    return null
  }
}

// Private browsing and blocked site data make localStorage throw; the card
// then simply starts from its usual spot each visit.
export function loadPlacement(key: string): Placement | null {
  try {
    return parsePlacement(localStorage.getItem(key))
  } catch {
    return null
  }
}

/** Remember a placement, or forget it with null. */
export function savePlacement(key: string, placement: Placement | null): void {
  try {
    if (placement) {
      const round = (n: number) => Math.round(n * 10000) / 10000
      localStorage.setItem(key, JSON.stringify({ x: round(placement.x), y: round(placement.y) }))
    } else {
      localStorage.removeItem(key)
    }
  } catch { /* not remembered */ }
}

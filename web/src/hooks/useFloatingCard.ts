import { useCallback, useLayoutEffect, useRef, useState } from "react"
import {
  clampPoint,
  fromPlacement,
  loadPlacement,
  savePlacement,
  toPlacement,
  type Placement,
  type Point,
  type Size,
} from "@/lib/floatingCard"

interface FloatingCardOptions {
  /** localStorage key the card's place is remembered under. */
  storageKey: string
  /**
   * Where the card sits until the user moves it: its top-left corner inside
   * `area` (the card's positioned parent). Must be a stable function.
   */
  defaultPoint: (area: HTMLElement, card: Size) => Point
  /** Changes whenever the area's contents are rearranged without it resizing. */
  layoutKey?: string
}

const NUDGE = 12
const BIG_NUDGE = 48

// Parts of a card that keep their own mouse and touch behaviour (a map pans,
// a button clicks); everything else is the handle the card moves by.
const OWN_CONTROLS = "button, a, input, select, .leaflet-container"
const onOwnControl = (target: EventTarget) => target instanceof Element && target.closest(OWN_CONTROLS) !== null

/**
 * A card that can be moved anywhere inside its positioned parent and stays
 * where it was put: across clips, reloads and window sizes.
 *
 * Mouse, pen and touch drag it. With the keyboard, the arrow keys move it
 * (Shift moves further) and Home puts it back; double-click also puts it
 * back.
 */
export function useFloatingCard({ storageKey, defaultPoint, layoutKey = "" }: FloatingCardOptions) {
  const [card, setCard] = useState<HTMLDivElement | null>(null)
  const [placement, setPlacement] = useState<Placement | null>(() => loadPlacement(storageKey))
  const [point, setPoint] = useState<Point | null>(null)
  const [dragging, setDragging] = useState(false)
  const drag = useRef<{ pointerId: number; startX: number; startY: number; origin: Point } | null>(null)

  const measure = useCallback(() => {
    // The positioned parent the card moves within.
    const area = card?.offsetParent as HTMLElement | null | undefined
    if (!card || !area) return null
    return {
      area,
      areaSize: { width: area.clientWidth, height: area.clientHeight },
      cardSize: { width: card.offsetWidth, height: card.offsetHeight },
    }
  }, [card])

  // Put the card at its remembered (or usual) place, and again whenever the
  // area or the card changes size.
  useLayoutEffect(() => {
    if (!card) return
    const place = () => {
      if (drag.current) return
      const m = measure()
      if (!m) return
      const next = placement
        ? fromPlacement(placement, m.areaSize, m.cardSize)
        : clampPoint(defaultPoint(m.area, m.cardSize), m.areaSize, m.cardSize)
      setPoint((prev) => prev && prev.x === next.x && prev.y === next.y ? prev : next)
    }
    place()
    if (typeof ResizeObserver === "undefined") return
    const observer = new ResizeObserver(place)
    observer.observe(card)
    if (card.offsetParent) observer.observe(card.offsetParent)
    return () => observer.disconnect()
  }, [card, placement, defaultPoint, measure, layoutKey])

  const settle = useCallback((next: Point) => {
    const m = measure()
    if (!m) return
    const clamped = clampPoint(next, m.areaSize, m.cardSize)
    const nextPlacement = toPlacement(clamped, m.areaSize, m.cardSize)
    setPoint(clamped)
    setPlacement(nextPlacement)
    savePlacement(storageKey, nextPlacement)
  }, [measure, storageKey])

  const reset = useCallback(() => {
    setPlacement(null)
    savePlacement(storageKey, null)
  }, [storageKey])

  const draggedTo = (e: React.PointerEvent): Point | null => {
    const active = drag.current
    const m = measure()
    if (!active || active.pointerId !== e.pointerId || !m) return null
    return clampPoint(
      { x: active.origin.x + e.clientX - active.startX, y: active.origin.y + e.clientY - active.startY },
      m.areaSize,
      m.cardSize,
    )
  }

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!point || (e.pointerType === "mouse" && e.button !== 0)) return
    if (onOwnControl(e.target)) return
    drag.current = { pointerId: e.pointerId, startX: e.clientX, startY: e.clientY, origin: point }
    // Capture keeps the drag going when the pointer outruns the card. It can
    // be refused (the pointer is already gone); the drag then just ends early.
    try { e.currentTarget.setPointerCapture(e.pointerId) } catch { /* not captured */ }
    setDragging(true)
  }

  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const next = draggedTo(e)
    if (next) setPoint(next)
  }

  const endDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    const next = draggedTo(e)
    if (!drag.current || drag.current.pointerId !== e.pointerId) return
    const moved = next && (next.x !== drag.current.origin.x || next.y !== drag.current.origin.y)
    drag.current = null
    setDragging(false)
    try { e.currentTarget.releasePointerCapture(e.pointerId) } catch { /* was not captured */ }
    // A plain click must not turn the usual spot into a remembered one.
    if (next && moved) settle(next)
  }

  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.target !== e.currentTarget || !point) return
    const step = e.shiftKey ? BIG_NUDGE : NUDGE
    const moves: Record<string, Point> = {
      ArrowLeft: { x: -step, y: 0 },
      ArrowRight: { x: step, y: 0 },
      ArrowUp: { x: 0, y: -step },
      ArrowDown: { x: 0, y: step },
    }
    const move = moves[e.key]
    if (!move && e.key !== "Home") return
    // The page uses the arrow keys to skip through the clip; while the card
    // has focus they move the card instead.
    e.preventDefault()
    e.stopPropagation()
    if (move) settle({ x: point.x + move.x, y: point.y + move.y })
    else reset()
  }

  const style: React.CSSProperties = point
    ? { position: "absolute", left: point.x, top: point.y, touchAction: "none", cursor: dragging ? "grabbing" : "grab" }
    : { position: "absolute", visibility: "hidden", touchAction: "none" }

  return {
    ref: setCard,
    dragging,
    moved: placement !== null,
    cardProps: {
      style,
      tabIndex: 0,
      onPointerDown,
      onPointerMove,
      onPointerUp: endDrag,
      onPointerCancel: endDrag,
      onDoubleClick: (e: React.MouseEvent<HTMLDivElement>) => { if (!onOwnControl(e.target)) reset() },
      onKeyDown,
    },
  }
}

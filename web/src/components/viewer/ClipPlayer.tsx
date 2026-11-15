import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react"
import type { ClipEntry, ClipTelemetry, TelemetryFrame } from "@/lib/api"
import { useClipPlayer, type ClipSet } from "@/hooks/useClipPlayer"
import { useTelemetry } from "@/hooks/useTelemetry"
import { fitCameraGrid } from "@/lib/cameraGrid"
import { eventCamera, eventPosition } from "@/lib/clipEvent"
import { clipMomentMs, formatClipClock, formatClipDate, formatClipDay, parseEventTime } from "@/lib/clipTime"
import { useClockValue, type PlaybackClock } from "@/lib/playbackClock"
import { cn } from "@/lib/utils"
import CameraGrid from "./CameraGrid"
import { formatEventReason } from "./clipEvents"
import ControlBar from "./ControlBar"
import TelemetryOverlay from "./TelemetryOverlay"
import ViewerNotice from "./ViewerNotice"

// Lazy so the map library only downloads when a clip with GPS telemetry is
// opened. Clips without it never fetch the map chunk.
const MiniMap = lazy(() => import("./MiniMap"))

// Pillars on top, repeaters below, as Sentry Studio lays them out.
const CAMERAS_HW4 = ["left_pillar", "front", "right_pillar", "left_repeater", "back", "right_repeater"]
// Older cars have no pillar cameras.
const CAMERAS_HW3 = ["front", "back", "left_repeater", "right_repeater"]
// The picture shape of each, until the first video reports its own.
const ASPECT_HW4 = 2896 / 1876
const ASPECT_HW3 = 4 / 3
const GAP = 3
const CAMERAS_KEY = "viewer-cameras"
// Telemetry waits for the cameras to have their pictures, but not for ever.
const TELEMETRY_WAIT_MS = 1500
const TELEMETRY_CHECK_MS = 80

// Every camera at once is about 35 Mbit/s and six video decoders. Wide
// screens start that way; phones start with the front camera and add the
// others on a tap. Either choice, once made with the button, is remembered.
function startsWithAllCameras(): boolean {
  try {
    const saved = localStorage.getItem(CAMERAS_KEY)
    if (saved !== null) return saved === "all"
  } catch { /* not remembered */ }
  return typeof window === "undefined" || window.innerWidth >= 768
}

interface TelemetryCardsProps {
  clock: PlaybackClock
  telemetry: ClipTelemetry | null
  held: ClipTelemetry | null
  frameAtTime: (seconds: number) => TelemetryFrame | null
  metric: boolean
  layoutKey: string
  /** Start time of the minute on screen, for the time shown on the card. */
  stamp: string
  playing: boolean
  /** A Sentry clip: the car is parked, which is why there is no telemetry. */
  parkedClip: boolean
}

// The telemetry and map cards. They follow the clock on their own, ten
// times a second, without the rest of the player redrawing.
function TelemetryCards({ clock, telemetry, held, frameAtTime, metric, layoutKey, stamp, playing, parkedClip }: TelemetryCardsProps) {
  const tenth = useClockValue(clock, (t) => Math.round(t * 10))
  // While the next minute's telemetry loads, the cards keep the last values
  // of the minute before (dimmed) instead of disappearing every minute.
  const shown = telemetry ?? held
  if (!shown) return null
  const stale = !telemetry
  const frame = telemetry ? frameAtTime(tenth / 10) : shown.frames[shown.frames.length - 1] ?? null
  const moment = clipMomentMs(stamp, Math.floor(tenth / 10 + 0.001))
  return (
    <>
      <TelemetryOverlay
        frame={frame}
        metric={metric}
        // Date and time, as on Studio's compact dashboard.
        time={moment === null ? null : `${formatClipDate(moment)}\u2002${formatClipClock(moment)}`}
        playing={playing}
        // A minute with no telemetry at all: in a Sentry clip that is a
        // parked car. Elsewhere it may be a car too old to record any, so
        // the card stays away rather than claim the car was parked.
        parked={parkedClip && !!telemetry && telemetry.frames.length === 0}
        stale={stale}
        layoutKey={layoutKey}
      />
      {shown.has_gps && (
        <Suspense fallback={null}>
          <MiniMap telemetry={shown} currentFrame={frame} stale={stale} />
        </Suspense>
      )}
    </>
  )
}

interface ClipPlayerProps {
  clip: ClipEntry
  clipSets: ClipSet[]
  metric: boolean
  /** Download the clip's folder; absent for clips that have none of their own. */
  onDownload?: () => void
  /** The clip browser, which floats over the cameras. */
  panel: ReactNode
  /** Called when playback starts, so the clip browser can get out of the way. */
  onPlay?: () => void
  /** Set while the Pi is archiving and the user has not hidden the notice. */
  archiving?: { etaSeconds: number | null } | null
  onDismissArchiving?: () => void
}

// The player for one clip: the cameras filling the space at their true
// shape, cards floating over them, and one row of controls underneath.
// Mounted afresh for each clip.
export default function ClipPlayer({ clip, clipSets, metric, onDownload, panel, onPlay, archiving = null, onDismissArchiving }: ClipPlayerProps) {
  const hasPillars = useMemo(
    () => clipSets.some((set) => "left_pillar" in set.cameras || "right_pillar" in set.cameras),
    [clipSets],
  )
  const cameras = hasPillars ? CAMERAS_HW4 : CAMERAS_HW3

  const nearEnd = useRef<(next: number) => void>(() => {})
  const onNearEnd = useCallback((next: number) => nearEnd.current(next), [])
  const [startWithAll] = useState(startsWithAllCameras)
  const player = useClipPlayer({ clipSets, cameras, startWithAll, onNearEnd })
  const { clock, segment, focused, setFocused, skip, seekToGlobal, durations, playing } = player
  const playerToggle = player.togglePlay
  const togglePlay = useCallback(() => {
    if (!playing) onPlay?.()
    playerToggle()
  }, [playing, onPlay, playerToggle])

  const currentSet = clipSets[segment]
  const frontFile = currentSet?.cameras["front"]?.split("/").pop() ?? null
  // Each minute's telemetry is asked for once its pictures are up. The
  // browser keeps only six connections open to the Pi; asked for at the same
  // moment as six cameras, the telemetry made one of them wait its turn.
  const busy = player.busy
  const afterPictures = useCallback((signal: AbortSignal) => new Promise<void>((resolve) => {
    if (!busy()) { resolve(); return }
    const started = performance.now()
    const timer = setInterval(() => {
      if (!signal.aborted && busy() && performance.now() - started < TELEMETRY_WAIT_MS) return
      clearInterval(timer)
      resolve()
    }, TELEMETRY_CHECK_MS)
  }), [busy])
  const { telemetry, held, frameAtTime, prefetch } = useTelemetry(clip.path, frontFile, afterPictures)
  // The next minute's telemetry is fetched as its video starts loading.
  useEffect(() => {
    nearEnd.current = (next) => {
      const file = clipSets[next]?.cameras["front"]?.split("/").pop()
      if (file) prefetch(file)
    }
  }, [clipSets, prefetch])

  // Where on the timeline the Sentry or Saved event happened.
  const event = clip.event
  const triggeredCamera = eventCamera(event?.camera)
  const eventMs = useMemo(() => parseEventTime(event?.timestamp), [event?.timestamp])
  const stamps = useMemo(() => clipSets.map((set) => set.timestamp), [clipSets])
  const eventGlobal = useMemo(
    () => eventPosition(eventMs, stamps, durations)?.global ?? null,
    [eventMs, stamps, durations],
  )
  const eventTitle = eventMs === null ? "" : [
    event?.reason ? formatEventReason(event.reason).label : "Event",
    `${formatClipDay(eventMs)}, ${formatClipClock(eventMs)}`,
  ].join(" · ")
  const jumpToEvent = useCallback(() => {
    if (eventGlobal !== null) seekToGlobal(eventGlobal)
  }, [eventGlobal, seekToGlobal])

  // The space the cameras have, so the grid can fill it at their true shape.
  const stageRef = useRef<HTMLDivElement>(null)
  const [stage, setStage] = useState({ width: 0, height: 0 })
  useEffect(() => {
    const el = stageRef.current
    if (!el) return
    const measure = () => setStage((prev) => (
      prev.width === el.clientWidth && prev.height === el.clientHeight ? prev : { width: el.clientWidth, height: el.clientHeight }
    ))
    measure()
    if (typeof ResizeObserver === "undefined") return
    const observer = new ResizeObserver(measure)
    observer.observe(el)
    return () => observer.disconnect()
  }, [])
  const shownCameras = useMemo(() => (focused ? [focused] : cameras), [focused, cameras])
  const aspect = player.aspect ?? (hasPillars ? ASPECT_HW4 : ASPECT_HW3)
  const fit = fitCameraGrid(stage, shownCameras.length, aspect, GAP)

  // Fullscreen
  const rootRef = useRef<HTMLDivElement>(null)
  const [fullscreen, setFullscreen] = useState(false)
  const toggleFullscreen = useCallback(() => {
    if (document.fullscreenElement) void document.exitFullscreen()
    else void rootRef.current?.requestFullscreen?.()
  }, [])
  useEffect(() => {
    const onChange = () => setFullscreen(!!document.fullscreenElement)
    document.addEventListener("fullscreenchange", onChange)
    return () => document.removeEventListener("fullscreenchange", onChange)
  }, [])

  // Keyboard shortcuts
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      // Typing in a field, or choosing from a list, is not a shortcut.
      const tag = (e.target as { tagName?: string } | null)?.tagName
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return
      switch (e.key) {
        case " ":
          // Space on a focused button presses that button, as usual.
          if (tag === "BUTTON") return
          e.preventDefault()
          togglePlay()
          break
        case "ArrowLeft":
          e.preventDefault()
          skip(e.shiftKey ? -15 : -5)
          break
        case "ArrowRight":
          e.preventDefault()
          skip(e.shiftKey ? 15 : 5)
          break
        case "f":
          e.preventDefault()
          toggleFullscreen()
          break
        case "e":
          if (eventGlobal !== null) { e.preventDefault(); jumpToEvent() }
          break
        case "Escape":
          if (focused) { e.preventDefault(); setFocused(null) }
          break
      }
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [togglePlay, skip, toggleFullscreen, focused, setFocused, eventGlobal, jumpToEvent])

  const setAllCameras = player.setAllCameras
  const chooseCameras = useCallback((all: boolean) => {
    try { localStorage.setItem(CAMERAS_KEY, all ? "all" : "front") } catch { /* not remembered */ }
    setAllCameras(all)
  }, [setAllCameras])
  // The button's choice is remembered; the "connection is slow" notice's is not.
  const controls = { ...player, setAllCameras: chooseCameras, togglePlay }

  return (
    <div ref={rootRef} className={cn("flex h-full min-h-0 flex-col gap-1.5", fullscreen && "bg-slate-950 p-2")}>
      {/* A size container, so the telemetry card can scale to the space. */}
      <div ref={stageRef} className="@container relative min-h-0 flex-1 overflow-hidden rounded-lg">
        <CameraGrid
          player={player}
          clipSets={clipSets}
          cameras={shownCameras}
          fit={fit}
          gap={GAP}
          triggeredCamera={triggeredCamera}
        />
        <TelemetryCards
          clock={clock}
          telemetry={telemetry}
          held={held}
          frameAtTime={frameAtTime}
          metric={metric}
          layoutKey={`${focused ?? "grid"}-${fit.cols}x${fit.rows}-${fit.width}x${fit.height}`}
          stamp={currentSet?.timestamp ?? ""}
          playing={playing}
          parkedClip={clip.path.includes("/SentryClips/")}
        />
        {panel}
      </div>

      {/* Above the controls rather than over the cameras, where it would
          cover the dashboard. */}
      <ViewerNotice
        archiving={archiving}
        reduced={player.reduced}
        onPlayAll={() => player.setAllCameras(true)}
        onDismiss={onDismissArchiving}
        className="shrink-0"
      />

      <ControlBar
        player={controls}
        stamp={currentSet?.timestamp ?? ""}
        stamps={stamps}
        cameraCount={cameras.length}
        eventGlobal={eventGlobal}
        eventTitle={eventTitle}
        onJumpToEvent={jumpToEvent}
        onDownload={onDownload}
        fullscreen={fullscreen}
        onFullscreen={toggleFullscreen}
      />
    </div>
  )
}

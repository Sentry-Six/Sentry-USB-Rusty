import { useEffect, useRef, useState } from "react"
import {
  BoltIcon,
  DownloadIcon,
  FullscreenExitIcon,
  FullscreenIcon,
  PauseIcon,
  PlayArrowIcon,
  ScheduleIcon,
  SkipNextIcon,
  SkipPreviousIcon,
  VideocamIcon,
} from "@/components/icons"
import type { ClipPlayer } from "@/hooks/useClipPlayer"
import { clipMomentIso, clipMomentMs, formatClipClock, formatClipDay, formatElapsed } from "@/lib/clipTime"
import { useClockValue, type PlaybackClock } from "@/lib/playbackClock"
import { createSeekQueue } from "@/lib/seekQueue"
import { cn } from "@/lib/utils"

const SPEEDS = [0.5, 1, 1.5, 2, 4]
// A day of Recent clips is hundreds of minutes; that many ticks is just noise.
const MAX_SEGMENT_TICKS = 40

// While the timeline is dragged, how often to check whether the cameras have
// caught up with the last place they were sent.
const SCRUB_CHECK_MS = 50
// A load that has not finished by now no longer holds up the next one.
const SCRUB_PATIENCE_MS = 1500

const iconButton = "rounded-lg p-1.5 text-slate-400 transition-colors hover:bg-white/10 hover:text-slate-100"

// Elapsed and total time. Only this redraws as the seconds pass. While the
// timeline is dragged it shows the place under the pointer instead.
function Elapsed({ clock, before, total, scrub }: { clock: PlaybackClock; before: number; total: number; scrub: number | null }) {
  const seconds = useClockValue(clock, Math.floor)
  return (
    <span className="shrink-0 whitespace-nowrap text-xs tabular-nums text-slate-400">
      {formatElapsed(scrub ?? before + seconds)} / {formatElapsed(total)}
    </span>
  )
}

// Date and time of the moment on screen, by the car's clock: the current
// minute's own file name plus the time into it.
interface ClipClockProps {
  clock: PlaybackClock
  stamp: string
  /** While the timeline is dragged: the minute under the pointer, and how far into it. */
  scrub: { stamp: string; offset: number } | null
  className?: string
}

function ClipClock({ clock, stamp, scrub, className }: ClipClockProps) {
  const second = useClockValue(clock, (t) => Math.floor(t + 0.001))
  const moment = scrub ? clipMomentMs(scrub.stamp, Math.floor(scrub.offset + 0.001)) : clipMomentMs(stamp, second)
  if (moment === null) return null
  return (
    <time
      dateTime={clipMomentIso(moment)}
      title="When this moment was recorded, by the car's clock"
      className={cn("flex items-center gap-1.5 whitespace-nowrap text-xs", className)}
    >
      <ScheduleIcon className="h-3.5 w-3.5 shrink-0 text-slate-500" aria-hidden="true" />
      <span className="sr-only">Clip time: </span>
      <span className="text-slate-400">{formatClipDay(moment)}</span>
      <span className="font-medium tabular-nums text-slate-100">{formatClipClock(moment)}</span>
    </time>
  )
}

interface SeekBarProps {
  clock: PlaybackClock
  before: number
  total: number
  durations: number[]
  eventGlobal: number | null
  eventTitle: string
  onSeek: (seconds: number) => void
  /** True while the cameras are still getting to where they were last sent. */
  isBusy: () => boolean
  /** The place under the pointer while the timeline is dragged, else null. */
  scrub: number | null
  onScrub: (seconds: number | null) => void
  className?: string
}

// The timeline. Works with mouse, pen and touch; the arrow keys skip.
//
// The thumb follows the pointer at once, but the cameras are only sent to
// one place at a time (see seekQueue).
function SeekBar({ clock, before, total, durations, eventGlobal, eventTitle, onSeek, isBusy, scrub, onScrub, className }: SeekBarProps) {
  const quarter = useClockValue(clock, (t) => Math.round(t * 4))
  const dragging = useRef(false)
  const [queue] = useState(() => createSeekQueue(SCRUB_PATIENCE_MS))
  const timer = useRef<ReturnType<typeof setInterval> | null>(null)
  const position = Math.min(scrub ?? before + quarter / 4, total)
  const percent = total > 0 ? (position / total) * 100 : 0

  const stop = () => {
    if (timer.current !== null) clearInterval(timer.current)
    timer.current = null
  }
  useEffect(() => stop, [])

  // Send the cameras to the wanted place, if they are free to go.
  const pump = (force = false) => {
    queue.pump(performance.now(), isBusy(), onSeek, force)
    if (!dragging.current && queue.idle) {
      stop()
      onScrub(null)
    }
  }

  const want = (e: React.PointerEvent<HTMLDivElement>) => {
    const box = e.currentTarget.getBoundingClientRect()
    if (box.width <= 0 || total <= 0) return
    const target = Math.max(0, Math.min(1, (e.clientX - box.left) / box.width)) * total
    queue.want(target)
    onScrub(target)
  }

  const ticks: number[] = []
  if (durations.length > 1 && durations.length <= MAX_SEGMENT_TICKS && total > 0) {
    let sum = 0
    for (let i = 0; i < durations.length - 1; i++) {
      sum += durations[i]
      ticks.push((sum / total) * 100)
    }
  }

  return (
    <div
      role="slider"
      tabIndex={0}
      aria-label="Position in the clip"
      aria-valuemin={0}
      aria-valuemax={Math.round(total)}
      aria-valuenow={Math.round(position)}
      aria-valuetext={`${formatElapsed(position)} of ${formatElapsed(total)}`}
      className={cn("group flex h-6 cursor-pointer touch-none items-center outline-none", className)}
      onPointerDown={(e) => {
        if (e.pointerType === "mouse" && e.button !== 0) return
        dragging.current = true
        try { e.currentTarget.setPointerCapture(e.pointerId) } catch { /* not captured */ }
        want(e)
        // A click goes straight there; a drag carries on from it.
        pump(true)
        if (timer.current === null) timer.current = setInterval(pump, SCRUB_CHECK_MS)
      }}
      onPointerMove={(e) => { if (dragging.current) want(e) }}
      onPointerUp={() => { dragging.current = false; pump() }}
      onPointerCancel={() => { dragging.current = false; pump() }}
    >
      <div className="relative h-1 w-full rounded-full bg-white/15 transition-[height] group-hover:h-1.5 group-focus-visible:ring-2 group-focus-visible:ring-blue-400">
        <div className="h-full rounded-full bg-blue-500" style={{ width: `${percent}%` }} />
        {ticks.map((tick, i) => (
          <div key={i} className="absolute top-0 h-full w-px bg-black/50" style={{ left: `${tick}%` }} />
        ))}
        {/* The moment the event happened */}
        {eventGlobal !== null && total > 0 && (
          <div
            className="absolute -top-1 h-[calc(100%+8px)] w-0.5 -translate-x-1/2 rounded-full bg-amber-400"
            style={{ left: `${Math.min(100, (eventGlobal / total) * 100)}%` }}
            title={eventTitle}
          />
        )}
        <div
          className="absolute top-1/2 h-3 w-3 -translate-x-1/2 -translate-y-1/2 rounded-full bg-white shadow"
          style={{ left: `${percent}%` }}
        />
      </div>
    </div>
  )
}

interface ControlBarProps {
  player: ClipPlayer
  /** Start time of the minute on screen, from its file name. */
  stamp: string
  /** Start time of every minute of the clip, in order. */
  stamps: string[]
  /** How many cameras this clip has. */
  cameraCount: number
  eventGlobal: number | null
  eventTitle: string
  onJumpToEvent: () => void
  onDownload?: () => void
  fullscreen: boolean
  onFullscreen: () => void
}

// One slim row of controls, like Sentry Studio's. On a narrow player the
// timeline and the clip time take a line each above the buttons.
export default function ControlBar({
  player, stamp, stamps, cameraCount, eventGlobal, eventTitle, onJumpToEvent, onDownload, fullscreen, onFullscreen,
}: ControlBarProps) {
  const { clock, before, total, durations, playing, speed } = player
  const allCameras = player.active.size >= cameraCount
  // The place under the pointer while the timeline is dragged.
  const [scrub, setScrub] = useState<number | null>(null)
  const scrubPlace = scrub === null ? null : player.locate(scrub)
  const scrubMoment = scrubPlace && stamps[scrubPlace.index] ? { stamp: stamps[scrubPlace.index], offset: scrubPlace.offset } : null
  const seekBar = (className: string) => (
    <SeekBar
      clock={clock}
      before={before}
      total={total}
      durations={durations}
      eventGlobal={eventGlobal}
      eventTitle={eventTitle}
      onSeek={player.seekToGlobal}
      isBusy={player.busy}
      scrub={scrub}
      onScrub={setScrub}
      className={className}
    />
  )

  return (
    <div className="glass-card @container shrink-0 px-2 py-1">
      {seekBar("@3xl:hidden")}
      <ClipClock clock={clock} stamp={stamp} scrub={scrubMoment} className="mb-1 @3xl:hidden" />

      <div className="flex items-center gap-1 @sm:gap-1.5">
        <button
          onClick={() => player.skip(-5)}
          className={iconButton}
          title="Back 5 seconds (←, or Shift+← for 15)"
          aria-label="Back 5 seconds"
        >
          <SkipPreviousIcon className="h-4 w-4" />
        </button>
        <button
          onClick={player.togglePlay}
          className="flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-blue-500/20 text-blue-400 transition-colors hover:bg-blue-500/30"
          title="Play or pause (Space)"
          aria-label={playing ? "Pause" : "Play"}
        >
          {playing ? <PauseIcon className="h-4 w-4" /> : <PlayArrowIcon className="h-4 w-4 translate-x-px" />}
        </button>
        <button
          onClick={() => player.skip(5)}
          className={iconButton}
          title="Forward 5 seconds (→, or Shift+→ for 15)"
          aria-label="Forward 5 seconds"
        >
          <SkipNextIcon className="h-4 w-4" />
        </button>

        <Elapsed clock={clock} before={before} total={total} scrub={scrub} />
        <ClipClock clock={clock} stamp={stamp} scrub={scrubMoment} className="ml-1 hidden @3xl:flex" />

        {seekBar("mx-2 hidden min-w-0 flex-1 @3xl:flex")}
        <div className="flex-1 @3xl:hidden" />

        {eventGlobal !== null && (
          <button
            onClick={onJumpToEvent}
            className="flex items-center gap-1 rounded-lg px-1.5 py-1 text-[11px] font-medium text-amber-400 transition-colors hover:bg-amber-500/15"
            title={`Jump to the event (E)${eventTitle ? ` — ${eventTitle}` : ""}`}
            aria-label="Jump to the event"
          >
            <BoltIcon className="h-4 w-4" aria-hidden="true" />
            <span className="hidden @xl:inline">Event</span>
          </button>
        )}

        <select
          value={speed}
          onChange={(e) => player.setSpeed(Number(e.target.value))}
          className="rounded-lg border border-white/10 bg-transparent py-1 pl-1.5 pr-1 text-[11px] font-medium text-slate-300 outline-none hover:bg-white/10 focus-visible:ring-2 focus-visible:ring-blue-400"
          title="Playback speed"
          aria-label="Playback speed"
        >
          {SPEEDS.map((s) => <option key={s} value={s} className="bg-slate-900">{s}x</option>)}
        </select>

        {cameraCount > 1 && (
          <button
            onClick={() => player.setAllCameras(!allCameras)}
            aria-pressed={allCameras}
            // Left out on the narrowest phones, where tapping a camera does the same job.
            className={cn(iconButton, "hidden items-center gap-1 text-[11px] font-medium tabular-nums @sm:flex", allCameras && "text-blue-400")}
            title={allCameras ? "Playing every camera. Play only the front camera" : "Play every camera"}
            aria-label={allCameras ? "Play only the front camera" : "Play every camera"}
          >
            <VideocamIcon className="h-4 w-4" aria-hidden="true" />
            {player.active.size}
          </button>
        )}

        {onDownload && (
          <button onClick={onDownload} className={iconButton} title="Download this clip's folder" aria-label="Download this clip's folder">
            <DownloadIcon className="h-4 w-4" />
          </button>
        )}

        <button onClick={onFullscreen} className={iconButton} title="Fullscreen (F)" aria-label={fullscreen ? "Leave fullscreen" : "Fullscreen"}>
          {fullscreen ? <FullscreenExitIcon className="h-4 w-4" /> : <FullscreenIcon className="h-4 w-4" />}
        </button>
      </div>
    </div>
  )
}

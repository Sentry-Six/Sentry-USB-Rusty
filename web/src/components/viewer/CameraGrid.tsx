import { memo, useEffect, useRef } from "react"
import { PlayArrowIcon, ProgressActivityIcon, VideocamIcon } from "@/components/icons"
import type { ClipPlayer, ClipSet, Slot, VideoEvent } from "@/hooks/useClipPlayer"
import type { GridFit } from "@/lib/cameraGrid"
import { cn } from "@/lib/utils"
import { CAMERA_LABELS } from "./clipEvents"

const EVENTS: [keyof React.VideoHTMLAttributes<HTMLVideoElement>, VideoEvent][] = [
  ["onLoadStart", "loadstart"],
  ["onLoadedMetadata", "loadedmetadata"],
  ["onCanPlay", "canplay"],
  ["onPlaying", "playing"],
  ["onWaiting", "waiting"],
  ["onSeeked", "seeked"],
  ["onEnded", "ended"],
  ["onError", "error"],
]

// Each tile has a canvas behind its videos holding the last picture that was
// on screen. A video shows nothing while a new minute loads, so without it
// every jump to another minute would flash the tile black.
const heldPicture = (el: HTMLVideoElement) => el.parentElement?.querySelector("canvas") ?? null

function holdPicture(el: HTMLVideoElement) {
  const canvas = heldPicture(el)
  if (!canvas || el.readyState < 2 || !el.videoWidth || !el.videoHeight || !el.clientWidth) return
  // No larger than it is shown: the full frame would only be scaled down again.
  const scale = Math.min(1, (el.clientWidth * (window.devicePixelRatio || 1)) / el.videoWidth)
  canvas.width = Math.max(1, Math.round(el.videoWidth * scale))
  canvas.height = Math.max(1, Math.round(el.videoHeight * scale))
  try { canvas.getContext("2d")?.drawImage(el, 0, 0, canvas.width, canvas.height) } catch { /* nothing held */ }
}

function dropPicture(el: HTMLVideoElement) {
  const canvas = heldPicture(el)
  // Resizing a canvas clears it.
  if (canvas) canvas.width = canvas.height = 1
}

interface CameraVideoProps {
  cam: string
  slot: Slot
  src: string | undefined
  shown: boolean
  register: ClipPlayer["register"]
  onEvent: ClipPlayer["onVideoEvent"]
}

// One of a camera's two video buffers. It is created once and reused for
// every minute of the clip; only its `src` changes.
const CameraVideo = memo(function CameraVideo({ cam, slot, src, shown, register, onEvent }: CameraVideoProps) {
  const ref = useRef<HTMLVideoElement>(null)
  const wasShown = useRef(shown)

  useEffect(() => {
    register(cam, slot, ref.current)
    return () => register(cam, slot, null)
  }, [cam, slot, register])

  // The file is attached here rather than as a prop, so that letting go of
  // it is always paired with attaching it: a buffer with nothing to show,
  // or a player that has gone away, stops its download instead of leaving
  // the browser fetching a minute nobody is watching.
  useEffect(() => {
    const el = ref.current
    if (!el || !src) return
    el.src = src
    return () => {
      // The picture on screen stays up, on the tile's canvas, until the
      // next file has one of its own.
      if (wasShown.current) holdPicture(el)
      el.removeAttribute("src")
      el.load?.()
    }
  }, [src])
  // After the clean-up above, which needs to know what was showing before.
  useEffect(() => { wasShown.current = shown })

  const handlers = Object.fromEntries(
    EVENTS.map(([prop, type]) => [prop, (e: React.SyntheticEvent<HTMLVideoElement>) => {
      // A file that will not play must not leave an older picture standing in for it.
      if (type === "error" && shown) dropPicture(e.currentTarget)
      onEvent(cam, slot, type, e.currentTarget)
    }]),
  )

  return (
    <video
      ref={ref}
      muted
      playsInline
      preload="auto"
      aria-hidden={!shown}
      className={cn("absolute inset-0 h-full w-full object-contain", !shown && "pointer-events-none opacity-0")}
      {...handlers}
    />
  )
})

interface CameraGridProps {
  player: ClipPlayer
  clipSets: ClipSet[]
  /** Cameras on screen: the whole grid, or the one enlarged camera. */
  cameras: string[]
  fit: GridFit
  gap: number
  /** The camera that triggered the event, if any. */
  triggeredCamera: string | null
}

export default function CameraGrid({ player, clipSets, cameras, fit, gap, triggeredCamera }: CameraGridProps) {
  const { segment, slots, front, active, focused, waiting } = player
  const measured = fit.width > 0

  return (
    <div
      className={cn("grid", measured ? "absolute" : "h-full w-full")}
      style={{
        gridTemplateColumns: `repeat(${fit.cols}, minmax(0, 1fr))`,
        gridTemplateRows: `repeat(${fit.rows}, minmax(0, 1fr))`,
        gap,
        ...(measured ? { width: fit.width, height: fit.height, left: "50%", top: "50%", marginLeft: -fit.width / 2, marginTop: -fit.height / 2 } : {}),
      }}
    >
      {cameras.map((cam) => {
        const hasFile = !!clipSets[segment]?.cameras[cam]
        const isActive = active.has(cam)
        const isTriggered = triggeredCamera === cam
        const isFocused = focused === cam
        const label = CAMERA_LABELS[cam] ?? cam
        return (
          <div
            key={cam}
            data-camera={cam}
            role="button"
            tabIndex={0}
            aria-label={!isActive && hasFile ? `Play the ${label} camera` : isFocused ? `${label} camera, enlarged. Show all cameras` : `Enlarge the ${label} camera`}
            className={cn(
              "relative cursor-pointer overflow-hidden rounded-md bg-black outline-none focus-visible:ring-2 focus-visible:ring-blue-400",
              isTriggered && !isFocused && "ring-1 ring-amber-500/70",
            )}
            onClick={() => {
              if (!isActive && hasFile) player.activate(cam)
              else player.setFocused(isFocused ? null : cam)
            }}
            onKeyDown={(e) => {
              if (e.key !== "Enter") return
              e.preventDefault()
              if (!isActive && hasFile) player.activate(cam)
              else player.setFocused(isFocused ? null : cam)
            }}
          >
            {isActive && <canvas aria-hidden="true" className="absolute inset-0 h-full w-full object-contain" />}
            {isActive && ([0, 1] as const).map((slot) => {
              const index = slots[slot]
              return (
                <CameraVideo
                  key={slot}
                  cam={cam}
                  slot={slot}
                  src={index === null ? undefined : clipSets[index]?.cameras[cam]}
                  shown={slot === front}
                  register={player.register}
                  onEvent={player.onVideoEvent}
                />
              )
            })}
            {!isActive && hasFile && (
              <div className="flex h-full flex-col items-center justify-center gap-1.5 bg-slate-900/80">
                <PlayArrowIcon className="h-5 w-5 text-slate-500" />
                <span className="text-[10px] text-slate-500">Tap to play</span>
              </div>
            )}
            {!hasFile && (
              <div className="absolute inset-0 flex items-center justify-center bg-black">
                <VideocamIcon className="h-6 w-6 text-slate-600" />
              </div>
            )}
            {/* Waiting for data: shown only after a moment, so quick seeks do not flash it. */}
            {isActive && hasFile && waiting.has(cam) && (
              <div className="viewer-buffering pointer-events-none absolute inset-0 flex items-center justify-center bg-black/30" role="status" aria-label={`${label} camera is loading`}>
                <ProgressActivityIcon className="h-6 w-6 animate-spin text-white/80" />
              </div>
            )}
            <span
              className={cn(
                "pointer-events-none absolute left-1.5 top-1.5 rounded px-1.5 py-0.5 text-[10px] font-medium",
                isTriggered ? "bg-amber-500/85 text-black" : "bg-black/55 text-slate-300",
              )}
            >
              {label}
              {isTriggered && " ⚡"}
            </span>
            {isFocused && (
              <span className="pointer-events-none absolute right-1.5 top-1.5 rounded bg-black/55 px-1.5 py-0.5 text-[10px] text-slate-400">
                Click to show all &middot; Esc
              </span>
            )}
          </div>
        )
      })}
    </div>
  )
}

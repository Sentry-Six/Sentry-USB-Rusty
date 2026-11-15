import { memo, useId } from "react"
import type { TelemetryFrame } from "@/lib/api"
import { useFloatingCard } from "@/hooks/useFloatingCard"
import type { Point, Size } from "@/lib/floatingCard"
import { accelPercent } from "@/lib/telemetry"
import { cn } from "@/lib/utils"

// The compact dashboard from Sentry Studio, drawn the same way: the same
// order, wording, colours and artwork (the pedals and steering wheel are
// Studio's own images, shipped in /public), so the two products read alike.

const PLACE_KEY = "viewer-telemetry-card"

// Where the card sits until it is moved. If the cameras leave a free band
// underneath them, it goes there, off the picture. Otherwise it sits at the
// bottom centre of the front camera (as Studio's does); with another camera
// enlarged there is no front tile, so it uses the bottom centre of the
// whole picture.
function defaultPoint(area: HTMLElement, card: Size): Point {
  const areaBox = area.getBoundingClientRect()
  const front = area.querySelector('[data-camera="front"]')
  const grid = (front ?? area.querySelector("[data-camera]"))?.parentElement?.getBoundingClientRect()
  if (grid) {
    const below = areaBox.bottom - grid.bottom
    if (below >= card.height + 12) {
      return { x: (areaBox.width - card.width) / 2, y: grid.bottom - areaBox.top + (below - card.height) / 2 }
    }
  }
  const tile = front?.getBoundingClientRect() ?? grid ?? areaBox
  return {
    x: tile.left - areaBox.left + (tile.width - card.width) / 2,
    y: tile.bottom - areaBox.top - card.height - 8,
  }
}

// Studio's colours.
const BLUE = "#0048ff"
const GREEN = "#34d399"
const BRAKE_RED = "rgb(235, 25, 25)"
const PEDAL_LIT = "rgb(217, 217, 217)"
// An icon that is off: grey at 60% brightness and half opacity.
const ICON_OFF = "rgba(153, 153, 153, 0.5)"
const TEXT_SECONDARY = "#b8bfcf"
const TEXT_MUTED = "#8a93a3"

// Studio's words for the gear and the driving state.
const GEARS: Record<number, string> = { 0: "PARK", 1: "DRIVE", 2: "REVERSE", 3: "NEUTRAL" }
const DRIVING_STATE: Record<number, string> = { 0: "Manual", 1: "Self Driving", 2: "Autosteer", 3: "TACC" }

// Bits of a frame's `flags`.
const LEFT_BLINKER = 1
const RIGHT_BLINKER = 2
const BRAKE = 4

// Sizes are in em, so the whole card scales with its font size: Studio's
// floating dashboard is 480 x 56 at a 0.74 scale, which is 11.84 px here.
const ICON = { width: "1.75em", height: "1.75em" } as const
// On a narrow player the card shrinks to 96% of the camera area's width
// (2.37cqw x 40.54em), down to a size that still reads on a phone.
const FONT_SIZE = "clamp(7.6px, 2.37cqw, 11.84px)"

// Blinkers flash on a 0.7 s cycle, lit for the first 57% of it, as in Studio.
const BLINK_CSS = "@keyframes telemetry-blink{0%,57%{opacity:1}57.01%,100%{opacity:.15}}"

const mask = (src: string, color: string): React.CSSProperties => ({
  backgroundColor: color,
  maskImage: `url(${src})`,
  WebkitMaskImage: `url(${src})`,
  maskSize: "contain",
  WebkitMaskSize: "contain",
  maskRepeat: "no-repeat",
  WebkitMaskRepeat: "no-repeat",
  maskPosition: "center",
  WebkitMaskPosition: "center",
})

function Blinker({ side, on, flashing }: { side: "Left" | "Right"; on: boolean; flashing: boolean }) {
  return (
    <svg
      viewBox="-100 -90 200 175"
      className={cn("shrink-0", side === "Right" && "-scale-x-100")}
      style={{
        ...ICON,
        color: on ? GREEN : "#fff",
        opacity: on ? 1 : 0.15,
        animation: on && flashing ? "telemetry-blink 0.7s linear infinite" : undefined,
      }}
      fill="currentColor"
      role="img"
      aria-label={`${side} blinker ${on ? "on" : "off"}`}
    >
      <path d="M-13.11,-33.71 C-15.61,-33.7 -16.06,-36.09 -15.15,-38.05 C-9.81,-49.52 -5.3,-59.32 -1.63,-67.45 C1.42,-74.21 -0.76,-78.22 -5.55,-83.46 C-5.8,-83.72 -6.07,-83.94 -6.38,-84.12 C-14.17,-88.49 -20.03,-84.98 -25.77,-79.01 C-36.67,-67.69 -45.88,-58.89 -57.03,-47.37 C-62.85,-41.36 -73.29,-30.89 -88.34,-15.96 C-91.84,-12.49 -95.74,-8.94 -97.13,-4.62 C-99.07,1.36 -96.56,6.15 -92.31,10.41 C-59.34,43.5 -38.6,64.39 -30.11,73.08 C-27.75,75.49 -26.47,76.81 -26.28,77.03 C-23.64,80 -20.08,83.56 -16.05,83.93 C-13.88,84.12 -11.77,84.13 -9.72,83.94 C-9.38,83.91 -9.05,83.83 -8.73,83.7 C-1.64,80.77 1.19,75.32 -0.25,67.36 C-0.32,66.99 -0.43,66.63 -0.58,66.29 C-5.37,55.61 -9.91,45.68 -14.2,36.51 C-15.9,32.89 -13.31,31.3 -10.28,31.29 C5.65,31.24 34.48,31.23 76.21,31.24 C83.28,31.24 87.73,30.78 89.56,29.85 C93.33,27.94 96.02,24.45 97.65,19.38 C97.74,19.08 97.79,18.76 97.79,18.44 L97.73,-20.92 C97.73,-21.05 97.71,-21.18 97.68,-21.31 C96.46,-26.68 93.59,-30.44 89.07,-32.6 C87.14,-33.53 83.84,-33.99 79.19,-33.98 C43.76,-33.92 13,-33.83 -13.11,-33.71Z" />
    </svg>
  )
}

interface TelemetryOverlayProps {
  frame: TelemetryFrame | null
  metric?: boolean
  /** Recording date and time of the moment on screen, shown under the driving state. */
  time?: string | null
  /** Blinkers flash while the clip plays and hold steady while it is paused. */
  playing?: boolean
  /** The car is parked and sends no telemetry: show PARKED and the time, as Studio does. */
  parked?: boolean
  /** True while the values are held over from the minute before and the next minute's are loading. */
  stale?: boolean
  /** Changes when the cameras are rearranged, so an unmoved card follows the front camera. */
  layoutKey?: string
}

export default memo(function TelemetryOverlay({
  frame, metric = false, time = null, playing = false, parked = false, stale = false, layoutKey,
}: TelemetryOverlayProps) {
  const { ref, cardProps, dragging } = useFloatingCard({ storageKey: PLACE_KEY, defaultPoint, layoutKey })
  const hintId = useId()

  if (!frame && !parked) return null

  const shell = (children: React.ReactNode) => (
    <div
      ref={ref}
      {...cardProps}
      role="group"
      aria-label="Telemetry"
      aria-describedby={hintId}
      aria-busy={stale}
      title="Drag to move"
      className={cn(
        "z-10 flex h-[4.73em] w-[40.54em] max-w-full select-none items-center justify-evenly rounded-full border border-white/[0.08] px-[0.5em] leading-none text-white",
        "outline-none backdrop-blur-xl transition-opacity focus-visible:ring-2 focus-visible:ring-blue-400",
        dragging && "border-white/25",
        stale && "opacity-60",
      )}
      style={{
        ...cardProps.style,
        fontSize: FONT_SIZE,
        background: "linear-gradient(150deg, rgba(255,255,255,0.05), rgba(255,255,255,0.02) 55%, rgba(255,255,255,0.03)), rgba(8,12,18,0.6)",
        boxShadow: "0 12px 34px rgba(0,0,0,0.4)",
      }}
    >
      <span id={hintId} className="sr-only">
        Drag to move. With the keyboard, the arrow keys move this card, Shift moves it further, and Home puts it back.
      </span>
      {children}
    </div>
  )

  if (!frame) {
    return shell(
      <div className="flex items-center gap-[1em]">
        <span className="text-[1.25em] font-bold">PARKED</span>
        {time && <span className="text-[max(0.94em,9px)] font-medium tabular-nums" style={{ color: TEXT_SECONDARY }}>{time}</span>}
      </div>,
    )
  }

  const speed = Math.round(Math.abs(frame.speed_mps) * (metric ? 3.6 : 2.236936292054402))
  // Studio lights the gear and wheel blue for Self Driving and Autosteer.
  const autopilotOn = frame.autopilot === 1 || frame.autopilot === 2
  const drivingState = DRIVING_STATE[frame.autopilot] ?? DRIVING_STATE[0]
  const accelPct = Math.round(accelPercent(frame.accel_pos))
  // Older Sentry USB versions send speed, gear and pedal only.
  const flags = frame.flags
  const steering = frame.steering_angle_deg
  const braking = flags !== undefined && (flags & BRAKE) !== 0

  return shell(
    <>
      <style href="telemetry-blink" precedence="default">{BLINK_CSS}</style>

      {flags !== undefined && (
        <span
          className="shrink-0"
          style={{ ...ICON, ...mask("/pedal_brake.png", braking ? BRAKE_RED : ICON_OFF) }}
          role="img"
          aria-label={`Brake ${braking ? "on" : "off"}`}
        />
      )}

      <span className="text-[1.375em] font-semibold" style={{ color: autopilotOn ? BLUE : "#fff" }}>
        {GEARS[frame.gear] ?? "--"}
      </span>

      {flags !== undefined && <Blinker side="Left" on={(flags & LEFT_BLINKER) !== 0} flashing={playing} />}

      <div className="flex w-[4.06em] flex-col items-center gap-[0.125em]">
        <span className="text-[1.3125em] font-bold tabular-nums">{speed}</span>
        <span className="text-[max(0.6875em,7px)] font-medium" style={{ color: TEXT_MUTED }}>{metric ? "KM/H" : "MPH"}</span>
      </div>

      <div className="flex flex-col items-center gap-[0.125em] whitespace-nowrap">
        <span className="text-[1.25em]" style={{ color: autopilotOn ? BLUE : TEXT_MUTED }}>{drivingState}</span>
        {time && <span className="text-[max(0.75em,7px)] font-medium tabular-nums" style={{ color: TEXT_SECONDARY }}>{time}</span>}
      </div>

      {flags !== undefined && <Blinker side="Right" on={(flags & RIGHT_BLINKER) !== 0} flashing={playing} />}

      {steering !== undefined && (
        // Under Self Driving or Autosteer the wheel is lit and glows blue.
        <span
          className="shrink-0 rounded-full"
          style={{ ...ICON, boxShadow: autopilotOn ? "0 0 0.85em rgba(0, 72, 255, 0.5)" : undefined }}
          role="img"
          aria-label={`Steering ${Math.round(Math.abs(steering))} degrees ${steering < 0 ? "left" : "right"}`}
        >
          <img
            src="/autosteer-icon.png"
            alt=""
            draggable={false}
            className="h-full w-full"
            style={{
              transform: `rotate(${steering}deg)`,
              transition: "transform 120ms linear",
              filter: autopilotOn ? undefined : "grayscale(1) brightness(0.6)",
              opacity: autopilotOn ? 1 : 0.5,
            }}
          />
        </span>
      )}

      {/* The accelerator pedal fills from the bottom with how far it is pressed. */}
      <span className="relative shrink-0" style={ICON} role="img" aria-label={`Accelerator ${accelPct}%`} title={`Accelerator ${accelPct}%`}>
        <span className="absolute inset-0" style={mask("/pedal_acc.png", ICON_OFF)} />
        <span
          className="absolute inset-0"
          style={{ ...mask("/pedal_acc.png", PEDAL_LIT), clipPath: `inset(${100 - accelPct}% 0 0 0)` }}
        />
      </span>
    </>,
  )
})

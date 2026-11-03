import { memo, useMemo } from "react"
import { AlbumIcon } from "@/components/icons"
import {
  CartesianGrid,
  Legend,
  Line,
  LineChart,
  ReferenceArea,
  ReferenceLine,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts"
import { useUnits } from "@/lib/units"
import { tireChartDomainPsi } from "@/lib/tireBands"

// Telemetry is stored in PSI and converted only for display.
const PSI_TO_BAR = 0.0689476

// Zone colors from the unsafe-high band down to unsafe-low. The thresholds
// come from the saved TIRE_PSI_BANDS (see lib/tireBands), in PSI.
const ZONE_STYLES = {
  unsafeHigh: { fill: "rgba(127, 29, 29, 0.55)", labelColor: "#fca5a5", line: "rgba(252, 165, 165, 0.7)" },
  harsh: { fill: "rgba(63, 98, 18, 0.55)", labelColor: "#bef264", line: "rgba(190, 242, 100, 0.7)" },
  optimal: { fill: "rgba(22, 78, 51, 0.55)", labelColor: "rgba(167, 243, 208, 0.85)" },
  reduced: { fill: "rgba(124, 45, 18, 0.55)", labelColor: "#fcd34d", line: "rgba(252, 211, 77, 0.7)" },
  unsafeLow: { fill: "rgba(127, 29, 29, 0.55)", labelColor: "#fca5a5", line: "rgba(252, 165, 165, 0.7)" },
} as const

// Distinct line colors remain legible against the zone bands.
const TIRE_COLORS = {
  fl: "#34d399",
  fr: "#a3e635",
  rl: "#5eead4",
  rr: "#facc15",
} as const

export interface TirePoint {
  ts: number
  fl?: number
  fr?: number
  rl?: number
  rr?: number
}

export interface TireHistoryResponse {
  points: TirePoint[]
  days: number
}

interface TirePressureCardProps {
  // The parent controls mounting so Recharts is absent without tire data.
  data: TireHistoryResponse
  days?: number
  // Chart-only mode lets CarStatusCard provide the surrounding chrome.
  chartOnly?: boolean
}

export const TirePressureCard = memo(function TirePressureCard({
  data,
  days = 30,
  chartOnly = false,
}: TirePressureCardProps) {
  const { pressureBar, tireBands: b } = useUnits()

  // Convert the full series once so axes, chips, and tooltips share a unit.
  const points = useMemo(
    () =>
      pressureBar
        ? data.points.map((p) => ({
            ts: p.ts,
            fl: p.fl !== undefined ? p.fl * PSI_TO_BAR : undefined,
            fr: p.fr !== undefined ? p.fr * PSI_TO_BAR : undefined,
            rl: p.rl !== undefined ? p.rl * PSI_TO_BAR : undefined,
            rr: p.rr !== undefined ? p.rr * PSI_TO_BAR : undefined,
          }))
        : data.points,
    [data.points, pressureBar],
  )

  // Latest reading per tire for the header strip.
  const latest = useMemo(() => {
    const out: Partial<Record<"fl" | "fr" | "rl" | "rr", number>> = {}
    for (let i = points.length - 1; i >= 0; i--) {
      const p = points[i]
      if (out.fl === undefined && p.fl !== undefined) out.fl = p.fl
      if (out.fr === undefined && p.fr !== undefined) out.fr = p.fr
      if (out.rl === undefined && p.rl !== undefined) out.rl = p.rl
      if (out.rr === undefined && p.rr !== undefined) out.rr = p.rr
      if (
        out.fl !== undefined &&
        out.fr !== undefined &&
        out.rl !== undefined &&
        out.rr !== undefined
      )
        break
    }
    return out
  }, [points])

  const conv = (psi: number) => (pressureBar ? psi * PSI_TO_BAR : psi)
  const unitUpper = pressureBar ? "BAR" : "PSI"
  const fmtThreshold = (psi: number) =>
    pressureBar ? (psi * PSI_TO_BAR).toFixed(1) : String(Math.round(psi * 10) / 10)
  const fmtTick = (n: number) => (pressureBar ? n.toFixed(1) : String(Math.round(n)))
  const fmtValue = (v: number) => (pressureBar ? `${v.toFixed(2)} bar` : `${v.toFixed(1)} psi`)

  // Cap zone bands to the visible domain: Recharts centers labels using literal
  // y1/y2 values, including portions outside the viewport.
  const [lo, hi] = tireChartDomainPsi(b)
  const zonesPsi = [
    { y1: b.unsafeHigh, y2: hi, style: ZONE_STYLES.unsafeHigh, gt: b.unsafeHigh, text: "UNSAFE" },
    { y1: b.optimalHigh, y2: b.unsafeHigh, style: ZONE_STYLES.harsh, gt: b.optimalHigh, text: "HARSHER RIDE & WEAR" },
    { y1: b.optimalLow, y2: b.optimalHigh, style: ZONE_STYLES.optimal, text: "OPTIMAL" },
    { y1: b.unsafeLow, y2: b.optimalLow, style: ZONE_STYLES.reduced, lt: b.optimalLow, text: "REDUCED HANDLING & EFFICIENCY" },
    { y1: lo, y2: b.unsafeLow, style: ZONE_STYLES.unsafeLow, lt: b.unsafeLow, text: "UNSAFE" },
  ]
  const zones = zonesPsi.map((z) => ({
    key: z.y1,
    y1: conv(z.y1),
    y2: conv(z.y2),
    fill: z.style.fill,
    labelColor: z.style.labelColor,
    label:
      z.gt !== undefined
        ? `>${fmtThreshold(z.gt)} ${unitUpper} • ${z.text}`
        : z.lt !== undefined
          ? `<${fmtThreshold(z.lt)} ${unitUpper} • ${z.text}`
          : z.text,
  }))
  const boundaries = [
    { y: b.unsafeHigh, color: ZONE_STYLES.unsafeHigh.line },
    { y: b.optimalHigh, color: ZONE_STYLES.harsh.line },
    { y: b.optimalLow, color: ZONE_STYLES.reduced.line },
    { y: b.unsafeLow, color: ZONE_STYLES.unsafeLow.line },
  ].map((x) => ({ key: x.y, y: conv(x.y), color: x.color }))
  const domain: [number, number] = [conv(lo), conv(hi)]

  const chart = (
    <div className="h-72 w-full" aria-label="Tire pressure chart">
          <ResponsiveContainer minHeight={0} minWidth={0}>
            <LineChart
              data={points}
              margin={{ top: 8, right: 20, bottom: 24, left: 0 }}
            >
              <CartesianGrid
                stroke="#1e242f"
                strokeDasharray="3 3"
                vertical={false}
              />
              {zones.map((z) => (
                <ReferenceArea
                  key={z.key}
                  y1={z.y1}
                  y2={z.y2}
                  fill={z.fill}
                  stroke="transparent"
                  label={{
                    value: z.label,
                    position: "center",
                    fill: z.labelColor,
                    fontSize: 10,
                    fontWeight: 600,
                    letterSpacing: "0.08em",
                  }}
                  ifOverflow="hidden"
                />
              ))}
              {boundaries.map((b) => (
                <ReferenceLine
                  key={b.key}
                  y={b.y}
                  stroke={b.color}
                  strokeWidth={1}
                  strokeDasharray="6 4"
                  ifOverflow="hidden"
                />
              ))}
              <XAxis
                dataKey="ts"
                type="number"
                domain={["dataMin", "dataMax"]}
                tickFormatter={formatXTick}
                stroke="#475569"
                tick={{ fill: "#64748b", fontSize: 11 }}
                tickLine={false}
                axisLine={false}
                tickMargin={10}
                minTickGap={64}
              />
              <YAxis
                domain={domain}
                stroke="#475569"
                tick={{ fill: "#64748b", fontSize: 11 }}
                tickFormatter={(n: number) => fmtTick(n)}
                tickLine={false}
                axisLine={false}
                tickMargin={4}
                width={pressureBar ? 40 : 32}
              />
              <Tooltip
                content={({ active, payload }) => {
                  if (!active || !payload || payload.length === 0) return null
                  const p = payload[0].payload as TirePoint
                  return (
                    <div className="rounded-md border border-white/10 bg-slate-900/95 px-2 py-1.5 text-xs text-slate-200 shadow-xl">
                      <div className="mb-1 text-[10px] text-slate-500 tabular-nums">
                        {formatTooltipTime(p.ts)}
                      </div>
                      <TooltipRow label="FL" value={p.fl} color={TIRE_COLORS.fl} format={fmtValue} />
                      <TooltipRow label="FR" value={p.fr} color={TIRE_COLORS.fr} format={fmtValue} />
                      <TooltipRow label="RL" value={p.rl} color={TIRE_COLORS.rl} format={fmtValue} />
                      <TooltipRow label="RR" value={p.rr} color={TIRE_COLORS.rr} format={fmtValue} />
                    </div>
                  )
                }}
              />
              <Legend
                verticalAlign="bottom"
                height={20}
                iconType="line"
                wrapperStyle={{ fontSize: 11, color: "#94a3b8" }}
              />
              <Line
                type="monotone"
                name="Front L"
                dataKey="fl"
                stroke={TIRE_COLORS.fl}
                strokeWidth={1.75}
                dot={false}
                isAnimationActive={false}
                connectNulls
              />
              <Line
                type="monotone"
                name="Front R"
                dataKey="fr"
                stroke={TIRE_COLORS.fr}
                strokeWidth={1.75}
                dot={false}
                isAnimationActive={false}
                connectNulls
              />
              <Line
                type="monotone"
                name="Rear L"
                dataKey="rl"
                stroke={TIRE_COLORS.rl}
                strokeWidth={1.75}
                dot={false}
                isAnimationActive={false}
                connectNulls
              />
              <Line
                type="monotone"
                name="Rear R"
                dataKey="rr"
                stroke={TIRE_COLORS.rr}
                strokeWidth={1.75}
                dot={false}
                isAnimationActive={false}
                connectNulls
              />
            </LineChart>
          </ResponsiveContainer>
      </div>
  )

  if (chartOnly) {
    return chart
  }

  return (
    <div className="glass-card p-4">
      <div className="mb-3 flex flex-wrap items-center gap-3">
        <span className="tile-icon halo-blue">
          <AlbumIcon className="h-4 w-4" />
        </span>
        <div className="min-w-0">
          <div className="text-sm font-semibold text-slate-100">
            Tire pressure
          </div>
          <div className="text-[11px] uppercase tracking-wider text-slate-500">
            Last {days} days
          </div>
        </div>
        <div className="ml-auto flex flex-wrap gap-3 text-xs tabular-nums text-slate-300">
          <LatestChip label="FL" value={latest.fl} color={TIRE_COLORS.fl} format={fmtValue} />
          <LatestChip label="FR" value={latest.fr} color={TIRE_COLORS.fr} format={fmtValue} />
          <LatestChip label="RL" value={latest.rl} color={TIRE_COLORS.rl} format={fmtValue} />
          <LatestChip label="RR" value={latest.rr} color={TIRE_COLORS.rr} format={fmtValue} />
        </div>
      </div>
      {chart}
    </div>
  )
})

function LatestChip({
  label,
  value,
  color,
  format,
}: {
  label: string
  value: number | undefined
  color: string
  format: (v: number) => string
}) {
  return (
    <span className="inline-flex items-center gap-1.5">
      <span
        className="inline-block h-2 w-2 rounded-full"
        style={{ background: color }}
        aria-hidden
      />
      <span className="text-slate-500">{label}</span>
      <span className="text-slate-100">
        {value !== undefined ? format(value) : "—"}
      </span>
    </span>
  )
}

function TooltipRow({
  label,
  value,
  color,
  format,
}: {
  label: string
  value: number | undefined
  color: string
  format: (v: number) => string
}) {
  return (
    <div className="flex items-center gap-2 tabular-nums">
      <span
        className="inline-block h-2 w-2 rounded-full"
        style={{ background: color }}
        aria-hidden
      />
      <span className="text-slate-400">{label}</span>
      <span className="ml-auto font-medium">
        {value !== undefined ? format(value) : "—"}
      </span>
    </div>
  )
}

function formatXTick(ms: number): string {
  const t = new Date(ms)
  if (Number.isNaN(t.getTime())) return ""
  return t.toLocaleDateString([], { month: "short", day: "numeric" })
}

function formatTooltipTime(ms: number): string {
  const t = new Date(ms)
  if (Number.isNaN(t.getTime())) return ""
  return t.toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  })
}

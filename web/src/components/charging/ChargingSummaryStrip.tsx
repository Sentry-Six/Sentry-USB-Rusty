import type { CSSProperties } from "react"
import {
  AttachMoneyIcon,
  BatteryAndroidFrameBoltIcon,
  BoltIcon,
  NestEcoLeafIcon,
  ScheduleIcon,
  SpeedIcon,
} from "@/components/icons"
import { fmtDuration, fmtMoney, fmtPercent } from "@/lib/charge-format"

export interface ChargingStats {
  count: number
  totalEnergyKwh: number
  energySessionCount?: number
  costsByCurrency?: { currency: string; amount: number }[]
  totalDurationSecs: number
  // Null means no rate-derived cost or computable efficiency is available.
  totalCost: number | null
  currency: string
  avgEfficiency: number | null
}

// Aggregates the current charging filter set.
export function ChargingSummaryStrip({
  stats,
  loading,
}: {
  stats: ChargingStats
  loading: boolean
}) {
  if (loading && stats.count === 0) {
    return (
      <div className="flex flex-wrap items-center gap-x-5 gap-y-2">
        <div className="h-8 w-24 animate-pulse rounded-md bg-white/[0.04]" />
        <div className="h-8 w-20 animate-pulse rounded-md bg-white/[0.04]" />
        <div className="h-8 w-20 animate-pulse rounded-md bg-white/[0.04]" />
      </div>
    )
  }

  const energyCount = stats.energySessionCount ?? stats.count
  const avgKwh = energyCount > 0 ? stats.totalEnergyKwh / energyCount : 0
  const costs = stats.costsByCurrency ?? (stats.totalCost == null ? [] : [{ currency: stats.currency, amount: stats.totalCost }])

  const columns = 3 + Number(energyCount > 0) + Number(stats.avgEfficiency != null) + Number(costs.length > 0)
  return (
    <div className="@container">
      <dl
        aria-label="Charging summary"
        style={{ "--summary-columns": `repeat(${columns}, minmax(0, 1fr))` } as CSSProperties}
        className="grid grid-cols-2 gap-x-4 gap-y-4 @min-[520px]:grid-cols-3 @min-[800px]:[grid-template-columns:var(--summary-columns)] @min-[800px]:gap-x-3"
      >
        <StatCell
          icon={<BatteryAndroidFrameBoltIcon className="h-3.5 w-3.5" />}
          label="Sessions"
          value={stats.count.toLocaleString()}
        />
        <StatCell
          icon={<BoltIcon className="h-3.5 w-3.5 text-emerald-300" />}
          label={energyCount < stats.count ? "Recorded energy" : "Energy added"}
          value={energyCount ? `${stats.totalEnergyKwh.toFixed(1)} kWh` : "—"}
        />
        <StatCell
          icon={<ScheduleIcon className="h-3.5 w-3.5" />}
          label="Time charging"
          value={fmtDuration(stats.totalDurationSecs)}
        />
        {energyCount > 0 && (
          <StatCell
            icon={<SpeedIcon className="h-3.5 w-3.5" />}
            label="Avg / session"
            value={`${avgKwh.toFixed(1)} kWh`}
          />
        )}
        {stats.avgEfficiency != null && (
          <StatCell
            icon={<NestEcoLeafIcon className="h-3.5 w-3.5 text-emerald-300" />}
            label="Avg efficiency"
            value={fmtPercent(stats.avgEfficiency)}
          />
        )}
        {costs.length > 0 && (
          <StatCell
            icon={<AttachMoneyIcon className="h-3.5 w-3.5 text-emerald-300" />}
            label="Total cost"
            value={costs.map(({ currency, amount }) => (
              <span key={currency} className="block">{fmtMoney(amount, currency)}</span>
            ))}
          />
        )}
      </dl>
    </div>
  )
}

function StatCell({
  icon,
  label,
  value,
}: {
  icon: React.ReactNode
  label: string
  value: React.ReactNode
}) {
  return (
    <div data-summary-stat={label} className="flex min-w-0 items-center gap-2">
      <span
        className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-white/[0.04] ring-1 ring-inset ring-white/10 text-slate-300"
        aria-hidden
      >
        {icon}
      </span>
      <div className="min-w-0">
        <dt className="text-[9px] font-semibold uppercase tracking-wide text-slate-500">
          {label}
        </dt>
        <dd className="whitespace-nowrap text-sm font-semibold tabular-nums leading-tight text-slate-100">
          {value}
        </dd>
      </div>
    </div>
  )
}

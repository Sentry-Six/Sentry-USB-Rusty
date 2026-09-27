import type { ReactNode } from "react"
import { CheckCircleIcon, CancelIcon, CloseIcon, WarningIcon } from "@/components/icons"
import { cn } from "@/lib/utils"

export interface NotificationEvent {
  id: string
  ts: number
  type: string
  title: string
  message: string
  summary?: string | null
  providers: string[]
  results?: Record<string, string>
  provider_errors?: Record<string, string>
}

interface Props {
  event: NotificationEvent
  onDismiss: (id: string) => void
  icon?: ReactNode
  label?: string
  color?: string
  background?: string
  timeLabel?: string
}

export function NotificationHistoryItem({
  event, onDismiss, icon, label = event.type.replace(/_/g, " "),
  color = "text-slate-400", background = "bg-white/5", timeLabel,
}: Props) {
  const results = event.results ?? {}
  const values = Object.values(results)
  const allOk = values.length > 0 && values.every(value => value === "ok")
  const allError = values.length > 0 && values.every(value => value !== "ok")
  const date = new Date(event.ts * 1000)
  const validDate = Number.isFinite(date.getTime())
  const absoluteTime = validDate
    ? date.toLocaleString(undefined, { year: "numeric", month: "short", day: "numeric", hour: "numeric", minute: "2-digit", second: "2-digit", timeZoneName: "short" })
    : "Unknown time"
  // Include providers mentioned only by legacy results or newer error details.
  const providers = [...new Set([...event.providers, ...Object.keys(results), ...Object.keys(event.provider_errors ?? {})])]
  return (
    <div className="glass-card group relative overflow-hidden p-4 transition-colors hover:bg-white/[0.04]">
      <button onClick={() => onDismiss(event.id)} aria-label="Dismiss notification"
        className="absolute right-3 top-3 rounded-md p-1 text-slate-500 transition-all hover:bg-white/10 hover:text-slate-300 focus-visible:outline-2 focus-visible:outline-blue-400"
        title="Dismiss">
        <CloseIcon className="h-3.5 w-3.5" />
      </button>
      <div className="flex gap-3">
        {icon && <div className={cn("flex h-9 w-9 shrink-0 items-center justify-center rounded-xl", background)}>{icon}</div>}
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2 pr-6">
            <span className={cn("text-xs font-semibold uppercase tracking-wider", color)}>{label}</span>
            {values.length > 0 && (allOk
              ? <CheckCircleIcon className="h-3.5 w-3.5 text-emerald-400" />
              : allError ? <CancelIcon className="h-3.5 w-3.5 text-red-400" />
                : <WarningIcon className="h-3.5 w-3.5 text-amber-400" />)}
          </div>
          <p data-notification-summary className="mt-0.5 whitespace-pre-wrap break-words text-sm leading-relaxed text-slate-300">{event.summary?.trim() || event.message}</p>
          <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1">
            <span className="text-xs text-slate-500" title={absoluteTime}>{timeLabel || absoluteTime}</span>
            {providers.map(provider => (
              <span key={provider} title={results[provider] === "ok" ? "Accepted by provider" : results[provider] ? "Provider reported an error" : "Delivery status unavailable"}
                className={cn("rounded-md px-1.5 py-0.5 text-[10px] font-medium",
                  results[provider] === "ok" ? "bg-emerald-500/10 text-emerald-400"
                    : results[provider] ? "bg-red-500/10 text-red-400" : "bg-white/5 text-slate-400")}>
                {provider.replace(/_/g, " ")}
              </span>
            ))}
          </div>
          <details className="mt-2 text-xs text-slate-400">
            <summary className="w-fit cursor-pointer rounded py-1 text-slate-300 focus-visible:outline-2 focus-visible:outline-blue-400">Details</summary>
            <div className="mt-2 space-y-2 border-t border-white/10 pt-2">
              <p className="break-words font-medium">{event.title}</p>
              <time dateTime={validDate ? date.toISOString() : undefined}>{absoluteTime}</time>
              <p className="select-text whitespace-pre-wrap break-words">{event.message}</p>
              {providers.map(provider => {
                const error = event.provider_errors?.[provider]
                const status = results[provider]
                return <p key={provider} className="select-text whitespace-pre-wrap break-words">
                  <span className="font-medium">{provider.replace(/_/g, " ")}: </span>
                  {error || (status === "ok" ? "Accepted by provider" : status || "Delivery status unavailable")}
                </p>
              })}
            </div>
          </details>
        </div>
      </div>
    </div>
  )
}

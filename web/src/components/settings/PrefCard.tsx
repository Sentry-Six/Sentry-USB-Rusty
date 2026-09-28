import { Children, type ReactNode } from "react"
import { cn } from "@/lib/utils"
import type { Halo } from "@/components/ui/StatusTile"
import { SectionErrorBoundary } from "@/components/ErrorBoundary"

/** Explanation and action shown when a card’s controls are unavailable. */
export interface DisabledConfig {
  /** Centered text explaining what the user needs to enable. */
  reason: string
  /** Optional "go enable this" affordance. */
  cta?: {
    label: string
    onClick?: () => void
    href?: string
  }
}

interface PrefCardProps {
  icon: ReactNode
  halo?: Halo
  title: ReactNode
  badge?: ReactNode
  help?: ReactNode
  footer?: ReactNode
  className?: string
  /**
   * Keep unavailable controls mounted but hidden and inert. Header help and
   * the action for enabling the feature remain available.
   */
  disabled?: DisabledConfig
  children: ReactNode
}

export function PrefCard({
  icon,
  halo = "slate",
  title,
  badge,
  help,
  footer,
  className,
  disabled,
  children,
}: PrefCardProps) {
  return (
    <div
      className={cn("glass-card overflow-hidden", className)}
      data-disabled={disabled ? "true" : undefined}
    >
      <div className="flex items-center gap-2.5 border-b border-white/5 px-3.5 py-2.5">
        <span
          className={cn(
            "halo-" + halo,
            "inline-flex h-7 w-7 shrink-0 items-center justify-center rounded-lg",
          )}
        >
          {icon}
        </span>
        <span className="t-md font-semibold">{title}</span>
        {help}
        {badge && <span className="ml-auto">{badge}</span>}
      </div>
      {disabled ? (
        <div>
          {/* Preserve control state without reserving space for unavailable controls. */}
          <div
            inert
            aria-hidden
            aria-disabled="true"
            hidden
          >
            {children}
          </div>
          <div className="flex flex-col items-start gap-3 p-4">
            <p className="text-sm text-slate-400">{disabled.reason}</p>
            {disabled.cta && <DisabledCta cta={disabled.cta} />}
          </div>
        </div>
      ) : (
        <div className="flex flex-col gap-2.5 p-3.5">{children}</div>
      )}
      {footer && <div className="border-t border-white/5 px-3.5 py-2.5">{footer}</div>}
    </div>
  )
}

function DisabledCta({ cta }: { cta: NonNullable<DisabledConfig["cta"]> }) {
  const cls =
    "rounded-lg bg-blue-500/15 px-3 py-1.5 text-xs font-medium text-blue-400 transition-colors hover:bg-blue-500/25"
  if (cta.href) {
    return (
      <a href={cta.href} className={cls}>
        {cta.label}
      </a>
    )
  }
  return (
    <button type="button" onClick={cta.onClick} className={cls}>
      {cta.label}
    </button>
  )
}

/** Callers group related cards in independent columns; row heights never couple. */
export function PrefGrid({ children }: { children: ReactNode; min?: number }) {
  return <div className="grid items-start gap-4 xl:grid-cols-2">
    {Children.map(children, child => <div className="flex min-w-0 flex-col gap-4"><SectionErrorBoundary>{child}</SectionErrorBoundary></div>)}
  </div>
}

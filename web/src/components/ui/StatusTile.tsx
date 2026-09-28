import type { ReactNode } from "react"
import { cn } from "@/lib/utils"

export type Halo =
  | "accent"
  | "amber"
  | "red"
  | "rose"
  | "blue"
  | "violet"
  | "purple"
  | "slate"

interface StatusTileProps {
  icon: ReactNode
  halo?: Halo
  title: string
  badge?: ReactNode
  action?: ReactNode
  className?: string
  children: ReactNode
}

export function StatusTile({
  icon,
  halo = "slate",
  title,
  badge,
  action,
  className,
  children,
}: StatusTileProps) {
  return (
    <div className={cn("glass-card tile h-full min-w-0", className)}>
      <div className="tile-header">
        <span className={cn("tile-icon", `halo-${halo}`)}>{icon}</span>
        <span className="tile-title">{title}</span>
        {badge && <span className="tile-action">{badge}</span>}
        {action && <span className="tile-action">{action}</span>}
      </div>
      <div className="tile-body">{children}</div>
    </div>
  )
}

interface RowProps {
  icon?: ReactNode
  label: ReactNode
  value?: ReactNode
  sub?: ReactNode
  valueColor?: string
}

export function Row({ icon, label, value, sub, valueColor }: RowProps) {
  return (
    <div className="tile-row">
      {icon && (
        <span className="inline-flex shrink-0 text-slate-500">{icon}</span>
      )}
      <span className="lbl">{label}</span>
      {value !== undefined && value !== null && (
        <span className="val" style={valueColor ? { color: valueColor } : undefined}>
          {value}
        </span>
      )}
      {sub && <span className="sub ml-auto">{sub}</span>}
    </div>
  )
}

export function TileDivider() {
  return <div className="tile-divider" />
}

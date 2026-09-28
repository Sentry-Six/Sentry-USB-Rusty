import { useId, type ReactNode } from "react"

interface ToggleProps {
  checked: boolean
  onChange: (next: boolean) => void
  label: ReactNode
  sub?: ReactNode
  help?: ReactNode
  disabled?: boolean
}

export function Toggle({ checked, onChange, label, sub, help, disabled }: ToggleProps) {
  const id = useId()
  return <div className="flex items-center gap-3">
    <div className="min-w-0 flex-1">
      <div className="flex items-center gap-1">
        <label htmlFor={id} className="t-md cursor-pointer">{label}</label>
        {help}
      </div>
      {sub && <div className="t-xs mt-0.5">{sub}</div>}
    </div>
    <input id={id} type="checkbox" className="toggle-switch" checked={checked} disabled={disabled}
      onChange={event => onChange(event.target.checked)} />
  </div>
}

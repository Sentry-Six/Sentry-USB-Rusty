import { useState } from "react"
import { VisibilityIcon, VisibilityOffIcon } from "@/components/icons"

interface SecretInputProps {
  id?: string
  label?: string
  readOnly?: boolean
  value: string
  onChange: (value: string) => void
  placeholder?: string
  className?: string
}

export function SecretInput({ id, label, readOnly, value, onChange, placeholder, className }: SecretInputProps) {
  const [visible, setVisible] = useState(false)

  return (
    <div className="relative">
      <input
        id={id}
        aria-label={label}
        readOnly={readOnly}
        type={visible ? "text" : "password"}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        className={className}
      />
      <button
        type="button"
        aria-label={visible ? "Hide secret" : "Show secret"}
        aria-pressed={visible}
        onClick={() => setVisible((v) => !v)}
        className="absolute right-2 top-1/2 -translate-y-1/2 rounded p-1 text-slate-500 transition-colors hover:text-slate-300"
      >
        {visible ? <VisibilityOffIcon className="h-3.5 w-3.5" /> : <VisibilityIcon className="h-3.5 w-3.5" />}
      </button>
    </div>
  )
}

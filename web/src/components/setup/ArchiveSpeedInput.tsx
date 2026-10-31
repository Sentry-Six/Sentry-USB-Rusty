import { useState } from "react"
import { archiveSpeedError, DEFAULT_ARCHIVE_SPEED_MPS } from "./archiveSpeed"

export function ArchiveSpeedInput({ value, metric, disabled, onChange }: {
  value: string | undefined
  metric: boolean
  disabled: boolean
  onChange: (value: string) => void
}) {
  const factor = metric ? 3.6 : 2.2369362920544
  const [focused, setFocused] = useState(false)
  const format = (raw: string | undefined) => archiveSpeedError(raw) ? "" :
    String(Math.round(Number(raw ?? DEFAULT_ARCHIVE_SPEED_MPS) * factor))
  const [draft, setDraft] = useState(() => format(value))
  const error = archiveSpeedError(value)
  return (
    <div className="ml-6 space-y-1">
      <label className="flex items-center gap-2 text-sm text-slate-300">
        Speed threshold ({metric ? "km/h" : "mph"})
        <input
          type="number" min="0" step="1" value={focused ? draft : format(value)} disabled={disabled}
          aria-invalid={!!error}
          onFocus={() => { setDraft(format(value)); setFocused(true) }} onBlur={() => setFocused(false)}
          onChange={(e) => {
            const raw = e.target.value.trim() === "" ? "" : String(Math.round(Number(e.target.value)))
            setDraft(raw)
            onChange(raw.trim() === "" ? "" : String(Number(raw) / factor))
          }}
          className="w-28 rounded border border-white/10 bg-white/5 px-2 py-1 disabled:opacity-40"
        />
      </label>
      {error && <p role="alert" className="text-xs text-red-400">{error}</p>}
      <p className="text-xs text-slate-500">
        A speed sample must exceed this threshold. Any non-Park gear sample also qualifies, regardless of speed.
      </p>
    </div>
  )
}

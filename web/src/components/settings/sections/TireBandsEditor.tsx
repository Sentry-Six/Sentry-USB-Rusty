import { useState } from "react"
import { useUnits } from "@/lib/units"
import { DEFAULT_TIRE_BANDS, isDefaultTireBands, tireBandsValid, type TireBands } from "@/lib/tireBands"

const PSI_TO_BAR = 0.0689476

const FIELDS: { key: keyof TireBands; label: string }[] = [
  { key: "unsafeLow", label: "Unsafe below" },
  { key: "optimalLow", label: "Optimal from" },
  { key: "optimalHigh", label: "Optimal up to" },
  { key: "unsafeHigh", label: "Unsafe above" },
]

type Draft = Record<keyof TireBands, string>

/**
 * Tire-pressure bands for the dashboard tile and history chart, edited in the
 * display unit and saved in PSI. The defaults suit Tesla's passenger cars; a
 * Cybertruck or other tires with a higher spec need their own range.
 */
export function TireBandsEditor() {
  const { tireBands, pressureBar, loaded, setTireBands } = useUnits()
  const unit = pressureBar ? "bar" : "psi"
  const toDisplay = (psi: number) => (pressureBar ? (psi * PSI_TO_BAR).toFixed(2) : String(Math.round(psi * 10) / 10))
  const toDraft = (b: TireBands): Draft => ({
    unsafeLow: toDisplay(b.unsafeLow),
    optimalLow: toDisplay(b.optimalLow),
    optimalHigh: toDisplay(b.optimalHigh),
    unsafeHigh: toDisplay(b.unsafeHigh),
  })
  const [draft, setDraft] = useState<Draft>(() => toDraft(tireBands))
  const [status, setStatus] = useState<"idle" | "saving" | "saved" | "failed">("idle")

  // Reload the fields when the saved bands or the unit change (after loading,
  // saving, or switching psi and bar).
  const savedKey = `${tireBands.unsafeLow},${tireBands.optimalLow},${tireBands.optimalHigh},${tireBands.unsafeHigh},${unit},${loaded}`
  const [shownKey, setShownKey] = useState(savedKey)
  if (shownKey !== savedKey) {
    setShownKey(savedKey)
    setDraft(toDraft(tireBands))
  }

  const parsed: TireBands = {
    unsafeLow: fromDisplay(draft.unsafeLow, pressureBar),
    optimalLow: fromDisplay(draft.optimalLow, pressureBar),
    optimalHigh: fromDisplay(draft.optimalHigh, pressureBar),
    unsafeHigh: fromDisplay(draft.unsafeHigh, pressureBar),
  }
  const valid = tireBandsValid(parsed)
  const dirty = JSON.stringify(toDraft(parsed)) !== JSON.stringify(toDraft(tireBands))

  async function save(bands: TireBands | null) {
    setStatus("saving")
    setStatus((await setTireBands(bands)) ? "saved" : "failed")
  }

  return (
    <div className="space-y-2 border-t border-white/[0.06] pt-3">
      <div>
        <div className="text-sm text-slate-200">Tire pressure ranges</div>
        <div className="text-xs text-slate-500">
          Bands on the Tires tile and history chart, in {unit}. Set them to your tires&apos; recommended
          pressure, for example on a Cybertruck.
        </div>
      </div>
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
        {FIELDS.map((f) => (
          <label key={f.key} className="space-y-1">
            <span className="block text-[11px] text-slate-400">{f.label}</span>
            <span className="flex items-center gap-1.5">
              <input
                type="number"
                inputMode="decimal"
                step={pressureBar ? "0.05" : "1"}
                min="0"
                value={draft[f.key]}
                onChange={(e) => {
                  setDraft((d) => ({ ...d, [f.key]: e.target.value }))
                  setStatus("idle")
                }}
                aria-label={`${f.label} (${unit})`}
                className="w-20 rounded-lg border border-white/10 bg-white/5 px-2 py-1.5 text-xs text-slate-200 tabular-nums placeholder:text-slate-600 focus:border-blue-500/40 focus:outline-none"
              />
              <span className="text-[10px] text-slate-500">{unit}</span>
            </span>
          </label>
        ))}
      </div>
      {!valid && (
        <div className="text-xs text-amber-400">
          Each value must be higher than the one before it.
        </div>
      )}
      <div className="flex items-center gap-2">
        <button
          type="button"
          disabled={!valid || !dirty || status === "saving"}
          onClick={() => void save(parsed)}
          className="rounded-lg bg-blue-500/15 px-3 py-1.5 text-xs font-medium text-blue-400 transition-colors hover:bg-blue-500/25 disabled:opacity-40"
        >
          {status === "saving" ? "Saving…" : "Save"}
        </button>
        <button
          type="button"
          disabled={isDefaultTireBands(tireBands) || status === "saving"}
          onClick={() => void save(null)}
          className="rounded-lg border border-white/10 px-3 py-1.5 text-xs font-medium text-slate-300 transition-colors hover:bg-white/[0.05] disabled:opacity-40"
        >
          Reset to {toDisplay(DEFAULT_TIRE_BANDS.optimalLow)}–{toDisplay(DEFAULT_TIRE_BANDS.optimalHigh)} {unit}
        </button>
        {status === "saved" && <span className="text-xs text-emerald-400">Saved</span>}
        {status === "failed" && <span className="text-xs text-rose-400">Couldn&apos;t save. Try again.</span>}
      </div>
    </div>
  )
}

function fromDisplay(value: string, bar: boolean): number {
  const n = Number(value.trim().replace(",", "."))
  if (value.trim() === "" || !Number.isFinite(n)) return Number.NaN
  return bar ? n / PSI_TO_BAR : n
}

import { useEffect, useId, useRef, useState } from "react"
import { CalendarMonthIcon, CheckIcon, ExpandMoreIcon } from "@/components/icons"
import { DROPDOWN_SURFACE, DROPDOWN_TRIGGER, DROPDOWN_OPTION } from "@/components/ui/dropdownStyles"
import { DateRangeCalendar } from "./DateRangeCalendar"
import { validDateRange, type DatePreset, type DateRange } from "@/lib/date-range"
import { cn } from "@/lib/utils"

interface DatePopoverProps { range: DateRange; onChange: (range: DateRange) => void }
const PRESETS: { value: DatePreset; label: string }[] = [
  { value: "today", label: "Today" }, { value: "yesterday", label: "Yesterday" },
  { value: "last7", label: "Last 7 days" }, { value: "last30", label: "Last 30 days" },
  { value: "thisYear", label: "This year" }, { value: "lastYear", label: "Last year" },
  { value: "all", label: "All time" },
]
function presetLabel(range: DateRange): string {
  return range.kind === "custom" ? `${range.start} – ${range.end}` : PRESETS.find((preset) => preset.value === range.preset)?.label ?? "Last 7 days"
}

export function DatePopover({ range, onChange }: DatePopoverProps) {
  const [open, setOpen] = useState(false)
  const wrapRef = useRef<HTMLDivElement>(null)
  const trigger = useRef<HTMLButtonElement>(null)
  const id = useId()
  const [customStart, setCustomStart] = useState(range.kind === "custom" ? range.start : "")
  const [customEnd, setCustomEnd] = useState(range.kind === "custom" ? range.end : "")
  const valid = validDateRange(customStart, customEnd)
  const invalid = !!customStart && !!customEnd && !valid

  useEffect(() => {
    if (!open) return
    const outside = (event: MouseEvent) => {
      const popup = (event.target as HTMLElement | null)?.closest?.("[data-select-menu-popup]")
      const owner = popup?.getAttribute("data-select-owner")
      if (owner && wrapRef.current?.contains(document.getElementById(owner))) return
      if (!wrapRef.current?.contains(event.target as Node)) setOpen(false)
    }
    document.addEventListener("mousedown", outside)
    return () => document.removeEventListener("mousedown", outside)
  }, [open])

  const commit = (next: DateRange) => { onChange(next); setOpen(false); trigger.current?.focus() }
  const activePreset = range.kind === "preset" ? range.preset : null
  return <div ref={wrapRef} className="relative min-w-0" onKeyDown={(event) => {
    if (event.key === "Escape" && open) { event.preventDefault(); event.stopPropagation(); setOpen(false); trigger.current?.focus() }
  }}>
    <button ref={trigger} type="button" aria-haspopup="dialog" aria-controls={open ? id : undefined} aria-expanded={open}
      onClick={() => {
        if (!open && range.kind === "custom") { setCustomStart(range.start); setCustomEnd(range.end) }
        setOpen((previous) => !previous)
      }} className={cn(DROPDOWN_TRIGGER, "max-w-full")}>
      <CalendarMonthIcon className="h-4 w-4 shrink-0" /><span className="truncate">{presetLabel(range)}</span><ExpandMoreIcon className="h-4 w-4 shrink-0 text-slate-400" />
    </button>
    {open && <div id={id} role="dialog" aria-label="Choose date range" className={cn(DROPDOWN_SURFACE, "absolute left-0 top-full z-50 mt-2 max-h-[75dvh] w-[22rem] max-w-[calc(100vw-2rem)] overflow-y-auto p-3")}>
      <div className="grid grid-cols-2 gap-1" aria-label="Quick date ranges">
        {PRESETS.map((preset) => <button key={preset.value} type="button" onClick={() => commit({ kind: "preset", preset: preset.value })}
          className={cn(DROPDOWN_OPTION, "min-h-9 justify-between gap-1 px-2 py-1.5 text-xs", preset.value === activePreset && "bg-white/10 text-emerald-300")}>
          {preset.label}{preset.value === activePreset && <CheckIcon className="h-3.5 w-3.5 shrink-0" />}
        </button>)}
      </div>
      <div className="mt-3 border-t border-white/10 pt-3">
        <DateRangeCalendar start={customStart} end={customEnd} onChange={(start, end) => { setCustomStart(start); setCustomEnd(end) }} />
        <div className="mt-3 grid grid-cols-2 gap-2">
          <label className="min-w-0 text-xs text-slate-400">Start date
            <input type="text" aria-label="Start date" aria-invalid={invalid || undefined} autoComplete="off" placeholder="YYYY-MM-DD" maxLength={10}
              value={customStart} onChange={(event) => setCustomStart(event.target.value)}
              className="mt-1 w-full min-w-0 rounded-xl border border-white/10 bg-white/5 px-2 py-2 text-xs tabular-nums text-slate-100 outline-none focus:border-emerald-400/60" />
          </label>
          <label className="min-w-0 text-xs text-slate-400">End date
            <input type="text" aria-label="End date" aria-invalid={invalid || undefined} autoComplete="off" placeholder="YYYY-MM-DD" maxLength={10}
              value={customEnd} onChange={(event) => setCustomEnd(event.target.value)}
              className="mt-1 w-full min-w-0 rounded-xl border border-white/10 bg-white/5 px-2 py-2 text-xs tabular-nums text-slate-100 outline-none focus:border-emerald-400/60" />
          </label>
        </div>
        {invalid && <p className="mt-2 text-xs text-rose-300">Enter valid dates in YYYY-MM-DD order, with the end on or after the start.</p>}
        <button type="button" disabled={!valid} onClick={() => { if (valid) commit({ kind: "custom", start: customStart, end: customEnd }) }}
          className="mt-3 min-h-10 w-full rounded-full bg-emerald-400 px-3 py-2 text-sm font-medium text-slate-950 disabled:opacity-40">Apply range</button>
      </div>
    </div>}
  </div>
}

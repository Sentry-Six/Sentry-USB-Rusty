import { useId, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react"
import { ChevronLeftIcon, ChevronRightIcon } from "@/components/icons"
import { SelectMenu } from "@/components/ui/SelectMenu"
import { calendarDateText, parseLocalDate, shiftCalendarMonth } from "@/lib/date-range"
import { cn } from "@/lib/utils"

const WEEKDAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"]
const MONTHS = Array.from({ length: 12 }, (_, month) => ({ value: String(month), label: new Date(2000, month, 1).toLocaleDateString([], { month: "long" }) }))

export function DateRangeCalendar({ start, end, onChange }: {
  start: string
  end: string
  onChange: (start: string, end: string) => void
}) {
  const [today] = useState(() => calendarDateText(new Date()))
  const [observedStart, setObservedStart] = useState(start)
  const [shown, setShown] = useState(() => parseLocalDate(start) ?? parseLocalDate(today)!)
  const [focused, setFocused] = useState(() => calendarDateText(parseLocalDate(start) ?? parseLocalDate(today)!))
  const cells = useRef(new Map<string, HTMLButtonElement>())
  const restoreDayFocus = useRef(false)
  const helpId = useId()
  const validStart = parseLocalDate(start)
  const validEnd = parseLocalDate(end)
  const selectingEnd = !!validStart && (!validEnd || end < start)
  const year = shown.getFullYear(), month = shown.getMonth()
  const todayYear = parseLocalDate(today)!.getFullYear()
  const years = [...new Set([...Array.from({ length: 41 }, (_, index) => todayYear - 30 + index), year])].sort((a, b) => a - b)
  const first = new Date(year, month, 1)
  first.setDate(first.getDate() - first.getDay())
  const days = Array.from({ length: 42 }, (_, index) => {
    const day = new Date(first)
    day.setDate(day.getDate() + index)
    return day
  })
  const monthLabel = shown.toLocaleDateString([], { month: "long", year: "numeric" })

  if (start !== observedStart) {
    setObservedStart(start)
    const date = parseLocalDate(start)
    if (date) {
      setShown(date)
      setFocused(calendarDateText(date))
    }
  }
  useLayoutEffect(() => {
    if (!restoreDayFocus.current) return
    cells.current.get(focused)?.focus()
    restoreDayFocus.current = false
  }, [focused, year, month])

  const move = (date: Date, focusDay = false) => {
    restoreDayFocus.current = focusDay
    setShown(date)
    setFocused(calendarDateText(date))
  }
  const choose = (date: Date) => {
    const text = calendarDateText(date)
    if (selectingEnd && validStart) {
      onChange(text < start ? text : start, text < start ? start : text)
    } else {
      onChange(text, "")
    }
    move(date, true)
  }
  const navigateDay = (event: KeyboardEvent<HTMLButtonElement>, date: Date) => {
    const next = new Date(date)
    if (event.key === "ArrowLeft") next.setDate(next.getDate() - 1)
    else if (event.key === "ArrowRight") next.setDate(next.getDate() + 1)
    else if (event.key === "ArrowUp") next.setDate(next.getDate() - 7)
    else if (event.key === "ArrowDown") next.setDate(next.getDate() + 7)
    else if (event.key === "Home") next.setDate(next.getDate() - next.getDay())
    else if (event.key === "End") next.setDate(next.getDate() + 6 - next.getDay())
    else if (event.key === "PageUp" || event.key === "PageDown") {
      event.preventDefault()
      move(shiftCalendarMonth(date, (event.key === "PageUp" ? -1 : 1) * (event.shiftKey ? 12 : 1)), true)
      return
    } else return
    event.preventDefault()
    move(next, true)
  }

  return <div className="min-w-0" aria-describedby={helpId}>
    <div className="mb-3 flex items-center gap-1.5">
      <button type="button" aria-label="Previous month" onClick={() => move(shiftCalendarMonth(shown, -1))} className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full text-slate-300 hover:bg-white/10 focus-visible:outline-2 focus-visible:outline-emerald-400"><ChevronLeftIcon className="h-4 w-4" /></button>
      <div className="min-w-0 flex-1"><SelectMenu label="Calendar month" value={String(month)} options={MONTHS} fullWidth className="min-h-9 gap-1 px-3 py-1 text-xs" onChange={(value) => move(new Date(year, Number(value), 1))} /></div>
      <SelectMenu label="Calendar year" value={String(year)} options={years.map((value) => ({ value: String(value), label: String(value) }))} className="min-h-9 gap-1 px-3 py-1 text-xs" onChange={(value) => move(new Date(Number(value), month, 1))} />
      <button type="button" aria-label="Next month" onClick={() => move(shiftCalendarMonth(shown, 1))} className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full text-slate-300 hover:bg-white/10 focus-visible:outline-2 focus-visible:outline-emerald-400"><ChevronRightIcon className="h-4 w-4" /></button>
    </div>
    <table role="grid" aria-label={monthLabel} aria-multiselectable="true" className="w-full table-fixed border-separate border-spacing-y-1 text-center">
      <thead><tr>{WEEKDAYS.map((day) => <th key={day} scope="col" abbr={day} className="pb-1 text-[10px] font-medium text-slate-400">{day.slice(0, 3)}</th>)}</tr></thead>
      <tbody>{Array.from({ length: 6 }, (_, week) => <tr key={week}>
        {days.slice(week * 7, week * 7 + 7).map((date) => {
          const text = calendarDateText(date)
          const boundary = (!!validStart && text === start) || (!!validEnd && text === end)
          const between = !!validStart && !!validEnd && text > start && text < end
          return <td key={text} aria-selected={boundary || between} className={cn("p-0", between && "bg-emerald-400/10")}>
            <button type="button" data-date={text}
              ref={(element) => { if (element) cells.current.set(text, element); else cells.current.delete(text) }}
              aria-label={date.toLocaleDateString([], { weekday: "long", year: "numeric", month: "long", day: "numeric" })}
              aria-current={text === today ? "date" : undefined} tabIndex={text === focused ? 0 : -1}
              onFocus={() => setFocused(text)} onClick={() => choose(date)} onKeyDown={(event) => navigateDay(event, date)}
              className={cn("mx-auto flex h-9 w-full max-w-9 items-center justify-center rounded-full text-xs tabular-nums outline-offset-2 focus-visible:outline-2 focus-visible:outline-emerald-300",
                boundary ? "bg-emerald-400 font-semibold text-slate-950" : date.getMonth() === month ? "text-slate-200 hover:bg-white/10" : "text-slate-500 hover:bg-white/5",
                text === today && !boundary && "ring-1 ring-inset ring-emerald-400/60")}>
              {date.getDate()}
            </button>
          </td>
        })}
      </tr>)}</tbody>
    </table>
    <p id={helpId} className="mt-2 text-xs text-slate-400" aria-live="polite">{selectingEnd ? "Choose the end date." : "Choose the start date."}</p>
  </div>
}

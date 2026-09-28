export type DateRange =
  | { kind: "preset"; preset: DatePreset }
  | { kind: "custom"; start: string; end: string }

export type DatePreset =
  | "today"
  | "yesterday"
  | "last7"
  | "last30"
  | "thisYear"
  | "lastYear"
  | "all"

export function parseLocalDate(value: string): Date | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value)
  if (!match) return null
  const [, y, m, d] = match.map(Number)
  const date = new Date(y, m - 1, d)
  return date.getFullYear() === y && date.getMonth() === m - 1 && date.getDate() === d ? date : null
}

export function validDateRange(start: string, end: string): boolean {
  const a = parseLocalDate(start), b = parseLocalDate(end)
  return !!a && !!b && a <= b
}

export function rangeBounds(range: DateRange, now: Date): { from?: Date; to?: Date } {
  if (range.kind === "custom") {
    const from = parseLocalDate(range.start)
    const end = parseLocalDate(range.end)
    if (!from || !end || end < from) return { from: new Date(8640000000000000) }
    end.setDate(end.getDate() + 1)
    return { from, to: end }
  }
  const startOfToday = new Date(now)
  startOfToday.setHours(0, 0, 0, 0)
  switch (range.preset) {
    case "today":
      return { from: startOfToday }
    case "yesterday": {
      const y = new Date(startOfToday)
      y.setDate(y.getDate() - 1)
      return { from: y, to: startOfToday }
    }
    case "last7": {
      const f = new Date(startOfToday)
      f.setDate(f.getDate() - 7)
      return { from: f }
    }
    case "last30": {
      const f = new Date(startOfToday)
      f.setDate(f.getDate() - 30)
      return { from: f }
    }
    case "thisYear": {
      const f = new Date(now.getFullYear(), 0, 1)
      return { from: f }
    }
    case "lastYear": {
      const f = new Date(now.getFullYear() - 1, 0, 1)
      const t = new Date(now.getFullYear(), 0, 1)
      return { from: f, to: t }
    }
    case "all":
    default:
      return {}
  }
}


export function calendarDateText(date: Date): string {
  const pad = (value: number) => String(value).padStart(2, "0")
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`
}

export function shiftCalendarMonth(date: Date, delta: number): Date {
  const shifted = new Date(date)
  shifted.setDate(1)
  shifted.setMonth(shifted.getMonth() + delta)
  const last = new Date(shifted.getFullYear(), shifted.getMonth() + 1, 0).getDate()
  shifted.setDate(Math.min(date.getDate(), last))
  return shifted
}

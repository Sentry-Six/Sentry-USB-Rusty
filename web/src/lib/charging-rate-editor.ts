export interface RateSchedule {
  sourceIndex?: number
  label: string
  start: string // local "HH:MM"
  end: string // local "HH:MM"; an end before start wraps past midnight
  days: number[] // 0=Sun..6=Sat; empty = every day
  startMonth: number // 1=Jan..12=Dec
  endMonth: number // 1..12; an end month before the start wraps the year
  rate: number
}

// The first matching schedule wins, followed by the tag and global flat rates.
export interface TagRate {
  flat: number | null
  schedules: RateSchedule[]
}

export interface ChargingRates {
  currency: string
  defaultRate: number | null
  tags: Record<string, TagRate>
}

// Preference values may be numbers or numeric strings.
function toRate(v: unknown): number | null {
  const n =
    typeof v === "number"
      ? v
      : typeof v === "string"
        ? v.trim() ? Number(v.trim()) : NaN
        : NaN
  return Number.isFinite(n) && n >= 0 ? n : null
}

function clockText(raw: unknown): string | null {
  const text = typeof raw === "string" ? raw.trim() : ""
  let minutes = typeof raw === "number" ? raw : /^\d+$/.test(text) ? Number(text) : NaN
  const clock = /^(\d{1,2}):([0-5]\d)$/.exec(text)
  if (clock && Number(clock[1]) <= 24) minutes = Math.min(Number(clock[1]) * 60 + Number(clock[2]), 1440)
  if (!Number.isInteger(minutes) || minutes < 0 || minutes > 1440) return null
  return `${String(Math.floor(minutes / 60) % 24).padStart(2, "0")}:${String(minutes % 60).padStart(2, "0")}`
}

// Empty means every day; discard duplicates and out-of-range values.
function parseDays(raw: unknown): number[] {
  if (!Array.isArray(raw)) return []
  const out: number[] = []
  for (const d of raw) {
    const n =
      typeof d === "number" ? d : typeof d === "string" ? parseInt(d, 10) : NaN
    if (Number.isInteger(n) && n >= 0 && n <= 6 && !out.includes(n)) out.push(n)
  }
  return out.length === 7 ? [] : out.sort((a, b) => a - b)
}

// Accept numeric strings and enforce the 1–12 month range.
function parseMonth(raw: unknown, fallback: number): number {
  const n =
    typeof raw === "number" ? raw : typeof raw === "string" ? parseInt(raw, 10) : NaN
  return Number.isInteger(n) && n >= 1 && n <= 12 ? n : fallback
}

function parseSchedule(raw: unknown): RateSchedule | null {
  if (!raw || typeof raw !== "object") return null
  const o = raw as Record<string, unknown>
  const rate = toRate(o.rate)
  const start = clockText(o.start)
  const end = clockText(o.end)
  if (rate == null || start == null || end == null) return null
  return {
    label: typeof o.label === "string" ? o.label : "",
    start,
    end,
    days: parseDays(o.days),
    startMonth: parseMonth(o.startMonth, 1),
    endMonth: parseMonth(o.endMonth, 12),
    rate,
  }
}

// Accept both bare flat rates and structured plans.
function parseTagRate(raw: unknown): TagRate {
  if (raw && typeof raw === "object" && !Array.isArray(raw)) {
    const o = raw as Record<string, unknown>
    const schedules: RateSchedule[] = []
    if (Array.isArray(o.schedules)) {
      for (const [sourceIndex, s] of o.schedules.entries()) {
        const parsed = parseSchedule(s)
        if (parsed) schedules.push({ ...parsed, sourceIndex })
      }
    }
    return { flat: toRate(o.flat), schedules }
  }
  return { flat: toRate(raw), schedules: [] }
}


export type RateDocument = Record<string, unknown>
const isObject = (value: unknown): value is RateDocument => !!value && typeof value === "object" && !Array.isArray(value)
function same(a: unknown, b: unknown): boolean {
  if (a === b) return true
  if (Array.isArray(a) && Array.isArray(b)) return a.length === b.length && a.every((value, index) => same(value, b[index]))
  if (isObject(a) && isObject(b)) {
    const keys = Object.keys(a)
    return keys.length === Object.keys(b).length && keys.every(key => Object.hasOwn(b, key) && same(a[key], b[key]))
  }
  return false
}
function plansOf(document: RateDocument): RateDocument {
  let raw = document.charging_tag_rates
  if (typeof raw === "string") raw = JSON.parse(raw)
  if (raw == null) return {}
  if (!isObject(raw)) throw new Error("Rate plans could not be read.")
  return raw
}
export function readRateDocument(raw: unknown): RateDocument {
  if (!isObject(raw) || Object.keys(raw).some(key => !["charging_currency", "charging_default_rate", "charging_tag_rates"].includes(key))) {
    throw new Error("Rates could not be read.")
  }
  plansOf(raw)
  return raw
}
export function parseRates(document: RateDocument): ChargingRates {
  return {
    currency: typeof document.charging_currency === "string" ? document.charging_currency.trim() || "$" : "$",
    defaultRate: toRate(document.charging_default_rate),
    tags: Object.fromEntries(Object.entries(plansOf(document)).map(([tag, plan]) => [tag, parseTagRate(plan)])),
  }
}

// Compare the editor's visible fields; retain the exact raw values everywhere
// else. Source indices follow schedules through removal/reordering.
export function applyRateChanges(document: RateDocument, next: ChargingRates): RateDocument {
  const before = parseRates(document)
  const result = { ...document }
  if (before.currency !== next.currency) result.charging_currency = next.currency
  if (before.defaultRate !== next.defaultRate) result.charging_default_rate = next.defaultRate
  const rawPlans = plansOf(document)
  const plans = new Map(Object.entries(rawPlans))
  let changed = false
  for (const tag of new Set([...Object.keys(before.tags), ...Object.keys(next.tags)])) {
    const prior = (Object.hasOwn(before.tags, tag) ? before.tags[tag] : undefined) ?? { flat: null, schedules: [] }
    const desired = (Object.hasOwn(next.tags, tag) ? next.tags[tag] : undefined) ?? { flat: null, schedules: [] }
    if (same(prior, desired)) continue
    const raw = Object.hasOwn(rawPlans, tag) ? rawPlans[tag] : undefined
    const plan: RateDocument = isObject(raw) ? { ...raw } : raw == null ? {} : { flat: raw }
    if (prior.flat !== desired.flat) plan.flat = desired.flat
    if (!same(prior.schedules, desired.schedules)) {
      const original = Array.isArray(plan.schedules) ? plan.schedules : []
      if ((plan.schedules != null && !Array.isArray(plan.schedules)) || original.length !== prior.schedules.length) {
        throw new Error(`Some schedules for ${tag} could not be read. They have been kept; no rates were saved.`)
      }
      const used = new Set<number>()
      plan.schedules = desired.schedules.map(schedule => {
        const { sourceIndex, ...values } = schedule
        if (sourceIndex == null) return values
        if (!Number.isInteger(sourceIndex) || sourceIndex < 0 || sourceIndex >= original.length || used.has(sourceIndex)) {
          throw new Error("The schedule list changed. Close and reopen the editor.")
        }
        used.add(sourceIndex)
        const seeded = prior.schedules.find(item => item.sourceIndex === sourceIndex)
        const originalValue = original[sourceIndex]
        if (same(seeded, schedule)) return originalValue
        const merged: RateDocument = isObject(originalValue) ? { ...originalValue } : {}
        for (const key of Object.keys(values) as Array<keyof typeof values>) {
          if (!same(seeded?.[key], values[key])) merged[key] = values[key]
        }
        return merged
      })
    }
    const extensions = Object.keys(plan).some(key => key !== "flat" && key !== "schedules")
    if (plan.flat == null && Array.isArray(plan.schedules) && !plan.schedules.length && !extensions) plans.delete(tag)
    else plans.set(tag, plan)
    changed = true
  }
  if (changed) result.charging_tag_rates = Object.fromEntries(plans)
  return result
}

export async function loadRates(fetcher: typeof fetch = fetch): Promise<RateDocument> {
  const response = await fetcher("/api/charging/rates", { cache: "no-store" })
  if (!response.ok) throw new Error("Rates could not be loaded. Try again.")
  return readRateDocument((await response.json()).document)
}
export async function saveRateChanges(document: RateDocument, next: ChargingRates, fetcher: typeof fetch = fetch): Promise<RateDocument> {
  const desired = applyRateChanges(document, next)
  const response = await fetcher("/api/charging/rates", {
    method: "PUT", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ expected: document, document: desired }),
  })
  if (!response.ok) {
    if (response.status === 409) throw new Error("Rates changed elsewhere. Close and reopen the editor before saving.")
    throw new Error("Rate save could not be confirmed. Close and reopen the editor to check.")
  }
  const confirmed = readRateDocument((await response.json()).document)
  if (!same(confirmed, desired)) throw new Error("Rate save could not be confirmed. Reopen the editor to check.")
  return confirmed
}

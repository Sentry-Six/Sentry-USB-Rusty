// Tire-pressure bands, always in PSI. TIRE_PSI_BANDS in sentryusb.conf holds
// four ascending values, "unsafe-low,optimal-low,optimal-high,unsafe-high";
// Sentry Connect for iOS reads and writes the same key.
export interface TireBands {
  unsafeLow: number
  optimalLow: number
  optimalHigh: number
  unsafeHigh: number
}

export const TIRE_BANDS_KEY = "TIRE_PSI_BANDS"

// Tesla's passenger cars: optimal 36–45 PSI, unsafe below 28 or above 50.
export const DEFAULT_TIRE_BANDS: TireBands = { unsafeLow: 28, optimalLow: 36, optimalHigh: 45, unsafeHigh: 50 }

// Anything outside this range is a typo, not a tire.
const MIN_PSI = 5
const MAX_PSI = 150

export function tireBandsValid(b: TireBands): boolean {
  const v = [b.unsafeLow, b.optimalLow, b.optimalHigh, b.unsafeHigh]
  return (
    v.every((n) => Number.isFinite(n) && n >= MIN_PSI && n <= MAX_PSI) &&
    v[0] < v[1] && v[1] < v[2] && v[2] < v[3]
  )
}

/** The saved bands, or the defaults when the key is unset or malformed. */
export function parseTireBands(raw: string | null | undefined): TireBands {
  const parts = (raw ?? "").split(",").map((s) => s.trim())
  if (parts.length !== 4 || parts.some((s) => s === "")) return DEFAULT_TIRE_BANDS
  const [unsafeLow, optimalLow, optimalHigh, unsafeHigh] = parts.map(Number)
  const bands = { unsafeLow, optimalLow, optimalHigh, unsafeHigh }
  return tireBandsValid(bands) ? bands : DEFAULT_TIRE_BANDS
}

const round1 = (n: number) => Math.round(n * 10) / 10

export function formatTireBands(b: TireBands): string {
  return [b.unsafeLow, b.optimalLow, b.optimalHigh, b.unsafeHigh].map((n) => String(round1(n))).join(",")
}

export function isDefaultTireBands(b: TireBands): boolean {
  return formatTireBands(b) === formatTireBands(DEFAULT_TIRE_BANDS)
}

export type TireBand = "optimal" | "check" | "unsafe"

export function classifyTirePsi(psi: number, b: TireBands): TireBand {
  if (psi < b.unsafeLow || psi > b.unsafeHigh) return "unsafe"
  if (psi < b.optimalLow || psi > b.optimalHigh) return "check"
  return "optimal"
}

/** Chart range in PSI: the lower unsafe band stays visible without unused range. */
export function tireChartDomainPsi(b: TireBands): [number, number] {
  return [Math.max(0, b.unsafeLow - 8), b.unsafeHigh + 5]
}

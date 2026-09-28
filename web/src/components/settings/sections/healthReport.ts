export type HealthStatus = "pass" | "warn" | "fail" | "unknown" | "recovering" | "not_applicable" | "info"
export interface HealthItem { name: string; status: HealthStatus; detail?: string; explanation?: string }
export interface HealthCategory { name: string; items: HealthItem[] }
export interface HealthReport { summary: string; categories: HealthCategory[] }

function percentageItem(report: HealthReport): HealthItem | undefined {
  const storage = report.categories.find(category => category.name === "Storage")
  if (!storage || storage.items.some(item => item.name === "Recording storage")) return undefined
  const item = storage.items.find(item => item.name === "Backingfiles free space" && (item.status === "warn" || item.status === "fail"))
  const match = item?.detail?.trim().match(/^(\d+(?:\.\d+)?)% free$/)
  if (!item || !match || Number(match[1]) <= 0 || Number(match[1]) > 100) return undefined
  if (storage.items.some(other => other !== item && (other.status === "warn" || other.status === "fail"))) return undefined
  return item
}
export const needsLegacyStorageProbe = (report: HealthReport) => !!percentageItem(report)

/** Percentage-only legacy warnings are informational only with independently measured reserve. */
export function normalizeLegacyStorage(report: HealthReport, probe: unknown): HealthReport {
  const item = percentageItem(report)
  if (!item || !probe || typeof probe !== "object") return report
  const free = probe as Record<string, unknown>
  const total = free.total_bytes
  const available = free.available_bytes
  if (free.mounted !== true || typeof total !== "number" || typeof available !== "number" ||
    !Number.isFinite(total) || !Number.isFinite(available) || total <= 0 || available < 0 || available > total ||
    available < 10 * 1024 ** 3 + total / 33) return report
  const categories = report.categories.map(category => ({ ...category, items: category.items.map(entry => entry !== item ? entry : {
    ...entry,
    status: "info" as const,
    explanation: `Original device check: ${entry.status} — ${entry.detail}. Older device software checks the percentage alone. A separate space reading shows the managed reserve is available. Percentage usage alone does not indicate a fault. This information does not prove that the filesystem is writable or that recording is healthy.`,
  }) }))
  const items = categories.flatMap(category => category.items)
  const issues = items.filter(entry => entry.status === "warn" || entry.status === "fail").length
  const incomplete = items.some(entry => entry.status === "unknown")
  const summary = issues ? `${issues} ${issues === 1 ? "issue needs" : "issues need"} attention` : incomplete ? "No actionable issues reported; some checks unavailable" : "No actionable issues reported"
  return { ...report, summary, categories }
}

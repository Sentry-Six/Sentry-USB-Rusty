import { computeFilteredStats, type DrivesFilteredStats } from "@/lib/drive-stats"
import type { DriveDetail, DriveSummary, RouteOverview } from "@/types/drives"

type Backend = "unknown" | "legacy" | "paged"
let backend: Backend = "unknown"
let additiveTags = false
let legacySnapshot: { drives: DriveSummary[]; at: number; revision: string } | null = null
let legacyPreviews: { routes: RouteOverview[]; at: number } | null = null
const CACHE_MS = 30_000

class DriveApiError extends Error {
  status: number
  unsupported: boolean
  constructor(message: string, status: number, unsupported = false) {
    super(message)
    this.status = status
    this.unsupported = unsupported
  }
}

async function responseJson(response: Response, label: string): Promise<unknown> {
  const contentType = response.headers.get("content-type") ?? ""
  const isJson = /(?:application\/json|[\w.+-]+\/[\w.+-]+\+json)(?:\s*;|$)/i.test(contentType)
  if (!response.ok) throw new DriveApiError(`${label}: HTTP ${response.status}`, response.status, [400,404,405].includes(response.status))
  if (!isJson) throw new DriveApiError(`${label}: the device returned a web page instead of API data.`, response.status, true)
  try { return await response.json() }
  catch { throw new DriveApiError(`${label}: invalid JSON response`, response.status) }
}
function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value)
}
function finite(value: unknown): value is number { return typeof value === "number" && Number.isFinite(value) }
function normalizeDrive(value: unknown): DriveSummary {
  if (!record(value) || !Number.isInteger(value.id) || typeof value.startTime !== "string" || !Number.isFinite(Date.parse(value.startTime))
    || typeof value.endTime !== "string" || !Number.isFinite(Date.parse(value.endTime))
    || !finite(value.distanceMi) || !finite(value.distanceKm) || !finite(value.durationMs)
    || (value.tags !== undefined && (!Array.isArray(value.tags) || !value.tags.every((tag) => typeof tag === "string")))) {
    throw new Error("Drives: invalid drive summary from the device")
  }
  const normalized = { ...value }
  for (const key of ["fsdEngagedMs", "fsdDistanceMi", "fsdDistanceKm", "fsdPercent", "fsdDisengagements", "autosteerEngagedMs", "autosteerDistanceKm", "taccEngagedMs", "taccDistanceKm"]) {
    if (normalized[key] == null) normalized[key] = 0
    else if (!finite(normalized[key])) throw new Error(`Drives: invalid ${key} in summary`)
  }
  return normalized as unknown as DriveSummary
}
function summaries(value: unknown): DriveSummary[] {
  if (!Array.isArray(value)) throw new Error("Drives: expected a history list from the device")
  return value.map(normalizeDrive)
}
function fingerprint(value: unknown): string {
  let hash = 2166136261
  for (const char of JSON.stringify(value)) hash = Math.imul(hash ^ char.charCodeAt(0), 16777619)
  return `legacy-${(hash >>> 0).toString(16)}`
}
function rememberLegacy(drives: DriveSummary[]) {
  backend = "legacy"
  additiveTags = false
  legacySnapshot = { drives, at: Date.now(), revision: fingerprint(drives) }
}
export function invalidateDriveApiCache() {
  legacySnapshot = null
  legacyPreviews = null
}

export async function fetchDrives(signal?: AbortSignal): Promise<DriveSummary[]> {
  const response = await fetch("/api/drives", { signal })
  const drives = summaries(await responseJson(response, "Drives"))
  if (signal?.aborted) throw new DOMException("Aborted", "AbortError")
  if (backend === "legacy") rememberLegacy(drives)
  return drives
}

export interface DrivePage { revision: string; drives: DriveSummary[]; total: number; page: number; limit: number; tags: string[]; stats: DrivesFilteredStats }
function legacyPage(drives: DriveSummary[], query: string, revision: string): DrivePage {
  const params = new URLSearchParams(query)
  const from = params.get("from"), to = params.get("to"), tag = params.get("tag")
  const minimum = params.has("min_distance") ? Number(params.get("min_distance")) : undefined
  const matching = drives.filter((drive) => (!from || drive.startTime >= from) && (!to || drive.startTime < to)
    && (!tag || drive.tags?.includes(tag)) && (minimum === undefined || drive.distanceMi >= minimum))
    .sort((a,b) => (params.get("sort") === "asc" ? 1 : -1) * a.startTime.localeCompare(b.startTime))
  const limit = Math.max(1, Math.min(100, Math.floor(Number(params.get("limit")) || 10)))
  const page = Math.max(1, Math.min(Math.ceil(matching.length / limit) || 1, Math.floor(Number(params.get("page")) || 1)))
  return { revision, drives: matching.slice((page-1)*limit, page*limit), total: matching.length, page, limit,
    tags: [...new Set(drives.flatMap((drive) => drive.tags ?? []))].sort(), stats: computeFilteredStats(matching) }
}
function paged(value: unknown): DrivePage {
  if (!record(value) || !Array.isArray(value.drives) || !Number.isInteger(value.total) || Number(value.total) < 0
    || !Number.isInteger(value.page) || Number(value.page) < 1 || !Number.isInteger(value.limit) || Number(value.limit) < 1
    || typeof value.revision !== "string" || !value.revision || !Array.isArray(value.tags) || !value.tags.every((tag) => typeof tag === "string")
    || !record(value.stats) || !Object.keys(computeFilteredStats([])).every((key) => finite((value.stats as Record<string, unknown>)[key]))) {
    throw new Error("Drives: invalid paginated response from the device")
  }
  const drives = summaries(value.drives)
  if (drives.length > Number(value.limit) || drives.length > Number(value.total) || value.stats.count !== value.total) throw new Error("Drives: inconsistent page totals")
  backend = "paged"
  additiveTags = record(value.capabilities) && value.capabilities.additiveTags === true
  return { ...value, drives } as unknown as DrivePage
}

export async function fetchDrivePage(query: string, signal?: AbortSignal): Promise<DrivePage> {
  if (backend === "legacy" && legacySnapshot && Date.now() - legacySnapshot.at < CACHE_MS) {
    return legacyPage(legacySnapshot.drives, query, legacySnapshot.revision)
  }
  let data: unknown
  try {
    data = await responseJson(await fetch(`/api/drives?${query}`, { signal }), "Drives")
  } catch (error) {
    if (!(error instanceof DriveApiError) || !error.unsupported || signal?.aborted) throw error
    data = await responseJson(await fetch("/api/drives", { signal }), "Drives")
  }
  if (signal?.aborted) throw new DOMException("Aborted", "AbortError")
  if (Array.isArray(data)) {
    const drives = summaries(data)
    rememberLegacy(drives)
    return legacyPage(drives, query, legacySnapshot!.revision)
  }
  return paged(data)
}

function uniqueDrive(drives: DriveSummary[], id: string | number): DriveSummary | undefined {
  const matching = drives.filter((drive) => String(drive.id) === String(id) || drive.startTime === String(id))
  if (matching.length > 1) throw new Error("Drive identity is ambiguous. Refresh history before continuing.")
  return matching[0]
}
function detail(value: unknown, expected?: string): DriveDetail {
  if (!record(value) || !Array.isArray(value.points) || typeof value.startTime !== "string" || !finite(value.id)
    || !value.points.every((point) => Array.isArray(point) && point.length >= 2 && finite(point[0]) && finite(point[1]))) throw new Error("Drive detail: invalid response from the device")
  if (expected && value.startTime !== expected) throw new Error("The drive changed while loading. Refresh history and try again.")
  return value as unknown as DriveDetail
}
const telemetryFields = ["batteryPctStart", "batteryPctEnd", "batteryPctUsed", "interiorTempMinC", "interiorTempMaxC", "exteriorTempAvgC", "hvacRuntimeS", "tireFlPsi", "tireFrPsi", "tireRlPsi", "tireRrPsi", "odometerMiStart", "odometerMiEnd", "odometerMiDriven", "startLocation", "endLocation"] as const
export async function fetchDriveDetail(id: string | number, signal?: AbortSignal): Promise<DriveDetail> {
  if (backend === "unknown") {
    try { await fetchDrivePage("limit=1&page=1", signal) }
    catch (error) { if (signal?.aborted) throw error }
  }
  const expected = /^\d+$/.test(String(id)) ? undefined : String(id)
  let result: DriveDetail
  let summary = legacySnapshot && uniqueDrive(legacySnapshot.drives, id)
  try {
    result = detail(await responseJson(await fetch(`/api/drives/${encodeURIComponent(id)}`, { signal }), "Drive detail"), expected)
  } catch (error) {
    if (!(error instanceof DriveApiError) || !error.unsupported || !expected || signal?.aborted) throw error
    const fresh = await fetchDrives(signal)
    summary = uniqueDrive(fresh, id)
    if (!summary) throw new Error("This drive is no longer available. Refresh history.", { cause: error })
    result = detail(await responseJson(await fetch(`/api/drives/${summary.id}`, { signal }), "Drive detail"), expected)
  }
  if (backend === "legacy") {
    if (!summary) summary = uniqueDrive(await fetchDrives(signal), id)
    if (summary?.startTime === result.startTime) {
      for (const key of telemetryFields) if (result[key] === undefined && summary[key] !== undefined) Object.assign(result, { [key]: summary[key] })
    }
  }
  return result
}
function routeOverviews(value: unknown): RouteOverview[] {
  if (!Array.isArray(value) || !value.every((route) => record(route) && typeof route.startTime === "string"
    && Array.isArray(route.points) && route.points.every((point) => Array.isArray(point) && finite(point[0]) && finite(point[1])))) throw new Error("Route previews: invalid response from the device")
  return value as RouteOverview[]
}
export async function fetchRouteOverviews(maxPoints = 20): Promise<RouteOverview[]> {
  return routeOverviews(await responseJson(await fetch(`/api/drives/routes?max_points=${maxPoints}`), "Route previews"))
}
export async function fetchVisibleRoutePreviews(starts: string[], signal?: AbortSignal): Promise<RouteOverview[]> {
  const requested = new Set(starts)
  if (backend === "legacy" && legacyPreviews && Date.now() - legacyPreviews.at < CACHE_MS) return legacyPreviews.routes.filter((route) => requested.has(route.startTime))
  const query = new URLSearchParams({ starts: starts.join(","), max_points: "20" })
  const response = await fetch(`/api/drives/routes?${query}`, { signal })
  const routes = routeOverviews(await responseJson(response, "Route previews"))
  if (backend === "legacy" && !signal?.aborted) legacyPreviews = { routes, at: Date.now() }
  return routes.filter((route) => requested.has(route.startTime))
}

export async function setDriveTags(id: string | number, tags: string[], add = false): Promise<void> {
  let nextTags = tags
  if (add && !additiveTags) {
    // Older Pis ignore `add`; send an authoritative union instead of replacing
    // every existing tag with the single new bulk tag.
    const current = uniqueDrive(await fetchDrives(), id)
    if (!current) throw new Error("This drive is no longer available. Refresh history before tagging.")
    nextTags = [...new Set([...(current.tags ?? []), ...tags])]
  }
  const response = await fetch(`/api/drives/${encodeURIComponent(id)}/tags`, {
    method: "PUT", headers: { "Content-Type": "application/json" },
    body: JSON.stringify(add && additiveTags ? { tags, add: true } : { tags: nextTags }),
  })
  const saved = await responseJson(response, "Save tags")
  if (!record(saved) || (saved.success !== true && saved.ok !== true)) throw new Error("Tag save could not be confirmed. Refresh history before trying again.")
  invalidateDriveApiCache()
}

export async function triggerProcessNew(): Promise<void> {
  const res = await fetch("/api/drives/process", { method: "POST" })
  if (!res.ok) {
    const body = await res.json().catch(() => ({}))
    throw new Error(body.error || `process: ${res.status}`)
  }
}

export async function triggerReprocessAll(): Promise<void> {
  const res = await fetch("/api/drives/reprocess", { method: "POST" })
  if (!res.ok) {
    const body = await res.json().catch(() => ({}))
    throw new Error(body.error || `reprocess: ${res.status}`)
  }
}

/** Targeted summon evidence re-read (backend runs it async — poll the
 *  drives list afterwards; summon pills appear once the scan finishes). */
export async function triggerSummonCheck(): Promise<void> {
  const res = await fetch("/api/drives/check-summon", { method: "POST" })
  if (!res.ok) {
    const body = await res.json().catch(() => ({}))
    throw new Error(body.error || `check-summon: ${res.status}`)
  }
}

export async function uploadDriveData(file: File): Promise<{ imported: number }> {
  const res = await fetch("/api/drives/data/upload", {
    method: "POST",
    body: file,
  })
  if (!res.ok) {
    const body = await res.json().catch(() => ({}))
    throw new Error(body.error || `upload: ${res.status}`)
  }
  return res.json()
}

export async function deleteAllDrives(): Promise<void> {
  const res = await fetch("/api/drives/data", { method: "DELETE" })
  if (!res.ok) {
    const body = await res.json().catch(() => ({}))
    throw new Error(body.error || `delete: ${res.status}`)
  }
}

export interface BulkDeleteResult {
  /** Number of underlying clip rows removed from the `routes` table. */
  deleted: number
  /** Number of drives that were resolved + deleted (excludes not_found). */
  drives: number
  /** Drive ids the backend could not resolve back to clip files. */
  not_found: string[]
}

export async function bulkDeleteDrives(ids: Array<string | number>): Promise<BulkDeleteResult> {
  const response = await fetch("/api/drives/bulk-delete", {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ ids: ids.map(String) }),
  })
  const value = await responseJson(response, "Delete drives")
  if (!record(value) || !finite(value.deleted) || !finite(value.drives)
    || !Array.isArray(value.not_found) || !value.not_found.every((id) => typeof id === "string")) {
    throw new Error("Delete result could not be confirmed. Refresh history before trying again.")
  }
  invalidateDriveApiCache()
  return value as unknown as BulkDeleteResult
}

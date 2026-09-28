import { useEffect, useMemo, useState } from "react"
import { useSearchParams } from "react-router-dom"
import type { DriveSummary, RouteOverview } from "@/types/drives"
import { fetchDrivePage, fetchVisibleRoutePreviews, invalidateDriveApiCache, type DrivePage } from "@/api/drives"
import { computeFilteredStats, type DrivesFilteredStats } from "@/lib/drive-stats"
import { rangeBounds, type DateRange, type DatePreset } from "@/lib/date-range"
export { rangeBounds } from "@/lib/date-range"
export type { DateRange, DatePreset } from "@/lib/date-range"
export type { DrivesFilteredStats }

const PAGE_SIZE = 10
const pages = new Map<string, { value: DrivePage; at: number }>()
const previews = new Map<string, RouteOverview>()
export interface DrivesFilters { tag?: string; minDistanceMi?: number }

function readRange(params: URLSearchParams): DateRange {
  const start = params.get("start"), end = params.get("end")
  if (start && end) return { kind: "custom", start, end }
  return { kind: "preset", preset: (params.get("range") as DatePreset) ?? "last7" }
}
function localIso(date: Date) {
  const pad = (value: number) => String(value).padStart(2, "0")
  return `${date.getFullYear()}-${pad(date.getMonth()+1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
}

export function useDrivesList() {
  const [params, setParams] = useSearchParams()
  const range = useMemo(() => readRange(params), [params])
  const filters = useMemo<DrivesFilters>(() => {
    const min = Number(params.get("minDist"))
    return { tag: params.get("tag") || undefined, minDistanceMi: params.has("minDist") && Number.isFinite(min) ? min : undefined }
  }, [params])
  const sortDir = params.get("sort") === "asc" ? "asc" : "desc"
  const requestedPage = Math.max(1, Math.floor(Number(params.get("page")) || 1))
  const bounds = rangeBounds(range, new Date())
  const query = new URLSearchParams({ limit: String(PAGE_SIZE), page: String(requestedPage), sort: sortDir })
  if (bounds.from) query.set("from", localIso(bounds.from))
  if (bounds.to) query.set("to", localIso(bounds.to))
  if (filters.tag) query.set("tag", filters.tag)
  if (filters.minDistanceMi !== undefined) query.set("min_distance", String(filters.minDistanceMi))
  const key = query.toString()
  const [loaded, setLoaded] = useState<{ key: string; value: DrivePage } | null>(() => pages.has(key) ? { key, value: pages.get(key)!.value } : null)
  const [failure, setFailure] = useState<{ key: string; refresh: number; message: string } | null>(null)
  const [refreshTick, setRefreshTick] = useState(0)
  const [, setRouteVersion] = useState(0)
  if (failure && (failure.key !== key || failure.refresh !== refreshTick)) setFailure(null)
  const value = loaded?.key === key ? loaded.value : pages.get(key)?.value
  const error = failure?.key === key && failure.refresh === refreshTick ? failure.message : null
  const loading = !value && !error
  const drives = value?.drives ?? []

  useEffect(() => {
    const cached = pages.get(key)
    if (cached && Date.now() - cached.at < 30_000 && refreshTick === 0) return
    const controller = new AbortController()
    fetchDrivePage(key, controller.signal).then((result) => {
      if (controller.signal.aborted) return
      pages.set(key, { value: result, at: Date.now() })
      if (pages.size > 20) pages.delete(pages.keys().next().value!)
      for (const drive of result.drives) previews.delete(drive.startTime)
      setLoaded({ key, value: result })
      setFailure(null)
    }).catch((reason) => { if (!controller.signal.aborted) setFailure({ key, refresh: refreshTick, message: reason instanceof Error ? reason.message : String(reason) }) })
    return () => controller.abort()
  }, [key, refreshTick])

  const visibleKey = drives.map((drive) => drive.startTime).join(",")
  useEffect(() => {
    if (!visibleKey) return
    const missing = visibleKey.split(",").filter((start) => !previews.has(start))
    if (!missing.length) return
    const controller = new AbortController()
    fetchVisibleRoutePreviews(missing, controller.signal).then((routes) => {
      if (controller.signal.aborted) return
      for (const route of routes) previews.set(route.startTime, route)
      while (previews.size > 200) previews.delete(previews.keys().next().value!)
      setRouteVersion((version) => version + 1)
    }).catch(() => { /* Preview failures do not prevent opening a drive. */ })
    return () => controller.abort()
  }, [visibleKey, value])
  const routesByStartTime = new Map([...previews].map(([start, route]) => [start, route.points]))
  const total = value?.total ?? 0
  const page = value?.page ?? requestedPage
  const pageCount = Math.max(1, Math.ceil(total/PAGE_SIZE))
  const update = (change: (next: URLSearchParams) => void) => {
    const next = new URLSearchParams(params); change(next); setParams(next, { replace: true })
  }
  const refresh = async () => { pages.clear(); previews.clear(); invalidateDriveApiCache(); setRefreshTick((tick) => tick + 1) }
  const patchDriveTags = (id: number, tags: string[]) => {
    const start = drives.find((drive) => drive.id === id)?.startTime
    if (!start) return
    for (const cached of pages.values()) cached.value = { ...cached.value, drives: cached.value.drives.map((drive) => drive.startTime === start ? { ...drive, tags } : drive) }
    setLoaded((previous) => previous ? { ...previous, value: { ...previous.value, drives: previous.value.drives.map((drive) => drive.startTime === start ? { ...drive, tags } : drive) } } : null)
  }
  // Fetch all matching summaries only for an explicit bulk action, in bounded pages.
  const fetchMatching = async (): Promise<DriveSummary[]> => {
    const request = new URLSearchParams(key); request.set("limit", "100")
    const all: DriveSummary[] = []
    let revision: string | undefined
    for (let page = 1; ; page++) {
      request.set("page", String(page))
      const result = await fetchDrivePage(request.toString())
      if (revision !== undefined && result.revision !== revision) throw new Error("Drive history changed while selecting. Refresh and try again.")
      revision = result.revision
      all.push(...result.drives)
      if (all.length >= result.total || !result.drives.length) return all
    }
  }
  return {
    drives, visible: drives, total, page, pageCount, pageStart: total ? (page-1)*PAGE_SIZE+1 : 0,
    pageEnd: Math.min(total,page*PAGE_SIZE), range, filters, sortDir, loading, error,
    filteredStats: value?.stats ?? computeFilteredStats([]), tags: value?.tags ?? [], routesByStartTime,
    setPage: (page: number) => update((next) => next.set("page", String(Math.max(1,Math.min(pageCount,page))))),
    setRange: (range: DateRange) => update((next) => {
      for (const name of ["page","start","end","range"]) next.delete(name)
      if (range.kind === "custom") { next.set("start",range.start); next.set("end",range.end) }
      else if (range.preset !== "last7") next.set("range",range.preset)
    }),
    setFilters: (filters: DrivesFilters) => update((next) => {
      for (const name of ["page","tag","minDist","origin","destination"]) next.delete(name)
      if (filters.tag) next.set("tag",filters.tag)
      if (filters.minDistanceMi !== undefined) next.set("minDist",String(filters.minDistanceMi))
    }),
    setSortDir: (sort: "asc" | "desc") => update((next) => { next.delete("page"); next.set("sort",sort) }),
    refresh, patchDriveTags, fetchMatching,
  }
}

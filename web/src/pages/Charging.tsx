import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useState } from "react"
import { useNavigate } from "react-router-dom"
import {
  BatteryAndroidFrameBoltIcon,
  BoltIcon,
  CheckBoxIcon,
  ChevronRightIcon,
  DeleteIcon,
  HomeIcon,
  LocationOnIcon,
  ProgressActivityIcon,
} from "@/components/icons"
import {
  bulkDeleteCharges,
  setChargeTags,
} from "@/api/charging"
import type { ChargeSessionSummary } from "@/types/charging"
import { useChargingHistory } from "@/hooks/useChargingHistory"
import { summarizeCharges } from "@/lib/charging-stats"
import { Pagination } from "@/components/drives/Pagination"
import { cn } from "@/lib/utils"
import { DatePopover } from "@/components/drives/DatePopover"
import { TagPopover } from "@/components/drives/TagPopover"
import { HomeLocationSection } from "@/components/charging/HomeLocationSection"
import {
  ChargingSummaryStrip,
} from "@/components/charging/ChargingSummaryStrip"
import { ChargingTagFilter } from "@/components/charging/ChargingTagFilter"
import { ChargingRatesButton } from "@/components/charging/ChargingRatesButton"
import { MiniPinMap } from "@/components/charging/MiniPinMap"
import { rangeBounds, type DateRange } from "@/hooks/useDrivesList"
import { useDistanceUnit } from "@/hooks/useDistanceUnit"
import { fmtDuration, fmtEnergy, fmtMoney, fmtSoc } from "@/lib/charge-format"

const PAGE_SIZE = 25
let viewState: { range: DateRange; selectedTags: string[]; query: string; page: number; scroll: number } = {
  range: { kind: "preset", preset: "all" }, selectedTags: [], query: "", page: 1, scroll: 0,
}

export default function Charging() {
  const [homeSeed, setHomeSeed] = useState<{ lat: number | null; lon: number | null } | null>(null)
  // Whether a home geofence exists at all. Without one NOTHING can be tagged
  // Home, so the answer is a single prompt — not a "set as home" button on
  // every row, which is what per-session affordances degrade into when the
  // feature is entirely unconfigured.
  const [homeConfigured, setHomeConfigured] = useState<boolean | null>(null)
  // Escape closes the editor. Nothing is written until the user confirms, so
  // backing out is always safe and should not need a mouse.
  useEffect(() => {
    if (!homeSeed) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setHomeSeed(null)
    }
    document.addEventListener("keydown", onKey)
    return () => document.removeEventListener("keydown", onKey)
  }, [homeSeed])
  useEffect(() => {
    let alive = true
    fetch("/api/system/keep-accessory-config")
      .then((r) => (r.ok ? r.json() : null))
      .then((d) => {
        if (alive) setHomeConfigured(d ? d.home_lat != null && d.home_lon != null : null)
      })
      .catch(() => {})
    return () => {
      alive = false
    }
  }, [])
  const { sessions, tags, current, loading, error, warning, reload, patchTags } = useChargingHistory()
  const metric = useDistanceUnit()
  const [range, updateRange] = useState<DateRange>(() => viewState.range)
  const [selectedTags, updateTags] = useState<string[]>(() => viewState.selectedTags)
  const [query, updateQuery] = useState(() => viewState.query)
  const [page, setPage] = useState(() => viewState.page)
  const setRange = (next: DateRange) => { updateRange(next); setPage(1); setSelected(new Set()) }
  const setSelectedTags = (next: string[]) => { updateTags(next); setPage(1); setSelected(new Set()) }
  useEffect(() => { viewState = { ...viewState, range, selectedTags, query, page } }, [range, selectedTags, query, page])
  useLayoutEffect(() => {
    const position = viewState.scroll
    window.scrollTo(0, position)
    return () => { viewState.scroll = window.scrollY }
  }, [])

  const [selectMode, setSelectMode] = useState(false)
  const [selected, setSelected] = useState<Set<number>>(new Set())
  const [confirmingBulkDelete, setConfirmingBulkDelete] = useState<{
    ids: number[]
  } | null>(null)
  const [deletingBulk, setDeletingBulk] = useState(false)
  const [bulkDeleteError, setBulkDeleteError] = useState<string | null>(null)

  const onTagsChange = useCallback(
    async (id: number, next: string[]) => {
      // Tagging a charge "Home" is the user saying "this is home" — so treat it
      // as a request to MOVE the geofence, not a tag write. The tag is derived
      // and the store strips it anyway, so storing it would silently do
      // nothing; this turns a dead action into the one they meant. The change
      // still has to be confirmed in the dialog, because it re-tags history.
      const isHome = (t: string) => t.trim().toLowerCase() === "home"
      const sess = sessions.find((x) => x.id === id)
      // "Home" is never in a session's stored tags (it is derived into `atHome`,
      // and the store strips it), so its presence here means the user just typed
      // or picked it. Unrelated edits carry it in neither list and never open the
      // dialog. Still filtered out below rather than trusted to that.
      const asksHome = next.some(isHome) && !(sess?.tags ?? []).some(isHome)
      if (asksHome && sess?.locationLat != null && sess?.locationLon != null) {
        setHomeSeed({ lat: sess.locationLat, lon: sess.locationLon })
      }
      // Strip it either way: it is derived, and the store discards it on write.
      next = next.filter((t) => !isHome(t))
      // Optimistic: show the new tags immediately, then resync (cost is
      // recomputed server-side from the tags).
      patchTags(id, next)
      const previous = sess?.tags ?? []
      try {
        await setChargeTags(id, next)
      } catch (error) {
        patchTags(id, previous)
        throw error
      }
      await reload()
    },
    [reload, sessions, patchTags],
  )

  const toggleSelectMode = () => {
    setSelectMode((s) => {
      if (s) setSelected(new Set())
      return !s
    })
  }

  const onToggleSelected = (id: number) => {
    setSelected((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  // The newest session is the in-progress one while the car reports
  // charging (sessions come back newest-first).
  const activeId = current?.charging ? (sessions[0]?.id ?? null) : null

  const visible = useMemo(() => {
    const { from, to } = rangeBounds(range, new Date())
    return sessions.filter((s) => {
      const search = query.trim().toLocaleLowerCase()
      if (search && !`${s.location ?? ""} ${s.tags.join(" ")} ${new Date(s.startMs).toLocaleDateString()}`.toLocaleLowerCase().includes(search)) return false
      const t = new Date(s.startMs)
      if (from && t < from) return false
      if (to && t >= to) return false
      if (selectedTags.length > 0) {
        // "Home" and "Fast charging" are derived filters (not stored tags) —
        // match them via atHome / fastCharging; everything else is a real tag.
        const sel = selectedTags.map((t) => t.toLowerCase())
        const tagMatch =
          s.tags.some((tag) => selectedTags.includes(tag)) ||
          (s.atHome && sel.includes("home")) ||
          (s.fastCharging && sel.includes("fast charging"))
        if (!tagMatch) return false
      }
      return true
    })
  }, [sessions, range, selectedTags, query])

  // Tags-dropdown options = real tags (incl. the API-surfaced "Home") plus the
  // derived "Fast charging" filter when any session qualifies — so it lives in
  // the Tags dropdown alongside Home, not as a separate pill.
  const filterTags = useMemo(() => {
    if (
      sessions.some((s) => s.fastCharging) &&
      !tags.some((t) => t.toLowerCase() === "fast charging")
    ) {
      return [...tags, "Fast charging"]
    }
    return tags
  }, [tags, sessions])

  const onSelectAll = () => {
    setSelected(new Set(visible.map((s) => s.id)))
  }

  const onDeleteSelected = useCallback(() => {
    if (selected.size === 0) return
    setBulkDeleteError(null)
    setConfirmingBulkDelete({ ids: Array.from(selected) })
  }, [selected])

  const confirmBulkDelete = async () => {
    if (!confirmingBulkDelete) return
    setDeletingBulk(true)
    setBulkDeleteError(null)
    try {
      await bulkDeleteCharges(confirmingBulkDelete.ids)
      setConfirmingBulkDelete(null)
      setSelected(new Set())
      setSelectMode(false)
      await reload()
    } catch (e) {
      setBulkDeleteError(e instanceof Error ? e.message : String(e))
    } finally {
      setDeletingBulk(false)
    }
  }

  const stats = useMemo(() => summarizeCharges(visible), [visible])
  const pageCount = Math.max(1, Math.ceil(visible.length / PAGE_SIZE))
  const safePage = Math.min(page, pageCount)
  const paged = visible.slice((safePage - 1) * PAGE_SIZE, safePage * PAGE_SIZE)
  const pagination = <Pagination page={safePage} pageCount={pageCount}
    pageStart={visible.length ? (safePage - 1) * PAGE_SIZE + 1 : 0}
    pageEnd={Math.min(safePage * PAGE_SIZE, visible.length)} total={visible.length} onChange={setPage} />

  return (
    <div className="mx-auto w-full max-w-6xl px-4 py-6 sm:px-6 sm:py-8">
      <div className="mb-4 flex flex-wrap items-center justify-between gap-3 sm:mb-6">
        <h1 className="text-2xl font-semibold text-slate-100 sm:text-3xl">
          Charging
        </h1>
        <button type="button" onClick={() => void reload()} className="rounded-full border border-white/10 px-3 py-1.5 text-sm text-slate-300">Refresh</button>
      </div>
      <div className="glass-card mb-5 p-4"><ChargingSummaryStrip stats={stats} loading={loading} /></div>

      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <DatePopover range={range} onChange={setRange} />
        <ChargingTagFilter
          tags={filterTags}
          selected={selectedTags}
          onChange={setSelectedTags}
        />
        <ChargingRatesButton tags={tags} onSaved={() => reload()} />
        <input type="search" aria-label="Search charging history" placeholder="Search location, tag or date"
          value={query} onChange={(event) => { updateQuery(event.target.value); setPage(1); setSelected(new Set()) }}
          className="order-first w-full rounded-full border border-white/10 bg-white/[.04] px-4 py-2 text-sm sm:w-64" />
        <div className="ml-auto flex flex-wrap items-center gap-2">
          {selectMode ? (
            <ChargingSelectBar
              selectedCount={selected.size}
              totalCount={visible.length}
              onSelectAll={onSelectAll}
              onDelete={onDeleteSelected}
              onCancel={toggleSelectMode}
            />
          ) : (
            <button
              type="button"
              onClick={toggleSelectMode}
              className="inline-flex items-center gap-2 rounded-full border border-white/10 bg-white/[0.03] px-3.5 py-1.5 text-sm font-medium text-slate-200 transition-colors hover:bg-white/[0.06]"
            >
              <CheckBoxIcon className="h-4 w-4" />
              Select
            </button>
          )}
        </div>
      </div>

      {homeConfigured === false && sessions.some((x) => x.locationLat != null) && (
        <div className="mb-3 flex flex-wrap items-center gap-3 rounded-lg border border-white/10 bg-white/[0.02] px-3 py-2.5">
          <HomeIcon className="h-4 w-4 shrink-0 text-slate-400" />
          <p className="min-w-0 flex-1 text-xs text-slate-400">
            No home location set, so none of these charges can be tagged{" "}
            <span className="text-slate-300">Home</span> or priced with a home rate.
          </p>
          <button
            type="button"
            onClick={() => {
              // Seed from the most recent charge that has coordinates — for most
              // people that is home, and it beats opening a world map. They can
              // drag the pin if it guessed wrong.
              const withFix = sessions.find((x) => x.locationLat != null && x.locationLon != null)
              setHomeSeed({
                lat: withFix?.locationLat ?? null,
                lon: withFix?.locationLon ?? null,
              })
            }}
            className="shrink-0 rounded-md border border-white/10 bg-white/[0.04] px-3 py-1.5 text-xs font-medium text-slate-200 hover:bg-white/[0.08]"
          >
            Set home location
          </button>
        </div>
      )}

      {(query || selectedTags.length > 0 || range.kind === "custom" || range.preset !== "all") && (
        <div className="mt-3 flex flex-wrap items-center gap-2 text-xs text-slate-400">
          {selectedTags.map((tag) => <button key={tag} type="button" onClick={() => setSelectedTags(selectedTags.filter((value) => value !== tag))} className="rounded-full border border-white/10 px-3 py-1">{tag} ×</button>)}
          <button type="button" onClick={() => { setRange({ kind: "preset", preset: "all" }); setSelectedTags([]); updateQuery("") }}>Clear filters</button>
        </div>
      )}
      {warning && <p role="status" className="mt-3 text-sm text-amber-200">{warning} <button type="button" onClick={() => void reload()} className="underline">Retry</button></p>}
      <div className="mt-4">{pagination}</div>
      <div className="mt-3 flex flex-col gap-3">
        {loading && (
          <div className="glass-card flex items-center justify-center gap-2 p-10 text-sm text-slate-400">
            <ProgressActivityIcon className="h-4 w-4 animate-spin" />
            Loading charging history…
          </div>
        )}
        {error && !loading && (
          <div className="rounded-2xl border border-rose-400/30 bg-rose-500/5 p-6 text-sm text-rose-200">
            {sessions.length ? "Charging history could not refresh." : "Charging history could not load."} <button type="button" onClick={() => void reload()} className="underline">Try again</button>
          </div>
        )}
        {!loading && !error && visible.length === 0 && (
          <div className="glass-card p-10 text-center text-sm text-slate-400">
            <BatteryAndroidFrameBoltIcon className="mx-auto mb-3 h-8 w-8 text-slate-600" />
            {sessions.length === 0
              ? "No charging sessions recorded yet. Sessions appear here once the car charges while the Pi is sampling."
              : "No charging sessions match these filters."}
          </div>
        )}
        {!loading &&
          paged.map((s) => (
            <ChargeRow
              key={s.id}
              session={s}
              metric={metric}
              active={s.id === activeId}
              livePowerKw={s.id === activeId ? (current?.powerKw ?? null) : null}
              selectMode={selectMode}
              selected={selected.has(s.id)}
              onToggleSelected={onToggleSelected}
              onTagsChange={onTagsChange}
              onOpenHomeLocation={setHomeSeed}
            />
          ))}
      </div>

      {visible.length > PAGE_SIZE && <div className="mt-4">{pagination}</div>}
      {homeSeed && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4"
          onClick={() => setHomeSeed(null)}
        >
          <div
            role="dialog"
            aria-modal="true"
            aria-labelledby="home-location-title"
            className="max-h-[85vh] w-full max-w-md overflow-y-auto rounded-xl border border-white/10 bg-slate-950 p-4"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="mb-3 flex items-center justify-between">
              <div>
                <h2 id="home-location-title" className="text-sm font-medium text-slate-200">
                  Home location
                </h2>
              </div>
              <button
                type="button"
                onClick={() => setHomeSeed(null)}
                className="rounded-md border border-white/10 bg-white/[0.04] px-3 py-1 text-xs font-medium text-slate-200 hover:bg-white/[0.08]"
              >
                Done
              </button>
            </div>
            <HomeLocationSection
              onSaved={() => {
                setHomeConfigured(true) // kill the banner without a page reload
                reload()
              }}
              onDone={() => setHomeSeed(null)}
              seedLat={homeSeed.lat}
              seedLon={homeSeed.lon}
            />
          </div>
        </div>
      )}

      {confirmingBulkDelete && (
        <div className="fixed inset-0 z-[2000] flex items-center justify-center bg-black/60 backdrop-blur-sm">
          <div className="w-full max-w-sm rounded-2xl border border-white/10 bg-slate-950 p-6 shadow-2xl">
            <h3 className="text-base font-semibold text-slate-100">
              {confirmingBulkDelete.ids.length === 1
                ? "Delete 1 charge?"
                : `Delete ${confirmingBulkDelete.ids.length} charges?`}
            </h3>
            <p className="mt-2 text-xs leading-relaxed text-slate-400">
              This removes the selected charge session
              {confirmingBulkDelete.ids.length === 1 ? "" : "s"} and their
              telemetry samples from the database. The action cannot be undone.
            </p>
            {bulkDeleteError && (
              <p className="mt-3 text-xs text-rose-300">{bulkDeleteError}</p>
            )}
            <div className="mt-5 flex items-center justify-end gap-2">
              <button
                type="button"
                disabled={deletingBulk}
                onClick={() => setConfirmingBulkDelete(null)}
                className="rounded-lg border border-white/10 bg-white/[0.03] px-4 py-1.5 text-xs font-medium text-slate-300 hover:bg-white/[0.06] disabled:opacity-50"
              >
                Cancel
              </button>
              <button
                type="button"
                disabled={deletingBulk}
                onClick={confirmBulkDelete}
                className="inline-flex items-center gap-1.5 rounded-lg bg-rose-600 px-4 py-1.5 text-xs font-medium text-white transition-colors hover:bg-rose-500 disabled:opacity-50"
              >
                {deletingBulk ? (
                  <ProgressActivityIcon className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <DeleteIcon className="h-3.5 w-3.5" />
                )}
                {deletingBulk
                  ? "Deleting…"
                  : confirmingBulkDelete.ids.length === 1
                    ? "Delete charge"
                    : `Delete ${confirmingBulkDelete.ids.length} charges`}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}

function ChargingSelectBar({
  selectedCount,
  totalCount,
  onSelectAll,
  onDelete,
  onCancel,
}: {
  selectedCount: number
  totalCount: number
  onSelectAll: () => void
  onDelete: () => void
  onCancel: () => void
}) {
  const hasSelection = selectedCount > 0
  return (
    <div className="flex items-center gap-2">
      <span className="mr-1 text-sm text-slate-400">
        {selectedCount} of {totalCount} selected
      </span>
      <button
        type="button"
        disabled={!hasSelection}
        onClick={onDelete}
        className="inline-flex items-center gap-1.5 rounded-full bg-rose-500/95 px-3 py-1 text-xs font-medium text-white transition-colors hover:bg-rose-400 disabled:opacity-50"
      >
        <DeleteIcon className="h-3.5 w-3.5" />
        Delete
      </button>
      <button
        type="button"
        onClick={onSelectAll}
        className="inline-flex items-center gap-1.5 rounded-full border border-white/10 bg-white/[0.03] px-3 py-1 text-xs font-medium text-slate-200 transition-colors hover:bg-white/[0.06]"
      >
        Select all
      </button>
      <button
        type="button"
        onClick={onCancel}
        className="inline-flex items-center gap-1.5 rounded-full border border-white/10 bg-white/[0.03] px-3 py-1 text-xs font-medium text-slate-200 transition-colors hover:bg-white/[0.06]"
      >
        Cancel
      </button>
    </div>
  )
}

const ChargeRow = memo(function ChargeRow({ session, metric, active, livePowerKw, selectMode, selected,
  onToggleSelected, onTagsChange, onOpenHomeLocation,
}: {
  session: ChargeSessionSummary; metric: boolean; active: boolean; livePowerKw: number | null
  selectMode: boolean; selected: boolean; onToggleSelected: (id: number) => void
  onTagsChange: (id: number, tags: string[]) => Promise<void>
  onOpenHomeLocation: (location: { lat: number | null; lon: number | null }) => void
}) {
  const navigate = useNavigate()
  const [mapOpen, setMapOpen] = useState(false)
  const start = new Date(session.startMs)
  const hasSoc = session.startSoc != null && session.endSoc != null
  const startPct = Math.max(0, Math.min(100, session.startSoc ?? 0))
  const endPct = Math.max(0, Math.min(100, session.endSoc ?? 0))
  const range = (value: number | null) => value == null ? "" : `${Math.round(value * (metric ? 1.609344 : 1))} ${metric ? "km" : "mi"}`
  return <article className={cn("glass-card overflow-visible", selected ? "ring-2 ring-emerald-400/60" : active ? "ring-1 ring-emerald-400/35" : "")}>
    <button type="button" onClick={() => selectMode ? onToggleSelected(session.id) : navigate(`/charging/${session.id}`)}
      aria-pressed={selectMode ? selected : undefined}
      aria-label={`${selectMode ? "Select" : "Open"} charge ${formatDate(start)} at ${session.location ?? "unknown location"}`}
      className="grid w-full grid-cols-[3.5rem_minmax(0,1fr)_auto] items-center gap-3 rounded-t-2xl p-4 text-left hover:bg-white/[0.03] sm:grid-cols-[4.5rem_minmax(0,1fr)_minmax(9rem,12rem)_auto] sm:gap-5">
      <span className="flex flex-col items-center border-r border-white/10 pr-3 tabular-nums">
        <span className="text-[10px] text-slate-400">{start.toLocaleDateString([], { month: "short", year: "numeric" })}</span>
        <span className="text-2xl font-semibold text-slate-100">{start.getDate()}</span>
        <span className="text-[10px] text-slate-400">{formatTime(start)}</span>
      </span>
      <span className="min-w-0">
        <span className="flex flex-wrap items-center gap-2 text-sm font-medium text-slate-100">
          <span className="truncate">{session.location ?? "Unknown location"}</span>
          {session.fastCharging && <span className="inline-flex items-center gap-1 rounded-full bg-amber-500/10 px-2 py-0.5 text-[10px] text-amber-200"><BoltIcon className="h-3 w-3" />Fast</span>}
        </span>
        <span className="mt-1 block text-xs text-slate-400">{fmtDuration(session.durationSecs)}
          {active && <span className="ml-2 text-emerald-300">Charging{livePowerKw == null ? "" : ` · ${livePowerKw} kW`}</span>}
        </span>
        <span className="mt-1 block text-xs text-slate-400 sm:hidden">{fmtSoc(session.startSoc)} → {fmtSoc(session.endSoc)}</span>
      </span>
      <span className="hidden min-w-0 sm:block">
        <span className="flex justify-between text-xs tabular-nums text-slate-300"><span>{fmtSoc(session.startSoc)}</span><span className="text-slate-500">Battery</span><span>{fmtSoc(session.endSoc)}</span></span>
        {hasSoc && <span aria-hidden="true" className="relative mt-2 block h-1.5 overflow-hidden rounded-full bg-white/10"><span className="absolute inset-y-0 left-0 bg-emerald-900" style={{ width: `${startPct}%` }} /><span className="absolute inset-y-0 bg-emerald-400" style={{ left: `${Math.min(startPct, endPct)}%`, width: `${Math.abs(endPct - startPct)}%` }} /></span>}
        {(session.startRangeMi != null || session.endRangeMi != null) && <span className="mt-1 block text-[10px] text-slate-500">{range(session.startRangeMi) || "—"} → {range(session.endRangeMi) || "—"}</span>}
      </span>
      <span className="flex items-center gap-3 text-right">
        <span><span className="block text-sm font-semibold tabular-nums text-emerald-300">{fmtEnergy(session.energyAddedKwh)}</span>
          {session.energyAddedKwh == null && <span className="block text-[10px] text-slate-400">Energy unavailable</span>}
          {session.cost != null && <span className="block text-xs tabular-nums text-slate-300">{fmtMoney(session.cost, session.currency)}</span>}
        </span>
        {selectMode && selected ? <CheckBoxIcon className="h-4 w-4 text-emerald-300" /> : <ChevronRightIcon className="h-4 w-4 text-slate-500" />}
      </span>
    </button>
    <div className="flex flex-wrap items-center gap-2 border-t border-white/5 px-4 py-2">
      {session.atHome && <button type="button" onClick={() => onOpenHomeLocation({ lat: null, lon: null })} className="inline-flex items-center gap-1 rounded-full bg-emerald-500/10 px-2 py-1 text-xs text-emerald-200"><HomeIcon className="h-3 w-3" />Home</button>}
      <TagPopover tags={session.tags} onChange={(tags) => onTagsChange(session.id, tags)} />
      {session.locationLat != null && session.locationLon != null && <button type="button" aria-expanded={mapOpen} onClick={() => setMapOpen((open) => !open)} className="ml-auto inline-flex items-center gap-1 text-xs text-slate-400"><LocationOnIcon className="h-3.5 w-3.5" />{mapOpen ? "Hide map" : "Show map"}</button>}
    </div>
    {mapOpen && <div className="px-4 pb-4"><MiniPinMap lat={session.locationLat} lon={session.locationLon} className="h-36 w-full" /></div>}
  </article>
})

function formatDate(d: Date): string {
  return d.toLocaleDateString([], {
    weekday: "short",
    month: "short",
    day: "numeric",
  })
}

function formatTime(d: Date): string {
  return d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })
}

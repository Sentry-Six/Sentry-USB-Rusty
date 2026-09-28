import { SelectMenu } from "@/components/ui/SelectMenu"
import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { DeleteIcon, HardDriveIcon, ProgressActivityIcon } from "@/components/icons"
import { Modal } from "@/components/ui/Modal"
import type { ManagedStorageHealth } from "@/lib/api"
import { responseError } from "@/lib/file-upload"

interface SnapshotEntry {
  id: string
  // Reclaim is cumulative: this snapshot and every older holder of shared blocks.
  cumulative_reclaim_bytes: number | null
  older_count: number
  created_unix: number
}
interface FreeSpace {
  total_bytes: number
  used_bytes: number
  available_bytes: number
  mounted: boolean
  storage_health?: ManagedStorageHealth
}
interface Deletion {
  entries: SnapshotEntry[]
  estimate: number | null
  through: boolean
}
const buttonClass =
  "inline-flex min-h-9 items-center justify-center gap-2 rounded-xl border border-white/10 px-3 py-2 text-sm text-slate-300 hover:bg-white/5 disabled:opacity-50"
function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B"
  const unit = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), 3)
  return `${(bytes / 1024 ** unit).toFixed(unit ? 1 : 0)} ${["B", "KB", "MB", "GB"][unit]}`
}
function formatDate(unix: number) {
  return unix ? new Date(unix * 1000).toLocaleString() : "Unknown date"
}

export default function Snapshots() {
  const [snapshots, setSnapshots] = useState<SnapshotEntry[]>([])
  const [allocated, setAllocated] = useState(0)
  const [free, setFree] = useState<FreeSpace | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [pending, setPending] = useState(false)
  const [sort, setSort] = useState("oldest")
  const [hoverPreview, setHoverPreview] = useState<{ id: string; through: boolean } | null>(null)
  const [focusPreview, setFocusPreview] = useState<{ id: string; through: boolean } | null>(null)
  const [deletion, setDeletion] = useState<Deletion | null>(null)
  const [deleting, setDeleting] = useState(false)
  const [completed, setCompleted] = useState(0)
  const inFlight = useRef(false)
  const requestId = useRef(0)
  const abort = useRef<AbortController | null>(null)
  const refresh = useCallback(async (force = false) => {
    if (inFlight.current && !force) return
    abort.current?.abort()
    const id = ++requestId.current
    inFlight.current = true
    const controller = new AbortController()
    abort.current = controller
    let timedOut = false
    const timeout = setTimeout(() => { timedOut = true; controller.abort() }, 20000)
    try {
      const [listResponse, spaceResponse] = await Promise.all([
        fetch("/api/snapshots", { signal: controller.signal }),
        fetch("/api/backingfiles/free-space", { signal: controller.signal }),
      ])
      if (!listResponse.ok)
        throw new Error(await responseError(listResponse, "Could not load snapshots"))
      const data = await listResponse.json()
      if (controller.signal.aborted || id !== requestId.current) return
      setSnapshots(data.snapshots ?? [])
      setAllocated(data.total_allocated_bytes ?? 0)
      setPending(data.sizes_pending === true)
      if (spaceResponse.ok) {
        const space = await spaceResponse.json()
        if (!controller.signal.aborted && id === requestId.current) setFree(space)
      }
    } catch (e) {
      if (id === requestId.current && (!controller.signal.aborted || timedOut))
        setError(timedOut ? "Loading snapshots timed out. Please retry." : e instanceof Error ? e.message : "Could not load snapshots")
    } finally {
      clearTimeout(timeout)
      if (id === requestId.current) {
        inFlight.current = false
        if (!controller.signal.aborted || timedOut) setLoading(false)
      }
    }
  }, [])
  useEffect(() => {
    void refresh(true)
    return () => abort.current?.abort()
  }, [refresh])
  useEffect(() => {
    if (!pending || deleting) return
    const timer = setInterval(() => {
      if (!document.hidden) void refresh()
    }, 4000)
    return () => clearInterval(timer)
  }, [pending, deleting, refresh])
  const oldest = useMemo(
    () =>
      // Equal timestamps retain API order: its cumulative estimates use that prefix.
      [...snapshots].sort((a, b) => a.created_unix - b.created_unix),
    [snapshots],
  )
  const visible = sort === "oldest" ? oldest : [...oldest].reverse()

  function selectionFor(snapshot: SnapshotEntry, through: boolean): Deletion {
    const index = oldest.findIndex((s) => s.id === snapshot.id)
    const entries = through ? oldest.slice(0, index + 1) : [snapshot]
    return {
      entries,
      estimate: through || index === 0 ? snapshot.cumulative_reclaim_bytes : null,
      through,
    }
  }
  const previewTarget = hoverPreview ?? focusPreview
  const previewSnapshot = previewTarget && snapshots.find(s => s.id === previewTarget.id)
  const preview = deletion ?? (previewSnapshot && previewTarget ? selectionFor(previewSnapshot, previewTarget.through) : null)
  const previewIds = new Set(preview?.entries.map(s => s.id))
  function askDelete(snapshot: SnapshotEntry, through: boolean) {
    setError(null)
    setCompleted(0)
    setHoverPreview(null)
    setFocusPreview(null)
    setDeletion(selectionFor(snapshot, through))
  }
  async function confirmDelete() {
    if (!deletion || deleting) return
    abort.current?.abort()
    requestId.current++
    inFlight.current = false
    setDeleting(true)
    setError(null)
    let removed = 0
    try {
      // Use exactly the reviewed IDs; new snapshots cannot enter this selection.
      for (const entry of deletion.entries) {
        const controller = new AbortController()
        let timeout: ReturnType<typeof setTimeout> | undefined
        const deadline = new Promise<never>((_, reject) => {
          timeout = setTimeout(() => {
            reject(new Error(`Deletion of ${entry.id} timed out and may still complete. Check the refreshed list before selecting again.`))
            controller.abort()
          }, 30000)
        })
        try {
          await Promise.race([deadline, (async () => {
            const response = await fetch(`/api/snapshots/${encodeURIComponent(entry.id)}`, {
              method: "DELETE", signal: controller.signal,
            })
            if (!response.ok && response.status !== 404)
              throw new Error(await responseError(response, `Could not delete ${entry.id}`))
          })()])
        } finally {
          clearTimeout(timeout)
        }
        removed++
        setCompleted(removed)
        setSnapshots((old) => old.filter((s) => s.id !== entry.id))
      }
      setDeletion(null)
    } catch (e) {
      setError(
        `${removed} of ${deletion.entries.length} snapshots confirmed removed. ${e instanceof Error ? e.message : "Deletion failed"}`,
      )
      setDeletion(null)
    } finally {
      setDeleting(false)
      await refresh(true)
    }
  }
  const health = free?.storage_health
  const usage = free?.total_bytes ? Math.min(100, (free.used_bytes / free.total_bytes) * 100) : 0
  const color =
    health?.state === "fail"
      ? "bg-red-400"
      : health?.state === "warn"
        ? "bg-amber-400"
        : "bg-emerald-400"
  return (
    <div className="mx-auto max-w-5xl space-y-5">
      <header className="flex flex-wrap items-center justify-between gap-3">
        <h1 className="text-2xl font-bold text-slate-100">Snapshots</h1>
        <button
          className={buttonClass}
          disabled={loading || deleting}
          onClick={() => {
            setError(null)
            void refresh()
          }}
        >
          Refresh
        </button>
      </header>
      <section className="glass-card space-y-3 p-5">
        <div className="flex flex-wrap justify-between gap-3">
          <div>
            <h2 className="flex items-center gap-2 font-medium text-slate-200">
              <HardDriveIcon className="h-4 w-4" />
              {health?.message ?? "Storage managed automatically"}
            </h2>
            <p className="mt-1 text-sm text-slate-400">
              {snapshots.length} snapshots · {formatBytes(allocated)} allocated
            </p>
          </div>
          {free?.mounted && (
            <p className="text-sm text-slate-300">
              {formatBytes(free.available_bytes)} free of {formatBytes(free.total_bytes)}
            </p>
          )}
        </div>
        {free?.mounted && (
          <div
            className="h-2 overflow-hidden rounded-full bg-white/10"
            role="meter"
            aria-label="Storage used"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={Math.round(usage)}
          >
            <div className={`h-full ${color}`} style={{ width: `${usage}%` }} />
          </div>
        )}
        {!!oldest.length && (
          <p className="text-sm text-slate-400">
            Snapshot dates: {formatDate(oldest[0].created_unix)} –{" "}
            {formatDate(oldest[oldest.length - 1].created_unix)}
          </p>
        )}
        <details className="text-sm text-slate-400">
          <summary className="cursor-pointer text-slate-300">How shared storage works</summary>
          <p className="mt-2">
            Old snapshots are removed automatically when space is needed. Snapshots share blocks, so
            removing one may free very little. The estimate below applies to deleting that snapshot
            and all older snapshots. Manual deletion is optional.
          </p>
        </details>
      </section>
      {error && (
        <p
          role="alert"
          className="rounded-xl border border-red-400/20 bg-red-400/10 p-3 text-sm text-red-300"
        >
          {error}
        </p>
      )}
      <div className={`sticky top-0 z-20 flex flex-wrap items-center justify-between gap-3 rounded-2xl border px-3 py-2 backdrop-blur ${preview ? "border-red-400/30 bg-[#20151b]" : "border-transparent bg-[#0a1117]/95"}`}>
        <p id="snapshot-delete-preview" role="status" aria-live="polite" className={`text-sm ${preview ? "text-red-200" : "text-slate-400"}`}>
          {preview ? `${preview.entries.length} ${preview.entries.length === 1 ? "snapshot" : "snapshots"} would be deleted${preview.estimate === null ? " · space estimate unavailable" : ` · ~${formatBytes(preview.estimate)} freed`}` : "Estimated space freed by deleting this and older"}
        </p>
        <SelectMenu label="Sort snapshots" value={sort} onChange={value => { setHoverPreview(null); setFocusPreview(null); setSort(value) }} align="end" options={[{ value: "oldest", label: "Oldest first" }, { value: "newest", label: "Newest first" }]} />
      </div>
      {loading ? (
        <p role="status" className="p-8 text-slate-400">
          Loading snapshots…
        </p>
      ) : !visible.length ? (
        <p className="glass-card p-8 text-center text-slate-400">No snapshots on this device.</p>
      ) : (
        <ul className="glass-card divide-y divide-white/10 overflow-hidden">
          {visible.map((s) => (
            <li key={s.id} data-snapshot-id={s.id} data-delete-preview={previewIds.has(s.id) || undefined}
              className={`flex flex-wrap items-center justify-between gap-3 border-l-2 px-4 py-3 transition-colors ${previewIds.has(s.id) ? "border-l-red-400 bg-red-500/10 shadow-[inset_0_0_20px_rgba(239,68,68,0.04)]" : "border-l-transparent"}`}>
              <div className="min-w-48 flex-1">
                <p className="text-sm font-medium text-slate-200">{formatDate(s.created_unix)}</p>
                <p className="mt-1 text-xs text-slate-400">
                  {s.id} ·{" "}
                  {s.cumulative_reclaim_bytes === null
                    ? pending
                      ? "Measuring space…"
                      : "Estimate unavailable"
                    : `~${formatBytes(s.cumulative_reclaim_bytes)} for this + ${s.older_count} older`}
                </p>
              </div>
              <div className="flex flex-wrap gap-2">
                <button
                  className={buttonClass}
                  disabled={deleting}
                  aria-label={`Delete only ${s.id}`}
                  onMouseEnter={() => setHoverPreview({ id: s.id, through: false })}
                  onMouseLeave={() => setHoverPreview(null)}
                  onFocus={() => setFocusPreview({ id: s.id, through: false })}
                  onBlur={() => setFocusPreview(null)}
                  aria-describedby="snapshot-delete-preview"
                  onClick={() => askDelete(s, false)}
                >
                  <DeleteIcon className="h-4 w-4" />
                  Delete only this
                </button>
                {s.older_count > 0 && (
                  <button
                    className={buttonClass}
                    disabled={deleting}
                    aria-label={`Delete through ${s.id}`}
                    aria-describedby="snapshot-delete-preview"
                    onMouseEnter={() => setHoverPreview({ id: s.id, through: true })}
                    onMouseLeave={() => setHoverPreview(null)}
                    onFocus={() => setFocusPreview({ id: s.id, through: true })}
                    onBlur={() => setFocusPreview(null)}
                    onClick={() => askDelete(s, true)}
                  >
                    Delete through this date
                  </button>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}
      {deletion && (
        <Modal
          title={deletion.through ? "Delete through this date?" : "Delete this snapshot?"}
          onClose={() => setDeletion(null)}
          dismissable={!deleting}
          footer={
            <div className="flex justify-end gap-2">
              <button className={buttonClass} disabled={deleting} onClick={() => setDeletion(null)}>
                Cancel
              </button>
              <button
                className="rounded-xl bg-red-600 px-4 py-2 text-sm font-medium text-white disabled:opacity-50"
                disabled={deleting}
                onClick={() => void confirmDelete()}
              >
                {deleting
                  ? `${completed} / ${deletion.entries.length} removed`
                  : "Delete permanently"}
              </button>
            </div>
          }
        >
          <p className="text-sm text-slate-200">
            {deletion.entries.length} {deletion.entries.length === 1 ? "snapshot" : "snapshots"}{" "}
            selected.
          </p>
          <p className="mt-2 text-sm text-slate-400">
            {formatDate(deletion.entries[0].created_unix)}
            {deletion.entries.length > 1
              ? ` – ${formatDate(deletion.entries[deletion.entries.length - 1].created_unix)}`
              : ""}
          </p>
          <p className="mt-3 text-sm text-slate-300">
            {deletion.estimate === null
              ? "Reclaimed space depends on blocks shared with remaining snapshots."
              : `Estimated space freed: ${formatBytes(deletion.estimate)}.`}
          </p>
          <p className="mt-3 text-sm text-red-300">
            Footage stored only in these snapshots will be lost. This cannot be undone. Live dashcam
            files are unaffected.
          </p>
          {deleting && <ProgressActivityIcon className="mt-3 h-5 w-5 animate-spin" />}
        </Modal>
      )}
    </div>
  )
}

import { useState } from "react"
import { ChevronLeftIcon, DeleteIcon, MovieIcon, ProgressActivityIcon, VideocamIcon } from "@/components/icons"
import type { ClipEntry, ClipGroup } from "@/lib/api"
import { eventCamera } from "@/lib/clipEvent"
import { formatClipClock, formatClipListLabel, parseEventTime } from "@/lib/clipTime"
import { cn } from "@/lib/utils"
import { CAMERA_LABELS, CATEGORIES, formatEventReason } from "./clipEvents"

const CATEGORY_LABELS: Record<string, string> = {
  RecentClips: "Recent",
  SavedClips: "Saved",
  SentryClips: "Sentry",
}

interface ClipsPanelProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  groups: ClipGroup[]
  category: string
  onCategory: (category: string) => void
  loading: boolean
  loadingMore: boolean
  onLoadMore: () => void
  selected: ClipEntry | null
  onSelect: (clip: ClipEntry) => void
  onDelete: (clip: ClipEntry) => void
}

// The clip browser: a panel that floats over the cameras and folds away to a
// small button, as in Sentry Studio, instead of a column that takes a fifth
// of the screen whether or not it is in use.
export default function ClipsPanel({
  open, onOpenChange, groups, category, onCategory, loading, loadingMore, onLoadMore, selected, onSelect, onDelete,
}: ClipsPanelProps) {
  const [deleteConfirm, setDeleteConfirm] = useState<string | null>(null)
  const group = groups.find((g) => g.name === category)
  const counts = Object.fromEntries(groups.map((g) => [g.name, g.clips.length]))
  const event = selected?.event

  if (!open) {
    return (
      <button
        onClick={() => onOpenChange(true)}
        className="absolute left-2 top-2 z-30 flex items-center gap-1.5 rounded-lg border border-white/10 bg-black/60 px-2.5 py-1.5 text-xs font-medium text-slate-200 backdrop-blur-md transition-colors hover:bg-black/75"
        title="Show clips"
        aria-expanded={false}
      >
        <MovieIcon className="h-3.5 w-3.5 text-blue-400" aria-hidden="true" />
        Clips
      </button>
    )
  }

  return (
    <section
      aria-label="Clips"
      className="absolute left-2 top-2 z-30 flex max-h-[calc(100%-1rem)] w-60 max-w-[calc(100%-1rem)] flex-col overflow-hidden rounded-xl border border-white/10 bg-slate-950/85 shadow-2xl backdrop-blur-md"
    >
      <div className="flex items-center gap-1.5 px-2.5 pb-1.5 pt-2">
        <MovieIcon className="h-4 w-4 text-blue-400" aria-hidden="true" />
        <h2 className="flex-1 text-sm font-semibold text-slate-100">Clips</h2>
        <button
          onClick={() => onOpenChange(false)}
          className="rounded-md p-1 text-slate-400 transition-colors hover:bg-white/10 hover:text-slate-200"
          title="Hide clips"
          aria-expanded={true}
          aria-label="Hide clips"
        >
          <ChevronLeftIcon className="h-4 w-4" />
        </button>
      </div>

      <div className="flex gap-0.5 px-2 pb-1.5">
        {CATEGORIES.map((cat) => (
          <button
            key={cat}
            onClick={() => onCategory(cat)}
            aria-pressed={category === cat}
            className={cn(
              "flex-1 rounded-md px-1.5 py-1 text-xs font-medium transition-colors",
              category === cat ? "bg-blue-500/15 text-blue-400" : "text-slate-400 hover:bg-white/5 hover:text-slate-200",
            )}
          >
            {CATEGORY_LABELS[cat]}
            {(counts[cat] ?? 0) > 0 && (
              <span className="ml-1 rounded-full bg-white/5 px-1.5 py-0.5 text-[10px] tabular-nums text-slate-500">
                {counts[cat]}
              </span>
            )}
          </button>
        ))}
      </div>

      {selected && event?.reason && <EventSummary event={event} />}

      <div className="min-h-0 flex-1 overflow-y-auto border-t border-white/5 p-1.5">
        {loading ? (
          <div className="flex items-center justify-center p-8">
            <ProgressActivityIcon className="h-5 w-5 animate-spin text-slate-500" />
          </div>
        ) : group && group.clips.length > 0 ? (
          group.clips.map((clip) => {
            const isSelected = selected?.date === clip.date
            const reason = clip.event?.reason ? formatEventReason(clip.event.reason).label : ""
            return (
              <div key={clip.date} className="group relative">
                <button
                  onClick={() => onSelect(clip)}
                  aria-current={isSelected}
                  className={cn(
                    "w-full rounded-lg px-2.5 py-1.5 text-left transition-colors",
                    isSelected ? "bg-blue-500/15 text-blue-400" : "text-slate-300 hover:bg-white/5 hover:text-slate-100",
                  )}
                >
                  <div className="text-xs font-medium">{formatClipListLabel(clip.date)}</div>
                  <div className="mt-0.5 flex items-center gap-1.5">
                    <span className="text-[10px] text-slate-500">{clip.files.length} files</span>
                    {reason && (
                      <span className={cn(
                        "rounded px-1 py-0.5 text-[9px] font-medium",
                        category === "SentryClips" ? "bg-red-500/15 text-red-400" : "bg-amber-500/15 text-amber-400",
                      )}>
                        {reason}
                      </span>
                    )}
                  </div>
                  {clip.event?.city && <div className="mt-0.5 truncate text-[10px] text-slate-500">{clip.event.city}</div>}
                </button>
                {/* Recent clips are rotated by the car and grouped by day here;
                    there is no folder of their own to delete. */}
                {category !== "RecentClips" && (
                  <>
                    <button
                      onClick={() => setDeleteConfirm(clip.date)}
                      className="absolute right-1 top-1 rounded p-1 text-slate-500 opacity-0 transition hover:bg-red-500/15 hover:text-red-400 focus-visible:opacity-100 group-hover:opacity-100"
                      title="Delete clip"
                      aria-label={`Delete the clip from ${formatClipListLabel(clip.date)}`}
                    >
                      <DeleteIcon className="h-3 w-3" />
                    </button>
                    {deleteConfirm === clip.date && (
                      <div className="mx-1 mb-1 flex items-center gap-1 rounded-md bg-red-500/10 px-2 py-1.5">
                        <span className="flex-1 text-[10px] text-red-400">Delete this clip?</span>
                        <button
                          onClick={() => { setDeleteConfirm(null); onDelete(clip) }}
                          className="rounded bg-red-500/20 px-2 py-0.5 text-[10px] font-medium text-red-400 hover:bg-red-500/30"
                        >
                          Yes
                        </button>
                        <button
                          onClick={() => setDeleteConfirm(null)}
                          className="rounded bg-white/5 px-2 py-0.5 text-[10px] text-slate-400 hover:bg-white/10"
                        >
                          No
                        </button>
                      </div>
                    )}
                  </>
                )}
              </div>
            )
          })
        ) : (
          <div className="flex flex-col items-center justify-center py-8 text-center">
            <VideocamIcon className="mb-2 h-8 w-8 text-slate-600" />
            <p className="text-xs text-slate-500">No {CATEGORY_LABELS[category]?.toLowerCase()} clips</p>
          </div>
        )}
        {group?.hasMore && (
          <button
            onClick={onLoadMore}
            disabled={loadingMore}
            className="mt-1 w-full rounded-lg px-2 py-1.5 text-[11px] font-medium text-slate-400 transition-colors hover:bg-white/5 hover:text-slate-200 disabled:opacity-50"
          >
            {loadingMore ? (
              <span className="flex items-center justify-center gap-1.5">
                <ProgressActivityIcon className="h-3 w-3 animate-spin" /> Loading…
              </span>
            ) : "Load more clips"}
          </button>
        )}
      </div>
    </section>
  )
}

// What happened in the selected Sentry or Saved clip.
function EventSummary({ event }: { event: NonNullable<ClipEntry["event"]> }) {
  const { label, Icon } = formatEventReason(event.reason ?? "")
  const camera = eventCamera(event.camera)
  const eventMs = parseEventTime(event.timestamp)
  return (
    <div className="flex items-start gap-2 border-t border-white/5 px-2.5 py-2">
      <div className="flex h-6 w-6 shrink-0 items-center justify-center rounded-md bg-amber-500/15">
        <Icon className="h-3 w-3 text-amber-400" />
      </div>
      <div className="min-w-0">
        <p className="text-xs font-medium text-slate-200">{label}</p>
        {eventMs !== null && <p className="text-[10px] tabular-nums text-slate-400">{formatClipClock(eventMs)}</p>}
        {camera && <p className="text-[10px] text-slate-400">Triggered: {CAMERA_LABELS[camera]}</p>}
        {event.city && <p className="truncate text-[10px] text-slate-500">{event.city}</p>}
      </div>
    </div>
  )
}

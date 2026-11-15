import { useCallback, useEffect, useMemo, useState } from "react"
import { OpenInNewIcon, VideocamIcon } from "@/components/icons"
import ClipPlayer from "@/components/viewer/ClipPlayer"
import ClipsPanel from "@/components/viewer/ClipsPanel"
import ViewerNotice from "@/components/viewer/ViewerNotice"
import { useArchiving } from "@/hooks/useArchiving"
import type { ClipSet } from "@/hooks/useClipPlayer"
import type { ClipEntry, ClipGroup } from "@/lib/api"

const CLIPS_PAGE_SIZE = 20

// One ClipSet per minute: the minute's start time and each camera's file.
function groupByTimestamp(files: string[], basePath: string): ClipSet[] {
  const map = new Map<string, Record<string, string>>()
  for (const f of files) {
    const match = f.match(/^(.+)-(front|back|left_repeater|right_repeater|left_pillar|right_pillar)\.mp4$/)
    if (!match) continue
    const [, ts, cam] = match
    if (!map.has(ts)) map.set(ts, {})
    map.get(ts)![cam] = `${basePath}/${f}`
  }
  return Array.from(map.entries())
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([timestamp, cameras]) => ({ timestamp, cameras }))
}

// The clip viewer, laid out like Sentry Studio's player: the cameras fill the
// window, the clip browser floats over them and folds away, and the controls
// are one slim row.
export default function Viewer() {
  const [groups, setGroups] = useState<ClipGroup[]>([])
  const [loading, setLoading] = useState(true)
  const [loadingMore, setLoadingMore] = useState(false)
  const [category, setCategory] = useState("RecentClips")
  const [selectedClip, setSelectedClip] = useState<ClipEntry | null>(null)
  const [panelOpen, setPanelOpen] = useState(true)
  const [metric, setMetric] = useState(false)

  // Archiving shares the Pi's Wi-Fi with the video; warn while it runs.
  // Hiding the notice lasts until that archive finishes.
  const { archiving, etaSeconds } = useArchiving()
  const [noticeHidden, setNoticeHidden] = useState(false)
  if (!archiving && noticeHidden) setNoticeHidden(false)
  const archivingNotice = archiving && !noticeHidden ? { etaSeconds } : null
  const hideNotice = useCallback(() => setNoticeHidden(true), [])

  // Load unit from setup config (DRIVE_MAP_UNIT set in wizard)
  useEffect(() => {
    fetch("/api/setup/config")
      .then((r) => r.json())
      .then((cfg) => {
        const entry = cfg.DRIVE_MAP_UNIT
        if (entry) {
          const val = typeof entry === "object" ? (entry.active ? entry.value : null) : entry
          if (val !== null) setMetric(val === "km")
        }
      })
      .catch(() => {})
  }, [])

  // Fetch clips for the open category only.
  useEffect(() => {
    const controller = new AbortController()
    fetch(`/api/clips?category=${category}&limit=${CLIPS_PAGE_SIZE}`, { signal: controller.signal })
      .then((r) => r.json())
      .then((data: ClipGroup[]) => {
        if (controller.signal.aborted) return
        setGroups((prev) => [...prev.filter((g) => g.name !== category), ...data])
        setLoading(false)
      })
      .catch(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [category])

  function loadMoreClips() {
    const group = groups.find((g) => g.name === category)
    const lastClip = group?.clips[group.clips.length - 1]
    if (!group?.hasMore || !lastClip) return
    setLoadingMore(true)
    fetch(`/api/clips?category=${category}&limit=${CLIPS_PAGE_SIZE}&before=${lastClip.date}`)
      .then((r) => r.json())
      .then((data: ClipGroup[]) => {
        const more = data.find((g) => g.name === category)
        if (more) {
          setGroups((prev) => prev.map((g) => (
            g.name === category ? { ...g, clips: [...g.clips, ...more.clips], hasMore: more.hasMore } : g
          )))
        }
        setLoadingMore(false)
      })
      .catch(() => setLoadingMore(false))
  }

  function changeCategory(next: string) {
    if (next !== category) setLoading(true)
    setCategory(next)
    setSelectedClip(null)
  }

  function selectClip(clip: ClipEntry) {
    setSelectedClip(clip)
    // On a phone the open panel covers the cameras; get it out of the way.
    if (window.innerWidth < 768) setPanelOpen(false)
  }

  async function deleteClip(clip: ClipEntry) {
    try {
      // Delete via /mutable/TeslaCam (the snapshot-symlink tree this view
      // actually lists from), NOT /mnt/cam. /mnt/cam is the live cam-disk
      // image, which is unmounted on the Pi side whenever the USB gadget is
      // presenting it to the car — so the old path silently no-op'd in
      // normal operation while the row vanished optimistically here. The
      // backend's delete handler also runs its SavedClips/SentryClips
      // snapshot-symlink cleanup off this exact path. Matches the download
      // button below and the Files page.
      const fullPath = `/mutable/TeslaCam/${category}/${clip.date}`
      await fetch(`/api/files?path=${encodeURIComponent(fullPath)}`, { method: "DELETE" })
      setGroups((prev) => prev.map((g) => (
        g.name === category ? { ...g, clips: g.clips.filter((c) => c.date !== clip.date) } : g
      )))
      if (selectedClip?.date === clip.date) setSelectedClip(null)
    } catch { /* ignore */ }
  }

  // Recent clips are grouped by day and have no folder of their own to zip.
  const download = selectedClip && category !== "RecentClips"
    ? () => {
        const fullPath = `/mutable/TeslaCam/${category}/${selectedClip.date}`
        window.open(`/api/files/download-zip?path=${encodeURIComponent(fullPath)}`, "_blank")
      }
    : undefined

  // Pressing play folds the clip browser away so nothing covers the cameras;
  // the Clips button brings it back.
  const foldPanel = useCallback(() => setPanelOpen(false), [])

  const clipSets = useMemo(
    () => (selectedClip ? groupByTimestamp(selectedClip.files, selectedClip.path) : []),
    [selectedClip],
  )

  const panel = (
    <ClipsPanel
      open={panelOpen}
      onOpenChange={setPanelOpen}
      groups={groups}
      category={category}
      onCategory={changeCategory}
      loading={loading}
      loadingMore={loadingMore}
      onLoadMore={loadMoreClips}
      selected={selectedClip}
      onSelect={selectClip}
      onDelete={deleteClip}
    />
  )

  return (
    <div className="min-h-[320px] flex-1 pb-2 md:pb-3">
      <h1 className="sr-only">Viewer</h1>
      {selectedClip && clipSets.length > 0 ? (
        <ClipPlayer
          key={selectedClip.path}
          clip={selectedClip}
          clipSets={clipSets}
          metric={metric}
          onDownload={download}
          panel={panel}
          onPlay={foldPanel}
          archiving={archivingNotice}
          onDismissArchiving={hideNotice}
        />
      ) : (
        <div className="relative flex h-full items-center justify-center overflow-hidden rounded-lg bg-black/40">
          <div className="max-w-xs px-4 text-center">
            <VideocamIcon className="mx-auto mb-3 h-14 w-14 text-slate-600" />
            <p className="text-sm font-medium text-slate-300">
              {selectedClip ? "No video files found" : "Pick a clip to watch"}
            </p>
            <p className="mt-1 text-xs text-slate-500">
              Every camera plays together, in sync. Space plays and pauses, the arrow keys skip, F is fullscreen.
            </p>
            <a
              href="https://github.com/ChadR23/Sentry-Six"
              target="_blank"
              rel="noopener noreferrer"
              className="mt-4 inline-flex items-center gap-1 text-[11px] font-medium text-blue-400 transition-colors hover:text-blue-300"
            >
              For exporting and sharing clips, try Sentry Studio <OpenInNewIcon className="h-2.5 w-2.5" />
            </a>
          </div>
          {panel}
          <ViewerNotice
            archiving={archivingNotice}
            onDismiss={hideNotice}
            className="absolute bottom-2 left-2 right-2 mx-auto max-w-xl"
          />
        </div>
      )}
    </div>
  )
}

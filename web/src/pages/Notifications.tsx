import { SelectMenu } from "@/components/ui/SelectMenu"
import { useState, useEffect, useCallback, useRef, lazy, Suspense } from "react"
import {
  ArchiveIcon,
  BatteryAndroidFrameFullIcon,
  BoltIcon,
  BuildIcon,
  CancelIcon,
  CheckCircleIcon,
  CloseIcon,
  DeleteIcon,
  DeviceThermostatIcon,
  DownloadIcon,
  FilterAltIcon,
  HardDriveIcon,
  MusicNoteIcon,
  NotificationsIcon,
  NotificationsOffIcon,
  PowerIcon,
  ProgressActivityIcon,
  ScheduleIcon,
  SettingsIcon,
} from "@/components/icons"
import { MobileNotificationsSection } from "@/components/settings/sections/MobileNotificationsSection"
import { cn } from "@/lib/utils"
import { NotificationHistoryItem, type NotificationEvent } from "@/components/notifications/NotificationHistoryItem"

const ProviderConfigSection = lazy(() => import("@/components/notifications/ProviderConfigSection").then(module => ({ default: module.ProviderConfigSection })))

// ─── Types ────────────────────────────────────────────────────────────────────


interface NotificationSettings {
  archive_start: boolean
  archive_complete: boolean
  archive_error: boolean
  temperature: boolean
  keep_awake_failure: boolean
  update: boolean
  drives: boolean
  rtc_battery: boolean
  music_sync: boolean
  keep_accessory: boolean
  storage_repair: boolean
}

interface HistoryResponse {
  events: NotificationEvent[]
  total: number
  limit: number
  offset: number
}

type Tab = "history" | "events" | "delivery"

// ─── Helpers ──────────────────────────────────────────────────────────────────

const NOTIFICATION_TYPES = [
  { key: "archive_start", label: "Archive Started", description: "When file archiving begins", icon: ArchiveIcon },
  { key: "archive_complete", label: "Archive Complete", description: "When file archiving finishes successfully", icon: CheckCircleIcon },
  { key: "archive_error", label: "Archive Errors", description: "When archiving encounters errors", icon: CancelIcon },
  { key: "temperature", label: "Temperature Alerts", description: "When CPU temperature exceeds safe thresholds", icon: DeviceThermostatIcon },
  { key: "keep_awake_failure", label: "Keep-Awake Failures", description: "When Sentry Mode keep-awake fails after retries", icon: BoltIcon },
  { key: "update", label: "Update Available", description: "When a new software update is detected", icon: DownloadIcon },
  { key: "drives", label: "New Drives Detected", description: "When new TeslaCam drives are mapped", icon: HardDriveIcon },
  { key: "rtc_battery", label: "RTC Battery Warning", description: "When the real-time clock battery is low or missing", icon: BatteryAndroidFrameFullIcon },
  { key: "music_sync", label: "Music Sync", description: "When music files finish syncing to USB", icon: MusicNoteIcon },
  { key: "keep_accessory", label: "Keep Accessory", description: "When the Pi releases 12V accessory power at home and is about to go offline", icon: PowerIcon },
  { key: "storage_repair", label: "Storage Auto Repair", description: "When boot-time repair of dashcam storage succeeds, fails, or needs manual action", icon: BuildIcon },
] as const

function typeIcon(type: string) {
  const found = NOTIFICATION_TYPES.find(t => t.key === type)
  return found?.icon || NotificationsIcon
}

function typeLabel(type: string): string {
  const found = NOTIFICATION_TYPES.find(t => t.key === type)
  return found?.label || type.replace(/_/g, " ").replace(/\b\w/g, c => c.toUpperCase())
}

function typeColor(type: string): string {
  switch (type) {
    case "archive_start": return "text-blue-400"
    case "archive_complete": return "text-emerald-400"
    case "archive_error": return "text-red-400"
    case "temperature": return "text-orange-400"
    case "keep_awake_failure": return "text-amber-400"
    case "update": return "text-cyan-400"
    case "drives": return "text-violet-400"
    case "rtc_battery": return "text-yellow-400"
    case "music_sync": return "text-pink-400"
    case "storage_repair": return "text-rose-400"
    default: return "text-slate-400"
  }
}

function typeBgColor(type: string): string {
  switch (type) {
    case "archive_start": return "bg-blue-500/15"
    case "archive_complete": return "bg-emerald-500/15"
    case "archive_error": return "bg-red-500/15"
    case "temperature": return "bg-orange-500/15"
    case "keep_awake_failure": return "bg-amber-500/15"
    case "update": return "bg-cyan-500/15"
    case "drives": return "bg-violet-500/15"
    case "rtc_battery": return "bg-yellow-500/15"
    case "music_sync": return "bg-pink-500/15"
    case "storage_repair": return "bg-rose-500/15"
    default: return "bg-white/5"
  }
}


function relativeTime(ts: number): string {
  const now = Math.floor(Date.now() / 1000)
  const diff = now - ts
  if (diff < 60) return "just now"
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`
  if (diff < 604800) return `${Math.floor(diff / 86400)}d ago`
  return new Date(ts * 1000).toLocaleDateString()
}


// ─── Main component ───────────────────────────────────────────────────────────

export default function Notifications() {
  const [activeTab, setActiveTab] = useState<Tab>(() => {
    const tab = new URLSearchParams(window.location.search).get("tab")
    return tab === "delivery" ? "delivery" : tab === "events" || tab === "settings" ? "events" : "history"
  })
  function selectTab(tab: Tab) {
    if (tab !== activeTab) {
      if (tab === "history") { setLoading(true); setHistoryError("") }
      if (tab === "events") { setSettingsLoading(true); setSettingsError("") }
    }
    setActiveTab(tab)
    const url = new URL(window.location.href)
    url.searchParams.set("tab", tab)
    window.history.replaceState(window.history.state, "", url)
  }
  const [events, setEvents] = useState<NotificationEvent[]>([])
  const [total, setTotal] = useState(0)
  const [loading, setLoading] = useState(true)
  const [historyError, setHistoryError] = useState("")
  const [settings, setSettings] = useState<NotificationSettings | null>(null)
  const [savingSettings, setSavingSettings] = useState(false)
  const [settingsError, setSettingsError] = useState("")
  const [settingsLoading, setSettingsLoading] = useState(true)
  const settingsSaving = useRef(false)
  const historyRequest = useRef<AbortController | null>(null)
  const settingsRequest = useRef<AbortController | null>(null)
  const [typeFilter, setTypeFilter] = useState<string>("")
  const [confirmClear, setConfirmClear] = useState(false)
  const [offset, setOffset] = useState(0)
  const PAGE_SIZE = 50

  const loadHistory = useCallback(async (currentOffset = 0, filter = "") => {
    historyRequest.current?.abort()
    const controller = new AbortController()
    historyRequest.current = controller
    const params = new URLSearchParams({ limit: String(PAGE_SIZE), offset: String(currentOffset) })
    if (filter) params.set("type", filter)
    return fetch(`/api/notifications/history?${params}`, { signal: controller.signal })
      .then(async res => {
        if (!res.ok) throw new Error("Failed to load history")
        const data: HistoryResponse = await res.json()
        if (controller.signal.aborted) return
        setEvents(data.events || [])
        setTotal(data.total)
      })
      .catch(() => { if (!controller.signal.aborted) setHistoryError("Could not load notification history. Retry, or check the device logs. Clear All will permanently reset history.") })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
  }, [])

  const loadSettings = useCallback(async () => {
    if (settingsSaving.current) return
    settingsRequest.current?.abort()
    const controller = new AbortController()
    settingsRequest.current = controller
    return fetch("/api/notifications/settings", { signal: controller.signal })
      .then(async res => {
        if (!res.ok) throw new Error("Failed to load settings")
        const data: NotificationSettings = await res.json()
        if (!NOTIFICATION_TYPES.every(({ key }) => typeof data[key] === "boolean")) throw new Error("Invalid event settings")
        if (!controller.signal.aborted) setSettings(data)
      })
      .catch(() => { if (!controller.signal.aborted) setSettingsError("Could not load event settings. Retry before making changes.") })
      .finally(() => { if (!controller.signal.aborted) setSettingsLoading(false) })
  }, [])

  useEffect(() => {
    if (activeTab === "history") void loadHistory(offset, typeFilter)
    return () => { historyRequest.current?.abort() }
  }, [activeTab, offset, typeFilter, loadHistory])
  useEffect(() => {
    if (activeTab === "events") void loadSettings()
    return () => { settingsRequest.current?.abort() }
  }, [activeTab, loadSettings])

  // Save settings
  async function handleToggle(key: keyof NotificationSettings) {
    if (!settings || settingsSaving.current || settingsLoading || settingsError) return
    settingsSaving.current = true
    settingsRequest.current?.abort()
    const updated = { ...settings, [key]: !settings[key] }
    setSettings(updated)
    setSavingSettings(true)
    try {
      const res = await fetch("/api/notifications/settings", {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(updated),
      })
      if (!res.ok) {
        setSettings(settings)
        setSettingsError("Could not save event settings. Retry to reload the saved values.")
      }
    } catch {
      setSettings(settings)
      setSettingsError("Could not save event settings. Retry to reload the saved values.")
    } finally {
      settingsSaving.current = false
      setSavingSettings(false)
    }
  }

  // Clear all history
  async function handleClearAll() {
    if (!confirmClear) {
      setConfirmClear(true)
      setTimeout(() => setConfirmClear(false), 5000)
      return
    }
    try {
      const response = await fetch("/api/notifications/history", { method: "DELETE" })
      if (!response.ok) throw new Error("Clear failed")
      historyRequest.current?.abort()
      setLoading(false)
      setEvents([])
      setTotal(0)
      setOffset(0)
      setHistoryError("")
    } catch {
      setHistoryError("Could not clear notification history. Retry or check the device logs.")
    }
    setConfirmClear(false)
  }

  // Delete single notification
  async function handleDeleteOne(id: string) {
    try {
      const response = await fetch(`/api/notifications/history/${id}`, { method: "DELETE" })
      if (!response.ok) throw new Error("Dismiss failed")
      setEvents(prev => prev.filter(e => e.id !== id))
      setTotal(prev => Math.max(0, prev - 1))
    } catch {
      setHistoryError("Could not dismiss this notification. Retry or check the device logs.")
    }
  }

  // Filter change
  function handleFilterChange(filter: string) {
    if (filter !== typeFilter || offset !== 0) { setLoading(true); setHistoryError("") }
    setTypeFilter(filter)
    setOffset(0)
  }

  // Pagination
  function handlePage(direction: "next" | "prev") {
    const newOffset = direction === "next" ? offset + PAGE_SIZE : Math.max(0, offset - PAGE_SIZE)
    if (newOffset !== offset) { setLoading(true); setHistoryError("") }
    setOffset(newOffset)
  }

  const TABS = [
    { id: "history" as const, label: "History", icon: ScheduleIcon },
    { id: "events" as const, label: "Events", icon: SettingsIcon },
    { id: "delivery" as const, label: "Delivery", icon: NotificationsIcon },
  ]

  return (
    <div className="space-y-6">
      {/* Header */}
      <div>
        <h1 className="text-2xl font-bold text-slate-100">Notifications</h1>
        <p className="mt-1 text-sm text-slate-500">
          History, alert types and delivery settings
        </p>
      </div>

      {/* Tab bar */}
      <div className="tab-bar">
        {TABS.map((tab) => (
          <button
            key={tab.id}
            onClick={() => selectTab(tab.id)}
            aria-pressed={activeTab === tab.id}
            className={cn("tab-item flex items-center justify-center gap-2", activeTab === tab.id && "active")}
          >
            <tab.icon className="h-3.5 w-3.5 hidden sm:block" />
            {tab.label}
          </button>
        ))}
      </div>

      {/* ── History Tab ──────────────────────────────────────────────── */}
      {activeTab === "history" && (
        <div className="space-y-4">
          {/* Toolbar */}
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            {/* Filter */}
            <div className="flex items-center gap-2">
              <FilterAltIcon className="h-4 w-4 text-slate-500" />
              <SelectMenu label="Notification type" value={typeFilter} onChange={handleFilterChange} options={[{ value: "", label: "All types" }, ...NOTIFICATION_TYPES.map(t => ({ value: t.key, label: t.label }))]} />
              {typeFilter && (
                <button
                  aria-label="Clear notification filter"
                  onClick={() => handleFilterChange("")}
                  className="rounded-md p-1 text-slate-500 transition-colors hover:bg-white/5 hover:text-slate-300"
                >
                  <CloseIcon className="h-3.5 w-3.5" />
                </button>
              )}
            </div>

            {/* Clear all */}
            <button
              onClick={handleClearAll}
              disabled={events.length === 0 && !historyError}
              className={cn(
                "flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-sm font-medium transition-colors",
                confirmClear
                  ? "bg-red-500/20 text-red-400 hover:bg-red-500/30"
                  : "border border-white/10 bg-white/5 text-slate-400 hover:bg-white/10 hover:text-slate-300",
                events.length === 0 && !historyError && "cursor-not-allowed opacity-50"
              )}
            >
              <DeleteIcon className="h-3.5 w-3.5" />
              {confirmClear ? "Click again to confirm" : "Clear All"}
            </button>
          </div>

          {/* Events list */}
          {loading && events.length === 0 ? (
            <div className="flex items-center justify-center py-16">
              <ProgressActivityIcon className="h-6 w-6 animate-spin text-blue-400" />
            </div>
          ) : historyError ? (
            <div role="alert" className="glass-card space-y-3 p-5 text-sm text-amber-300">
              <p>{historyError}</p>
              <button onClick={() => { setLoading(true); setHistoryError(""); void loadHistory(offset, typeFilter) }} className="rounded-lg border border-white/10 px-3 py-1.5 text-slate-200">Retry</button>
            </div>
          ) : events.length === 0 ? (
            <div className="glass-card flex flex-col items-center justify-center py-16 text-center">
              <div className="flex h-14 w-14 items-center justify-center rounded-2xl bg-white/5">
                <NotificationsOffIcon className="h-7 w-7 text-slate-600" />
              </div>
              <p className="mt-4 text-sm font-medium text-slate-400">No notifications yet</p>
              <p className="mt-1 text-xs text-slate-600">
                {typeFilter ? "No notifications match this filter" : "Notification events will appear here as they occur"}
              </p>
            </div>
          ) : (
            <div className="space-y-2">
              {events.map((event) => {
                const Icon = typeIcon(event.type)
                const color = typeColor(event.type)
                const bg = typeBgColor(event.type)
                return (
                  <NotificationHistoryItem
                    key={event.id} event={event} onDismiss={handleDeleteOne}
                    icon={<Icon className={cn("h-4.5 w-4.5", color)} />}
                    label={typeLabel(event.type)} color={color} background={bg}
                    timeLabel={relativeTime(event.ts)}
                  />
                )
              })}
            </div>
          )}

          {/* Pagination */}
          {total > PAGE_SIZE && (
            <div className="flex items-center justify-between pt-2">
              <button
                onClick={() => handlePage("prev")}
                disabled={offset === 0}
                className="rounded-lg border border-white/10 bg-white/5 px-3 py-1.5 text-sm text-slate-400 transition-colors hover:bg-white/10 disabled:cursor-not-allowed disabled:opacity-50"
              >
                Previous
              </button>
              <span className="text-xs text-slate-600">
                {offset + 1}–{Math.min(offset + PAGE_SIZE, total)} of {total}
              </span>
              <button
                onClick={() => handlePage("next")}
                disabled={offset + PAGE_SIZE >= total}
                className="rounded-lg border border-white/10 bg-white/5 px-3 py-1.5 text-sm text-slate-400 transition-colors hover:bg-white/10 disabled:cursor-not-allowed disabled:opacity-50"
              >
                Next
              </button>
            </div>
          )}
        </div>
      )}

      {activeTab === "delivery" && <div className="space-y-4">
        <MobileNotificationsSection />
        <Suspense fallback={<p role="status" className="text-sm text-slate-400">Loading providers…</p>}><ProviderConfigSection /></Suspense>
      </div>}
      {activeTab === "events" && settingsError && <div role="alert" className="glass-card p-4 text-sm text-rose-300">
        <p>{settingsError}</p>
        <button type="button" className="mt-2 text-blue-400" disabled={savingSettings} onClick={() => { setSettingsLoading(true); setSettingsError(""); void loadSettings() }}>Retry</button>
      </div>}
      {activeTab === "events" && !settings && !settingsError && <p role="status" className="text-sm text-slate-400">Loading event settings…</p>}
      {activeTab === "events" && settings && (
        <div className="space-y-4">
          <p className="text-sm text-slate-400">Choose which events send alerts to your configured providers.</p>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            {NOTIFICATION_TYPES.map((nt) => {
              const Icon = nt.icon
              const enabled = settings[nt.key as keyof NotificationSettings]
              const color = typeColor(nt.key)
              const bg = typeBgColor(nt.key)
              return (
                <div
                  key={nt.key}
                  className={cn(
                    "glass-card flex items-center gap-4 p-4 transition-colors",
                    enabled ? "border-white/10" : "border-white/5 opacity-60"
                  )}
                >
                  <div className={cn("flex h-10 w-10 shrink-0 items-center justify-center rounded-xl", enabled ? bg : "bg-white/5")}>
                    <Icon className={cn("h-5 w-5", enabled ? color : "text-slate-600")} />
                  </div>
                  <div className="min-w-0 flex-1">
                    <p className="text-sm font-medium text-slate-200">{nt.label}</p>
                    <p className="text-xs text-slate-500 leading-relaxed">{nt.description}</p>
                  </div>
                  <button
                    onClick={() => handleToggle(nt.key as keyof NotificationSettings)}
                    disabled={savingSettings || settingsLoading || !!settingsError}
                    role="switch"
                    aria-checked={enabled}
                    aria-label={nt.label}
                    className={cn(
                      "relative h-6 w-11 shrink-0 rounded-full transition-colors",
                      enabled ? "bg-blue-500" : "bg-white/10"
                    )}
                  >
                    <span
                      className={cn(
                        "absolute top-0.5 left-0.5 h-5 w-5 rounded-full bg-white transition-transform shadow-sm",
                        enabled && "translate-x-5"
                      )}
                    />
                  </button>
                </div>
              )
            })}
          </div>
        </div>
      )}
    </div>
  )
}

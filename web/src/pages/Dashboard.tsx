import { useEffect, useId, useRef, useState } from "react"
import { Link } from "react-router-dom"
import {
  AirIcon,
  BoltIcon,
  CardiologyIcon,
  ChevronRightIcon,
  DeviceThermostatIcon,
  DownloadIcon,
  HardDriveIcon,
  LanIcon,
  PhotoCameraIcon,
  ScheduleIcon,
  TimerIcon,
  VitalSignsIcon,
  WarningIcon,
  WifiIcon,
  WifiOffIcon,
} from "@/components/icons"
import { DROPDOWN_OPTION, DROPDOWN_SURFACE, DROPDOWN_TRIGGER } from "@/components/ui/dropdownStyles"
import { api } from "@/lib/api"
import { CancelArchiveButton } from "@/components/dashboard/CancelArchiveButton"
import { useKeepAwake } from "@/hooks/useKeepAwake"
import { useAwayMode } from "@/hooks/useAwayMode"
import { useUpdateAvailable } from "@/hooks/useUpdateAvailable"
import { useWifiFirmware } from "@/hooks/useWifiFirmware"
import { WifiFirmwareModal } from "@/components/dashboard/WifiFirmwareModal"
import { fetchCurrentCharge } from "@/api/charging"
import type { CurrentCharge } from "@/types/charging"
import type { PiStatus, DriveStats, StorageBreakdown, ArchiveCycle } from "@/lib/api"
import { wsClient } from "@/lib/ws"
import { formatUptime, formatBytes, formatTemp } from "@/lib/utils"
import { useUnits } from "@/lib/units"
import { CloudStatusBar } from "@/components/CloudStatusBar"
import {
  CarStatusCard,
  type CarStatusSample,
} from "@/components/dashboard/CarStatusCard"
import { StatusTile, Row, TileDivider } from "@/components/ui/StatusTile"
import { BannerStack, type BannerItem } from "@/components/ui/Banner"
import { Pill, LiveDot } from "@/components/ui/Pill"
import type { Halo } from "@/components/ui/StatusTile"
import type { TireHistoryResponse } from "@/components/dashboard/TirePressureCard"
import type { BleHealth } from "@/lib/bleHealth"

function getTempHalo(milliC: number): Halo {
  if (milliC <= 0) return "blue"
  if (milliC < 55000) return "accent"
  if (milliC < 70000) return "amber"
  return "red"
}

function getTempColor(milliC: number): string {
  if (milliC < 55000) return "oklch(0.82 0.18 150)"
  if (milliC < 70000) return "#fbbf24"
  return "#f87171"
}

function getStorageHalo(health: PiStatus["storage_health"]): Halo {
  if (health?.state === "fail") return "red"
  if (health?.state === "warn") return "amber"
  return "accent"
}

function formatThroughput(bps: number, state?: string): string {
  if (state === "sampling") return "Sampling…"
  if (state === "unavailable" || state === "disconnected") return "Unavailable"
  if (state === "stale") return "Waiting for sample"
  if (bps >= 1_000_000) return `${(bps / 1_000_000).toFixed(1)} Mbps`
  if (bps >= 1_000) return `${Math.round(bps / 1_000)} Kbps`
  return bps > 0 ? "< 1 Kbps" : "0 Mbps"
}

function getWifiStrengthBars(strength: string): number {
  if (!strength) return 0
  const parts = strength.split("/")
  if (parts.length !== 2) return 0
  const ratio = parseInt(parts[0]) / parseInt(parts[1])
  if (ratio > 0.75) return 4
  if (ratio > 0.5) return 3
  if (ratio > 0.25) return 2
  return 1
}

// Mini 4-bar signal indicator. Filled bars get the tile's accent colour;
// the rest are a muted slate so the gauge reads at a glance.
function WifiBars({ bars }: { bars: number }) {
  return (
    <span className="inline-flex items-end gap-[2px] align-middle" aria-label={`${bars}/4 bars`}>
      {[1, 2, 3, 4].map((n) => (
        <span
          key={n}
          className={n <= bars ? "bg-emerald-400" : "bg-slate-700"}
          style={{ width: 3, height: 3 + n * 2, borderRadius: 1 }}
        />
      ))}
    </span>
  )
}

interface ProcessProgress {
  current: number
  total: number
  etaSeconds?: number | null
  etaState?: string
  sampledAt?: number
}

function progressEstimate(progress: ProcessProgress): string {
  if (progress.sampledAt && Date.now() / 1000 - progress.sampledAt > 45) return "Waiting for update"
  if (progress.etaState === "stalled") return "Waiting for progress"
  if (progress.etaState === "finalizing" || progress.etaState === "complete") return "Finishing this phase…"
  const seconds = progress.etaSeconds
  if (seconds == null || !Number.isFinite(seconds)) return progress.etaState === "estimating" ? "Estimating…" : "Estimate unavailable"
  if (seconds < 60) return "Less than a minute remaining"
  if (seconds < 3600) return `About ${Math.round(seconds / 60)} min remaining`
  return `About ${(seconds / 3600).toFixed(1)} h remaining`
}

export default function Dashboard() {
  const [status, setStatus] = useState<PiStatus | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [lastUpdated, setLastUpdated] = useState<number | null>(null)
  const [uptime, setUptime] = useState(0)
  const [driveStats, setDriveStats] = useState<DriveStats | null>(null)
  const [storageBreakdown, setStorageBreakdown] =
    useState<StorageBreakdown | null>(null)
  const [archiveProgress, setArchiveProgress] = useState<ProcessProgress | null>(null)
  const [archiveCycle, setArchiveCycle] = useState<ArchiveCycle | null>(null)
  const [processing, setProcessing] = useState(false)
  const [processProgress, setProcessProgress] = useState<ProcessProgress | null>(null)
  // Units come from the shared store — coherent defaults and live-synced with
  // the Settings → Display & Units controls. systemTempF is the independent
  // System-tile CPU unit; tempF/km drive the dashboard temps and distances.
  const { tempF: useFahrenheit, systemTempF: systemUseFahrenheit, km: metric } = useUnits()
  const [rtcWarning, setRtcWarning] = useState<string | null>(null)
  // null = still probing, then either the response or `{points: []}`.
  // The card stays unmounted until points.length > 0, so vendor-charts
  // never loads for users without Tesla BLE telemetry.
  const [tireHistory, setTireHistory] = useState<TireHistoryResponse | null>(null)
  // Latest BLE-derived car-state snapshot for the CarStatusCard.
  // Polled at 30s — the BLE sampler itself runs once a minute while
  // parked + awake, so anything faster on the UI side is wasted.
  const [carStatusSample, setCarStatusSample] = useState<CarStatusSample | null>(null)
  const [bleHealth, setBleHealth] = useState<BleHealth | null>(null)
  const [bleHealthConfigured, setBleHealthConfigured] = useState(false)
  // Live charge status for the CarStatusCard battery chip.
  const [currentCharge, setCurrentCharge] = useState<CurrentCharge | null>(null)
  const latestDriveEnd = driveStats?.latest_drive_end ?? null
  const [activeChimeName, setActiveChimeName] = useState<string | null>(null)

  const updateInfo = useUpdateAvailable()
  const wifiFirmware = useWifiFirmware()
  const [wifiFwOpen, setWifiFwOpen] = useState(false)
  const { status: awayStatus } = useAwayMode()
  const { mode: keepAwakeMode } = useKeepAwake()

  useEffect(() => {
    let mounted = true
    const controller = new AbortController()
    const pending = new Set<string>()
    let processEventVersion = 0
    async function withDeadline<T>(read: (signal: AbortSignal) => Promise<T>): Promise<T> {
      const request = new AbortController()
      const abort = () => request.abort()
      controller.signal.addEventListener("abort", abort, { once: true })
      const timeout = setTimeout(abort, 15_000)
      try { return await read(request.signal) }
      finally {
        clearTimeout(timeout)
        controller.signal.removeEventListener("abort", abort)
      }
    }
    const pollFetch = (url: string) => withDeadline(signal => fetch(url, { signal }))

    async function fetchStatus() {
      if (pending.has("status")) return
      pending.add("status")
      try {
        const data = await withDeadline(api.getStatus)
        if (!mounted) return
        setStatus(data)
        setLastUpdated(Date.now())
        setUptime(parseFloat(data.uptime))
        setError(null)
      } catch {
        if (mounted) setError("Unable to connect to Sentry USB")
      } finally { pending.delete("status") }
    }

    async function fetchDriveStats() {
      if (pending.has("stats")) return
      pending.add("stats")
      const eventVersion = processEventVersion
      try {
        const [stats, progress] = await Promise.allSettled([
          withDeadline(api.getDriveStats), withDeadline(api.getDriveStatus),
        ])
        if (!mounted) return
        if (stats.status === "fulfilled") setDriveStats(stats.value)
        if (progress.status !== "fulfilled") return
        const driveStatus = progress.value
        setArchiveCycle(driveStatus.archive_cycle ?? null)
        if (eventVersion === processEventVersion) {
          setProcessing(driveStatus.running)
          setProcessProgress(driveStatus.running && (driveStatus.process_total ?? 0) > 0 ? {
            current: driveStatus.process_current ?? 0, total: driveStatus.process_total!,
            etaSeconds: driveStatus.process_eta_seconds, etaState: driveStatus.process_eta_state,
            sampledAt: driveStatus.process_sampled_at,
          } : null)
        }
        setArchiveProgress(driveStatus.phase === "archiving" && driveStatus.total != null ? {
          current: driveStatus.current ?? 0, total: driveStatus.total,
          etaSeconds: driveStatus.eta_seconds, etaState: driveStatus.eta_state, sampledAt: driveStatus.sampled_at,
        } : null)
      } finally { pending.delete("stats") }
    }

    async function fetchStorageBreakdown() {
      if (pending.has("storage")) return
      pending.add("storage")
      try {
        const data = await withDeadline(api.getStorageBreakdown)
        if (mounted) setStorageBreakdown(data)
      } catch {
        /* non-critical */
      } finally { pending.delete("storage") }
    }

    fetchStatus()
    fetchDriveStats()
    fetchStorageBreakdown()

    pollFetch("/api/system/rtc-status")
      .then((r) => r.json())
      .then((rtc) => {
        if (mounted && rtc.is_pi5 && !rtc.rtc_healthy && rtc.battery_warning) {
          setRtcWarning(rtc.battery_warning)
        }
      })
      .catch(() => {})

    // Tire history: probe once at mount. The card only mounts (and
    // pulls in recharts) when the response has samples. Empty
    // response = the user hasn't paired BLE telemetry; we just hide
    // the card to keep the dashboard clean.
    pollFetch("/api/telemetry/tire-history?days=30")
      .then((r) => (r.ok ? r.json() : { points: [], days: 30 }))
      .then((d: TireHistoryResponse) => { if (mounted) setTireHistory(d) })
      .catch(() => { if (mounted) setTireHistory({ points: [], days: 30 }) })

    // Latest BLE sample drives the CarStatusCard's battery + temps +
    // tire-health summary. Hide-on-error since this is purely an
    // overview tile; the user can still pair BLE from Settings.
    async function fetchCarStatusSample() {
      if (pending.has("car")) return
      pending.add("car")
      try {
        const [sampleRes, healthRes] = await Promise.all([
          pollFetch("/api/system/ble-latest-sample"),
          pollFetch("/api/system/ble-connected"),
        ])
        if (!mounted) return
        if (healthRes.ok) {
          const d = (await healthRes.json()) as {
            configured?: boolean
            health?: BleHealth
          }
          if (mounted) setBleHealthConfigured(Boolean(d.configured))
          if (mounted) setBleHealth(d.health ?? null)
        } else if (mounted) {
          setBleHealthConfigured(false)
          setBleHealth(null)
        }
        if (sampleRes.ok) {
          const d = (await sampleRes.json()) as CarStatusSample
          if (mounted) setCarStatusSample(d)
        } else if (mounted) {
          setCarStatusSample(null)
        }
      } catch {
        if (mounted) {
          setBleHealthConfigured(false)
          setBleHealth(null)
          setCarStatusSample(null)
        }
      } finally { pending.delete("car") }
    }

    async function fetchActiveChime() {
      if (pending.has("chime")) return
      pending.add("chime")
      try {
        const res = await pollFetch("/api/lockchime/list")
        if (!res.ok) return
        const d = (await res.json()) as {
          active_set?: boolean
          active_name?: string
        }
        if (!mounted) return
        setActiveChimeName(d.active_set && d.active_name ? d.active_name : null)
      } catch {
        /* non-critical */
      } finally { pending.delete("chime") }
    }

    async function fetchChargeStatus() {
      if (pending.has("charge")) return
      pending.add("charge")
      try {
        const c = await withDeadline(fetchCurrentCharge)
        if (mounted) setCurrentCharge(c)
      } catch {
        // Keep the last observed car state through short reconnects.
      } finally { pending.delete("charge") }
    }

    fetchCarStatusSample()
    fetchActiveChime()
    fetchChargeStatus()
    // Pause every poller while the tab is hidden (phone in a pocket, a
    // backgrounded tab) so the dashboard stops hitting the Pi every 1-2s
    // and draining the phone battery for data nobody's looking at. The
    // `visibilitychange` handler below refreshes immediately on return so
    // the tiles don't sit stale waiting for the next tick.
    const carStatusInterval = setInterval(() => {
      if (!document.hidden) fetchCarStatusSample()
    }, 30_000)
    const chargeInterval = setInterval(() => {
      if (!document.hidden) fetchChargeStatus()
    }, 30_000)
    const chimeInterval = setInterval(() => {
      if (!document.hidden) fetchActiveChime()
    }, 300_000)

    // Status drives the live-tile values (CPU, mem, temp). 2s is fast
    // enough that a glance still feels real-time and halves the
    // server hits vs the previous 1s cadence. The uptime tile uses a
    // separate local 1s interval below so the seconds counter still
    // advances smoothly between server polls.
    const statusInterval = setInterval(() => {
      if (!document.hidden) fetchStatus()
    }, 2000)
    const statsInterval = setInterval(() => {
      if (!document.hidden) fetchDriveStats()
    }, 5000)
    const storageInterval = setInterval(() => {
      if (!document.hidden) fetchStorageBreakdown()
    }, 10000)
    // Local-only counter, but still skip it while hidden so React isn't
    // re-rendering the dashboard once a second for a backgrounded tab.
    const uptimeInterval = setInterval(() => {
      if (!document.hidden) setUptime((p) => p + 1)
    }, 1000)

    // Snap the live tiles back to current the moment the tab is shown
    // again, rather than waiting up to 30s for the slow intervals.
    const onVisible = () => {
      if (document.hidden) return
      fetchStatus()
      fetchDriveStats()
      fetchStorageBreakdown()
      fetchCarStatusSample()
      fetchChargeStatus()
    }
    document.addEventListener("visibilitychange", onVisible)

    const unsubscribe = wsClient.subscribe("drive_process", (data) => {
      if (!mounted) return
      processEventVersion++
      const msg = data as { status: string; current?: number; processed?: number; total?: number; eta_seconds?: number | null; eta_state?: string; sampled_at?: number }
      if (msg.status === "started") {
        setProcessing(true)
        setProcessProgress(null)
      } else if (
        msg.status === "progress" &&
        (msg.current !== undefined || msg.processed !== undefined) &&
        msg.total !== undefined
      ) {
        setProcessing(true)
        setProcessProgress({ current: msg.current ?? msg.processed ?? 0, total: msg.total, etaSeconds: msg.eta_seconds, etaState: msg.eta_state, sampledAt: msg.sampled_at })
      } else if (msg.status === "complete" || msg.status === "error" || msg.status === "cancelled") {
        setProcessing(false)
        setProcessProgress(null)
        fetchDriveStats()
      }
    })

    return () => {
      mounted = false
      controller.abort()
      clearInterval(statusInterval)
      clearInterval(statsInterval)
      clearInterval(storageInterval)
      clearInterval(uptimeInterval)
      clearInterval(carStatusInterval)
      clearInterval(chargeInterval)
      clearInterval(chimeInterval)
      document.removeEventListener("visibilitychange", onVisible)
      unsubscribe()
    }
  }, [])

  if (error && !status) {
    return (
      <div className="flex flex-col items-center justify-center py-20">
        <VitalSignsIcon className="mb-4 h-12 w-12 text-slate-600" />
        <p className="text-lg font-medium text-slate-400">{error}</p>
        <p className="mt-1 text-sm text-slate-600">
          Make sure the Sentry USB API server is running
        </p>
      </div>
    )
  }

  if (!status) {
    return (
      <div className="space-y-4">
        <h1 className="text-2xl font-bold text-slate-100">Dashboard</h1>
        <div className="tile-grid">
          {[...Array(4)].map((_, i) => (
            <div key={i} className="glass-card h-32 animate-pulse" />
          ))}
        </div>
      </div>
    )
  }

  // Build banner stack — priority sorted (warn > update).
  const banners: BannerItem[] = []
  if (rtcWarning) {
    banners.push({
      id: "rtc",
      kind: "warn",
      icon: <WarningIcon className="h-4 w-4" />,
      title: "RTC Battery Warning",
      sub: rtcWarning,
    })
  }
  if (wifiFirmware.installing) {
    // Progress has to be reachable even if the modal was closed, otherwise a
    // user who dismissed it assumes the update finished.
    banners.push({
      id: "wifi-firmware-running",
      kind: "info",
      icon: <WifiIcon className="h-4 w-4" />,
      title: `Updating Wi-Fi firmware… ${wifiFirmware.status?.install?.progress ?? 0}%`,
      sub: wifiFirmware.status?.install?.message || "Working…",
      action: (
        <button
          onClick={() => setWifiFwOpen(true)}
          className="action-chip action-chip--accent shrink-0"
        >
          View <ChevronRightIcon className="h-3.5 w-3.5" />
        </button>
      ),
    })
  }
  if (wifiFirmware.offerRevert) {
    banners.push({
      id: "wifi-firmware-revert",
      kind: "info",
      icon: <WifiIcon className="h-4 w-4" />,
      title: `Wi-Fi firmware updated to ${wifiFirmware.status?.target_version ?? ""}`,
      // Only ask for a restart while one is genuinely outstanding.
      sub: wifiFirmware.status?.reboot_pending
        ? "Reboot to finish. Reloading the radio in place can leave Wi-Fi slower than normal until the Pi restarts."
        : "You can put the previous firmware back if anything looks wrong.",
      action: (
        <div className="flex shrink-0 gap-2">
          <button onClick={() => setWifiFwOpen(true)} className="action-chip">
            {wifiFirmware.status?.reboot_pending ? "Finish" : "Revert"}
          </button>
          <button onClick={wifiFirmware.dismissRevert} className="action-chip">
            Dismiss
          </button>
        </div>
      ),
    })
  }
  if (wifiFirmware.show) {
    // Stronger wording when the fault's fingerprint is actually in this
    // device's kernel log, rather than a generic "you might hit this".
    const seen = wifiFirmware.status?.symptom_detected
    banners.push({
      id: "wifi-firmware",
      kind: "warn",
      icon: <WifiIcon className="h-4 w-4" />,
      title: seen
        ? "Wi-Fi firmware issue detected"
        : "Wi-Fi firmware update available",
      sub: seen
        ? "This Pi hit the Wi-Fi fault that slows archiving and breaks Bluetooth keep-awake."
        : "A newer Broadcom firmware fixes slow archiving and Bluetooth drop-outs.",
      action: (
        <button
          onClick={() => setWifiFwOpen(true)}
          className="action-chip action-chip--accent shrink-0"
        >
          Review <ChevronRightIcon className="h-3.5 w-3.5" />
        </button>
      ),
    })
  }
  if (updateInfo.available) {
    banners.push({
      id: "update",
      kind: "update",
      icon: <DownloadIcon className="h-4 w-4" />,
      title: `Update Available${
        updateInfo.latestVersion ? `: ${updateInfo.latestVersion}` : ""
      }`,
      sub: "Go to Settings to install",
      action: (
        <Link
          to="/settings?tab=Device"
          className="action-chip action-chip--accent shrink-0"
        >
          Install <ChevronRightIcon className="h-3.5 w-3.5" />
        </Link>
      ),
    })
  }

  const isAwayActive = awayStatus.state === "active"

  return (
    <div className="space-y-3">
      <div>
        <h1 className="text-2xl font-bold text-slate-100">Dashboard</h1>
        <p className="mt-0.5 text-sm text-slate-500">System overview and status</p>
      </div>

      {error && (
        <div role="status" className="rounded-xl border border-amber-500/20 bg-amber-500/10 px-4 py-3 text-sm text-amber-200">
          Reconnecting · showing the last update{lastUpdated ? ` from ${new Date(lastUpdated).toLocaleTimeString()}` : ""}
        </div>
      )}
      <BannerStack banners={banners} />

      {wifiFwOpen && wifiFirmware.status && (
        <WifiFirmwareModal
          status={wifiFirmware.status}
          onClose={() => setWifiFwOpen(false)}
          onRefresh={wifiFirmware.refresh}
        />
      )}

      <CloudStatusBar />

      <ActivityTile driveStats={driveStats} archiveCycle={archiveCycle} archiveProgress={archiveProgress}
        processProgress={processProgress} processing={processing} metric={metric} status={status} />

      <div className="dashboard-status-grid grid items-stretch gap-3 md:grid-cols-2 xl:grid-cols-3">
        <SystemTile status={status} uptime={uptime} useFahrenheit={systemUseFahrenheit} keepAwakeIdle={keepAwakeMode === ""} />
        <NetworkTile status={status} />
        <StorageTile status={status} breakdown={storageBreakdown} />
        {isAwayActive && <AwayModeTile />}
      </div>

      {(carStatusSample?.ts != null ||
        currentCharge?.soc != null ||
        (bleHealthConfigured && bleHealth != null)) && (
        <CarStatusCard
          sample={carStatusSample}
          bleHealth={bleHealth}
          latestDriveEnd={latestDriveEnd}
          tireHistory={tireHistory ?? undefined}
          useFahrenheit={useFahrenheit}
          metric={metric}
          currentCharge={currentCharge}
          lockChimeName={activeChimeName}
        />
      )}
    </div>
  )
}

// ─── Tiles ──────────────────────────────────────────────────────────────────

function SystemTile({
  status,
  uptime,
  useFahrenheit,
  keepAwakeIdle,
}: {
  status: PiStatus
  uptime: number
  useFahrenheit: boolean
  keepAwakeIdle: boolean
}) {
  const cpuTemp = parseInt(status.cpu_temp)
  return (
    <StatusTile
      icon={<VitalSignsIcon className="h-4 w-4" />}
      halo={getTempHalo(cpuTemp)}
      title="System"
    >
      <Row
        icon={<ScheduleIcon className="h-3.5 w-3.5" />}
        label="Uptime"
        value={formatUptime(uptime)}
      />
      <Row
        icon={<DeviceThermostatIcon className="h-3.5 w-3.5" />}
        label="CPU"
        value={cpuTemp > 0 ? formatTemp(cpuTemp, useFahrenheit) : "N/A"}
        valueColor={cpuTemp > 0 ? getTempColor(cpuTemp) : undefined}
      />
      {status.fan_speed && (
        <Row
          icon={<AirIcon className="h-3.5 w-3.5" />}
          label="Fan"
          value={`${status.fan_speed} RPM`}
        />
      )}
      {/* Three-state: "Connected" needs the host link up ("configured"),
          not just the gadget bound in configfs — a bound gadget with a
          dead link is exactly how the car shows an X while the old
          two-state pill stayed green. udc_state is absent on older
          backends; treat absent as link-unknown and keep the old view.
          Label and color derive from ONE state value so they can't drift. */}
      <Row
        icon={<HardDriveIcon className="h-3.5 w-3.5" />}
        label="USB Drives"
        {...(() => {
          const drivesState =
            status.drives_active !== "yes"
              ? "disconnected"
              : status.udc_state && status.udc_state !== "configured"
                ? "no-link"
                : "connected"
          const pill = {
            disconnected: { value: "Disconnected", valueColor: "#fbbf24" },
            "no-link": { value: "Waiting for car", valueColor: "#94a3b8" },
            connected: { value: "Connected", valueColor: "oklch(0.82 0.18 150)" },
          } as const
          return pill[drivesState]
        })()}
      />
      {keepAwakeIdle && (
        <Row
          icon={<CardiologyIcon className="h-3.5 w-3.5" />}
          label="Web app keep-awake"
          value={
            <Link
              to="/settings?tab=Device"
              className="text-blue-400 hover:text-blue-300"
            >
              Off
            </Link>
          }
        />
      )}
    </StatusTile>
  )
}

function NetworkTile({ status }: { status: PiStatus }) {
  const haveWifi = !!status.wifi_ssid
  const haveEth = !!status.ether_ip || (!!status.ether_speed && status.ether_speed !== "Unknown!")
  const rates = (kind: "wifi" | "ether") => (
    <div className="flex flex-wrap gap-x-4 gap-y-1 py-1 text-xs tabular-nums">
      <span className="text-emerald-400">↓ {formatThroughput(status[`${kind}_rx_bps`] ?? 0, status[`${kind}_rate_state`])}</span>
      <span className="text-sky-400">↑ {formatThroughput(status[`${kind}_tx_bps`] ?? 0, status[`${kind}_rate_state`])}</span>
    </div>
  )
  return (
    <StatusTile icon={haveWifi || haveEth ? <WifiIcon className="h-4 w-4" /> : <WifiOffIcon className="h-4 w-4" />} halo={haveWifi || haveEth ? "accent" : "amber"} title="Network">
      <Row icon={<WifiIcon className="h-3.5 w-3.5" />} label={status.wifi_ssid || "Wi-Fi"} sub={haveWifi ? <WifiBars bars={getWifiStrengthBars(status.wifi_strength)} /> : "Not connected"} />
      {haveWifi && rates("wifi")}
      <Row icon={<LanIcon className="h-3.5 w-3.5" />} label="Ethernet" sub={haveEth ? "Connected" : "Not connected"} />
      {haveEth && rates("ether")}
      {(haveWifi || haveEth) && (
        <details className="text-xs text-slate-400">
          <summary className="cursor-pointer py-2">Connection details</summary>
          {haveWifi && <>
            <Row label="Wi-Fi address" value={status.wifi_ip || "Unavailable"} />
            {status.wifi_signal_dbm != null && <Row label="Signal" value={`${status.wifi_signal_dbm} dBm`} />}
          </>}
          {haveEth && <>
            <Row label="Ethernet address" value={status.ether_ip || "Unavailable"} />
            <Row label="Link speed" value={status.ether_speed || "Unavailable"} />
          </>}
        </details>
      )}
    </StatusTile>
  )
}

function StorageTile({
  status,
  breakdown,
}: {
  status: PiStatus
  breakdown: StorageBreakdown | null
}) {
  const totalSpace = parseInt(status.total_space)
  const freeSpace = parseInt(status.free_space)
  const usedSpace = totalSpace - freeSpace
  const usedPct = totalSpace > 0 ? (usedSpace / totalSpace) * 100 : 0
  const usedPctStr = totalSpace > 0 ? `${Math.round(usedPct)}%` : "0%"
  const snaps = parseInt(status.num_snapshots)

  const segments = breakdown
    ? [
        { label: "Dashcam", size: breakdown.cam_size, color: "#3b82f6" },
        { label: "Music", size: breakdown.music_size, color: "#a855f7" },
        { label: "Lightshow", size: breakdown.lightshow_size, color: "#f59e0b" },
        { label: "Boombox", size: breakdown.boombox_size, color: "#ec4899" },
        { label: "Snapshots", size: breakdown.snapshots_size, color: "#6366f1" },
      ].filter((s) => s.size > 0)
    : []

  return (
    <StatusTile
      icon={<HardDriveIcon className="h-4 w-4" />}
      halo={getStorageHalo(status.storage_health)}
      title="Storage"
    >
      <div className="flex items-baseline gap-1.5">
        <span className="text-sm font-semibold text-slate-100">
          {formatBytes(usedSpace)}
        </span>
        <span className="text-[11px] text-slate-500">
          / {formatBytes(totalSpace)} · {usedPctStr} used
        </span>
      </div>
      <details className="text-xs text-slate-400">
        <summary className="cursor-pointer py-1">{status.storage_health?.message ?? "Storage managed automatically"}</summary>
        <p className="mt-1">Older snapshots are released when recording needs space. High usage is expected.</p>
      </details>
      {breakdown && segments.length > 0 ? (
        <>
          <div className="seg-bar">
            {segments.map((s) => (
              <div
                key={s.label}
                style={{
                  width: `${Math.max((s.size / breakdown.total_space) * 100, 0.5)}%`,
                  backgroundColor: s.color,
                }}
                title={`${s.label}: ${formatBytes(s.size)}`}
              />
            ))}
          </div>
          <div className="mt-1 flex flex-wrap gap-x-3 gap-y-1">
            {segments.map((s) => (
              <div key={s.label} className="flex items-center gap-1.5 text-[10px]">
                <span
                  className="inline-block h-1.5 w-1.5 rounded-full"
                  style={{ backgroundColor: s.color }}
                />
                <span className="text-slate-400">{s.label}</span>
                <span className="font-medium text-slate-300">
                  {formatBytes(s.size)}
                </span>
              </div>
            ))}
            <div className="flex items-center gap-1.5 text-[10px]">
              <span className="inline-block h-1.5 w-1.5 rounded-full bg-slate-700" />
              <span className="text-slate-400">Free</span>
              <span className="font-medium text-slate-300">
                {formatBytes(breakdown.free_space)}
              </span>
            </div>
          </div>
        </>
      ) : (
        <div className="bar">
          <div
            className="bg-gradient-to-r from-blue-500 to-blue-400"
            style={{ width: `${usedPct}%` }}
          />
        </div>
      )}
      <TileDivider />
      <Row
        icon={<PhotoCameraIcon className="h-3.5 w-3.5" />}
        label={`${snaps.toLocaleString()} snapshots`}
        sub={
          snaps > 0 &&
          Number.isFinite(parseInt(status.snapshot_oldest)) &&
          Number.isFinite(parseInt(status.snapshot_newest))
            ? `${new Date(
                parseInt(status.snapshot_oldest) * 1000
              ).toLocaleDateString()} → ${new Date(
                parseInt(status.snapshot_newest) * 1000
              ).toLocaleDateString()}`
            : "—"
        }
      />
    </StatusTile>
  )
}

function ActivityTile({ driveStats, archiveCycle, archiveProgress, processProgress, processing, metric, status }: {
  driveStats: DriveStats | null
  archiveCycle: ArchiveCycle | null
  archiveProgress: ProcessProgress | null
  processProgress: ProcessProgress | null
  processing: boolean
  metric: boolean
  status: PiStatus
}) {
  const keepAwake = useKeepAwake()
  const active = Boolean(archiveProgress || processing || archiveCycle)
  const progress = archiveProgress ?? processProgress
  const title = archiveProgress ? "Archiving footage" : processing ? "Processing clips" : archiveCycle ? "Archive in progress" : "Your library"
  const rateState = [status.wifi_rate_state, status.ether_rate_state].includes("live") ? "live" : status.wifi_ssid ? status.wifi_rate_state : status.ether_rate_state
  const upload = formatThroughput((status.wifi_tx_bps ?? 0) + (status.ether_tx_bps ?? 0), rateState)
  return (
    <section aria-label="Archive and library" className="glass-card p-4 sm:p-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h2 className="flex items-center gap-2 text-base font-semibold text-slate-100"><BoltIcon className="h-5 w-5 text-emerald-400" />{title}</h2>
        {active && <Pill kind="accent"><LiveDot />{archiveProgress ? "Transfer" : processing ? "Processing" : "Working"}</Pill>}
      </div>
      {active && (
        <div className="mt-4 space-y-3">
          {progress && progress.total > 0 ? <ProgressBlock current={progress.current} total={progress.total} eta={progressEstimate(progress)} color={archiveProgress ? "emerald" : "blue"} /> : <p className="text-sm text-slate-400">Preparing the next phase…</p>}
          <div className="flex flex-wrap items-center justify-between gap-3">
            <span className="text-xs text-slate-400">{archiveProgress ? `Upload ${upload} · estimate for transfer` : "Estimates apply to the current phase"}</span>
            <div className="min-w-40"><CancelArchiveButton key={archiveCycle?.id ?? "idle"} cycle={archiveCycle} unavailable={Boolean(archiveProgress)} /></div>
          </div>
        </div>
      )}
      {driveStats ? (
        <dl className="mt-4 grid grid-cols-2 gap-4 border-t border-white/10 pt-4 sm:grid-cols-4">
          <div><dt className="text-xs text-slate-400">Clips</dt><dd className="mt-1 font-semibold text-slate-100">{driveStats.processed_count.toLocaleString()}</dd></div>
          <div><dt className="text-xs text-slate-400">Drives</dt><dd className="mt-1 font-semibold text-slate-100"><Link to="/drives" className="hover:text-emerald-300">{driveStats.drives_count.toLocaleString()}</Link></dd></div>
          <div><dt className="text-xs text-slate-400">Distance</dt><dd className="mt-1 font-semibold text-slate-100">{(metric ? driveStats.total_distance_km : driveStats.total_distance_mi).toFixed(0)} {metric ? "km" : "mi"}</dd></div>
          <div><dt className="text-xs text-slate-400">FSD</dt><dd className="mt-1 font-semibold"><Link to="/fsd" className="text-emerald-400">{driveStats.fsd_percent}%</Link></dd></div>
        </dl>
      ) : <p className="mt-4 text-sm text-slate-400">Loading library totals…</p>}
      {(keepAwake.mode === "manual" || keepAwake.mode === "auto") && <div className="mt-4 border-t border-white/10 pt-3"><KeepAwakeInline keepAwake={keepAwake} /></div>}
    </section>
  )
}

function ProgressBlock({
  current,
  total,
  eta,
  color,
}: {
  current: number
  total: number
  eta: string | null
  color: "emerald" | "blue"
}) {
  const pct = Math.max(0, Math.min(100, (current / total) * 100))
  const grad =
    color === "emerald"
      ? "bg-gradient-to-r from-emerald-500 to-emerald-400"
      : "bg-gradient-to-r from-blue-500 to-blue-400"
  return (
    <>
      <div className="flex flex-wrap items-center justify-between gap-2 text-sm text-slate-300 t-num">
        <span>
          {current.toLocaleString()} / {total.toLocaleString()}
          {eta && (
            <span
              className={`ml-1.5 ${
                color === "emerald" ? "text-emerald-400/70" : "text-blue-400/70"
              }`}
            >
              {eta}
            </span>
          )}
        </span>
        <span>{Math.round(pct)}%</span>
      </div>
      <div className="bar" role="progressbar" aria-label="Current phase progress" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(pct)}>
        <div className={grad} style={{ width: `${pct}%` }} />
      </div>
    </>
  )
}

const KEEP_AWAKE_DURATIONS = [
  { label: "15m", value: 15 },
  { label: "30m", value: 30 },
  { label: "1h", value: 60 },
  { label: "2h", value: 120 },
]

function KeepAwakeInline({ keepAwake }: { keepAwake: ReturnType<typeof useKeepAwake> }) {
  const { status, mode, start, stop, pending, error } = keepAwake
  const [showDurations, setShowDurations] = useState(false)
  const durationId = useId()
  const durationRoot = useRef<HTMLDivElement>(null)
  const durationTrigger = useRef<HTMLButtonElement>(null)
  const durationMenu = useRef<HTMLDivElement>(null)
  const focusLast = useRef(false)
  const focusAfterStart = useRef(false)
  const actionRoot = useRef<HTMLSpanElement>(null)
  useEffect(() => {
    if (!showDurations) return
    const buttons = durationMenu.current?.querySelectorAll<HTMLButtonElement>('button')
    ;(focusLast.current ? buttons?.[buttons.length - 1] : buttons?.[0])?.focus()
    function outside(event: PointerEvent) {
      if (!durationRoot.current?.contains(event.target as Node)) setShowDurations(false)
    }
    document.addEventListener("pointerdown", outside)
    return () => document.removeEventListener("pointerdown", outside)
  }, [showDurations])
  useEffect(() => {
    if (focusAfterStart.current && !pending) {
      actionRoot.current?.querySelector<HTMLButtonElement>("button")?.focus()
      focusAfterStart.current = false
    }
  }, [pending, status.state])

  const isActive = status.state === "active"
  const isPending = status.state === "pending"
  const isIdle = status.state === "idle"
  const remainingMin = status.remaining_sec ? Math.ceil(status.remaining_sec / 60) : 0

  const value = isActive
    ? `${remainingMin}m`
    : isPending
    ? "Pending"
    : mode === "auto"
    ? "Auto"
    : "Idle"
  const sub = isActive
    ? "Keeping car awake"
    : isPending
    ? "Waiting for archive..."
    : mode === "auto"
    ? "Activates on interaction"
    : "Tap to start"

  const iconColor = isActive
    ? "text-rose-400"
    : isPending
    ? "text-amber-400"
    : "text-blue-400"

  const actionBtn =
    mode === "manual" && isIdle ? (
      <div ref={durationRoot} className="relative" onBlur={event => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setShowDurations(false)
      }}>
        <button ref={durationTrigger} aria-label="Start web app keep-awake" aria-haspopup="menu" aria-expanded={showDurations}
          aria-controls={showDurations ? durationId : undefined}
          onKeyDown={event => {
            if (event.key === "ArrowDown" || event.key === "ArrowUp") { event.preventDefault(); focusLast.current = event.key === "ArrowUp"; setShowDurations(true) }
          }}
          onClick={() => { focusLast.current = false; setShowDurations(!showDurations) }}
          disabled={pending}
          className={DROPDOWN_TRIGGER}
        >
          Start
        </button>
        {showDurations && (
          <div ref={durationMenu} id={durationId} role="menu" aria-label="Keep-awake duration"
            className={`${DROPDOWN_SURFACE} absolute right-0 top-full z-50 mt-2 min-w-40 p-1.5`}
            onKeyDown={event => {
              const buttons = [...(durationMenu.current?.querySelectorAll<HTMLButtonElement>("button") ?? [])]
              const index = buttons.indexOf(document.activeElement as HTMLButtonElement)
              if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
                event.preventDefault()
                const next = event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1 : (index + (event.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length
                buttons[next]?.focus()
              } else if (event.key === "Escape" || event.key === "Tab") {
                if (event.key === "Escape") { event.preventDefault(); event.stopPropagation() }
                setShowDurations(false); durationTrigger.current?.focus()
              }
            }}>
            {KEEP_AWAKE_DURATIONS.map((opt) => (
              <button
                key={opt.value}
                role="menuitem" tabIndex={-1}
                disabled={pending}
                onClick={() => {
                  focusAfterStart.current = true
                  void start(opt.value)
                  setShowDurations(false)
                }}
                className={`${DROPDOWN_OPTION} w-full`}
              >
                {opt.label}
              </button>
            ))}
          </div>
        )}
      </div>
    ) : isActive || isPending ? (
      <button
        onClick={stop}
        disabled={pending}
        className="rounded-lg bg-red-500/15 px-2.5 py-1 text-[11px] font-medium text-red-400 transition-colors hover:bg-red-500/25"
      >
        Stop
      </button>
    ) : null

  return (
    <div>
      <div className="flex items-center gap-2">
        <span className={`inline-flex ${iconColor}`}>
          {isActive ? (
            <CardiologyIcon className="h-3.5 w-3.5 animate-pulse" />
          ) : isPending ? (
            <TimerIcon className="h-3.5 w-3.5 animate-pulse" />
          ) : (
            <CardiologyIcon className="h-3.5 w-3.5" />
          )}
        </span>
        <span className="text-[10px] font-semibold uppercase tracking-wider text-slate-500">
          Web app keep-awake
        </span>
        {actionBtn && <span ref={actionRoot} className="ml-auto">{actionBtn}</span>}
      </div>
      <div className="mt-1 flex items-baseline gap-2">
        <span className="text-base font-semibold text-slate-100">{value}</span>
      </div>
      <p className="t-xs">{pending ? "Saving…" : sub}</p>
      {error && <p role="alert" className="mt-1 text-xs text-red-300">{error}</p>}
    </div>
  )
}

function AwayModeTile() {
  const { status } = useAwayMode()
  const automatic = status.mode === "auto"
  const hasDeadline = !automatic && !!status.expires_at && status.remaining_sec !== undefined
  const remaining = Math.max(0, status.remaining_sec ?? 0)
  const h = Math.floor(remaining / 3600)
  const m = Math.floor((remaining % 3600) / 60)

  let totalSec = 0
  if (status.enabled_at && status.expires_at) {
    totalSec =
      (new Date(status.expires_at).getTime() -
        new Date(status.enabled_at).getTime()) /
      1000
  }
  const pct = totalSec > 0 ? ((totalSec - remaining) / totalSec) * 100 : 0

  return (
    <StatusTile
      icon={<WifiIcon className="h-4 w-4" />}
      halo="blue"
      title="Away Mode"
      badge={
        <Pill kind="sky">
          <LiveDot /> Active
        </Pill>
      }
    >
      <div className="flex items-baseline gap-1.5">
        <span className="text-lg font-semibold text-slate-100">
          {automatic ? "Automatic" : hasDeadline ? `${h}h ${m}m` : "Active"}
        </span>
        {hasDeadline && <span className="t-xs">remaining</span>}
      </div>
      {hasDeadline && <div className="bar" role="progressbar" aria-label="Away mode duration" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(Math.max(0, Math.min(100, pct)))}>
        <div className="bg-sky-400" style={{ width: `${Math.max(0, Math.min(100, pct))}%` }} />
      </div>}
      {automatic && <p className="t-xs">Active while away</p>}
      {status.ap_ssid && (
        <p className="t-xs">
          AP <span className="t-mono text-slate-300">{status.ap_ssid}</span>
        </p>
      )}
    </StatusTile>
  )
}

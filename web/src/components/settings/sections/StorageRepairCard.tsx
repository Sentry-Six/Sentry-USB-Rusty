import { useCallback, useEffect, useRef, useState } from "react"
import {
  BuildIcon,
  CheckCircleIcon,
  ErrorIcon,
  HardDriveIcon,
  ProgressActivityIcon,
  RotateRightIcon,
  WarningIcon,
} from "@/components/icons"
import { cn } from "@/lib/utils"
import { PrefCard } from "@/components/settings/PrefCard"
import { wsClient } from "@/lib/ws"

type HealthState =
  | "healthy"
  | "unmounted"
  | "corrupt"
  | "missing_images"
  | "not_repairable"
  | "no_external"
  | "unknown"

interface StorageHealth {
  state: HealthState
  external: boolean
  /** Whether Repair Storage can run on this device (older servers omit it). */
  repairable?: boolean
  repair_blocked?: string | null
  device: string | null
  fstype: string | null
  mounted: boolean
  mountpoint: string
  cam_disk_present: boolean
  dmesg_errors: string[]
  last_repair_log: string | null
}

/** Mirrors the backend `storage_repair` WS payloads. */
interface RepairMsg {
  status: "running" | "needs_force" | "rebooting" | "reboot_required" | "error"
  phase?: string
  seconds?: number
  line?: string
  message?: string
  error?: string
  device?: string
  cam_disk_present?: boolean
  lost_found_count?: number
}

type Phase = "idle" | "running" | "needs_force" | "rebooting" | "reboot_required" | "error"

/** Saved outcome of the latest repair (GET /api/storage/repair). */
interface RepairRecord {
  phase: "running" | "needs_force" | "rebooting" | "failed" | "interrupted"
  device: string | null
  message: string | null
  log_file: string | null
}

const STATE_META: Record<HealthState, { label: string; tone: string }> = {
  healthy: { label: "Healthy", tone: "bg-emerald-500/15 text-emerald-300" },
  unmounted: { label: "Not mounted", tone: "bg-amber-500/15 text-amber-300" },
  corrupt: { label: "Corrupt", tone: "bg-red-500/15 text-red-300" },
  missing_images: { label: "Images missing", tone: "bg-amber-500/15 text-amber-300" },
  not_repairable: { label: "Not repairable", tone: "bg-slate-500/15 text-slate-300" },
  no_external: { label: "No external drive", tone: "bg-slate-500/15 text-slate-300" },
  unknown: { label: "Unknown", tone: "bg-slate-500/15 text-slate-300" },
}

/**
 * Repairs the XFS footage partition (external drive or the SD card's own
 * footage partition). Destructive `xfs_repair -L` requires explicit
 * confirmation. The latest outcome is restored from the server on load.
 */
export function StorageRepairCard() {
  const [health, setHealth] = useState<StorageHealth | null>(null)
  const [phase, setPhase] = useState<Phase>("idle")
  const [lines, setLines] = useState<string[]>([])
  const [forceMsg, setForceMsg] = useState<string>("")
  const [doneMsg, setDoneMsg] = useState<string>("")
  const [errorMsg, setErrorMsg] = useState<string>("")
  const [requestError, setRequestError] = useState("")
  const [starting, setStarting] = useState(false)
  const [rebooting, setRebooting] = useState(false)
  const [countdown, setCountdown] = useState(0)
  const [autoRepair, setAutoRepair] = useState(false)
  const [autoForce, setAutoForce] = useState(false)
  const [prefsLoaded, setPrefsLoaded] = useState(false)
  const logRef = useRef<HTMLDivElement>(null)
  const rebootPollRef = useRef<number | null>(null)
  const pendingMessagesRef = useRef<RepairMsg[] | null>(null)
  // Bumped by every live WS message; a saved-state fetch that started
  // before one arrived is older than what the card shows.
  const liveSeqRef = useRef(0)
  // Bumped per saved-state fetch; only the newest response is applied.
  const fetchSeqRef = useRef(0)
  const [stateSaved, setStateSaved] = useState(true)

  const refreshHealth = useCallback(() => {
    fetch("/api/storage/health")
      .then((r) => r.json())
      .then((d: StorageHealth) => setHealth(d))
      .catch(() => setHealth({
        state: "unknown",
        external: false,
        device: null,
        fstype: null,
        mounted: false,
        mountpoint: "/backingfiles",
        cam_disk_present: false,
        dmesg_errors: [],
        last_repair_log: null,
      }))
  }, [])

  useEffect(() => {
    refreshHealth()
  }, [refreshHealth])

  // A refresh, a service restart or a dropped connection must not forget
  // "the destructive step is needed" or how a repair ended: the server keeps
  // the latest outcome. Live WS messages are newer, so they win if any
  // arrived while this was loading.
  const loadSaved = useCallback(() => {
    const live = liveSeqRef.current
    const mine = ++fetchSeqRef.current
    fetch("/api/storage/repair", { cache: "no-store" })
      .then((r) => (r.ok ? r.json() : null))
      .then((d: { active?: boolean; record?: RepairRecord | null; saved?: boolean } | null) => {
        // Older than a later fetch or a live message: drop all of it.
        if (!d || fetchSeqRef.current !== mine || liveSeqRef.current !== live) return
        setStateSaved(d.saved !== false)
        const rec = d.record
        if (!rec) {
          // Nothing saved and nothing running: a repair the card still shows
          // as running ended while the connection was down (a successful
          // repair is cleared once the drive is back after the reboot).
          if (!d.active) {
            setPhase((current) => (current === "running" ? "idle" : current))
            refreshHealth()
          }
          return
        }
        if (rec.phase === "running" && d.active) {
          setPhase("running")
        } else if (rec.phase === "needs_force") {
          setForceMsg(rec.message || "Destructive repair required.")
          setPhase("needs_force")
        } else if (rec.phase === "rebooting") {
          setDoneMsg(rec.message || "Repair complete. Rebooting.")
          setPhase("rebooting")
        } else {
          setErrorMsg(rec.message || "The last repair did not finish.")
          setPhase("error")
        }
      })
      .catch(() => {})
  }, [refreshHealth])

  useEffect(() => {
    loadSaved()
  }, [loadSaved])

  // Messages sent while the connection was down are lost: catch up on reconnect.
  useEffect(() => wsClient.onStatusChange((connected) => {
    if (connected) loadSaved()
  }), [loadSaved])

  // Preferences may contain booleans or their string representations.
  useEffect(() => {
    const isOn = (v: unknown) => v === true || v === "true"
    Promise.all([
      fetch("/api/config/preference?key=storage_auto_repair")
        .then((r) => r.json())
        .catch(() => ({ value: null })),
      fetch("/api/config/preference?key=storage_auto_force_repair")
        .then((r) => r.json())
        .catch(() => ({ value: null })),
    ]).then(([auto, force]) => {
      setAutoRepair(isOn(auto?.value))
      setAutoForce(isOn(force?.value))
      setPrefsLoaded(true)
    })
  }, [])

  async function setAutoPref(
    key: "storage_auto_repair" | "storage_auto_force_repair",
    value: boolean
  ) {
    if (key === "storage_auto_repair") setAutoRepair(value)
    else setAutoForce(value)
    await fetch("/api/config/preference", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ key, value }),
    }).catch(() => {})
  }

  const applyRepairMsg = useCallback((d: RepairMsg) => {
    setRequestError("")
    if (d.status === "running") {
      setPhase("running")
      if (d.line) setLines((prev) => [...prev, d.line as string])
    } else if (d.status === "needs_force") {
      setPhase("needs_force")
      setForceMsg(d.message || "Destructive repair required.")
    } else if (d.status === "rebooting") {
      setPhase("rebooting")
      setDoneMsg(d.message || "Repair complete. Rebooting.")
      setCountdown(d.seconds ?? 15)
    } else if (d.status === "reboot_required") {
      setPhase("reboot_required")
      setDoneMsg(d.message || "Repair complete. A reboot is required.")
    } else if (d.status === "error") {
      setPhase("error")
      setErrorMsg(d.error || "Repair failed.")
    }
  }, [])

  // Continue showing live progress while POST admission is pending. Keep a
  // copy so acceptance can clear the old run without losing early WS events.
  useEffect(() => {
    const unsub = wsClient.subscribe("storage_repair", (data: unknown) => {
      const d = data as RepairMsg
      liveSeqRef.current += 1
      pendingMessagesRef.current?.push(d)
      applyRepairMsg(d)
      // Learn whether the outcome was saved on the Pi.
      if (d.status !== "running") loadSaved()
    })
    return () => unsub()
  }, [applyRepairMsg, loadSaved])

  useEffect(() => {
    if (logRef.current) logRef.current.scrollTop = logRef.current.scrollHeight
  }, [lines])

  useEffect(
    () => () => {
      if (rebootPollRef.current) window.clearInterval(rebootPollRef.current)
    },
    []
  )

  // Reload only after observing both shutdown and recovery.
  const waitForRestart = useCallback(() => {
    let sawDown = false
    if (rebootPollRef.current) window.clearInterval(rebootPollRef.current)
    rebootPollRef.current = window.setInterval(async () => {
      try {
        const res = await fetch("/api/status", { cache: "no-store" })
        if (res.ok && sawDown) {
          if (rebootPollRef.current) window.clearInterval(rebootPollRef.current)
          window.location.reload()
        }
      } catch {
        sawDown = true
      }
    }, 3000)
  }, [])

  // The server reboots after its countdown; count along, then wait.
  useEffect(() => {
    if (phase !== "rebooting") return
    if (countdown > 0) {
      const timer = window.setTimeout(() => setCountdown((c) => c - 1), 1000)
      return () => window.clearTimeout(timer)
    }
    waitForRestart()
  }, [phase, countdown, waitForRestart])

  async function startRepair(force: boolean) {
    // The ref also blocks two clicks before React renders the disabled button.
    if (pendingMessagesRef.current) return
    pendingMessagesRef.current = []
    setStarting(true)
    setRequestError("")
    try {
      const res = await fetch("/api/storage/repair", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ confirm_destructive: force }),
      })
      if (res.ok) {
        // Saved-state replies requested before this repair started are stale.
        fetchSeqRef.current += 1
        setLines([])
        setForceMsg("")
        setDoneMsg("")
        setErrorMsg("")
        setPhase("running")
        for (const message of pendingMessagesRef.current) applyRepairMsg(message)
        // A terminal result that arrived early: ask again whether it was saved.
        if (pendingMessagesRef.current.some((m) => m.status !== "running")) loadSaved()
      } else {
        const body = await res.json().catch(() => ({}))
        setRequestError(body.error || `Could not start repair (HTTP ${res.status}).`)
      }
    } catch {
      setRequestError("Could not reach the device to start the repair.")
    } finally {
      pendingMessagesRef.current = null
      setStarting(false)
    }
  }

  async function reboot() {
    setRebooting(true)
    try {
      await fetch("/api/system/reboot", { method: "POST" })
    } catch {
      /* the box is going down — a failed fetch here is expected */
    }
    waitForRestart()
  }

  const repairable = health ? (health.repairable ?? health.external) : true
  if (health && !repairable) {
    return (
      <PrefCard
        icon={<HardDriveIcon className="h-3.5 w-3.5" />}
        halo="slate"
        title="Repair Storage"
        disabled={{
          reason: health.repair_blocked
            ? `Storage repair isn't available here: ${health.repair_blocked}.`
            : "Storage repair isn't available: no footage partition was found.",
        }}
      >
        <p className="t-xs">
          Detects and repairs filesystem corruption on the drive that holds
          your dashcam footage.
        </p>
      </PrefCard>
    )
  }

  const meta = health ? STATE_META[health.state] ?? STATE_META.unknown : null
  const canRepair =
    health != null &&
    (health.state === "corrupt" || health.state === "unmounted")
  const running = phase === "running"

  return (
    <PrefCard
      icon={<HardDriveIcon className="h-3.5 w-3.5" />}
      halo={health?.state === "corrupt" ? "red" : "amber"}
      title="Repair Storage"
      badge={
        meta && (
          <span className={cn("rounded-md px-2 py-0.5 text-[10px] font-medium", meta.tone)}>
            {meta.label}
          </span>
        )
      }
    >
      <p className="t-xs">
        Checks and repairs the drive that holds your dashcam footage
        (<span className="text-slate-300">{health?.mountpoint || "/backingfiles"}</span>).
        Use this if SentryUSB can&apos;t find the footage drive after a power
        loss or update.
      </p>

      {health && (
        <div className="rounded-lg border border-white/5 bg-white/[0.02] px-3 py-2 text-[11px] text-slate-400">
          <div className="flex justify-between gap-3">
            <span>Device</span>
            <span className="text-slate-300">{health.device || "not found"}</span>
          </div>
          <div className="flex justify-between gap-3">
            <span>Filesystem</span>
            <span className="text-slate-300">{health.fstype || "—"}</span>
          </div>
          <div className="flex justify-between gap-3">
            <span>Mounted</span>
            <span className="text-slate-300">{health.mounted ? "yes" : "no"}</span>
          </div>
          <div className="flex justify-between gap-3">
            <span>cam_disk.bin</span>
            <span className="text-slate-300">{health.cam_disk_present ? "present" : "missing"}</span>
          </div>
        </div>
      )}

      {health && health.dmesg_errors.length > 0 && phase === "idle" && (
        <details className="rounded-lg border border-red-500/15 bg-red-500/5 px-3 py-2">
          <summary className="cursor-pointer text-[11px] font-medium text-red-300">
            {health.dmesg_errors.length} recent filesystem error
            {health.dmesg_errors.length === 1 ? "" : "s"} in the kernel log
          </summary>
          <pre className="mt-2 max-h-40 overflow-auto whitespace-pre-wrap break-words text-[10px] leading-relaxed text-slate-400">
            {health.dmesg_errors.join("\n")}
          </pre>
        </details>
      )}

      {health?.state === "healthy" && phase === "idle" && (
        <p className="flex items-center gap-1.5 text-[11px] text-emerald-300/90">
          <CheckCircleIcon className="h-3.5 w-3.5" />
          Storage is mounted and healthy — no repair needed.
        </p>
      )}

      {health?.state === "missing_images" && phase === "idle" && (
        <p className="text-[11px] text-amber-300/90">
          The filesystem is healthy but <span className="font-mono">cam_disk.bin</span> is
          missing. Re-run the Setup Wizard to recreate the backing files —
          repair won't bring the images back.
        </p>
      )}

      {(running || lines.length > 0) && (
        <div
          ref={logRef}
          className="max-h-56 overflow-auto rounded-lg border border-white/5 bg-black/40 p-2.5 font-mono text-[10px] leading-relaxed text-slate-300"
        >
          {lines.map((l, i) => (
            <div key={i} className="whitespace-pre-wrap break-words">{l}</div>
          ))}
          {running && (
            <div className="mt-1 flex items-center gap-1.5 text-slate-500">
              <ProgressActivityIcon className="h-3 w-3 animate-spin" /> working…
            </div>
          )}
        </div>
      )}

      {phase === "needs_force" && (
        <div className="rounded-xl border border-red-500/25 bg-red-500/5 p-3">
          <div className="flex items-start gap-2.5">
            <WarningIcon className="mt-0.5 h-4 w-4 shrink-0 text-red-400" />
            <div>
              <p className="text-xs font-medium text-red-300">Destructive repair required</p>
              <p className="mt-1 text-[11px] text-slate-400">{forceMsg}</p>
              <div className="mt-2.5 flex gap-2">
                <button
                  disabled={starting}
                  onClick={() => {
                    setPhase("idle")
                    setForceMsg("")
                  }}
                  className="rounded-lg border border-white/10 px-3 py-1.5 text-xs font-medium text-slate-400 transition-colors hover:bg-white/5"
                >
                  Cancel
                </button>
                <button
                  disabled={starting}
                  onClick={() => startRepair(true)}
                  className="rounded-lg bg-red-500/20 px-3 py-1.5 text-xs font-medium text-red-300 transition-colors hover:bg-red-500/30"
                >
                  Force repair (destroys XFS log)
                </button>
              </div>
            </div>
          </div>
        </div>
      )}

      {phase === "rebooting" && (
        <div className="rounded-xl border border-emerald-500/20 bg-emerald-500/5 p-3">
          <div className="flex items-start gap-2.5">
            <CheckCircleIcon className="mt-0.5 h-4 w-4 shrink-0 text-emerald-400" />
            <div>
              <p className="text-xs font-medium text-emerald-300">Repair complete</p>
              <p className="mt-1 text-[11px] text-slate-400">{doneMsg}</p>
              <p className="mt-2 flex items-center gap-1.5 text-[11px] text-slate-300">
                <ProgressActivityIcon className="h-3.5 w-3.5 animate-spin" />
                {countdown > 0
                  ? `Rebooting in ${countdown} s…`
                  : "Rebooting — this page reloads when the Pi is back."}
              </p>
            </div>
          </div>
        </div>
      )}

      {phase === "reboot_required" && (
        <div className="rounded-xl border border-emerald-500/20 bg-emerald-500/5 p-3">
          <div className="flex items-start gap-2.5">
            <CheckCircleIcon className="mt-0.5 h-4 w-4 shrink-0 text-emerald-400" />
            <div>
              <p className="text-xs font-medium text-emerald-300">Repair complete</p>
              <p className="mt-1 text-[11px] text-slate-400">{doneMsg}</p>
              <button
                onClick={reboot}
                disabled={rebooting}
                className="mt-2.5 flex items-center gap-1.5 rounded-lg bg-blue-500/15 px-3 py-1.5 text-xs font-medium text-blue-300 transition-colors hover:bg-blue-500/25 disabled:opacity-60"
              >
                {rebooting ? (
                  <>
                    <ProgressActivityIcon className="h-3.5 w-3.5 animate-spin" /> Rebooting…
                  </>
                ) : (
                  <>
                    <RotateRightIcon className="h-3.5 w-3.5" /> Reboot now
                  </>
                )}
              </button>
            </div>
          </div>
        </div>
      )}

      {!stateSaved && phase !== "idle" && (
        <p className="text-[10px] text-amber-300/90">
          This result couldn&apos;t be saved on the Pi (its settings partition
          isn&apos;t available), so it won&apos;t be shown again after a restart.
        </p>
      )}

      {(phase === "error" || requestError) && (
        <div role="alert" className="flex items-start gap-2 rounded-lg bg-red-500/10 px-3 py-2 text-[11px] text-red-300">
          <ErrorIcon className="mt-0.5 h-3.5 w-3.5 shrink-0" />
          <span>{requestError || errorMsg}</span>
        </div>
      )}

      <div className="flex flex-wrap items-center gap-2">
        {canRepair && phase !== "running" && phase !== "needs_force" && phase !== "rebooting" && phase !== "reboot_required" && (
          <button
            disabled={starting}
            onClick={() => startRepair(false)}
            className="rounded-lg bg-amber-500/15 px-3 py-1.5 text-xs font-medium text-amber-300 transition-colors hover:bg-amber-500/25"
          >
            Repair storage
          </button>
        )}
        {phase === "idle" && (
          <button
            onClick={refreshHealth}
            className="rounded-lg border border-white/10 bg-white/5 px-3 py-1.5 text-xs font-medium text-slate-300 transition-colors hover:bg-white/10"
          >
            Refresh
          </button>
        )}
      </div>

      <div className="space-y-3 border-t border-white/5 pt-3">
        <label className="flex cursor-pointer items-start justify-between gap-3">
          <div className="flex items-start gap-2">
            <BuildIcon className="mt-0.5 h-3.5 w-3.5 shrink-0 text-blue-400" />
            <div>
              <span className="text-xs font-medium text-slate-200">Auto repair at boot</span>
              <span className="mt-0.5 block text-[10px] text-slate-500">
                If storage is corrupt after a power loss, run the
                non-destructive repair automatically at boot and reboot when
                it succeeds. You&apos;ll get a notification either way.
              </span>
            </div>
          </div>
          <input
            type="checkbox"
            checked={autoRepair}
            disabled={!prefsLoaded}
            onChange={(e) => setAutoPref("storage_auto_repair", e.target.checked)}
            className="toggle-switch mt-0.5"
          />
        </label>

        <label
          className={cn(
            "flex items-start justify-between gap-3",
            autoRepair ? "cursor-pointer" : "cursor-not-allowed opacity-50"
          )}
        >
          <div className="flex items-start gap-2">
            <WarningIcon className="mt-0.5 h-3.5 w-3.5 shrink-0 text-red-400" />
            <div>
              <span className="text-xs font-medium text-slate-200">
                Auto force fix (destructive)
              </span>
              <span className="mt-0.5 block text-[10px] text-slate-500">
                If the safe repair fails, automatically run{" "}
                <span className="font-mono">xfs_repair -L</span> and reboot.
                It discards the pending XFS log, and the repair can then
                remove or move files, including disk images and snapshots.
                Losses can go beyond the newest clips.
              </span>
            </div>
          </div>
          <input
            type="checkbox"
            checked={autoForce}
            disabled={!prefsLoaded || !autoRepair}
            onChange={(e) => setAutoPref("storage_auto_force_repair", e.target.checked)}
            className="toggle-switch mt-0.5"
          />
        </label>
      </div>

      <p className="text-[10px] leading-relaxed text-slate-500">
        While repairing, the archive loop and the car-facing USB drive are
        stopped (the Tesla pauses recording). The web UI stays up. After a
        successful repair the Pi reboots by itself to reconnect the drive.
        Nothing destructive runs without your confirmation unless you turn
        on the auto force fix.
      </p>
    </PrefCard>
  )
}

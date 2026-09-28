import { useState, useEffect, useRef } from "react"
import { CheckCircleIcon, DownloadIcon, ErrorIcon, ProgressActivityIcon } from "@/components/icons"
import { cn } from "@/lib/utils"
import { wsClient } from "@/lib/ws"
import { useVersion } from "@/hooks/useVersion"
import { PrefCard } from "@/components/settings/PrefCard"
import { Pill } from "@/components/ui/Pill"
import { Toggle } from "@/components/ui/Toggle"
import { Modal } from "@/components/ui/Modal"
import { InfoButton } from "@/components/ui/InfoButton"
import { AutoUpdateToggle } from "./AutoUpdateToggle"

type UpdateStatus =
  | "idle"
  | "checking_internet"
  | "checking"
  | "downloading"
  | "installing"
  | "updating_scripts"
  | "restarting"
  | "reconnecting"
  | "done"
  | "error"

type ReleaseInfo = {
  version: string
  release_url: string
  release_notes: string
}

interface Props {
  /** Optional callback so the parent can react when an install kicks off. */
  onInstallStart?: () => void
}

export function UpdateSection({ onInstallStart }: Props) {
  const version = useVersion()
  const [updateStatus, setUpdateStatus] = useState<UpdateStatus>("idle")
  const [updateError, setUpdateError] = useState<string | null>(null)
  const [updateMessage, setUpdateMessage] = useState<string | null>(null)
  const [installedVersion, setInstalledVersion] = useState<string | null>(null)
  const [isCheckingUpdate, setIsCheckingUpdate] = useState(false)
  const [stableUpdate, setStableUpdate] = useState<ReleaseInfo | null>(null)
  const [prereleaseUpdate, setPrereleaseUpdate] = useState<ReleaseInfo | null>(null)
  const [revertStable, setRevertStable] = useState<ReleaseInfo | null>(null)
  const [autoUpdateEnabled, setAutoUpdateEnabled] = useState<boolean | null>(null)
  const [includePrerelease, setIncludePrerelease] = useState<boolean | null>(null)
  const [preferenceError, setPreferenceError] = useState<string | null>(null)
  const [preferencePending, setPreferencePending] = useState(false)
  const preferenceBusy = useRef(false)
  const [showUpdateModal, setShowUpdateModal] = useState(false)
  const [downloadPercent, setDownloadPercent] = useState<number | null>(null)
  const initialUpdateRequest = useRef<AbortController | null>(null)

  useEffect(() => {
    if (!showUpdateModal) return
    if (updateStatus === "done") {
      const t = setTimeout(() => setShowUpdateModal(false), 3000)
      return () => clearTimeout(t)
    }
  }, [showUpdateModal, updateStatus])

  useEffect(() => {
    const controller = new AbortController()
    initialUpdateRequest.current = controller
    fetch("/api/system/update-status", { signal: controller.signal })
      .then((r) => {
        if (!r.ok) throw new Error("Could not read available updates. Check again.")
        return r.json()
      })
      .then((data) => {
        if (controller.signal.aborted) return
        if (data.stable?.available) {
          setStableUpdate({
            version: data.stable.version,
            release_url: data.stable.release_url,
            release_notes: data.stable.release_notes,
          })
        } else if (data.update_available) {
          setStableUpdate({
            version: data.latest_version,
            release_url: data.release_url,
            release_notes: data.release_notes,
          })
        }
        if (data.prerelease?.available) {
          setPrereleaseUpdate({
            version: data.prerelease.version,
            release_url: data.prerelease.release_url,
            release_notes: data.prerelease.release_notes,
          })
        }
        if (data.revert_stable) {
          setRevertStable({
            version: data.revert_stable.version,
            release_url: data.revert_stable.release_url,
            release_notes: data.revert_stable.release_notes,
          })
        }
      })
      .catch(() => { if (!controller.signal.aborted) setUpdateError("Could not read available updates. Check again.") })
    return () => controller.abort()
  }, [])

  useEffect(() => {
    const controller = new AbortController()
    for (const [key, apply] of [
      ["auto_update_check", (value: unknown) => setAutoUpdateEnabled(value !== "disabled")],
      ["update_channel", (value: unknown) => setIncludePrerelease(value === "prerelease")],
    ] as const) {
      fetch(`/api/config/preference?key=${key}`, { signal: controller.signal })
        .then(async response => {
          if (!response.ok) throw new Error("Could not load update preferences. Reload to try again.")
          const data = await response.json()
          if (!("value" in data)) throw new Error("Could not read update preferences.")
          if (!controller.signal.aborted) apply(data.value)
        })
        .catch(error => { if (!controller.signal.aborted) setPreferenceError(error instanceof Error ? error.message : "Could not load update preferences.") })
    }
    return () => controller.abort()
  }, [])

  async function savePreference(key: string, value: string, apply: () => void) {
    if (preferenceBusy.current) return
    preferenceBusy.current = true
    setPreferencePending(true)
    setPreferenceError(null)
    try {
      const response = await fetch("/api/config/preference", {
        method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ key, value }),
      })
      if (!response.ok) throw new Error("Could not save update preferences. Try again.")
      apply()
    } catch (error) { setPreferenceError(error instanceof Error ? error.message : "Could not save update preferences.") }
    finally { preferenceBusy.current = false; setPreferencePending(false) }
  }

  async function handleCheckForUpdate(oneTimePrerelease = false) {
    initialUpdateRequest.current?.abort()
    setIsCheckingUpdate(true)
    setStableUpdate(null)
    setPrereleaseUpdate(null)
    setRevertStable(null)
    setUpdateError(null)
    try {
      const wantPrerelease = includePrerelease || oneTimePrerelease
      const url =
        "/api/system/check-update" + (wantPrerelease ? "?include_prerelease=true" : "")
      const res = await fetch(url, { method: "POST" })
      if (!res.ok) throw new Error("Failed to check for updates")
      const data = await res.json()
      if (data.error) {
        setUpdateError(data.error)
      } else {
        let foundAny = false
        if (data.stable?.available) {
          setStableUpdate({
            version: data.stable.version,
            release_url: data.stable.release_url,
            release_notes: data.stable.release_notes,
          })
          foundAny = true
        } else if (data.update_available) {
          setStableUpdate({
            version: data.latest_version,
            release_url: data.release_url,
            release_notes: data.release_notes,
          })
          foundAny = true
        }
        if (data.prerelease?.available) {
          setPrereleaseUpdate({
            version: data.prerelease.version,
            release_url: data.prerelease.release_url,
            release_notes: data.prerelease.release_notes,
          })
          foundAny = true
        }
        if (data.revert_stable) {
          setRevertStable({
            version: data.revert_stable.version,
            release_url: data.revert_stable.release_url,
            release_notes: data.revert_stable.release_notes,
          })
          foundAny = true
        }
        if (!foundAny) {
          setUpdateStatus("done")
          setUpdateMessage(`You're up to date (${data.current_version || version})`)
          setTimeout(() => {
            setShowUpdateModal(false)
            setUpdateStatus("idle")
            setUpdateMessage(null)
          }, 4000)
        }
      }
    } catch (err) {
      setUpdateError(err instanceof Error ? err.message : "Failed to check for updates")
    } finally {
      setIsCheckingUpdate(false)
    }
  }

  async function handleInstallUpdate(targetVersion?: string) {
    initialUpdateRequest.current?.abort()
    onInstallStart?.()
    setUpdateStatus("checking_internet")
    setUpdateError(null)
    setUpdateMessage("Checking internet connection...")
    setShowUpdateModal(true)
    setDownloadPercent(null)
    // Retain the target version while the pre-reboot daemon serves the old one.
    const preUpdateVersion = version
    let newVersion: string | null = targetVersion ?? null
    setInstalledVersion(newVersion)

    // Version files change before reboot, so require a new kernel boot_id.
    // Missing boot IDs never prove success; the watchdog handles that case.
    let preBootId: string | null = null
    try {
      const r = await fetch("/api/system/version", { cache: "no-store" })
      if (r.ok) preBootId = (await r.json()).boot_id || null
    } catch {
      /* gate refuses success without a baseline */
    }

    let sawRestartPending = false
    let enteredReconnect = false
    let reconnected = false
    let fallbackTimer: ReturnType<typeof setTimeout> | null = null

    const unsubscribe = wsClient.subscribe("update_status", (data: unknown) => {
      const msg = data as {
        status?: string
        message?: string
        error?: string
        output?: string
        percent?: number | null
      }
      if (msg.error) {
        cleanup()
        setShowUpdateModal(false)
        setUpdateStatus("error")
        setUpdateError(msg.error)
        setUpdateMessage(null)
        return
      }
      if (msg.status) {
        const statusMap: Record<string, UpdateStatus> = {
          checking_internet: "checking_internet",
          checking: "checking",
          remounting: "installing",
          downloading: "downloading",
          installing: "installing",
          updating_scripts: "updating_scripts",
          restarting: "restarting",
        }
        setUpdateStatus(statusMap[msg.status] || "installing")
      }
      setDownloadPercent(
        msg.status === "downloading" && typeof msg.percent === "number"
          ? msg.percent
          : null
      )
      if (msg.status === "complete") {
        sawRestartPending = true
        if (msg.output) {
          const m = msg.output.match(/Updated to (\S+?)\.?\s*$/)
          if (m) {
            newVersion = m[1]
            setInstalledVersion(newVersion)
          }
        }
      }
      if (msg.status === "restarting") {
        sawRestartPending = true
        // The boot-id gate rejects polls that arrive before reboot.
        setTimeout(enterReconnect, 3000)
      }
      if (msg.message) {
        setUpdateMessage(msg.message)
      }
    })

    // A socket drop after restart begins is evidence to enter reconnect mode.
    const unsubStatus = wsClient.onStatusChange((connected) => {
      if (!connected && sawRestartPending) enterReconnect()
    })

    function cleanup() {
      unsubscribe()
      unsubStatus()
      if (fallbackTimer) clearTimeout(fallbackTimer)
    }

    function enterReconnect() {
      if (enteredReconnect) return
      enteredReconnect = true
      cleanup()
      setUpdateStatus("reconnecting")
      setUpdateMessage("Waiting for device to come back online...")

      const pollInterval = setInterval(async () => {
        try {
          const r = await fetch("/api/system/version", { cache: "no-store" })
          if (!r.ok) return
          const data = await r.json()
          const polled = (data.version || "").trim()
          const bootId = data.boot_id || null
          // Require both a proven reboot and the expected version.
          if (!preBootId || !bootId || bootId === preBootId) return
          const norm = (s: string) => s.replace(/^v/, "")
          const versionOk = newVersion
            ? norm(polled) === norm(newVersion)
            : Boolean(polled) && polled !== preUpdateVersion
          if (!versionOk) return
          reconnected = true
          clearInterval(pollInterval)
          setStableUpdate(null)
          setPrereleaseUpdate(null)
          setRevertStable(null)
          setUpdateStatus("done")
          setUpdateMessage(`Update complete — now running ${newVersion || polled || "latest"}`)
          setTimeout(() => {
            setShowUpdateModal(false)
            setUpdateStatus("idle")
            setUpdateMessage(null)
            setInstalledVersion(null)
            // Reload cached chunks and hooks against the installed backend.
            window.location.reload()
          }, 6000)
        } catch {
          /* Still restarting */
        }
      }, 3000)
      setTimeout(() => {
        if (!reconnected) {
          clearInterval(pollInterval)
          setShowUpdateModal(false)
          setUpdateStatus("idle")
          setUpdateMessage(null)
          setInstalledVersion(null)
          setUpdateError("Update may still be in progress. Refresh the page in a moment.")
        }
      }, 180000)
    }

    try {
      const checkRes = await fetch("/api/system/check-internet")
      const checkData = await checkRes.json()
      if (!checkData.connected) {
        cleanup()
        setShowUpdateModal(false)
        setUpdateStatus("error")
        setUpdateError("No internet connection. Connect to WiFi first.")
        setUpdateMessage(null)
        return
      }

      const res = await fetch("/api/system/update", {
        method: "POST",
        headers: targetVersion ? { "Content-Type": "application/json" } : {},
        body: targetVersion ? JSON.stringify({ version: targetVersion }) : undefined,
      })
      if (!res.ok) throw new Error("Failed to start update")

      // Fallback when no progress or restart event arrives over WebSocket.
      fallbackTimer = setTimeout(enterReconnect, 120000)
    } catch (err) {
      cleanup()
      setShowUpdateModal(false)
      setUpdateStatus("error")
      setUpdateError(err instanceof Error ? err.message : "Update failed")
      setUpdateMessage(null)
      setInstalledVersion(null)
      setRevertStable(null)
    }
  }

  const installInProgress =
    updateStatus !== "idle" &&
    updateStatus !== "error" &&
    updateStatus !== "done"

  const headerIcon =
    updateStatus === "error" ? (
      <ErrorIcon className="h-3.5 w-3.5" />
    ) : updateStatus === "done" ? (
      <CheckCircleIcon className="h-3.5 w-3.5" />
    ) : installInProgress ? (
      <ProgressActivityIcon className="h-3.5 w-3.5 animate-spin" />
    ) : (
      <DownloadIcon className="h-3.5 w-3.5" />
    )

  const headerHalo =
    updateStatus === "error"
      ? "red"
      : updateStatus === "done"
      ? "accent"
      : stableUpdate || prereleaseUpdate
      ? "accent"
      : "slate"

  return (
    <>
      <PrefCard
        icon={headerIcon}
        halo={headerHalo}
        title="Software Updates"
        badge={
          // Badge text stays on the installed version; accent signals availability.
          <Pill kind={stableUpdate || prereleaseUpdate ? "accent" : "slate"}>
            {version ?? "…"}
          </Pill>
        }
      >
        <p className="t-xs">
          {updateStatus === "idle" && !updateError && !stableUpdate && !prereleaseUpdate &&
            "Check for and install the latest version."}
          {updateStatus === "idle" && updateError && (
            <span className="text-red-400">{updateError}</span>
          )}
          {updateStatus === "error" && (
            <span className="text-red-400">{updateError || "Update failed."}</span>
          )}
          {updateStatus === "done" && (
            <span className="text-emerald-400">{updateMessage || "Update complete!"}</span>
          )}
          {installInProgress && (updateMessage || "Installing…")}
        </p>

        {stableUpdate && updateStatus === "idle" && (
          <div className="rounded-lg border border-emerald-500/20 bg-emerald-500/5 p-3">
            <div className="flex items-center justify-between gap-2">
              <div className="min-w-0">
                <p className="text-xs font-semibold text-emerald-300">
                  Stable: {stableUpdate.version}
                </p>
                <p className="mt-0.5 text-[11px] text-slate-400">
                  Updates server, scripts &amp; BLE daemon.{" "}
                  <a
                    href={stableUpdate.release_url}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-blue-400 underline hover:text-blue-300"
                  >
                    Notes
                  </a>
                </p>
              </div>
              <button
                onClick={() => handleInstallUpdate(stableUpdate.version)}
                className="shrink-0 rounded-lg bg-emerald-500 px-3 py-1.5 text-[11px] font-medium text-white hover:bg-emerald-600"
              >
                Install
              </button>
            </div>
          </div>
        )}

        {prereleaseUpdate && updateStatus === "idle" && (
          <div className="rounded-lg border border-amber-500/20 bg-amber-500/5 p-3">
            <div className="flex items-center justify-between gap-2">
              <div className="min-w-0">
                <p className="text-xs font-semibold text-amber-300">
                  Pre-release: {prereleaseUpdate.version}
                </p>
                <p className="mt-0.5 text-[11px] text-slate-400">
                  Test build — may contain bugs.{" "}
                  <a
                    href={prereleaseUpdate.release_url}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-blue-400 underline hover:text-blue-300"
                  >
                    Notes
                  </a>
                </p>
              </div>
              <button
                onClick={() => handleInstallUpdate(prereleaseUpdate.version)}
                className="shrink-0 rounded-lg bg-amber-500 px-3 py-1.5 text-[11px] font-medium text-white hover:bg-amber-600"
              >
                Install
              </button>
            </div>
          </div>
        )}

        {revertStable && updateStatus === "idle" && (
          <div className="rounded-lg border border-blue-500/20 bg-blue-500/5 p-3">
            <div className="flex items-center justify-between gap-2">
              <div className="min-w-0">
                <p className="text-xs font-semibold text-blue-300">
                  Revert to Stable: {revertStable.version}
                </p>
                <p className="mt-0.5 text-[11px] text-slate-400">
                  Downgrade from pre-release to latest stable.{" "}
                  <a
                    href={revertStable.release_url}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-blue-400 underline hover:text-blue-300"
                  >
                    Notes
                  </a>
                </p>
              </div>
              <button
                onClick={() => handleInstallUpdate(revertStable.version)}
                className="shrink-0 rounded-lg bg-blue-500 px-3 py-1.5 text-[11px] font-medium text-white hover:bg-blue-600"
              >
                Revert
              </button>
            </div>
          </div>
        )}

        <button
          onClick={() => handleCheckForUpdate()}
          disabled={isCheckingUpdate || installInProgress}
          className={cn(
            "self-start rounded-lg px-3 py-1.5 text-xs font-medium transition-colors disabled:opacity-50",
            "bg-emerald-500/15 text-emerald-400 hover:bg-emerald-500/25"
          )}
        >
          {isCheckingUpdate ? (
            <span className="inline-flex items-center gap-1.5">
              <ProgressActivityIcon className="h-3.5 w-3.5 animate-spin" /> Checking
            </span>
          ) : (
            "Check for Updates"
          )}
        </button>
      </PrefCard>

      <PrefCard
        icon={<DownloadIcon className="h-3.5 w-3.5" />}
        halo="slate"
        title="Update Preferences"
      >
        <AutoUpdateToggle />
        <Toggle
          checked={autoUpdateEnabled ?? false}
          disabled={autoUpdateEnabled === null || includePrerelease === null || preferencePending}
          onChange={next => savePreference("auto_update_check", next ? "enabled" : "disabled", () => setAutoUpdateEnabled(next))}
          label="Check after archiving"
          help={<InfoButton title="Update checks"><p>Checks GitHub releases after each archive cycle. This only checks availability; automatic installation is controlled separately.</p></InfoButton>}
        />
        <Toggle
          checked={includePrerelease ?? false}
          disabled={autoUpdateEnabled === null || includePrerelease === null || preferencePending}
          onChange={next => savePreference("update_channel", next ? "prerelease" : "stable", () => setIncludePrerelease(next))}
          label="Include pre-releases"
          help={<InfoButton title="Pre-release updates"><p>Includes test builds when checking for updates. These builds may contain bugs. Automatic installation still uses stable releases only.</p></InfoButton>}
        />
        {(autoUpdateEnabled === null || includePrerelease === null) && !preferenceError && <p role="status" className="t-xs">Loading preferences…</p>}
        {preferencePending && <p role="status" className="t-xs">Saving…</p>}
        {preferenceError && <p role="alert" className="text-xs text-red-400">{preferenceError}</p>}
      </PrefCard>

      {showUpdateModal && (
        <Modal
          title={updateStatus === "done" ? "Update Complete" : "Installing Update"}
          onClose={() => setShowUpdateModal(false)}
          dismissable={false}
          size="sm"
        >
          <div className="flex flex-col items-center gap-3 py-6 text-center">
            {updateStatus === "done" ? (
              <CheckCircleIcon className="h-12 w-12 text-emerald-400" />
            ) : (
              <ProgressActivityIcon className="h-12 w-12 animate-spin text-blue-400" />
            )}
            <h2 className="text-lg font-semibold text-slate-100">
              {updateStatus === "checking_internet" && "Checking connection"}
              {updateStatus === "checking" && "Checking release"}
              {updateStatus === "downloading" && "Downloading update"}
              {updateStatus === "installing" && "Installing update"}
              {updateStatus === "updating_scripts" && "Updating scripts"}
              {updateStatus === "restarting" && "Restarting Pi"}
              {updateStatus === "reconnecting" && "Waiting for Pi to come back online"}
              {updateStatus === "done" && "Update complete"}
            </h2>
            {updateStatus === "downloading" && (
              <div className="w-full px-4">
                <div className="h-2 w-full overflow-hidden rounded-full bg-slate-700/50">
                  <div
                    className={cn(
                      "h-full rounded-full bg-blue-400 transition-[width] duration-300",
                      downloadPercent === null && "animate-pulse"
                    )}
                    style={{ width: `${downloadPercent ?? 100}%` }}
                  />
                </div>
                {downloadPercent !== null && (
                  <p className="mt-1.5 text-xs text-slate-400">{downloadPercent}%</p>
                )}
              </div>
            )}
            <p className="text-sm text-slate-400">
              {updateStatus === "restarting" &&
                "Applying update — this takes about 30 seconds."}
              {updateStatus === "reconnecting" && "Don't close this tab."}
              {updateStatus === "done" ? (
                <>
                  Now running <span className="font-mono text-slate-200">{installedVersion ?? version}</span>.
                </>
              ) : (
                updateStatus !== "restarting" &&
                updateStatus !== "reconnecting" &&
                (updateMessage || "Don't close this tab.")
              )}
            </p>
          </div>
        </Modal>
      )}
    </>
  )
}

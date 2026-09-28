import { useState, useEffect, useCallback, useRef } from "react"
import { NotificationsIcon, ProgressActivityIcon } from "@/components/icons"
import { cn } from "@/lib/utils"
import { PrefCard } from "@/components/settings/PrefCard"

type PairedDevice = {
  id?: string
  pairing_id?: string
  device_name: string
  platform: string
  paired_at: string
}
const devicePairingId = (d: PairedDevice) => d.id ?? d.pairing_id ?? ""
const DEVICE_LOAD_TIMEOUT_MS = 12_000

function pairedDeviceList(value: unknown): PairedDevice[] {
  if (!value || typeof value !== "object" || !("devices" in value) || !Array.isArray(value.devices)) {
    throw new Error("The device returned an invalid paired-device list.")
  }
  if (("error" in value && value.error) || ("success" in value && value.success === false)) {
    throw new Error("Could not load paired devices. Please retry.")
  }
  const devices = value.devices as unknown[]
  const ids = new Set<string>()
  return devices.map((item) => {
    if (!item || typeof item !== "object") throw new Error("The device returned an invalid paired-device list.")
    const device = item as Record<string, unknown>
    const id = typeof device.id === "string" && device.id ? device.id : device.pairing_id
    if (typeof id !== "string" || !id || ids.has(id) || typeof device.device_name !== "string" || typeof device.platform !== "string") {
      throw new Error("The device returned an invalid paired-device list.")
    }
    ids.add(id)
    return { id, device_name: device.device_name, platform: device.platform, paired_at: typeof device.paired_at === "string" ? device.paired_at : "" }
  })
}

export function MobileNotificationsSection() {
  const [pairingCode, setPairingCode] = useState<string | null>(null)
  const [expiresAt, setExpiresAt] = useState<string | null>(null)
  const [pairedDevices, setPairedDevices] = useState<PairedDevice[]>([])
  const [loading, setLoading] = useState(false)
  const [devicesLoading, setDevicesLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [devicesError, setDevicesError] = useState<string | null>(null)
  const [devicesLoaded, setDevicesLoaded] = useState(false)
  const devicesRequest = useRef<{ controller: AbortController; deadline: ReturnType<typeof setTimeout> | null } | null>(null)
  const mounted = useRef(false)
  const [removing, setRemoving] = useState<string | null>(null)
  const [countdown, setCountdown] = useState(0)
  const [testState, setTestState] = useState<"idle" | "loading" | "success" | "error">("idle")

  const loadPairedDevices = useCallback(async () => {
    const previous = devicesRequest.current
    previous?.controller.abort()
    if (previous?.deadline) clearTimeout(previous.deadline)
    const request = { controller: new AbortController(), deadline: null as ReturnType<typeof setTimeout> | null }
    devicesRequest.current = request
    const current = () => mounted.current && devicesRequest.current === request && !request.controller.signal.aborted
    request.deadline = setTimeout(() => {
      if (!current()) return
      request.controller.abort()
      devicesRequest.current = null
      setDevicesLoading(false)
      setDevicesError("Loading paired devices timed out. Check the Pi’s internet connection and retry.")
    }, DEVICE_LOAD_TIMEOUT_MS)
    try {
      const response = await fetch("/api/notifications/paired-devices", { signal: request.controller.signal })
      if (!response.ok) throw new Error("Could not load paired devices. Check the Pi’s internet connection and retry.")
      if (!/application\/json|\+json/i.test(response.headers.get("content-type") ?? "")) {
        throw new Error("The device returned a web page instead of paired-device data.")
      }
      const devices = pairedDeviceList(await response.json())
      if (!current()) return
      setPairedDevices(devices)
      setDevicesLoaded(true)
    } catch (reason) {
      if (current()) setDevicesError(reason instanceof Error ? reason.message : "Could not load paired devices.")
    } finally {
      if (request.deadline) clearTimeout(request.deadline)
      if (current()) {
        devicesRequest.current = null
        setDevicesLoading(false)
      }
    }
  }, [])

  useEffect(() => {
    mounted.current = true
    void loadPairedDevices()
    return () => {
      mounted.current = false
      const request = devicesRequest.current
      devicesRequest.current = null
      request?.controller.abort()
      if (request?.deadline) clearTimeout(request.deadline)
    }
  }, [loadPairedDevices])

  useEffect(() => {
    if (!expiresAt) return
    const interval = setInterval(() => {
      const remaining = Math.max(
        0,
        Math.floor((new Date(expiresAt).getTime() - Date.now()) / 1000)
      )
      setCountdown(remaining)
      if (remaining <= 0) {
        setPairingCode(null)
        setExpiresAt(null)
      }
    }, 1000)
    return () => clearInterval(interval)
  }, [expiresAt])

  async function generateCode() {
    setLoading(true)
    setError(null)
    try {
      const res = await fetch("/api/notifications/generate-code", { method: "POST" })
      if (!res.ok) {
        const data = await res.json()
        throw new Error(data.error || "Failed to generate code")
      }
      const data = await res.json()
      setPairingCode(data.code)
      setExpiresAt(data.expires_at)
      setCountdown(Math.max(0, Math.floor((new Date(data.expires_at).getTime() - Date.now()) / 1000)))
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to generate code")
    }
    setLoading(false)
  }

  async function removeDevice(pairingId: string) {
    if (!pairingId || removing) return
    setRemoving(pairingId)
    setError(null)
    try {
      const res = await fetch(`/api/notifications/paired-devices/${encodeURIComponent(pairingId)}`, { method: "DELETE" })
      if (!res.ok) throw new Error("Could not remove this device. Please retry.")
      setPairedDevices(previous => previous.filter(device => devicePairingId(device) !== pairingId))
    } catch (e) { setError(e instanceof Error ? e.message : "Could not remove this device.") }
    finally { setRemoving(null) }
  }

  async function sendTest() {
    setTestState("loading")
    try {
      const res = await fetch("/api/notifications/test", { method: "POST" })
      setTestState(res.ok ? "success" : "error")
    } catch {
      setTestState("error")
    }
    setTimeout(() => setTestState("idle"), 3000)
  }

  return (
    <PrefCard icon={<NotificationsIcon className="h-3.5 w-3.5" />} halo="violet" title="Mobile Notifications">
      <div className="flex items-center gap-3">
        {pairingCode ? (
          <div className="flex items-center gap-4">
            <span className="font-mono text-xl font-bold tracking-widest text-blue-400">
              {pairingCode}
            </span>
            <span className="text-xs text-slate-500">
              Expires in {Math.floor(countdown / 60)}:
              {String(countdown % 60).padStart(2, "0")}
            </span>
          </div>
        ) : (
          <button
            onClick={generateCode}
            disabled={loading}
            className="rounded-lg bg-blue-500 px-3 py-2 text-xs font-medium text-white transition-colors hover:bg-blue-600 disabled:opacity-50"
          >
            {loading && <ProgressActivityIcon className="mr-1 inline h-3.5 w-3.5 animate-spin" />}
            Generate Code
          </button>
        )}
      </div>

      {pairingCode && (
        <p className="text-xs text-slate-600">
          Enter this code in the Sentry USB mobile app under Settings → Pair for Notifications.
        </p>
      )}

      {error && <p role="alert" className="text-xs text-red-400">{error}</p>}

      {devicesError && <p role="alert" className="text-xs text-rose-300">{devicesError} <button type="button" className="text-blue-400" disabled={devicesLoading} onClick={() => { setDevicesLoading(true); setDevicesError(null); void loadPairedDevices() }}>Retry</button></p>}
      {devicesLoading && <p role="status" className="text-xs text-slate-400">{devicesLoaded ? "Refreshing paired devices..." : "Loading paired devices..."}</p>}
      {pairedDevices.length > 0 ? (
        <div className="space-y-2">
          <div className="flex items-center justify-between gap-3">
            <p className="section-label">Paired Devices</p>
            <button type="button" aria-label="Refresh paired devices" disabled={devicesLoading || removing !== null} onClick={() => { setDevicesLoading(true); setDevicesError(null); void loadPairedDevices() }} className="text-xs text-blue-400 disabled:opacity-50">Refresh</button>
          </div>
          {pairedDevices.map((device) => (
            <div
              key={devicePairingId(device)}
              className="flex items-center gap-3 rounded-xl border border-white/5 bg-white/[0.02] px-3 py-2.5"
            >
              <span className="text-sm text-slate-300">{device.device_name}</span>
              <span className="rounded-md bg-white/5 px-1.5 py-0.5 text-[10px] font-medium text-slate-500">
                {device.platform.toUpperCase()}
              </span>
              <span className="flex-1" />
              <button
                aria-label={`Remove ${device.device_name}`}
                disabled={removing !== null || devicesLoading}
                onClick={() => removeDevice(devicePairingId(device))}
                className="text-xs text-red-400/60 transition-colors hover:text-red-400"
              >
                Remove
              </button>
            </div>
          ))}
          <button
            onClick={sendTest}
            disabled={testState === "loading"}
            className={cn(
              "mt-1 w-full rounded-xl border border-white/5 bg-white/[0.03] px-3 py-2.5 text-xs transition-colors disabled:opacity-50",
              testState === "success"
                ? "text-emerald-400"
                : testState === "error"
                ? "text-red-400"
                : "text-slate-400 hover:bg-white/[0.06] hover:text-slate-300"
            )}
          >
            {testState === "loading"
              ? "Sending..."
              : testState === "success"
              ? "✓ Test sent!"
              : testState === "error"
              ? "Failed to send"
              : "Send Test Notification"}
          </button>
        </div>
      ) : devicesLoaded && !devicesLoading && !devicesError && (
        <p className="text-xs text-slate-600">No mobile devices paired yet.</p>
      )}
    </PrefCard>
  )
}

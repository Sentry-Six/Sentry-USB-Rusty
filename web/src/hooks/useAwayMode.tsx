import { createContext, useCallback, useContext, useEffect, useRef, useState } from "react"

export type AwayModeKind = "manual" | "auto"

interface AwayModeStatus {
    /** Backend automation mode; undefined until the first compatible response. */
    mode?: AwayModeKind
    state: "idle" | "active"
    has_rtc?: boolean
    /** False when the AP is unavailable; ap_error distinguishes a failed check. */
    ap_configured?: boolean
    ap_error?: string
    ap_starting?: boolean
    ap_start_error?: string
    ap_ssid?: string
    ap_ip?: string
    expires_at?: string
    remaining_sec?: number
    enabled_at?: string
    /** Committed home/away decision: true=home, false=away, null=undecided. */
    is_home?: boolean | null
    /** Whether automatic mode currently commands the AP on. */
    ap_on?: boolean
    geofence_configured?: boolean
    /** Whether the BLE GPS source is enabled. */
    ble_ready?: boolean
    /** Last GPS fix is missing or too old to act on. */
    gps_stale?: boolean
    last_fix_age_sec?: number | null
    home_lat?: number | null
    home_lon?: number | null
    home_radius_m?: number
}

/** Home geofence values for the Automatic-mode editor. */
export interface AwayGeofenceValues {
    homeLat: number | null
    homeLon: number | null
    radiusM: number
}

interface AwayModeContextValue {
    status: AwayModeStatus
    statusError: string | null
    actionError: string | null
    refresh: () => Promise<void>
    enable: (durationMin: number) => Promise<void>
    disable: () => Promise<void>
    setMode: (mode: AwayModeKind) => Promise<void>
    config: AwayGeofenceValues
    updateConfig: (patch: Partial<AwayGeofenceValues>) => void
    /** Last geofence-save failure, cleared after a successful save. */
    saveError: string | null
    useCurrentLocation: () => Promise<{ lat: number; lon: number } | null>
}

const DEFAULT_CONFIG: AwayGeofenceValues = { homeLat: null, homeLon: null, radiusM: 120 }

const AwayModeContext = createContext<AwayModeContextValue>({
    status: { state: "idle" },
    statusError: null,
    actionError: null,
    refresh: async () => { },
    enable: async () => { },
    disable: async () => { },
    setMode: async () => { },
    config: DEFAULT_CONFIG,
    updateConfig: () => { },
    saveError: null,
    useCurrentLocation: async () => null,
})

export function useAwayMode() {
    return useContext(AwayModeContext)
}

const AWAY_MODE_LS_KEY = "sentryusb_away_mode"

async function responseBody(res: Response) {
    const data = await res.json().catch(() => null)
    if (!res.ok) throw new Error(typeof data?.error === "string" ? data.error : `Request failed (HTTP ${res.status}).`)
    return data
}

function requireStatus(data: AwayModeStatus | null): AwayModeStatus {
    if (!data || (data.state !== "idle" && data.state !== "active")) {
        throw new Error("The device returned an invalid Away Mode status.")
    }
    return data
}

/** Returns locally stored Away Mode info (survives connection loss). */
export function getStoredAwayMode(): { enabled_at: string; ap_ssid: string; ap_ip: string } | null {
    try {
        const raw = localStorage.getItem(AWAY_MODE_LS_KEY)
        if (!raw) return null
        return JSON.parse(raw)
    } catch {
        return null
    }
}

export function AwayModeProvider({ children }: { children: React.ReactNode }) {
    const [status, setStatus] = useState<AwayModeStatus>({ state: "idle" })
    const [statusError, setStatusError] = useState<string | null>(null)
    const [actionError, setActionError] = useState<string | null>(null)
    const [config, setConfig] = useState<AwayGeofenceValues>(DEFAULT_CONFIG)
    const [saveError, setSaveError] = useState<string | null>(null)
    const lastMutation = useRef(0)
    const lastRefresh = useRef({ id: 0 })
    const saveTimer = useRef<number | null>(null)

    const refresh = useCallback(async () => {
        const mutation = lastMutation.current
        const request = ++lastRefresh.current.id
        const isCurrent = () => mutation === lastMutation.current && request === lastRefresh.current.id
        try {
            const res = await fetch("/api/away-mode/status")
            const data = requireStatus(await responseBody(res))
            if (isCurrent()) {
                setStatus(data)
                setStatusError(null)
                if (data.state === "idle" && data.mode !== "auto" && !data.ap_starting) {
                    localStorage.removeItem(AWAY_MODE_LS_KEY)
                }
            }
        } catch (error) {
            if (isCurrent()) setStatusError(`Could not check Away Mode: ${error instanceof Error ? error.message : "The device is unreachable."}`)
        }
    }, [])

    useEffect(() => {
        let mounted = true
        const requests = lastRefresh.current
        const tick = () => { if (mounted) refresh() }
        tick()
        const iv = setInterval(tick, 5000)
        return () => { mounted = false; clearInterval(iv); requests.id++ }
    }, [refresh])

    useEffect(() => {
        let alive = true
        fetch("/api/away-mode/config")
            .then((r) => r.json())
            .then((d) => {
                if (!alive) return
                setConfig({
                    homeLat: typeof d.home_lat === "number" ? d.home_lat : null,
                    homeLon: typeof d.home_lon === "number" ? d.home_lon : null,
                    radiusM: typeof d.home_radius_m === "number" ? d.home_radius_m : 120,
                })
            })
            .catch(() => { })
        return () => { alive = false }
    }, [])

    useEffect(() => () => {
        if (saveTimer.current) clearTimeout(saveTimer.current)
    }, [])

    const enable = useCallback(async (durationMin: number) => {
        lastMutation.current++
        setActionError(null)
        try {
            const res = await fetch("/api/away-mode/enable", {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({ duration_min: durationMin }),
            })
            const data = requireStatus(await responseBody(res))
            lastMutation.current++
            setStatus(data)
            setStatusError(null)
            // Preserve the deadline for the offline connection banner.
            localStorage.setItem(AWAY_MODE_LS_KEY, JSON.stringify({
                enabled_at: data.enabled_at ?? new Date().toISOString(),
                ap_ssid: data.ap_ssid ?? "",
                ap_ip: data.ap_ip ?? "",
            }))
        } catch (error) {
            setActionError(`Could not enable Away Mode: ${error instanceof Error ? error.message : "The device is unreachable."}`)
        }
    }, [])

    const disable = useCallback(async () => {
        lastMutation.current++
        setActionError(null)
        try {
            await responseBody(await fetch("/api/away-mode", { method: "DELETE" }))
            lastMutation.current++
            setStatus((prev) => ({ ...prev, state: "idle", ap_starting: false, ap_on: false, ap_start_error: undefined }))
            localStorage.removeItem(AWAY_MODE_LS_KEY)
        } catch (error) {
            setActionError(`Could not disable Away Mode: ${error instanceof Error ? error.message : "The device is unreachable."}`)
        }
    }, [])

    const setMode = useCallback(async (mode: AwayModeKind) => {
        lastMutation.current++
        setActionError(null)
        try {
            const res = await fetch("/api/away-mode/mode", {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({ mode }),
            })
            const data = requireStatus(await responseBody(res))
            lastMutation.current++
            setStatus((prev) => ({ ...prev, ...data }))
            // The mode response lacks AP metadata; fetch it after success.
            void refresh()
        } catch (error) {
            setActionError(`Could not change Away Mode: ${error instanceof Error ? error.message : "The device is unreachable."}`)
        }
    }, [refresh])

    const updateConfig = useCallback((patch: Partial<AwayGeofenceValues>) => {
        setConfig((prev) => {
            const next = { ...prev, ...patch }
            if (saveTimer.current) clearTimeout(saveTimer.current)
            // Each PUT remounts the read-only root, so debounce field edits.
            saveTimer.current = window.setTimeout(() => {
                fetch("/api/away-mode/config", {
                    method: "PUT",
                    headers: { "Content-Type": "application/json" },
                    body: JSON.stringify({
                        home_lat: next.homeLat,
                        home_lon: next.homeLon,
                        home_radius_m: next.radiusM,
                    }),
                })
                    .then((res) => {
                        setSaveError(res.ok ? null : "Couldn't save the geofence — the Pi rejected the change.")
                    })
                    .catch(() => {
                        setSaveError("Couldn't save the geofence — the Pi is unreachable.")
                    })
            }, 600)
            return next
        })
    }, [])

    /** Fetch the car's last GPS fix (for the "Use current location" button). */
    const useCurrentLocation = useCallback(async (): Promise<{ lat: number; lon: number } | null> => {
        try {
            const d = await fetch("/api/system/keep-accessory-gps").then((r) => r.json())
            if (typeof d.lat === "number" && typeof d.lon === "number") {
                return { lat: d.lat, lon: d.lon }
            }
            return null
        } catch {
            return null
        }
    }, [])

    return (
        <AwayModeContext.Provider
            value={{ status, statusError, actionError, refresh, enable, disable, setMode, config, updateConfig, saveError, useCurrentLocation }}
        >
            {children}
        </AwayModeContext.Provider>
    )
}

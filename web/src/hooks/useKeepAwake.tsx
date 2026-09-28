import { createContext, useCallback, useContext, useEffect, useRef, useState } from "react"

interface KeepAwakeStatus {
  state: "idle" | "pending" | "active"
  mode: string
  expires_at?: string
  remaining_sec?: number
}
interface KeepAwakeContextValue {
  status: KeepAwakeStatus
  mode: string | null
  pending: boolean
  error: string | null
  start: (durationMin: number) => Promise<void>
  stop: () => Promise<void>
  updateMode: (newMode: string) => Promise<void>
  reloadPreferences: () => Promise<void>
}
const KeepAwakeContext = createContext<KeepAwakeContextValue>({
  status: { state: "idle", mode: "" }, mode: null, pending: false, error: null,
  start: async () => {}, stop: async () => {}, updateMode: async () => {}, reloadPreferences: async () => {},
})
export function useKeepAwake() { return useContext(KeepAwakeContext) }

async function checkedFetch(url: string, init?: RequestInit) {
  const response = await fetch(url, init)
  if (!response.ok) throw new Error(`Request failed (${response.status}). Try again.`)
  return response
}

export function KeepAwakeProvider({ children }: { children: React.ReactNode }) {
  const [status, setStatus] = useState<KeepAwakeStatus>({ state: "idle", mode: "" })
  const [mode, setMode] = useState<string | null>(null)
  const [pending, setPending] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const lastHeartbeat = useRef(0)
  const mutation = useRef(0)
  const busy = useRef(false)
  const mounted = useRef(true)
  useEffect(() => { mounted.current = true; return () => { mounted.current = false } }, [])

  const reloadPreferences = useCallback(async () => {
    setError(null)
    try {
      const response = await checkedFetch("/api/config/preference?key=keep_awake_webui_mode")
      const data = await response.json()
      if (mounted.current) setMode(data.value || "")
    } catch (e) {
      if (mounted.current) setError(e instanceof Error ? e.message : "Could not load keep-awake settings.")
    }
  }, [])
  useEffect(() => { void reloadPreferences() }, [reloadPreferences])

  useEffect(() => {
    let stopped = false
    let inFlight = false
    async function poll() {
      if (document.hidden || inFlight || busy.current) return
      inFlight = true
      const version = mutation.current
      try {
        const response = await checkedFetch("/api/keep-awake/status")
        const data: KeepAwakeStatus = await response.json()
        if (!stopped && version === mutation.current && !busy.current) setStatus(data)
      } catch { /* Retain the last known status during reconnects. */ }
      finally { inFlight = false }
    }
    void poll()
    const interval = setInterval(poll, 5000)
    document.addEventListener("visibilitychange", poll)
    return () => { stopped = true; clearInterval(interval); document.removeEventListener("visibilitychange", poll) }
  }, [])

  useEffect(() => {
    if (mode !== "auto") return
    let stopped = false
    let inFlight = false
    async function sendHeartbeat() {
      const now = Date.now()
      if (document.hidden || busy.current || inFlight || now - lastHeartbeat.current < 30_000) return
      inFlight = true
      lastHeartbeat.current = now
      const version = mutation.current
      try {
        const response = await checkedFetch("/api/keep-awake/heartbeat", { method: "POST" })
        const data = await response.json()
        if (!stopped && version === mutation.current) setStatus(previous => ({ ...previous, state: data.state }))
      } catch { /* A later user interaction retries the heartbeat. */ }
      finally { inFlight = false }
    }
    const events = ["click", "keydown", "scroll", "touchstart", "mousemove"] as const
    events.forEach(event => window.addEventListener(event, sendHeartbeat, { passive: true }))
    void sendHeartbeat()
    return () => { stopped = true; events.forEach(event => window.removeEventListener(event, sendHeartbeat)) }
  }, [mode])

  const runMutation = useCallback(async (operation: () => Promise<void>) => {
    if (busy.current) return
    busy.current = true
    mutation.current++
    setPending(true)
    setError(null)
    try { await operation() }
    catch (e) { if (mounted.current) setError(e instanceof Error ? e.message : "Could not update keep-awake. Try again.") }
    finally { busy.current = false; if (mounted.current) setPending(false) }
  }, [])
  const start = useCallback((durationMin: number) => runMutation(async () => {
    const response = await checkedFetch("/api/keep-awake/start", {
      method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ mode: "manual", duration_min: durationMin }),
    })
    const data: KeepAwakeStatus = await response.json()
    if (mounted.current) setStatus(data)
  }), [runMutation])
  const stop = useCallback(() => runMutation(async () => {
    await checkedFetch("/api/keep-awake", { method: "DELETE" })
    if (mounted.current) setStatus({ state: "idle", mode: "" })
  }), [runMutation])
  const updateMode = useCallback((newMode: string) => runMutation(async () => {
    await checkedFetch("/api/config/preference", {
      method: "PUT", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ key: "keep_awake_webui_mode", value: newMode }),
    })
    if (!mounted.current) return
    setMode(newMode)
    if (newMode === "auto") {
      const response = await checkedFetch("/api/keep-awake/heartbeat", { method: "POST" })
      const data = await response.json()
      lastHeartbeat.current = Date.now()
      if (mounted.current) setStatus(previous => ({ ...previous, state: data.state }))
    } else if (newMode === "") {
      await checkedFetch("/api/keep-awake", { method: "DELETE" })
      if (mounted.current) setStatus({ state: "idle", mode: "" })
    }
  }), [runMutation])

  return <KeepAwakeContext.Provider value={{ status, mode, pending, error, start, stop, updateMode, reloadPreferences }}>
    {children}
  </KeepAwakeContext.Provider>
}

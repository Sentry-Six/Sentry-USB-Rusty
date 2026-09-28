import { useCallback, useEffect, useRef, useState } from "react"
import { fetchChargeSessions, fetchChargeTags, fetchCurrentCharge } from "@/api/charging"
import type { ChargeSessionSummary, CurrentCharge } from "@/types/charging"

type History = { sessions: ChargeSessionSummary[]; tags: string[]; current: CurrentCharge | null; at: number; hasHistory: boolean }
let cache: History | null = null
export function invalidateChargingHistory() { if (cache) cache.at = 0 }

export function useChargingHistory() {
  const [sessions, setSessions] = useState(() => cache?.sessions ?? [])
  const [tags, setTags] = useState(() => cache?.tags ?? [])
  const [current, setCurrent] = useState<CurrentCharge | null>(() => cache?.current ?? null)
  const [loading, setLoading] = useState(!cache?.hasHistory)
  const [error, setError] = useState<string | null>(null)
  const [warning, setWarning] = useState<string | null>(null)
  const active = useRef<AbortController | null>(null)
  const activeDeadline = useRef<ReturnType<typeof setTimeout> | null>(null)
  const mounted = useRef(false)
  const lastCharging = useRef(cache?.current?.charging ?? false)

  const remember = useCallback((change: Partial<History>) => {
    cache = { sessions: [], tags: [], current: null, at: 0, hasHistory: false, ...cache, ...change }
  }, [])

  const load = useCallback(async (fresh = true) => {
    active.current?.abort()
    if (activeDeadline.current) clearTimeout(activeDeadline.current)
    const request = new AbortController()
    active.current = request
    const valid = () => mounted.current && active.current === request && !request.signal.aborted
    let nextWarning: string | null = null
    const deadline = setTimeout(() => {
      if (!valid()) return
      if (!cache?.hasHistory) setError("Charging history timed out. Try again.")
      else setWarning("Charging refresh timed out. Showing the last available history.")
      setLoading(false)
      request.abort()
    }, 15_000)
    activeDeadline.current = deadline
    await Promise.allSettled([
      fetchChargeSessions(request.signal, fresh).then((rows) => {
        if (!valid()) return
        setSessions(rows)
        remember({ sessions: rows, at: Date.now(), hasHistory: true })
        setError(null)
      }).catch((reason) => {
        if (valid()) setError(reason instanceof Error ? reason.message : String(reason))
      }).finally(() => { if (valid()) setLoading(false) }),
      fetchChargeTags(request.signal).then((next) => {
        if (valid()) { setTags(next); remember({ tags: next }) }
      }).catch(() => { if (valid()) setWarning(nextWarning = "Tags could not refresh. Try again.") }),
      fetchCurrentCharge(request.signal).then((next) => {
        if (valid()) { setCurrent(next); lastCharging.current = next.charging; remember({ current: next }) }
      }).catch(() => { if (valid()) setWarning(nextWarning = "Live charging status is unavailable. History is still available.") }),
    ])
    clearTimeout(deadline)
    if (activeDeadline.current === deadline) activeDeadline.current = null
    if (valid()) setWarning(nextWarning)
    if (active.current === request) active.current = null
  }, [remember])

  const reload = useCallback((fresh = true) => {
    if (!cache?.hasHistory) setLoading(true)
    setError(null)
    setWarning(null)
    return load(fresh)
  }, [load])

  useEffect(() => {
    mounted.current = true
    let stopped = false
    let timer: ReturnType<typeof setTimeout>
    let pollRequest: AbortController | null = null
    let polling = false
    const poll = async () => {
      if (polling || active.current || document.hidden || stopped) return
      polling = true
      pollRequest = new AbortController()
      try {
        const next = await fetchCurrentCharge(pollRequest.signal)
        if (stopped || pollRequest.signal.aborted || active.current) return
        const finished = lastCharging.current && !next.charging
        lastCharging.current = next.charging
        setCurrent(next)
        remember({ current: next })
        if (next.charging || finished) await load(finished)
      } catch {
        if (!stopped && !pollRequest.signal.aborted) setWarning("Live charging status could not refresh. Try again.")
      } finally { polling = false }
    }
    const schedule = () => { timer = setTimeout(async () => { await poll(); if (!stopped) schedule() }, 30_000) }
    const foreground = () => {
      if (!document.hidden && !active.current && !polling) void poll()
    }
    if (!cache?.hasHistory || Date.now() - cache.at >= 15_000) void load(cache?.at === 0)
    schedule()
    document.addEventListener("visibilitychange", foreground)
    return () => {
      stopped = true
      mounted.current = false
      active.current?.abort()
      if (activeDeadline.current) clearTimeout(activeDeadline.current)
      activeDeadline.current = null
      pollRequest?.abort()
      clearTimeout(timer)
      document.removeEventListener("visibilitychange", foreground)
    }
  }, [load, remember])

  const patchTags = useCallback((id: number, next: string[]) => {
    const updated = (cache?.sessions ?? []).map((session) => session.id === id ? { ...session, tags: next } : session)
    remember({ sessions: updated })
    setSessions(updated)
  }, [remember])

  return { sessions, tags, current, loading, error, warning, reload, patchTags }
}

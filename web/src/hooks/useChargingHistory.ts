import { useCallback, useEffect, useRef, useState } from "react"
import { fetchChargeSessions, fetchChargeTags, fetchCurrentCharge } from "@/api/charging"
import type { ChargeSessionSummary, CurrentCharge } from "@/types/charging"

type History = { sessions: ChargeSessionSummary[]; tags: string[]; current: CurrentCharge | null; at: number }
let cache: History | null = null
export function invalidateChargingHistory() { if (cache) cache.at = 0 }

export function useChargingHistory() {
  const [sessions, setSessions] = useState(() => cache?.sessions ?? [])
  const [tags, setTags] = useState(() => cache?.tags ?? [])
  const [current, setCurrent] = useState<CurrentCharge | null>(() => cache?.current ?? null)
  const [loading, setLoading] = useState(!cache)
  const [error, setError] = useState<string | null>(null)
  const [warning, setWarning] = useState<string | null>(null)
  const active = useRef<AbortController | null>(null)
  const mounted = useRef(false)
  const lastCharging = useRef(cache?.current?.charging ?? false)

  const remember = useCallback((change: Partial<History>) => {
    cache = { sessions: [], tags: [], current: null, at: 0, ...cache, ...change }
  }, [])

  const reload = useCallback(async (fresh = true) => {
    active.current?.abort()
    const request = new AbortController()
    active.current = request
    const valid = () => mounted.current && active.current === request && !request.signal.aborted
    if (!cache) setLoading(true)
    setError(null)
    setWarning(null)
    const deadline = setTimeout(() => {
      if (!valid()) return
      if (!cache?.at && !cache?.sessions.length) setError("Charging history timed out. Try again.")
      else setWarning("Charging refresh timed out. Showing the last available history.")
      setLoading(false)
      request.abort()
    }, 15_000)
    await Promise.allSettled([
      fetchChargeSessions(request.signal, fresh).then((rows) => {
        if (!valid()) return
        setSessions(rows)
        remember({ sessions: rows, at: Date.now() })
      }).catch((reason) => {
        if (valid()) setError(reason instanceof Error ? reason.message : String(reason))
      }).finally(() => { if (valid()) setLoading(false) }),
      fetchChargeTags(request.signal).then((next) => {
        if (valid()) { setTags(next); remember({ tags: next }) }
      }).catch(() => { if (valid()) setWarning("Tags could not refresh. Try again.") }),
      fetchCurrentCharge(request.signal).then((next) => {
        if (valid()) { setCurrent(next); lastCharging.current = next.charging; remember({ current: next }) }
      }).catch(() => { if (valid()) setWarning("Live charging status is unavailable. History is still available.") }),
    ])
    clearTimeout(deadline)
    if (active.current === request) active.current = null
  }, [remember])

  useEffect(() => {
    mounted.current = true
    let timer: ReturnType<typeof setTimeout>
    let pollRequest: AbortController | null = null
    let polling = false
    const poll = async () => {
      if (polling || active.current || document.hidden || !mounted.current) return
      polling = true
      pollRequest = new AbortController()
      try {
        const next = await fetchCurrentCharge(pollRequest.signal)
        if (!mounted.current || active.current) return
        const finished = lastCharging.current && !next.charging
        lastCharging.current = next.charging
        setCurrent(next)
        remember({ current: next })
        if (next.charging || finished) await reload(finished)
      } catch {
        if (mounted.current) setWarning("Live charging status could not refresh. Try again.")
      } finally { polling = false }
    }
    const schedule = () => { timer = setTimeout(async () => { await poll(); if (mounted.current) schedule() }, 30_000) }
    const foreground = () => {
      if (!document.hidden && !active.current && !polling) void poll()
    }
    if (!cache || Date.now() - cache.at >= 15_000) void reload(cache?.at === 0)
    schedule()
    document.addEventListener("visibilitychange", foreground)
    return () => {
      mounted.current = false
      active.current?.abort()
      pollRequest?.abort()
      clearTimeout(timer)
      document.removeEventListener("visibilitychange", foreground)
    }
  }, [reload, remember])

  const patchTags = useCallback((id: number, next: string[]) => {
    setSessions((previous) => {
      const updated = previous.map((session) => session.id === id ? { ...session, tags: next } : session)
      remember({ sessions: updated })
      return updated
    })
  }, [remember])

  return { sessions, tags, current, loading, error, warning, reload, patchTags }
}

import { useState, useEffect, useCallback, useRef } from "react"
import { api } from "@/lib/api"
import type { ClipTelemetry, TelemetryFrame } from "@/lib/api"

// A day of Recent clips is hundreds of one-minute files; keep only the
// telemetry around where the user is watching.
const CACHE_LIMIT = 8

const keyFor = (clipPath: string, file: string) => JSON.stringify([clipPath, file])

function remember(cache: Map<string, ClipTelemetry>, key: string, data: ClipTelemetry) {
  const next = new Map(cache)
  next.delete(key)
  next.set(key, data)
  while (next.size > CACHE_LIMIT) next.delete(next.keys().next().value!)
  return next
}

/**
 * `waitFor`, when given, is waited on before each minute's telemetry is asked
 * for. The viewer uses it to let the cameras load first: the browser keeps
 * only a few connections open to the Pi, and the picture matters more.
 */
export function useTelemetry(
  clipPath: string | null,
  frontFile: string | null,
  waitFor?: (signal: AbortSignal) => Promise<void>,
) {
  const [cache, setCache] = useState(() => new Map<string, ClipTelemetry>())
  const [failedKey, setFailedKey] = useState<string | null>(null)
  const [last, setLast] = useState<{ clipPath: string; telemetry: ClipTelemetry } | null>(null)
  const key = clipPath && frontFile ? keyFor(clipPath, frontFile) : null
  if (failedKey !== null && failedKey !== key) setFailedKey(null)
  const telemetry = key ? cache.get(key) ?? null : null
  const loading = !!key && !cache.has(key) && failedKey !== key

  // The newest telemetry shown for this clip, to hold on screen while the
  // next minute's loads instead of blanking the cards every minute.
  if (last && last.clipPath !== clipPath) setLast(null)
  else if (telemetry && clipPath && last?.telemetry !== telemetry) setLast({ clipPath, telemetry })
  const held = !telemetry && loading && last?.clipPath === clipPath ? last.telemetry : null

  useEffect(() => {
    if (!key || !clipPath || !frontFile || cache.has(key)) return
    const controller = new AbortController()
    const { signal } = controller
    const read = () => api.getClipTelemetry(clipPath, frontFile, signal)
      .then((data) => {
        if (signal.aborted) return
        setCache((previous) => remember(previous, key, data))
        setFailedKey(null)
      })
      .catch(() => { if (!signal.aborted) setFailedKey(key) })
    if (waitFor) void waitFor(signal).then(() => { if (!signal.aborted) void read() })
    else void read()
    return () => controller.abort()
  }, [key, clipPath, frontFile, cache, waitFor])

  // Fetch the next minute's telemetry shortly before it is needed.
  const cacheRef = useRef(cache)
  useEffect(() => { cacheRef.current = cache }, [cache])
  const prefetching = useRef(new Set<string>())
  const prefetch = useCallback((file: string) => {
    if (!clipPath) return
    const wanted = keyFor(clipPath, file)
    if (cacheRef.current.has(wanted) || prefetching.current.has(wanted)) return
    prefetching.current.add(wanted)
    api.getClipTelemetry(clipPath, file)
      .then((data) => setCache((previous) => remember(previous, wanted, data)))
      // The segment asks again when it is on screen, and reports a failure then.
      .catch(() => {})
      .finally(() => { prefetching.current.delete(wanted) })
  }, [clipPath])

  const frameAtTime = useCallback((seconds: number): TelemetryFrame | null => {
    if (!telemetry || !telemetry.frames.length) return null
    const frames = telemetry.frames
    if (seconds <= frames[0].t) return frames[0]
    if (seconds >= frames[frames.length - 1].t) return frames[frames.length - 1]

    let lo = 0
    let hi = frames.length - 1
    while (lo < hi - 1) {
      const mid = (lo + hi) >> 1
      if (frames[mid].t <= seconds) lo = mid
      else hi = mid
    }
    return (seconds - frames[lo].t) <= (frames[hi].t - seconds) ? frames[lo] : frames[hi]
  }, [telemetry])

  return { telemetry, held, loading, frameAtTime, prefetch }
}

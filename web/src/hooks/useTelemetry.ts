import { useState, useEffect, useCallback } from "react"
import { api } from "@/lib/api"
import type { ClipTelemetry, TelemetryFrame } from "@/lib/api"

export function useTelemetry(clipPath: string | null, frontFile: string | null) {
  const [cache, setCache] = useState(() => new Map<string, ClipTelemetry>())
  const [failedKey, setFailedKey] = useState<string | null>(null)
  const key = clipPath && frontFile ? JSON.stringify([clipPath, frontFile]) : null
  if (failedKey !== null && failedKey !== key) setFailedKey(null)
  const telemetry = key ? cache.get(key) ?? null : null
  const loading = !!key && !cache.has(key) && failedKey !== key

  useEffect(() => {
    if (!key || !clipPath || !frontFile || cache.has(key)) return
    const controller = new AbortController()
    api.getClipTelemetry(clipPath, frontFile, controller.signal)
      .then((data) => {
        if (controller.signal.aborted) return
        setCache((previous) => new Map(previous).set(key, data))
        setFailedKey(null)
      })
      .catch(() => { if (!controller.signal.aborted) setFailedKey(key) })
    return () => controller.abort()
  }, [key, clipPath, frontFile, cache])

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

  return { telemetry, loading, frameAtTime }
}

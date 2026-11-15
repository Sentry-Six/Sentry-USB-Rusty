import { useEffect, useState } from "react"
import { api } from "@/lib/api"

export interface ArchivingState {
  /** True while the Pi is copying clips to the archive. */
  archiving: boolean
  /** The Pi's estimate of how long it has left, when it has one. */
  etaSeconds: number | null
}

const POLL_MS = 15_000
const IDLE: ArchivingState = { archiving: false, etaSeconds: null }

/**
 * Whether the Pi is archiving right now. Archiving sends clips over the same
 * Wi-Fi link the browser streams video over, and it can take most of it.
 */
export function useArchiving(): ArchivingState {
  const [state, setState] = useState<ArchivingState>(IDLE)

  useEffect(() => {
    const controller = new AbortController()
    const read = () => {
      api.getDriveStatus(controller.signal)
        .then((status) => {
          if (controller.signal.aborted) return
          // Older versions report only the phase.
          const archiving = status.archiving === true || status.phase === "archiving"
          const eta = status.eta_seconds
          const etaSeconds = archiving && typeof eta === "number" && eta > 0 ? eta : null
          setState((prev) => (prev.archiving === archiving && prev.etaSeconds === etaSeconds ? prev : { archiving, etaSeconds }))
        })
        // An unreachable Pi is the connection banner's job, not this notice's.
        .catch(() => {})
    }
    read()
    const interval = setInterval(read, POLL_MS)
    return () => { controller.abort(); clearInterval(interval) }
  }, [])

  return state
}

/** `about 5 minutes`, `about 1 hour 10 minutes`: a rough wait in plain words. */
export function roughWait(seconds: number): string {
  const minutes = Math.max(1, Math.round(seconds / 60))
  if (minutes < 60) return `about ${minutes} minute${minutes === 1 ? "" : "s"}`
  const hours = Math.floor(minutes / 60)
  // Past an hour, the nearest ten minutes is as precise as the estimate is.
  const rest = Math.round((minutes % 60) / 10) * 10
  const hourWords = `${hours + (rest === 60 ? 1 : 0)} hour${hours + (rest === 60 ? 1 : 0) === 1 ? "" : "s"}`
  return rest === 0 || rest === 60 ? `about ${hourWords}` : `about ${hourWords} ${rest} minutes`
}

import { useEffect, useState } from "react"
import { fetchDriveDetail, setDriveTags } from "@/api/drives"
import type { DriveDetail } from "@/types/drives"

export interface DriveDetailState {
  drive: DriveDetail | null
  loading: boolean
  error: string | null
  saveTags: (tags: string[]) => Promise<void>
  refresh: () => Promise<void>
}

export function useDriveDetail(id: string | undefined): DriveDetailState {
  const [drive, setDrive] = useState<DriveDetail | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [tick, setTick] = useState(0)

  useEffect(() => {
    if (!id) {
      /* eslint-disable-next-line react-hooks/set-state-in-effect */
      setDrive(null)
      setLoading(false)
      setError(null)
      return
    }
    let cancelled = false
    const controller = new AbortController()
    setLoading(true)
    setError(null)
    fetchDriveDetail(id, controller.signal)
      .then((detail) => {
        if (!cancelled) setDrive(detail)
      })
      .catch((e) => {
        if (cancelled) return
        setError(e instanceof Error ? e.message : String(e))
      })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })
    return () => {
      cancelled = true
      controller.abort()
    }
  }, [id, tick])

  const saveTags = async (tags: string[]) => {
    if (!id) return
    await setDriveTags(id, tags)
    setDrive((d) => (d ? { ...d, tags } : d))
  }

  const refresh = async () => {
    setTick((t) => t + 1)
  }

  return { drive, loading, error, saveTags, refresh }
}

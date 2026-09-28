import { useCallback, useEffect, useRef, useState } from "react"
import { loadRates, parseRates, saveRateChanges, type RateDocument, type ChargingRates } from "@/lib/charging-rate-editor"
export type { ChargingRates, RateSchedule, TagRate } from "@/lib/charging-rate-editor"

export function useChargingRates() {
  const [rates, setRates] = useState<ChargingRates>({ currency: "$", defaultRate: null, tags: {} })
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const source = useRef<RateDocument | null>(null)
  const generation = useRef(0)
  const request = useRef<AbortController | null>(null)

  const read = useCallback(() => {
    request.current?.abort()
    const controller = new AbortController()
    request.current = controller
    const ticket = ++generation.current
    source.current = null
    const current = () => generation.current === ticket && !controller.signal.aborted
    return loadRates((url, init) => fetch(url, { ...init, signal: controller.signal }))
      .then((document) => {
        if (!current()) return null
        source.current = document
        const rates = parseRates(document)
        setRates(rates)
        setError(null)
        setLoading(false)
        return rates
      })
      .catch((error) => {
        if (current()) {
          setError(error instanceof Error ? error.message : "Rates could not be loaded.")
          setLoading(false)
        }
        return null
      })
      .finally(() => { if (request.current === controller) request.current = null })
  }, [])

  const refresh = useCallback(() => {
    setLoading(true)
    setError(null)
    return read()
  }, [read])

  const cancel = useCallback(() => {
    generation.current++
    source.current = null
    request.current?.abort()
    request.current = null
  }, [])

  useEffect(() => {
    void read()
    return cancel
  }, [read, cancel])

  const save = useCallback(async (next: ChargingRates) => {
    const expected = source.current
    if (!expected) throw new Error("Reload rates before saving.")
    const ticket = generation.current
    const document = await saveRateChanges(expected, next)
    if (generation.current !== ticket) throw new Error("Rates were reloaded while saving. Reopen the editor to check.")
    source.current = document
    setRates(parseRates(document))
  }, [])

  return { rates, loading, error, save, refresh }
}

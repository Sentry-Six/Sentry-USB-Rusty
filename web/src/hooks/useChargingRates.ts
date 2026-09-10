import { useCallback, useEffect, useRef, useState } from "react"
import { loadRates, parseRates, saveRateChanges, type RateDocument, type ChargingRates } from "@/lib/charging-rate-editor"
export type { ChargingRates, RateSchedule, TagRate } from "@/lib/charging-rate-editor"

export function useChargingRates() {
  const [rates, setRates] = useState<ChargingRates>({ currency: "$", defaultRate: null, tags: {} })
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const source = useRef<RateDocument | null>(null)
  const generation = useRef(0)

  const refresh = useCallback(async () => {
    const ticket = ++generation.current
    setLoading(true)
    setError(null)
    source.current = null
    try {
      const document = await loadRates()
      if (generation.current !== ticket) return null
      source.current = document
      const rates = parseRates(document)
      setRates(rates)
      return rates
    } catch (error) {
      if (generation.current === ticket) setError(error instanceof Error ? error.message : "Rates could not be loaded.")
      return null
    } finally {
      if (generation.current === ticket) setLoading(false)
    }
  }, [])

  const invalidate = useCallback(() => {
    generation.current++
    source.current = null
  }, [])

  useEffect(() => {
    void refresh()
    return invalidate
  }, [refresh, invalidate])

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

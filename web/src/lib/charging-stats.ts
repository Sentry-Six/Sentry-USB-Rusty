import type { ChargeSessionSummary } from "../types/charging"

export function summarizeCharges(sessions: ChargeSessionSummary[]) {
  const currencies = new Map<string, number>()
  let totalEnergyKwh = 0, energySessionCount = 0, totalDurationSecs = 0, efficiencySum = 0, efficiencyCount = 0
  for (const session of sessions) {
    if (session.energyAddedKwh != null && Number.isFinite(session.energyAddedKwh)) {
      totalEnergyKwh += session.energyAddedKwh
      energySessionCount++
    }
    totalDurationSecs += session.durationSecs
    if (session.cost != null && Number.isFinite(session.cost)) {
      currencies.set(session.currency, (currencies.get(session.currency) ?? 0) + session.cost)
    }
    if (session.efficiencyPct != null && Number.isFinite(session.efficiencyPct)) {
      efficiencySum += session.efficiencyPct
      efficiencyCount++
    }
  }
  const costsByCurrency = [...currencies].sort(([a], [b]) => a.localeCompare(b)).map(([currency, amount]) => ({ currency, amount }))
  return {
    count: sessions.length, totalEnergyKwh, energySessionCount, totalDurationSecs,
    totalCost: costsByCurrency.length === 1 ? costsByCurrency[0].amount : null,
    currency: costsByCurrency.length === 1 ? costsByCurrency[0].currency : "",
    costsByCurrency, avgEfficiency: efficiencyCount ? efficiencySum / efficiencyCount : null,
  }
}

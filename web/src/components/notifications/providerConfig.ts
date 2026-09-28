import { notificationFieldNames } from "@/components/setup/notificationFields"
export type ProviderValues = Record<string, string>
export interface ProviderConfig { values: ProviderValues; editable: boolean }
const record = (value: unknown): value is Record<string, unknown> => !!value && typeof value === "object" && !Array.isArray(value)
export const isJsonResponse = (response: Response) => /\bapplication\/(?:[\w.+-]+\+)?json\b/i.test(response.headers.get("content-type") ?? "")
export function providerValues(value: unknown): ProviderValues | null {
  if (!record(value) || !record(value.values)) return null
  return Object.entries(value.values).every(([key, entry]) => notificationFieldNames.has(key) && typeof entry === "string")
    ? value.values as ProviderValues : null
}
export async function readProviderConfig(signal: AbortSignal): Promise<ProviderConfig> {
  const response = await fetch("/api/notifications/providers", { cache: "no-store", signal })
  if (!response.ok && ![404, 405, 501].includes(response.status)) throw new Error("Could not load delivery settings. Please retry.")
  if (response.ok && isJsonResponse(response)) {
    let data: unknown
    try { data = await response.json() } catch { throw new Error("Could not read delivery settings. Please retry.") }
    const values = providerValues(data)
    if (values) return { values, editable: true }
  }
  // Older servers return the app HTML for unknown API routes. Only read their config.
  const legacy = await fetch("/api/setup/config", { cache: "no-store", signal })
  if (!legacy.ok || !isJsonResponse(legacy)) throw new Error("Could not load delivery settings from this device.")
  let data: unknown
  try { data = await legacy.json() } catch { throw new Error("Could not read delivery settings from this device.") }
  if (!record(data) || !Object.values(data).every(entry => record(entry) && typeof entry.value === "string" && typeof entry.active === "boolean")) {
    throw new Error("Could not read delivery settings from this device.")
  }
  const values: ProviderValues = {}
  for (const [key, entry] of Object.entries(data)) {
    const setting = entry as { value: string; active: boolean }
    if (notificationFieldNames.has(key) && setting.active) values[key] = setting.value
  }
  return { values, editable: false }
}

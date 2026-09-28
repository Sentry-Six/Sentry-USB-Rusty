import { useCallback, useEffect, useRef, useState } from "react"
import { NotificationsStep } from "@/components/setup/steps/NotificationsStep"
import { requiredByProvider } from "@/components/setup/notificationFields"

import { isJsonResponse, providerValues, readProviderConfig, type ProviderValues as Values } from "./providerConfig"

export function ProviderConfigSection() {
  const [original, setOriginal] = useState<Values | null>(null)
  const [values, setValues] = useState<Values>({})
  const [loading, setLoading] = useState(true)
  const [editable, setEditable] = useState(false)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState("")
  const [message, setMessage] = useState("")
  const request = useRef<AbortController | null>(null)
  const load = useCallback(() => {
    request.current?.abort()
    const controller = new AbortController()
    request.current = controller
    return readProviderConfig(controller.signal)
      .then(config => {
        if (controller.signal.aborted) return
        setOriginal(config.values)
        setValues(config.values)
        setEditable(config.editable)
      })
      .catch(error => {
        if (!controller.signal.aborted) setError(error instanceof Error ? error.message : "Could not load delivery settings.")
      })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
  }, [])
  useEffect(() => { void load(); return () => request.current?.abort() }, [load])

  async function save() {
    if (!original || !editable || saving || loading) return
    for (const fields of Object.values(requiredByProvider)) {
      const edited = fields.some(key => (values[key] ?? "") !== (original[key] ?? ""))
      if (edited && fields.some(key => values[key]?.trim()) && !fields.every(key => values[key]?.trim())) {
        setError("Complete all required fields for each configured provider.")
        return
      }
    }
    setSaving(true)
    setError("")
    setMessage("")
    const changes = Object.fromEntries(Object.entries(values).filter(([key, value]) => value !== (original[key] ?? "")))
    try {
      const response = await fetch("/api/notifications/providers", {
        method: "PUT", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ changes, expected: original }),
      })
      if (!isJsonResponse(response)) {
        setEditable(false)
        throw new Error("This device cannot save delivery settings here yet. Your changes were not saved.")
      }
      const result = await response.json().catch(() => null)
      if (!response.ok) throw new Error(typeof result?.error === "string" ? result.error : "Could not save delivery settings.")
      const saved = providerValues(result)
      if (!saved) throw new Error("Could not confirm saved delivery settings. Reload to check their current values.")
      setOriginal(saved)
      setValues(saved)
      setMessage("Saved. Providers use these settings for the next notification." +
        ("NOTIFICATION_TITLE" in changes ? " The new title applies to archive messages after the next Pi restart." : ""))
    } catch (e) { setError(e instanceof Error ? e.message : "Could not save delivery settings.") }
    finally { setSaving(false) }
  }
  const dirty = original && Object.entries(values).some(([key, value]) => value !== (original[key] ?? ""))
  return <section className="glass-card p-5" aria-label="Notification providers">
    {loading ? <p role="status" className="text-sm text-slate-400">Loading delivery settings…</p> : original && (
      <fieldset disabled={saving} className="min-w-0">
        {!editable && <p role="status" className="mb-4 text-sm text-slate-400">View only on this device version. Use the Setup Wizard to change providers, or update the Pi to enable editing here.</p>}
        <NotificationsStep setupAlreadyFinished readOnly={!editable} data={values} onChange={(key, value) => { setValues(previous => ({ ...previous, [key]: value })); setMessage("") }}
          onBatchChange={changes => setValues(previous => ({ ...previous, ...changes }))} />
      </fieldset>
    )}
    {error && <div role="alert" className="mt-4 text-sm text-rose-300">
      <p>{error}</p>
      <button type="button" onClick={() => { setLoading(true); setError(""); void load() }} className="mt-2 text-blue-400" disabled={saving || loading}>Reload delivery settings</button>
    </div>}
    {message && <p role="status" className="mt-4 text-sm text-emerald-300">{message}</p>}
    {original && editable && <div className="mt-5 flex justify-end gap-2 border-t border-white/10 pt-4">
      <button type="button" disabled={!dirty || saving} onClick={() => { setValues(original); setError(""); setMessage("") }}
        className="rounded-lg px-4 py-2 text-sm text-slate-300 disabled:opacity-40">Discard changes</button>
      <button type="button" disabled={!dirty || saving} onClick={() => void save()}
        className="rounded-lg bg-blue-500 px-4 py-2 text-sm font-medium text-white disabled:opacity-40">{saving ? "Saving…" : "Save delivery settings"}</button>
    </div>}
  </section>
}

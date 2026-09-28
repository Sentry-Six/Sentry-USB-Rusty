import { useState, useEffect, useRef, useCallback } from "react"
import { CancelIcon, CheckCircleIcon, ErrorIcon, InfoIcon, ProgressActivityIcon, StethoscopeIcon, WarningIcon } from "@/components/icons"
import { InfoButton } from "@/components/ui/InfoButton"
import { needsLegacyStorageProbe, normalizeLegacyStorage, type HealthItem, type HealthReport } from "./healthReport"
import { Modal } from "@/components/ui/Modal"

const STATUS = {
  info: { label: "Managed reserve", color: "text-slate-400", icon: InfoIcon },
  pass: { label: "Healthy", color: "text-emerald-400", icon: CheckCircleIcon },
  warn: { label: "Needs attention", color: "text-amber-400", icon: WarningIcon },
  fail: { label: "Action needed", color: "text-red-400", icon: CancelIcon },
  unknown: { label: "Not measured", color: "text-slate-400", icon: InfoIcon },
  recovering: { label: "Recovering", color: "text-blue-400", icon: ProgressActivityIcon },
  not_applicable: { label: "Not applicable", color: "text-slate-500", icon: InfoIcon },
}
function CheckRow({ item }: { item: HealthItem }) {
  const presentation = STATUS[item.status] ?? STATUS.unknown
  const Icon = presentation.icon
  return <li className="flex items-start gap-3 py-2">
    <Icon className={`mt-0.5 h-4 w-4 shrink-0 ${presentation.color}`} />
    <div className="min-w-0 flex-1">
      <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
        <span className="flex items-center gap-1 text-sm text-slate-200">{item.name}{item.explanation && <InfoButton title={item.name}><p>{item.explanation}</p></InfoButton>}</span>
        <span className={`text-xs ${presentation.color}`}>{presentation.label}</span>
      </div>
      {item.detail && <p className="mt-1 text-xs leading-relaxed text-slate-400">{item.detail}</p>}
    </div>
  </li>
}
async function readHealthReport(signal: AbortSignal): Promise<HealthReport> {
  const res = await fetch("/api/system/health-check", { signal })
  if (!res.ok) throw new Error(`Server responded with ${res.status}`)
  const data: HealthReport = await res.json()
  if (needsLegacyStorageProbe(data)) {
    try {
      const space = await fetch("/api/backingfiles/free-space", { signal, cache: "no-store" })
      if (space.ok && /application\/(?:[\w.+-]+\+)?json/i.test(space.headers.get("content-type") ?? "")) {
        return normalizeLegacyStorage(data, await space.json())
      }
    } catch { /* Preserve the original warning if the independent reading fails. */ }
  }
  return data
}

export function HealthCheckModal({ onClose }: { onClose: () => void }) {
  const [loading, setLoading] = useState(true)
  const [report, setReport] = useState<HealthReport | null>(null)
  const [error, setError] = useState<string | null>(null)
  const request = useRef<AbortController | null>(null)
  const runCheck = useCallback(() => {
    request.current?.abort()
    const controller = new AbortController()
    request.current = controller
    return readHealthReport(controller.signal)
      .then(data => { if (!controller.signal.aborted) setReport(data) })
      .catch(err => { if (!controller.signal.aborted) setError(err instanceof Error ? err.message : "Health check failed") })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
  }, [])
  useEffect(() => { void runCheck(); return () => request.current?.abort() }, [runCheck])
  const items = report?.categories.flatMap(category => category.items) ?? []
  const attention = items.filter(item => item.status === "warn" || item.status === "fail")
  const monitoring = items.filter(item => item.status === "recovering" || item.status === "unknown")
  return <Modal title={<span className="flex items-center gap-2"><StethoscopeIcon className="h-4 w-4" />Health Check</span>}
    onClose={onClose} size="lg" footer={<div className="flex justify-end">
      <button onClick={() => { setLoading(true); setError(null); void runCheck() }} disabled={loading} className="rounded-lg px-3 py-2 text-sm text-slate-300 hover:bg-white/5 disabled:opacity-50">{loading ? "Checking…" : "Run again"}</button>
    </div>}>
    <div aria-busy={loading}>
      {loading && <p role="status" className="flex items-center gap-2 py-4 text-sm text-slate-400"><ProgressActivityIcon className="h-4 w-4 animate-spin" />Checking device…</p>}
      {error && <div role="alert" className="flex items-center gap-2 py-4 text-sm text-red-300"><ErrorIcon className="h-4 w-4" />{error}</div>}
      {report && <>
        <p className="mb-3 text-sm font-medium text-slate-200">{report.summary}</p>
        {attention.length > 0 && <section aria-label="Needs attention">
          <h3 className="text-xs font-semibold uppercase tracking-wide text-slate-400">Needs attention</h3>
          <ul className="divide-y divide-white/5">{attention.map(item => <CheckRow key={item.name} item={item} />)}</ul>
        </section>}
        {monitoring.length > 0 && <section className="mt-4" aria-label="Device status">
          <h3 className="text-xs font-semibold uppercase tracking-wide text-slate-400">Device status</h3>
          <ul className="divide-y divide-white/5">{monitoring.map(item => <CheckRow key={item.name} item={item} />)}</ul>
        </section>}
        <details className="settings-details mt-4 border-t border-white/10 pt-2">
          <summary>All checks ({items.length})</summary>
          <div className="space-y-4 pt-2">{report.categories.map(category => <section key={category.name}>
            <h3 className="text-sm font-semibold text-slate-300">{category.name}</h3>
            <ul className="divide-y divide-white/5">{category.items.map(item => <CheckRow key={item.name} item={item} />)}</ul>
          </section>)}</div>
        </details>
      </>}
    </div>
  </Modal>
}

import { useEffect, useState } from "react"
import { CheckIcon, CloseIcon, ProgressActivityIcon, VerifiedUserIcon } from "@/components/icons"
import type { StepProps } from "../SetupWizard"
import { cn } from "@/lib/utils"

export function PrivacyStep({ data, onChange, setupAlreadyFinished }: StepProps) {
  const [choice, setChoice] = useState<boolean | null>(() =>
    data._analytics_choice === "true" ? true : data._analytics_choice === "false" ? false : null)
  const [saving, setSaving] = useState<boolean | null>(null)
  const [loading, setLoading] = useState(true)
  const [reload, setReload] = useState(0)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const controller = new AbortController()
    let cancelled = false
    const timeout = setTimeout(() => controller.abort(), 10_000)
    fetch("/api/config/preference?key=analytics_opt_in", { signal: controller.signal })
      .then(async (res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`)
        const result = await res.json()
        if (result?.value !== null && typeof result?.value !== "boolean") throw new Error("Invalid preference response")
        if (controller.signal.aborted) return
        setChoice(result.value)
        onChange("_analytics_choice", result.value === null ? "" : String(result.value))
      })
      .catch(() => {
        if (!cancelled) setError("Couldn't load your privacy choice. Try again or choose an option below.")
      })
      .finally(() => {
        clearTimeout(timeout)
        if (!cancelled) setLoading(false)
      })
    return () => {
      cancelled = true
      clearTimeout(timeout)
      controller.abort()
    }
  }, [onChange, reload])

  async function persist(value: boolean) {
    setSaving(value)
    onChange("_analytics_saving", "true")
    setError(null)
    try {
      const res = await fetch("/api/config/preference", {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ key: "analytics_opt_in", value }),
      })
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
      setChoice(value)
      onChange("_analytics_choice", String(value))
    } catch (e) {
      setError(`Couldn't save preference: ${e instanceof Error ? e.message : String(e)}`)
    } finally {
      setSaving(null)
      onChange("_analytics_saving", "false")
    }
  }

  return (
    <div className="flex flex-col items-center py-1">
      <div className="flex items-center gap-3">
        <VerifiedUserIcon className="h-6 w-6 text-emerald-400" />
        <h2 className="text-xl font-bold text-slate-100">Privacy</h2>
      </div>
      <section aria-labelledby="analytics-choice-title" className="mt-5 w-full max-w-2xl rounded-xl border border-white/10 bg-white/[0.02] p-5">
        <h3 id="analytics-choice-title" className="text-sm font-semibold text-slate-200">
          Help us count installations?
        </h3>
        <p className="mt-2 text-xs leading-relaxed text-slate-400">
          Share a stable, hashed ID derived from your board's serial number in daily update
          checks. This lets us count unique devices and software versions without counting
          reinstalls twice. You can change your choice in Settings → System.
        </p>

        <div className="mt-4 flex flex-col gap-2 sm:flex-row" role="group" aria-label="Share installation analytics">
          {[{ value: true, label: "Yes, count me", Icon: CheckIcon }, { value: false, label: "No thanks", Icon: CloseIcon }].map(({ value, label, Icon }) => (
            <button
              key={label}
              type="button"
              aria-pressed={choice === value}
              disabled={loading || saving !== null}
              onClick={() => persist(value)}
              className={cn(
                "flex flex-1 items-center justify-center gap-2 rounded-lg border px-4 py-3 text-sm font-medium transition-colors disabled:opacity-50",
                choice === value
                  ? "border-emerald-400/60 bg-emerald-500/15 text-emerald-200"
                  : "border-white/10 bg-white/[0.02] text-slate-300 hover:border-white/20 hover:bg-white/[0.05]"
              )}
            >
              {saving === value ? <ProgressActivityIcon className="h-4 w-4 animate-spin" /> : <Icon className="h-4 w-4" />}
              {label}
            </button>
          ))}
        </div>

        <p className="mt-3 text-xs text-slate-400" role="status">
          {loading ? "Loading your privacy choice…"
            : saving !== null ? "Saving your choice…"
              : choice !== null ? `Saved: ${choice ? "opted in" : "opted out"}.`
                : setupAlreadyFinished ? "Optional. No choice means the device ID is not shared."
                  : "Choose either option to continue. Both give you the same features."}
        </p>
        {error && (
          <div className="mt-3 text-xs text-rose-400" role="alert">
            <p>{error}</p>
            <button type="button" disabled={loading || saving !== null} onClick={() => {
              setLoading(true)
              setError(null)
              setReload(value => value + 1)
            }} className="mt-2 underline disabled:opacity-50">Reload saved choice</button>
          </div>
        )}
      </section>

      <details className="mt-4 w-full max-w-2xl rounded-xl border border-white/10 bg-white/[0.02] p-5">
        <summary className="cursor-pointer text-sm font-medium text-slate-300">
          Data we send, when, and why
        </summary>
        <div className="divide-y divide-white/5">
          <FlowRow
            when="Daily update check"
            what="Software version, CPU architecture, board model"
            why="Detect vulnerable builds, ship compatible binaries"
            note="No device identifier unless you opt in above; the source IP is briefly used for rate limiting."
          />
          <FlowRow
            when="Once per install"
            what="Empty ping with no payload or device identifier"
            why="Count gross install volume on the server"
            note="The source IP is briefly rate-limited; only a daily aggregate count is stored. See the privacy wiki to suppress it."
          />
          <FlowRow
            when="When you use Sentry Cloud"
            what="Your account login + the files you sync"
            why="Sync requires it — the feature can't work otherwise"
            note="Don't sign in to Cloud if you don't want this."
          />
          <FlowRow
            when="When you use AI Support & Help"
            what="Messages, product/software version, SBC model, selected non-secret support settings, the Pi connection's public IP for abuse prevention + only diagnostics you explicitly approve"
            why="Generate product-specific troubleshooting help"
            note="Online AI: messages are processed by Ollama Cloud and stored redacted on Sentry Six servers for up to 90 days after the last activity; maintainers may review them. Approved diagnostics are retained for 7 days."
          />
          <FlowRow
            when="When you submit a wrap or lock chime"
            what="The file you uploaded + your IP for rate-limiting"
            why="Sharing the submission with the community"
            note="No device fingerprint is sent. Your IP is used and retained for rate-limiting and abuse handling."
          />
          <FlowRow
            when="If you enable iOS push notifications"
            what="A randomly-generated device pairing ID"
            why="Routing push notifications to your phone"
            note="Not tied to your hardware. Cleared when you unpair."
          />
        </div>
      </details>
      <p className="mt-4 max-w-2xl text-xs text-slate-400">
          <a
            href="https://sentry-six.com/privacy"
            target="_blank"
            rel="noopener noreferrer"
            className="text-slate-400 underline hover:text-slate-300"
          >
            Privacy policy
          </a>
          {" · "}
          <a
            href="https://github.com/Sentry-Six/Sentry-USB-Rusty"
            target="_blank"
            rel="noopener noreferrer"
            className="text-slate-400 underline hover:text-slate-300"
          >
            Source code
          </a>
      </p>
    </div>
  )
}

function FlowRow({
  when,
  what,
  why,
  note,
}: {
  when: string
  what: string
  why: string
  note?: string
}) {
  return (
    <div className="grid grid-cols-1 gap-1 py-3 sm:grid-cols-[140px_1fr]">
      <div className="text-xs font-semibold text-slate-300">{when}</div>
      <div>
        <p className="text-xs text-slate-300">{what}</p>
        <p className="mt-0.5 text-[11px] text-slate-500">{why}</p>
        {note && (
          <p className="mt-1 text-[11px] italic text-slate-500/80">{note}</p>
        )}
      </div>
    </div>
  )
}

import { useCallback, useEffect, useRef, useState } from "react"
import { CheckIcon, CloseIcon, ProgressActivityIcon, VerifiedUserIcon } from "@/components/icons"
import type { StepProps } from "../SetupWizard"
import { cn } from "@/lib/utils"

export function PrivacyStep({ data, onChange, setupAlreadyFinished, setupStatusKnown = false, registerBeforeContinue }: StepProps) {
  const [savedChoice, setSavedChoice] = useState<boolean | null | "invalid" | undefined>(undefined)
  const [unsetConfirmed, setUnsetConfirmed] = useState(false)
  const [draft, setDraft] = useState<boolean | undefined>(() =>
    data._analytics_draft === "true" ? true : data._analytics_draft === "false" ? false : undefined)
  const [saving, setSaving] = useState(false)
  const [loading, setLoading] = useState(true)
  const [reload, setReload] = useState(0)
  const [error, setError] = useState<string | null>(null)
  const saveController = useRef<AbortController | null>(null)
  const newInstallation = setupStatusKnown && !setupAlreadyFinished
  const defaultSelected = !loading && unsetConfirmed && draft === undefined && savedChoice === null && newInstallation
  const choice = draft ?? (typeof savedChoice === "boolean" ? savedChoice : defaultSelected ? true : null)

  useEffect(() => {
    const controller = new AbortController()
    let cancelled = false
    const timeout = setTimeout(() => controller.abort(), 10_000)
    onChange("_analytics_ready", "false")
    fetch("/api/config/preference?key=analytics_opt_in", { signal: controller.signal })
      .then(async (res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`)
        const result = await res.json()
        if (!result || typeof result !== "object" || !Object.hasOwn(result, "value")) throw new Error("Invalid preference response")
        if (controller.signal.aborted) return
        const saved = typeof result.value === "boolean" || result.value === null ? result.value : "invalid"
        setSavedChoice(saved)
        setUnsetConfirmed(result.value === null && result.is_set === false)
        setError(saved === "invalid" ? "Your saved privacy choice needs confirmation. Choose either option to replace it." : null)
        onChange("_analytics_choice", typeof saved === "boolean" ? String(saved) : "")
        onChange("_analytics_ready", "true")
      })
      .catch(() => {
        if (!cancelled) {
          setSavedChoice(undefined)
          setUnsetConfirmed(false)
          onChange("_analytics_ready", "false")
          setError("Couldn't load your privacy choice. Reload it to continue.")
        }
      })
      .finally(() => {
        clearTimeout(timeout)
        if (!cancelled) setLoading(false)
      })
    return () => { cancelled = true; clearTimeout(timeout); controller.abort() }
  }, [onChange, reload])

  useEffect(() => () => saveController.current?.abort(), [])

  const persist = useCallback(async () => {
    if (loading || saving || !setupStatusKnown || savedChoice === undefined) return false
    if (choice === null) {
      if (newInstallation || savedChoice === "invalid") { setError("Choose Yes, count me or No thanks to continue."); return false }
      return true
    }
    if (choice === savedChoice) return true
    const controller = new AbortController()
    saveController.current = controller
    let timedOut = false
    const timeout = setTimeout(() => { timedOut = true; controller.abort() }, 10_000)
    setSaving(true)
    onChange("_analytics_saving", "true")
    setError(null)
    try {
      const res = await fetch("/api/config/preference", {
        method: "PUT",
        signal: controller.signal,
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ key: "analytics_opt_in", value: choice, ...(defaultSelected ? { only_if_unset: true } : {}) }),
      })
      if (!res.ok) {
        const failure = await res.json().catch(() => null)
        throw new Error(typeof failure?.error === "string" ? failure.error : `HTTP ${res.status}`)
      }
      const result = await res.json()
      if (result?.success !== true) throw new Error("Privacy choice was not confirmed")
      if (defaultSelected && typeof result.value !== "boolean") {
        if (result.is_set === true) {
          setSavedChoice(result.value === null ? null : "invalid")
          setUnsetConfirmed(false)
          setDraft(undefined)
          onChange("_analytics_draft", "")
          onChange("_analytics_choice", "")
        }
        throw new Error("Privacy choice was not confirmed. Choose an option or reload.")
      }
      if (controller.signal.aborted) return false
      const actual = typeof result.value === "boolean" ? result.value : choice
      setSavedChoice(actual)
      setDraft(undefined)
      onChange("_analytics_draft", "")
      onChange("_analytics_choice", String(actual))
      return true
    } catch (e) {
      if (timedOut || !controller.signal.aborted) setError(timedOut
        ? "Couldn't confirm your privacy choice: request timed out. Try again."
        : `Couldn't save preference: ${e instanceof Error ? e.message : String(e)}`)
      return false
    } finally {
      clearTimeout(timeout)
      if (timedOut || !controller.signal.aborted) {
        setSaving(false)
        onChange("_analytics_saving", "false")
      }
    }
  }, [loading, saving, setupStatusKnown, savedChoice, choice, defaultSelected, newInstallation, onChange])

  useEffect(() => {
    registerBeforeContinue?.(persist)
    return () => registerBeforeContinue?.(null)
  }, [registerBeforeContinue, persist])

  return (
    <div className="flex flex-col items-center py-1">
      <div className="flex items-center gap-3">
        <VerifiedUserIcon className="h-6 w-6 text-emerald-400" />
        <h2 className="text-xl font-bold text-slate-100">Privacy</h2>
      </div>
      <section aria-labelledby="analytics-choice-title" className="mt-5 w-full max-w-2xl rounded-xl border border-white/10 bg-white/[0.02] p-5">
        <h3 id="analytics-choice-title" className="text-sm font-semibold text-slate-200">
          Help us count devices?
        </h3>
        <p className="mt-2 text-xs leading-relaxed text-slate-400">
          Share a hashed device ID and running software version so we can count devices
          without counting reinstalls twice. We keep the first and last report times.
          You can turn this off in Settings → System.
        </p>

        <div className="mt-4 flex flex-col gap-2 sm:flex-row" role="group" aria-label="Share installation analytics">
          {[{ value: true, label: "Yes, count me", Icon: CheckIcon }, { value: false, label: "No thanks", Icon: CloseIcon }].map(({ value, label, Icon }) => (
            <button
              key={label}
              type="button"
              aria-pressed={choice === value}
              disabled={loading || saving || !setupStatusKnown || savedChoice === undefined}
              onClick={() => {
                setDraft(value)
                onChange("_analytics_draft", String(value))
                setError(null)
              }}
              className={cn(
                "flex flex-1 items-center justify-center gap-2 rounded-lg border px-4 py-3 text-sm font-medium transition-colors disabled:opacity-50",
                choice === value
                  ? "border-emerald-400/60 bg-emerald-500/15 text-emerald-200"
                  : "border-white/10 bg-white/[0.02] text-slate-300 hover:border-white/20 hover:bg-white/[0.05]"
              )}
            >
              {saving && choice === value ? <ProgressActivityIcon className="h-4 w-4 animate-spin" /> : <Icon className="h-4 w-4" />}
              {label}
            </button>
          ))}
        </div>

        <p className="mt-3 text-xs text-slate-400" role="status">
          {loading ? "Loading your privacy choice…"
            : !setupStatusKnown ? "Waiting for this device's setup status."
              : saving ? "Saving your choice…"
                : savedChoice === "invalid" && draft === undefined ? "Choose either option to replace the saved value."
                  : defaultSelected ? "On for new installations when you continue. Choose No thanks to keep it off."
                    : draft !== undefined && choice !== savedChoice ? "Your choice is saved when you continue."
                      : typeof savedChoice === "boolean" ? `Saved: ${savedChoice ? "On" : "Off"}.`
                        : "Off. Continue without enabling device counting."}
        </p>
        {error && (
          <div className="mt-3 text-xs text-rose-400" role="alert">
            <p>{error}</p>
            <button type="button" disabled={loading || saving} onClick={() => {
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
            when="Device counting (when enabled)"
            what="Hashed device ID and running software version"
            why="Count unique devices and show their latest reported versions"
            note="Reports run after startup, when enabled, and daily, with retries if offline. The server keeps the latest version and first/last report times until deletion. Turning this off stops future reports."
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

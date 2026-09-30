import { useCallback, useEffect, useState } from "react"
import {
  DownloadIcon,
  SettingsIcon,
  VerifiedUserIcon,
  WandStarsIcon,
} from "@/components/icons"
import { PrefCard, PrefGrid } from "@/components/settings/PrefCard"
import { ConfigBackupSection } from "@/components/settings/sections/ConfigBackupSection"
import { StorageRepairCard } from "@/components/settings/sections/StorageRepairCard"
import { InfoButton } from "@/components/ui/InfoButton"
import { Toggle } from "@/components/ui/Toggle"

interface Props {
  onOpenRawConfig: () => void
  onOpenWizard: () => void
  /** App version, written into the export file header. */
  version?: string | null
  /** Device hostname, written into the export file header. */
  hostname?: string | null
}

export function SystemTab({ onOpenRawConfig, onOpenWizard, version, hostname }: Props) {
  const [exportError, setExportError] = useState<string | null>(null)
  // Preferences remain comments so exported configuration stays safe to source.
  async function exportConfig(): Promise<void> {
    setExportError(null)
    try {
      const [configRes, prefsRes] = await Promise.all([
        fetch("/api/setup/config"),
        fetch("/api/config/preference"),
      ])
      if (!configRes.ok) throw new Error("Failed to read config")
      const config = (await configRes.json()) as Record<string, { value: string; active: boolean }>
      const prefs = prefsRes.ok ? ((await prefsRes.json()) as Record<string, unknown>) : {}

      const now = new Date().toISOString()
      const ver = version || "unknown"
      const host = hostname || "sentryusb"
      const escape = (s: string) => (s ?? "").replace(/'/g, "'\\''")

      let content = ""
      content += `# sentryusb.conf — exported from Sentry USB UI\n`
      content += `# Exported:  ${now}\n`
      content += `# Hostname:  ${host}\n`
      content += `# Version:   ${ver}\n`
      content += `#\n`
      content += `# This file is bash-sourceable. Active settings are 'export' lines;\n`
      content += `# inactive/default values are commented out for reference.\n`
      content += `\n`
      content += `# === Setup configuration ===\n`

      const keys = Object.keys(config).sort()
      for (const k of keys) {
        const e = config[k]
        const v = escape(e.value ?? "")
        if (e.active) {
          content += `export ${k}='${v}'\n`
        } else {
          content += `# export ${k}='${v}'\n`
        }
      }

      const prefKeys = Object.keys(prefs).sort()
      if (prefKeys.length > 0) {
        content += `\n`
        content += `# === Web UI preferences (Sentry USB Rusty) ===\n`
        content += `# Managed via the web UI; stored in /mutable/.sentryusb_preferences.json.\n`
        content += `# Listed here for export completeness — these are NOT sourced by bash.\n`
        for (const k of prefKeys) {
          const v = prefs[k]
          content += `# preference: ${k} = ${JSON.stringify(v)}\n`
        }
      }

      const blob = new Blob([content], { type: "text/plain" })
      const url = URL.createObjectURL(blob)
      const a = document.createElement("a")
      a.href = url
      a.download = "sentryusb.conf"
      a.click()
      URL.revokeObjectURL(url)
    } catch {
      setExportError("Could not export configuration. Please try again.")
    }
  }

  return (
    <PrefGrid>
      <>
      <ConfigBackupSection />
      <PrefCard icon={<SettingsIcon className="h-3.5 w-3.5" />} halo="slate" title="Configuration">
        <p className="t-xs">Export your configuration or edit advanced settings.</p>
        {exportError && (
          <p role="alert" className="text-xs text-rose-300">
            {exportError}
          </p>
        )}
        <div className="flex flex-wrap gap-2">
          <button
            onClick={onOpenRawConfig}
            className="rounded-lg border border-white/10 bg-white/5 px-3 py-1.5 text-xs font-medium text-slate-300 transition-colors hover:bg-white/10"
          >
            <SettingsIcon className="mr-1.5 inline h-3.5 w-3.5" />
            Open editor
          </button>
          <button
            onClick={exportConfig}
            className="rounded-lg border border-white/10 bg-white/5 px-3 py-1.5 text-xs font-medium text-slate-300 transition-colors hover:bg-white/10"
          >
            <DownloadIcon className="mr-1.5 inline h-3.5 w-3.5" />
            Download sentryusb.conf
          </button>
        </div>
      </PrefCard>

      </>
      <>
      {/* Storage repair */}
      <StorageRepairCard />

      {/* Setup and resources */}
      <PrefCard icon={<WandStarsIcon className="h-3.5 w-3.5" />} halo="accent" title="Setup Wizard"
        help={<InfoButton title="Setup Wizard"><p>Change your setup using your current configuration as the starting point. Any changes that erase drive data require a separate confirmation before applying.</p></InfoButton>}>

        <button
          onClick={onOpenWizard}
          className="self-start rounded-lg bg-blue-500/15 px-3 py-1.5 text-xs font-medium text-blue-400 transition-colors hover:bg-blue-500/25"
        >
          Launch Wizard
        </button>
        <div className="tile-divider" />
        <p className="section-label">Resources</p>
        <div className="flex flex-col gap-1">
          <a
            href="https://github.com/Sentry-Six/Sentry-USB-Rusty"
            target="_blank"
            rel="noopener noreferrer"
            className="t-sm text-blue-400 hover:text-blue-300"
          >
            GitHub repository ↗
          </a>
          <a
            href="https://discord.gg/9QZEzVwdnt"
            target="_blank"
            rel="noopener noreferrer"
            className="t-sm text-violet-400 hover:text-violet-300"
          >
            Discord community ↗
          </a>
        </div>
      </PrefCard>

      {/* Privacy */}
      <PrivacyCards />
      </>
    </PrefGrid>
  )
}

/** Settings → System privacy disclosure and analytics preference. */
function PrivacyCards() {
  const [choice, setChoice] = useState<boolean | null>(null)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [loaded, setLoaded] = useState(false)
  const [reload, setReload] = useState(0)

  useEffect(() => {
    const controller = new AbortController()
    fetch("/api/config/preference?key=analytics_opt_in", { signal: controller.signal })
      .then(async response => {
        if (!response.ok) throw new Error("Could not load privacy settings.")
        const data = await response.json()
        if (data.value !== null && typeof data.value !== "boolean") throw new Error("Could not read privacy settings.")
        if (!controller.signal.aborted) { setChoice(data.value); setLoaded(true); setError(null) }
      })
      .catch(error => { if (!controller.signal.aborted) setError(error instanceof Error ? error.message : "Could not load privacy settings.") })
    return () => controller.abort()
  }, [reload])

  const persist = useCallback(async (value: boolean) => {
    setSaving(true)
    setError(null)
    try {
      const response = await fetch("/api/config/preference", {
        method: "PUT", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ key: "analytics_opt_in", value }),
      })
      if (!response.ok) throw new Error("Could not save privacy settings. Try again.")
      setChoice(value)
    } catch (error) { setError(error instanceof Error ? error.message : "Could not save privacy settings.") }
    finally { setSaving(false) }
  }, [])

  return <PrefCard icon={<VerifiedUserIcon className="h-3.5 w-3.5" />} halo="accent" title="Privacy">
    <Toggle label="Device counting" checked={choice === true} disabled={!loaded || saving} onChange={persist}
      sub={loaded ? choice === null ? "Off by default" : choice ? "On" : "Off" : error ? "Unavailable" : "Loading…"}
      help={<InfoButton title="Device counting">
        <p>Optional: share a hashed device ID and running software version to count devices and show their latest reported versions.</p>
        <p>Reports run after startup, when enabled, and daily, with retries if offline. The board serial keeps the ID stable across reinstalls. Devices without a readable hardware serial are not counted.</p>
        <p>The server keeps the latest version and first/last report times until deletion. Turning this off stops future reports.</p>
        <p>Turning this off does not erase data sent earlier. Email <a href="mailto:privacy@sentry-six.com" className="text-blue-400 underline">privacy@sentry-six.com</a> to request deletion.</p>
      </InfoButton>} />
    <div className="flex items-center justify-between gap-2 border-t border-white/10 pt-2">
      <span className="t-md">Data we send</span>
      <InfoButton title="Data we send, and when">
          <div className="divide-y divide-white/5">
            <FlowRow
              when="Device counting (when enabled)"
              what="Hashed device ID and running version"
              note="After startup, when enabled, and daily. Stores the latest version and first/last report times; retries if offline."
            />
            <FlowRow
              when="Sentry Cloud (if signed in)"
              what="Account credentials + synced files"
              note="Stop using Cloud to stop this."
            />
            <FlowRow
              when="AI Support & Help (when used)"
              what="Messages, product/software version, SBC model, selected non-secret support settings, the Pi connection's public IP for abuse prevention + diagnostics you explicitly approve"
              note="Ollama Cloud processes messages. Redacted chats stay on Sentry Six servers up to 90 days after the last activity and may be reviewed; approved diagnostics stay 7 days."
            />
            <FlowRow
              when="Wraps / lock chime submissions"
              what="The file + your IP for rate-limiting"
              note="No device fingerprint."
            />
            <FlowRow
              when="iOS push pairing"
              what="A random pairing ID"
              note="Not tied to your hardware."
            />
          </div>
          <div className="tile-divider" />
          <div className="flex flex-col gap-1">
            <a
              href="https://sentry-six.com/privacy"
              target="_blank"
              rel="noopener noreferrer"
              className="t-sm text-blue-400 hover:text-blue-300"
            >
              Full privacy policy ↗
            </a>
            <a
              href="https://github.com/Sentry-Six/Sentry-USB-Rusty/wiki/Privacy"
              target="_blank"
              rel="noopener noreferrer"
              className="t-sm text-blue-400 hover:text-blue-300"
            >
              Wiki: what each flow does ↗
            </a>
          </div>

      </InfoButton>
    </div>
    {error && <p role="alert" className="text-xs text-rose-300">{error} {!loaded && <button type="button" className="text-blue-400 underline" onClick={() => setReload(value => value + 1)}>Retry</button>}</p>}
  </PrefCard>
}

function FlowRow({ when, what, note }: { when: string; what: string; note?: string }) {
  return (
    <div className="py-2">
      <p className="text-sm font-semibold text-slate-200">{when}</p>
      <p className="mt-0.5 text-sm text-slate-300">{what}</p>
      {note && <p className="mt-0.5 text-sm text-slate-400">{note}</p>}
    </div>
  )
}

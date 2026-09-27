import { useEffect, useState } from "react"
import { Toggle } from "@/components/ui/Toggle"

export function AutoUpdateToggle() {
  const [enabled, setEnabled] = useState(false)
  const [ready, setReady] = useState(false)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    let active = true
    fetch("/api/config/preference?key=auto_install_stable_updates")
      .then(async response => {
        if (!response.ok) throw new Error("Could not load automatic update settings.")
        return response.json()
      })
      .then(data => { if (active) { setEnabled(data.value === "enabled"); setReady(true) } })
      .catch(() => { if (active) setError("Could not load automatic update settings. Reload to try again.") })
    return () => { active = false }
  }, [])
  async function save(next: boolean) {
    setSaving(true)
    setError(null)
    try {
      const response = await fetch("/api/config/preference", {
        method: "PUT", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ key: "auto_install_stable_updates", value: next ? "enabled" : "disabled" }),
      })
      if (!response.ok) throw new Error("save failed")
      setEnabled(next)
    } catch { setError("Could not save automatic update settings. Try again.") }
    finally { setSaving(false) }
  }
  return <div>
    <Toggle checked={enabled} disabled={!ready || saving} onChange={save}
      label="Automatically install stable updates"
      sub="Installs newer stable releases after archiving new footage and finishing all follow-up work. Never installs pre-releases." />
    <div className="mt-2 space-y-2 text-xs text-slate-400">
      <ul className="list-disc space-y-1 pl-4">
        <li>The archive must still be reachable after downloading.</li>
        <li>With Tesla BLE paired, the car must report Park and Sentry Mode off. The Sentry Mode check is skipped when SentryUSB uses it for keep-awake.</li>
        <li>Without BLE pairing, only archive reachability is checked at this final step.</li>
      </ul>
      <p>If a required check fails or the car’s state is unknown, the update waits until the next successful archive with new footage.</p>
      <p>Installation restarts the device and briefly disconnects the USB drive.</p>
    </div>
    {error && <p role="alert" className="mt-2 text-xs text-red-400">{error}</p>}
  </div>
}

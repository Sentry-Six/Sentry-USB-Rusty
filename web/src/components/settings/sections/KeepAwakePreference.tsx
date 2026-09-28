import { CardiologyIcon } from "@/components/icons"
import { useKeepAwake } from "@/hooks/useKeepAwake"
import { PrefCard } from "@/components/settings/PrefCard"
import { InfoButton } from "@/components/ui/InfoButton"
import { SegPicker } from "@/components/ui/SegPicker"

type Mode = "" | "manual" | "auto"

const OPTIONS: { value: Mode; label: string; desc: string }[] = [
  { value: "", label: "Off", desc: "Web browsing will not keep the car awake" },
  { value: "manual", label: "Start manually", desc: "Button on Dashboard with duration picker" },
  { value: "auto", label: "While browsing", desc: "Stays awake while you're browsing" },
]

export function KeepAwakePreference() {
  const { mode, updateMode, pending, error, reloadPreferences } = useKeepAwake()
  const current = (mode ?? "") as Mode
  const desc = OPTIONS.find((o) => o.value === current)?.desc

  return (
    <PrefCard
      icon={<CardiologyIcon className="h-3.5 w-3.5" />}
      halo="rose"
      title="Web App Keep Awake"
      help={<InfoButton title="Web app keep awake">
        <p>Keeps the car awake while you use this web app. This does not change keep-awake during archiving.</p>
        <p>Start manually adds a Dashboard button with a duration picker. While browsing keeps the car awake as you interact with the web app, then expires after ten idle minutes.</p>
        <p>Configure archive keep-awake separately in the Setup Wizard.</p>
      </InfoButton>}
    >
      <SegPicker<Mode>
        options={OPTIONS.map((o) => ({ value: o.value, label: o.label }))}
        value={current}
        onChange={(v) => updateMode(v)}
        disabled={pending || mode === null}
      />
      {desc && <p className="t-xs" aria-live="polite">{pending ? "Saving…" : desc}</p>}
      {error && <p role="alert" className="text-xs text-rose-300">{error}</p>}
      {mode === null && error && <button className="text-left text-sm text-blue-400" onClick={() => void reloadPreferences()}>Retry</button>}
    </PrefCard>
  )
}

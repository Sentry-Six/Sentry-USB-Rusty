import { PrefGrid } from "@/components/settings/PrefCard"
import { KeepAwakePreference } from "@/components/settings/sections/KeepAwakePreference"
import { KeepAccessorySection } from "@/components/settings/sections/KeepAccessorySection"
import { DisplayUnitsSection } from "@/components/settings/sections/DisplayUnitsSection"
import { UpdateSection } from "@/components/settings/sections/UpdateSection"

interface Props {
  /** Forwarded to KeepAccessorySection so its disabled-state CTA can
   *  re-launch the Setup Wizard. */
  onOpenWizard?: () => void
}

export function DeviceTab({ onOpenWizard }: Props = {}) {
  return (
    <PrefGrid>
      <>
        <KeepAwakePreference />
        <DisplayUnitsSection />
        <KeepAccessorySection onOpenWizard={onOpenWizard} />
      </>
      <UpdateSection />
    </PrefGrid>
  )
}

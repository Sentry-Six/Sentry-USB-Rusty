import { LanIcon, WifiIcon } from "@/components/icons"
import { PrefCard } from "@/components/settings/PrefCard"
import { SectionErrorBoundary } from "@/components/ErrorBoundary"
import { Row } from "@/components/ui/StatusTile"
import { Pill } from "@/components/ui/Pill"
import CloudPairingSection from "@/components/CloudPairingSection"
import { AwayModeControl } from "@/components/settings/sections/AwayModeControl"
import { BleEnableToggle } from "@/components/settings/sections/BleEnableToggle"
import { BlePairButton } from "@/components/settings/sections/BlePairButton"
import type { PiStatus } from "@/lib/api"

interface Props {
  status: PiStatus | null
  /** Forwarded to AwayModeControl so its disabled-state CTA can re-launch
   *  the Setup Wizard. */
  onOpenWizard?: () => void
  openTravelMode?: boolean
  onTravelModeClose?: () => void
}

export function NetworkTab({ status, onOpenWizard, openTravelMode, onTravelModeClose }: Props) {
  const adapters = status?.wifi_adapters
  const wifiConnected = adapters ? adapters.some(adapter => adapter.connected) : !!status?.wifi_ssid
  const ethConnected =
    !!status?.ether_speed && status.ether_speed !== "Unknown!"

  return (
    <div className="grid grid-cols-1 gap-2.5 lg:grid-cols-2">
      {/* Network interfaces */}
      <PrefCard
        icon={<WifiIcon className="h-3.5 w-3.5" />}
        halo={wifiConnected ? "accent" : "slate"}
        title="WiFi"
        badge={wifiConnected ? <Pill kind="accent">Connected</Pill> : null}
      >
        {adapters?.length ? (
          <div className="space-y-4">
            {adapters.map(adapter => (
              <div key={adapter.device}>
                <div className="flex items-center justify-between gap-2">
                  <div className="t-md font-semibold">{adapter.device === "wlan0" ? "Internal Wi-Fi" : "USB Wi-Fi"} ({adapter.device})</div>
                  <Pill kind={adapter.connected ? "accent" : "slate"}>{adapter.connected ? "Connected" : "Not connected"}</Pill>
                </div>
                {adapter.connected && <>
                  <div className="t-xs mt-1">{adapter.ssid}</div>
                  <Row label="IP" value={<span className="t-mono">{adapter.ip || "—"}</span>} />
                  <Row label="Signal" value={adapter.signal_dbm != null ? `${adapter.signal_dbm} dBm` : adapter.strength || "—"} />
                  {adapter.band && <Row label="Band" value={adapter.band} />}
                  {adapter.channel != null && <Row label="Channel" value={adapter.channel} />}
                  {adapter.link_rx_mbps != null && <Row label="RX link speed" value={`${adapter.link_rx_mbps} Mbit/s`} />}
                  {adapter.link_tx_mbps != null && <Row label="TX link speed" value={`${adapter.link_tx_mbps} Mbit/s`} />}
                </>}
              </div>
            ))}
          </div>
        ) : wifiConnected && status ? (
          <>
            <div className="t-md font-semibold">{status.wifi_ssid}</div>
            <Row
              label="IP"
              value={<span className="t-mono">{status.wifi_ip || "—"}</span>}
            />
            {status.wifi_strength && (
              <Row label="Signal" value={status.wifi_strength} />
            )}
          </>
        ) : (
          <p className="t-xs">
            No WiFi configured. Use the Setup Wizard to scan and connect.
          </p>
        )}
      </PrefCard>

      <PrefCard
        icon={<LanIcon className="h-3.5 w-3.5" />}
        halo={ethConnected ? "accent" : "slate"}
        title="Ethernet"
        badge={
          ethConnected && status ? <Pill kind="accent">{status.ether_speed}</Pill> : null
        }
      >
        {ethConnected && status ? (
          <>
            <Row
              label="IP"
              value={<span className="t-mono">{status.ether_ip || "—"}</span>}
            />
            <Row label="Link" value={status.ether_speed} />
          </>
        ) : (
          <p className="t-xs">No Ethernet link detected.</p>
        )}
      </PrefCard>

      {/* Tesla BLE — enable the telemetry / keep-awake features, then pair.
          Kept adjacent so "enable" sits right next to "pair".
          The self-fetching sections each get an error boundary (this tab
          doesn't use PrefGrid, which wraps its children automatically): a
          render crash in one card must not blank the whole app. */}
      <SectionErrorBoundary>
        <BleEnableToggle />
      </SectionErrorBoundary>
      <SectionErrorBoundary>
        <BlePairButton />
      </SectionErrorBoundary>

      {/* Remote access */}
      <SectionErrorBoundary>
        <AwayModeControl
          onOpenWizard={onOpenWizard}
          openTravelMode={openTravelMode}
          onTravelModeClose={onTravelModeClose}
        />
      </SectionErrorBoundary>
      <SectionErrorBoundary>
        <CloudPairingSection />
      </SectionErrorBoundary>
    </div>
  )
}

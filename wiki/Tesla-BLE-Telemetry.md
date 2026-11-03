# Tesla BLE Telemetry

Tesla BLE Telemetry is the Bluetooth pipeline that pulls live data straight from your car and lays it on top of the dashcam record. It's what makes the **Battery**, **Climate**, **Odometer** (and parts of the **Assisted driving**) sections on each [Drive](Drives) actually populate — and it's what feeds the live-status tiles on the Dashboard.

## What it captures

| Channel | What's sampled |
|---|---|
| **Battery** | State of charge % (drives show start → end deltas) |
| **Climate** | Interior temperature, exterior temperature, HVAC state and runtime |
| **TPMS** | Tire pressure status. The dashboard's Tires tile and history chart rate each tire against bands you can set in **Settings → Display & Units → Tire pressure ranges** (default: optimal 36–45 psi, unsafe below 28 or above 50), for example for a Cybertruck's higher spec |
| **Odometer** | Total odometer reading (used for per-drive distance independent of GPS smoothing) |
| **Location** | Vehicle position, with reverse-geocoded place names for drive start/end |

Sampling cadence varies with what the car is doing — more frequent while driving, sparser while parked. The sampler reconnects automatically when it sees the vehicle wake.

## Why it matters

Tesla's dashcam clips carry GPS and timestamps but nothing else about the car's state. Pairing the Pi over BLE lets Sentry USB:

- Show **battery deltas** per trip and across periods
- Track **HVAC runtime and cabin temperature trends** alongside the drive's video
- Use the **odometer** for accurate per-drive distance
- Resolve **place names** for start/end locations instead of raw GPS

BLE also doubles as the [Keep Awake](Setup-Wizard-Guide#7-keep-awake) mechanism — nudging the car during archive cycles so USB power stays on. One pairing serves both purposes.

## Modes

From **Settings → Car & Network**, two independent toggles control the BLE pipeline:

- **Use BLE for telemetry** — reads battery, temps, HVAC, TPMS, odometer, and location. On by default once paired.
- **Use BLE for keep-awake** — nudges the car over BLE during archive cycles so the dashcam keeps recording. Off by default; turn it on if you don't want to use TeslaFi / Tessie / a webhook.

You can run either, both, or neither.

## Pairing

The [Setup Wizard's Keep Awake step](Setup-Wizard-Guide#7-keep-awake) walks you through pairing on first install. To re-pair or repair the link later, open **Settings → Car & Network**, then use **BLE Pairing → Re-pair**. You'll need to be near the car so it accepts the pair request.

**Pair as Driver key (experimental)** is available in the BLE Pairing card. It is off by default, so normal pairing still requests an Owner key. Driver may restrict certain functions; telemetry, keep-awake, charging, and accessory-power compatibility is not fully verified. The choice applies only to the next pairing request and does not show or change an existing key’s role. To change roles, first remove the Sentry USB key from the car’s Locks screen, then pair again with the desired option and confirm with your key card.

The card shows the paired VIN, current connection state, and a **Show output** button if you need the raw pairing log.

## Bluetooth adapter

The Pi has a built-in Bluetooth radio, but for the most reliable telemetry we recommend a **USB Bluetooth dongle** — it has its own antenna and isn't sharing one with WiFi. Sentry USB auto-detects the dongle when plugged in and uses it; unplug it and the Pi falls back to the built-in radio automatically. The choice applies to both Tesla BLE and the Sentry Connect app.

## Troubleshooting

If pairing fails, the connection drops, or telemetry stops flowing, the **Logs → Bluetooth** tab has a live diagnostic dump and a **Download bundle** button that grabs everything for sharing on Discord. Full steps: [Troubleshooting → BLE Keep Awake stops working / can't pair](Troubleshooting#ble-keep-awake-stops-working--cant-pair).

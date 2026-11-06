# Drives

The **Drives** tab catalogs every trip your Tesla took. Sentry USB builds each drive by grouping contiguous dashcam clips, geocodes the GPS samples for start/end locations, and (when [Tesla BLE Telemetry](Tesla-BLE-Telemetry) is paired) layers the BLE samples onto the record.

## Drives list

Each drive card shows:

- Start and end **locations** (geocoded from GPS)
- **Battery** at start → at end
- **Distance, duration, and FSD usage %**
- A mini **route map**

The top of the page rolls up your selected time range into four totals: **DISTANCE**, **TIME**, **FSD**, and **DISENGAGEMENTS**.

## Filtering and sorting

- **Time range chip** — Last 7 days by default; tap to pick a different window.
- **Filter** — narrow the list by date / tag / other attributes.
- **Sort** — date is the default; tap the sort header to change direction.

## Top-bar actions

| Button | What it does |
|---|---|
| **Process** | Re-runs drive detection. Pick **Process new drives** (incremental) or **Reprocess all drives** (rebuild every drive from scratch). Use after a major config change or if drives look wrong. |
| **Export** | Downloads your drive index — back it up or move it to another Pi. |
| **Import** | Loads a previously exported drive bundle. |
| **Delete all** | Removes every drive record. Underlying dashcam clips are untouched. |

## Bulk select

Click **Select** in the top-right to enter multi-select mode. **Tag** adds a tag to every selected drive, **Export** downloads the selection as a CSV file, and **Delete** removes the selected drive records.

## Drive detail

Click any drive to open its full breakdown:

- **Map** — the route with start/end markers
- **Multi-camera viewer** — synchronized 6-camera playback of every clip in the drive, with HW3-aware adaptive grid and drift correction
- **Speed** — average, max, and a speed-over-time chart
- **Assisted driving** — FSD usage %, FSD distance, disengagements, accel pushes
- **Odometer** — start, end, driven distance
- **Battery** — start %, end %, used %
- **Climate** — interior min/max, exterior average, HVAC runtime, with a temperature chart

The **Odometer**, **Battery**, and **Climate** sections are populated by [Tesla BLE Telemetry](Tesla-BLE-Telemetry). They'll show "—" for drives recorded before BLE was paired.

## FSD Analytics

The **FSD Analytics** page (open it from the **FSD** figure on the Dashboard) aggregates FSD usage across all your drives — distance, disengagement counts, accel pushes, and an FSD grade — with **Day / Week / All Time** period filters and a monthly drill-down for long-time users.

## How drives are built

A drive is a contiguous sequence of dashcam clips with no significant time gap. Sentry USB scans the dashcam tree, groups clips by timestamp continuity, geocodes the GPS at start and end, and joins the BLE telemetry samples that fall inside the drive's window. If you change BLE pairing or the timestamp data looks off, **Process → Reprocess all drives** rebuilds the index from current data.

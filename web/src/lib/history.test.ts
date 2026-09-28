import assert from "node:assert/strict"
import test from "node:test"
import { rangeBounds, validDateRange, parseLocalDate, calendarDateText, shiftCalendarMonth } from "./date-range.ts"
import { summarizeCharges } from "./charging-stats.ts"
import { drivesCsv } from "./drive-export.ts"
import type { ChargeSessionSummary } from "../types/charging.ts"
import type { DriveSummary } from "../types/drives.ts"

test("inclusive local date ranges include same-day records through DST", () => {
  const old = process.env.TZ
  try {
    for (const zone of ["America/Edmonton", "Asia/Tokyo", "UTC"]) {
      process.env.TZ = zone
      const bounds = rangeBounds({ kind: "custom", start: "2026-11-01", end: "2026-11-01" }, new Date())
      assert.equal(bounds.from!.getHours(), 0)
      assert.equal(bounds.from!.getDate(), 1)
      assert.equal(bounds.to!.getHours(), 0)
      assert.equal(bounds.to!.getDate(), 2)
      const noon = new Date(2026,10,1,12)
      assert.ok(noon >= bounds.from! && noon < bounds.to!)
      if (zone === "America/Edmonton") assert.equal((+bounds.to! - +bounds.from!) / 3600000, 25)
    }
    assert.equal(validDateRange("2026-09-27", "2026-09-26"), false)
    assert.equal(validDateRange("2026-02-30", "2026-03-01"), false)
    assert.equal(validDateRange("2026-09-27", "2026-09-27"), true)
  } finally { if (old === undefined) delete process.env.TZ; else process.env.TZ = old }
})

test("charging totals keep currencies separate and count measured energy", () => {
  const sessions = [
    { currency:"CAD", cost:5, energyAddedKwh:10, durationSecs:60, efficiencyPct:90 },
    { currency:"USD", cost:7, energyAddedKwh:null, durationSecs:60, efficiencyPct:null },
    { currency:"CAD", cost:2, energyAddedKwh:0, durationSecs:60, efficiencyPct:null },
  ] as ChargeSessionSummary[]
  const stats = summarizeCharges(sessions)
  assert.equal(stats.totalCost, null)
  assert.deepEqual(stats.costsByCurrency, [{ currency:"CAD", amount:7 }, { currency:"USD", amount:7 }])
  assert.equal(stats.totalEnergyKwh,10)
  assert.equal(stats.energySessionCount,2)
  assert.equal(stats.avgEfficiency,90)
})

test("selected drive CSV quotes delimiters and neutralizes spreadsheet formulas", () => {
  const csv = drivesCsv([{ startTime:"2026-09-27T12:00:00", startLocation:'=HYPERLINK("bad")', endLocation:'Place, A', tags:['Work'], durationMs:60000 } as DriveSummary])
  assert.match(csv, /"'=HYPERLINK\(""bad""\)"/)
  assert.match(csv, /"Place, A"/)
  assert.match(csv, /"60"/)
  assert.equal(csv.split("\r\n").length,3)
})


test("calendar month moves clamp leap days and preserve local dates", () => {
  assert.equal(calendarDateText(shiftCalendarMonth(parseLocalDate("2024-01-31")!, 1)), "2024-02-29")
  assert.equal(calendarDateText(shiftCalendarMonth(parseLocalDate("2023-01-31")!, 1)), "2023-02-28")
  assert.equal(calendarDateText(shiftCalendarMonth(parseLocalDate("2024-02-29")!, -12)), "2023-02-28")
  assert.equal(parseLocalDate("2023-02-29"), null)
  assert.equal(validDateRange("2024-02-29", "2024-02-29"), true)
})

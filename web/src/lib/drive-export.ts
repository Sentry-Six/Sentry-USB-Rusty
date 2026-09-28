import type { DriveSummary } from "../types/drives"

function csvCell(value: string | number | undefined): string {
  let text = value == null ? "" : String(value)
  if (typeof value === "string" && /^[=+\-@\t\r]/.test(text)) text = `'${text}`
  return `"${text.replaceAll('"', '""')}"`
}
export function drivesCsv(drives: DriveSummary[]): string {
  const rows: (string | number | undefined)[][] = [["Start", "End", "Origin", "Destination", "Distance km", "Distance mi", "Duration seconds", "FSD %", "Battery start %", "Battery end %", "Tags"]]
  for (const drive of drives) rows.push([drive.startTime, drive.endTime, drive.startLocation, drive.endLocation,
    drive.distanceKm, drive.distanceMi, drive.durationMs / 1000, drive.fsdPercent, drive.batteryPctStart,
    drive.batteryPctEnd, (drive.tags ?? []).join("; ")])
  return rows.map((row) => row.map(csvCell).join(",")).join("\r\n") + "\r\n"
}

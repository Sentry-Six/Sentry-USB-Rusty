// Readings from the telemetry Tesla embeds in dashcam video.

/**
 * Accelerator pedal position in percent, 0 to 100.
 *
 * Cars write it either as a share (0 to 1) or already as a percentage (0 to
 * 100), so anything above 1 is taken as a percentage. (The rule Sentry
 * Studio uses.) Reading a percentage as a share showed every press as 100%.
 */
export function accelPercent(raw: number | undefined | null): number {
  if (raw === undefined || raw === null || !Number.isFinite(raw) || raw <= 0) return 0
  return Math.min(raw > 1 ? raw : raw * 100, 100)
}

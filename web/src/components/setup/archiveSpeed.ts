export const DEFAULT_ARCHIVE_SPEED_MPS = "0.5"

export function archiveSpeedError(value: string | undefined): string | null {
  if (value === undefined) return null
  const number = Number(value)
  return value.trim() !== "" && Number.isFinite(number) && number >= 0 && number <= 3.402823466e38
    ? null : "Archive speed must be a finite number greater than or equal to zero."
}

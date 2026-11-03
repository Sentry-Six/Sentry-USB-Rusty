interface FinalRebootOptions {
  /** Boot ID from the status reply that reported setup finished. */
  bootIdBefore?: string
  timeoutMs?: number
  /** Past this, any answer counts: the reboot can't still be pending. */
  maxWaitMs?: number
  now?: () => number
}

/**
 * Watches for setup's final reboot to finish. Each call polls once and
 * resolves true once the Pi is back on a new boot.
 *
 * Seeing the Pi go offline is not enough on its own: a request sent while
 * it is down can wait for it to come back instead of failing, so a quick
 * reboot (about 15 s on a Pi 5) went unnoticed and the page never moved
 * on. /api/setup/status reports the kernel's boot ID, which changes on
 * every boot. Servers without it fall back to the offline check.
 */
export function createFinalRebootWatch({
  bootIdBefore,
  timeoutMs = 4000,
  maxWaitMs = 180_000,
  now = Date.now,
}: FinalRebootOptions = {}) {
  const started = now()
  let before = bootIdBefore || undefined
  let wentDown = false
  return async (): Promise<boolean> => {
    let bootId: unknown
    try {
      const res = await fetch("/api/setup/status", { signal: AbortSignal.timeout(timeoutMs) })
      if (!res.ok) return false
      bootId = (await res.json())?.boot_id
    } catch {
      wentDown = true
      return false
    }
    if (now() - started >= maxWaitMs) return true
    if (typeof bootId !== "string" || !bootId) return wentDown
    if (before === undefined) {
      before = bootId
      return wentDown
    }
    return bootId !== before
  }
}

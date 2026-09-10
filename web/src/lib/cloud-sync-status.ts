export type MutableSyncStatus = {
  running: boolean
  homePending?: boolean
  failedStages: string[]
  pendingEdits: { driveTags: number; charging: number; rates: number } | null
  lastAttemptAt: string | null
}

const stageNames: Record<string, string> = {
  credentials: "Device credentials",
  drive_tags: "Drive tags",
  charging: "Charging edits",
  rates: "Charging rates",
  incoming: "Changes from Cloud",
  home: "Home charging",
}

/** Keep pending edits distinct from uploaded source files. Older Pis omit this field. */
export function cloudSyncNotice(sync?: MutableSyncStatus | null, unavailable = false) {
  if (unavailable) return { warning: true, title: "Sync status unavailable", detail: "Could not read your Pi’s status." }
  if (!sync) return null
  const values = sync.pendingEdits && [sync.pendingEdits.driveTags, sync.pendingEdits.charging, sync.pendingEdits.rates]
  if (!values || values.some(value => !Number.isSafeInteger(value) || value < 0)) {
    return { warning: true, title: "Sync status unavailable", detail: "The pending queue could not be read." }
  }
  const count = values.reduce((sum, value) => sum + value, 0)
  const pending = `${count.toLocaleString()} ${count === 1 ? "change" : "changes"} waiting to sync`
  if (sync.running) return { warning: false, title: "Syncing SentryCloud", detail: count ? pending : "Checking for changes" }
  if (sync.failedStages?.length) {
    return { warning: true, title: "SentryCloud sync needs attention",
      detail: [...new Set(sync.failedStages.map(stage => stageNames[stage] || "Sync"))].join(" · ") }
  }
  if (sync.homePending) return { warning: false, title: "Updating Home charging", detail: "Continuing in the background" }
  return count ? { warning: false, title: pending, detail: "SentryCloud" } : null
}

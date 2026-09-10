import test from "node:test"
import assert from "node:assert/strict"
import { cloudSyncNotice } from "./cloud-sync-status.ts"
import type { MutableSyncStatus } from "./cloud-sync-status.ts"
const base: MutableSyncStatus = { running: false, failedStages: [], pendingEdits: { driveTags: 0, charging: 0, rates: 0 }, lastAttemptAt: null }

test("uploaded files do not imply that queued tag or charging edits have synced", () => {
  const notice = cloudSyncNotice({ ...base, pendingEdits: { driveTags: 2, charging: 1, rates: 0 } })
  assert.equal(notice?.title, "3 changes waiting to sync")
  assert.equal(notice?.warning, false)
  assert.equal(cloudSyncNotice(base), null)
})
test("a failed incoming read remains visible even with no outgoing edits", () => {
  const notice = cloudSyncNotice({ ...base, failedStages: ["incoming"] })
  assert.equal(notice?.warning, true)
  assert.equal(notice?.detail, "Changes from Cloud")
})
test("running retries show progress and an unreadable queue never appears empty", () => {
  assert.equal(cloudSyncNotice({ ...base, running: true, failedStages: ["drive_tags"] })?.title, "Syncing SentryCloud")
  assert.equal(cloudSyncNotice({ ...base, pendingEdits: null })?.warning, true)
  assert.equal(cloudSyncNotice({ ...base, pendingEdits: { driveTags: -1, charging: 0, rates: 0 } })?.warning, true)
  assert.equal(cloudSyncNotice(undefined), null, "older Pi responses remain compatible")
})

test("a lost Pi connection cannot leave the last running state presented as live", () => {
  const notice = cloudSyncNotice({ ...base, running: true }, true)
  assert.equal(notice?.title, "Sync status unavailable")
  assert.equal(notice?.warning, true)
})

test("bounded Home background work is pending rather than a failure", () => {
  const pending = cloudSyncNotice({ ...base, homePending: true })
  assert.equal(pending?.warning, false)
  assert.equal(pending?.title, "Updating Home charging")
  const failed = cloudSyncNotice({ ...base, homePending: true, failedStages: ["home"] })
  assert.equal(failed?.warning, true)
  assert.equal(failed?.detail, "Home charging")
})

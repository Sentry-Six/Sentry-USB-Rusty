import { useState } from "react"
import { api, type ArchiveCycle } from "@/lib/api"
import { InfoButton } from "@/components/ui/InfoButton"

export function CancelArchiveButton({ cycle, unavailable = false }: { cycle: ArchiveCycle | null; unavailable?: boolean }) {
  const [requested, setRequested] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  if (!cycle && !unavailable) return null
  const cancelling = Boolean(cycle && (cycle.cancelling || requested === cycle.id))

  async function cancel() {
    if (!cycle || cancelling) return
    setRequested(cycle.id)
    setError(null)
    try {
      await api.cancelArchive(cycle.id)
    } catch {
      setRequested(null)
      setError("Could not cancel the archive. Check the connection and try again.")
    }
  }

  return (
    <div className="mt-3">
      <div className="flex items-center gap-1">
        <button
          type="button"
          className="w-full rounded-lg border border-slate-600/60 px-3 py-2 text-xs font-medium text-slate-200 transition hover:border-red-400/50 hover:text-red-300 disabled:cursor-not-allowed disabled:opacity-60"
          disabled={!cycle || cancelling}
          title={!cycle ? "Cancellation unavailable for this run" : undefined}
          onClick={cancel}
        >
          {cancelling ? "Cancelling…" : "Cancel Archive"}
        </button>
        {!cycle && <InfoButton title="Archive controls unavailable">
          <p>The archive service isn’t reporting cancellation support for this run.</p>
          <p>Let this transfer finish, then complete the device update and restart the Pi.</p>
        </InfoButton>}
      </div>
      {cancelling && <p role="status" className="mt-2 text-xs text-slate-400">Stopping this run safely. Remaining footage will be kept.</p>}
      {error && <p role="alert" className="mt-2 text-xs text-red-300">{error}</p>}
    </div>
  )
}

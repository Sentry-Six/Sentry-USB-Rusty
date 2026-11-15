import { CloseIcon, WarningIcon } from "@/components/icons"
import { roughWait } from "@/hooks/useArchiving"
import { cn } from "@/lib/utils"

interface ViewerNoticeProps {
  /** The Pi is archiving right now; null when it is not. */
  archiving: { etaSeconds: number | null } | null
  /** The player has dropped to the front camera because video kept stalling. */
  reduced?: boolean
  /** Go back to every camera. */
  onPlayAll?: () => void
  /** Hide the archiving notice until that archive finishes. */
  onDismiss?: () => void
  className?: string
}

// One line above the controls, never over the picture, for the two things
// that make video slow through no fault of the viewer: the Pi archiving
// (which shares the Wi-Fi link with the video), and a connection that
// cannot keep up with every camera. When both are true it says so once.
export default function ViewerNotice({ archiving, reduced = false, onPlayAll, onDismiss, className }: ViewerNoticeProps) {
  if (!archiving && !reduced) return null
  const timeLeft = archiving && archiving.etaSeconds !== null ? ` It has ${roughWait(archiving.etaSeconds)} left.` : ""
  const message = reduced
    ? archiving
      ? `Your Pi is archiving clips right now, so only the front camera is playing.${timeLeft}`
      : "The connection is slow, so only the front camera is playing."
    : `Your Pi is archiving clips right now, so video may load slowly or stutter.${timeLeft}`

  return (
    <div
      role="status"
      className={cn(
        "flex items-center gap-2 rounded-lg border border-amber-500/25 bg-amber-500/10 px-2.5 py-1 text-xs text-amber-200",
        className,
      )}
    >
      <WarningIcon className="h-3.5 w-3.5 shrink-0 text-amber-400" aria-hidden="true" />
      <p className="min-w-0 flex-1">{message}</p>
      {reduced && onPlayAll && (
        <button
          onClick={onPlayAll}
          className="shrink-0 rounded-md bg-amber-500/20 px-2 py-0.5 font-medium text-amber-100 transition-colors hover:bg-amber-500/30"
        >
          Play every camera
        </button>
      )}
      {!reduced && onDismiss && (
        <button
          onClick={onDismiss}
          className="shrink-0 rounded p-0.5 text-amber-300/80 transition-colors hover:bg-amber-500/20 hover:text-amber-100"
          title="Hide this notice"
          aria-label="Hide this notice"
        >
          <CloseIcon className="h-3.5 w-3.5" />
        </button>
      )}
    </div>
  )
}

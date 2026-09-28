import { DeleteIcon, DownloadIcon, SellIcon } from "@/components/icons"

interface SelectModeBarProps {
  selectedCount: number
  totalCount: number
  onSelectAll: () => void
  onSelectPage: () => void
  busy: boolean
  onTag: () => void
  onExport: () => void
  onDelete: () => void
  onCancel: () => void
}

export function SelectModeBar({
  selectedCount,
  totalCount,
  onSelectAll,
  onSelectPage,
  busy,
  onTag,
  onExport,
  onDelete,
  onCancel,
}: SelectModeBarProps) {
  const hasSelection = selectedCount > 0
  return (
    <div className="flex flex-wrap items-center gap-2">
      <span className="mr-1 text-sm text-slate-400">
        {selectedCount} of {totalCount} selected
      </span>
      <Outlined onClick={onTag} disabled={!hasSelection || busy}>
        <SellIcon className="h-3.5 w-3.5" />
        Tag
      </Outlined>
      <Outlined onClick={onExport} disabled={!hasSelection || busy}>
        <DownloadIcon className="h-3.5 w-3.5" />
        Export CSV
      </Outlined>
      <button
        type="button"
        disabled={!hasSelection || busy}
        onClick={onDelete}
        className="inline-flex items-center gap-1.5 rounded-full bg-rose-500/95 px-3 py-1 text-xs font-medium text-white transition-colors hover:bg-rose-400 disabled:opacity-50"
      >
        <DeleteIcon className="h-3.5 w-3.5" />
        Delete
      </button>
      <Outlined onClick={onSelectPage} disabled={busy}>Select page</Outlined>
      <Outlined onClick={onSelectAll} disabled={busy}>{busy ? "Working…" : "Select all matching"}</Outlined>
      <Outlined onClick={onCancel} disabled={busy}>Cancel</Outlined>
    </div>
  )
}

interface OutlinedProps {
  onClick: () => void
  disabled?: boolean
  children: React.ReactNode
}

function Outlined({ onClick, disabled, children }: OutlinedProps) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className="inline-flex items-center gap-1.5 rounded-full border border-white/10 bg-white/[0.03] px-3 py-1 text-xs font-medium text-slate-200 transition-colors hover:bg-white/[0.06] disabled:opacity-40"
    >
      {children}
    </button>
  )
}

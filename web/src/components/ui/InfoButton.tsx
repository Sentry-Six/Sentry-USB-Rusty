import { useState, type ReactNode } from "react"
import { InfoIcon } from "@/components/icons"
import { Modal } from "./Modal"

export function InfoButton({ title, children }: { title: string; children: ReactNode }) {
  const [open, setOpen] = useState(false)
  return <>
    <button type="button" aria-label={`About ${title.toLowerCase()}`} aria-haspopup="dialog"
      onClick={() => setOpen(true)}
      className="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-slate-400 hover:bg-white/10 hover:text-slate-100 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-blue-400">
      <InfoIcon className="h-4 w-4" />
    </button>
    {open && <Modal title={title} onClose={() => setOpen(false)}>
      <div className="space-y-3 text-sm leading-relaxed text-slate-300">{children}</div>
    </Modal>}
  </>
}

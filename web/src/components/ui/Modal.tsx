import { useId, useLayoutEffect, useRef, type ReactNode } from "react"
import { createPortal } from "react-dom"
import { CloseIcon } from "@/components/icons"
import { cn } from "@/lib/utils"

const layers: HTMLElement[] = []
const originalInert = new Map<HTMLElement, boolean>()
let originalOverflow = ""

function syncLayers() {
  const active = layers.at(-1)
  for (const child of Array.from(document.body.children) as HTMLElement[]) {
    if (!originalInert.has(child)) originalInert.set(child, child.inert)
    child.inert = active ? child !== active : originalInert.get(child) ?? false
  }
  if (!active) originalInert.clear()
}

function focusable(layer: HTMLElement) {
  return Array.from(layer.querySelectorAll<HTMLElement>(
    'button:not(:disabled), a[href], input:not(:disabled), select:not(:disabled), textarea:not(:disabled), summary, [tabindex]:not([tabindex="-1"])',
  )).filter(element => {
    if (element.closest('[hidden], [inert]')) return false
    const closed = element.closest("details:not([open])")
    if (closed && element !== closed.querySelector("summary")) return false
    for (let ancestor: HTMLElement | null = element; ancestor && ancestor !== layer; ancestor = ancestor.parentElement) {
      const style = window.getComputedStyle(ancestor)
      if (style.display === "none" || style.visibility === "hidden") return false
    }
    return true
  })
}

export function DialogLayer({ children, onClose, dismissable = true, label, labelledBy, className }: {
  children: ReactNode
  onClose: () => void
  dismissable?: boolean
  label?: string
  labelledBy?: string
  className?: string
}) {
  const layer = useRef<HTMLDivElement>(null)
  const latest = useRef({ onClose, dismissable })
  useLayoutEffect(() => { latest.current = { onClose, dismissable } })
  useLayoutEffect(() => {
    const node = layer.current!
    const previous = document.activeElement as HTMLElement | null
    if (!layers.length) {
      originalOverflow = document.body.style.overflow
      document.body.style.overflow = "hidden"
    }
    layers.push(node)
    syncLayers()
    const focusFirst = () => (focusable(node)[0] ?? node).focus()
    focusFirst()
    function onKey(event: KeyboardEvent) {
      if (layers.at(-1) !== node) return
      const target = event.target as HTMLElement
      if ((event.key === "Escape" || event.key === "Tab") && target.closest?.('[data-select-popup], [data-select-trigger][aria-expanded="true"]')) return
      if (event.key === "Escape") {
        event.preventDefault()
        event.stopImmediatePropagation()
        if (latest.current.dismissable) latest.current.onClose()
      } else if (event.key === "Tab") {
        const elements = focusable(node)
        const first = elements[0]
        const last = elements.at(-1)
        if (!first) {
          event.preventDefault()
          node.focus()
        } else if (!node.contains(document.activeElement)) {
          event.preventDefault()
          ;(event.shiftKey ? last : first)?.focus()
        } else if (event.shiftKey && (document.activeElement === first || document.activeElement === node)) {
          event.preventDefault()
          last?.focus()
        } else if (!event.shiftKey && document.activeElement === last) {
          event.preventDefault()
          first.focus()
        }
      }
    }
    function onFocus(event: FocusEvent) {
      if (layers.at(-1) === node && !node.contains(event.target as Node)) focusFirst()
    }
    window.addEventListener("keydown", onKey, true)
    document.addEventListener("focusin", onFocus)
    return () => {
      window.removeEventListener("keydown", onKey, true)
      document.removeEventListener("focusin", onFocus)
      layers.splice(layers.indexOf(node), 1)
      syncLayers()
      if (!layers.length) document.body.style.overflow = originalOverflow
      if (previous?.isConnected && !previous.closest("[inert]")) previous.focus()
    }
  }, [])
  return createPortal(
    <div ref={layer} className={cn("modal-shell", className)} role="dialog" aria-modal="true"
      aria-label={label} aria-labelledby={labelledBy} tabIndex={-1}
      onClick={event => { if (event.target === event.currentTarget && dismissable) onClose() }}>
      {children}
    </div>, document.body,
  )
}

interface ModalProps {
  title: ReactNode
  onClose: () => void
  dismissable?: boolean
  size?: "sm" | "md" | "lg"
  footer?: ReactNode
  children: ReactNode
  className?: string
}

const SIZE_MAX = { sm: "420px", md: "560px", lg: "768px" }

export function Modal({ title, onClose, dismissable = true, size = "md", footer, children, className }: ModalProps) {
  const titleId = useId()
  return (
    <DialogLayer onClose={onClose} dismissable={dismissable} labelledBy={titleId}>
      <div className={cn("glass-card modal-card", className)} style={{ maxWidth: SIZE_MAX[size] }}>
        <div className="modal-header">
          <h2 id={titleId} className="modal-title">{title}</h2>
          {dismissable && <button type="button" className="modal-close" onClick={onClose} aria-label="Close">
            <CloseIcon className="h-4 w-4" />
          </button>}
        </div>
        <div className="modal-body">{children}</div>
        {footer && <div className="border-t border-white/5 px-4 py-3">{footer}</div>}
      </div>
    </DialogLayer>
  )
}

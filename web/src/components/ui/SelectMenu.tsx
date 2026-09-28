import { useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react"
import { createPortal } from "react-dom"
import { CheckIcon, ExpandMoreIcon } from "@/components/icons"
import { cn } from "@/lib/utils"
import { DROPDOWN_OPTION, DROPDOWN_SURFACE, DROPDOWN_TRIGGER } from "./dropdownStyles"

interface Option { value: string; label: string; group?: string; disabled?: boolean }
interface Props {
  label: string; value: string; onChange: (value: string) => void; options: Option[]; align?: "start" | "end"
  disabled?: boolean; className?: string; fullWidth?: boolean; searchable?: boolean; searchPlaceholder?: string
}

function controlDisabled(button: HTMLButtonElement | null): boolean {
  if (!button) return false
  if (button.disabled || button.matches(":disabled")) return true
  let fieldset = button.closest<HTMLFieldSetElement>("fieldset[disabled]")
  while (fieldset) {
    const legend = [...fieldset.children].find(child => child.tagName === "LEGEND")
    if (!legend?.contains(button)) return true
    fieldset = fieldset.parentElement?.closest<HTMLFieldSetElement>("fieldset[disabled]") ?? null
  }
  return false
}

export function SelectMenu({ label, value, onChange, options, align = "start", disabled, className, fullWidth, searchable, searchPlaceholder }: Props) {
  const id = useId()
  const root = useRef<HTMLDivElement>(null)
  const trigger = useRef<HTMLButtonElement>(null)
  const popup = useRef<HTMLDivElement>(null)
  const searchInput = useRef<HTMLInputElement>(null)
  const typed = useRef({ text: "", at: 0 })
  const [open, setOpen] = useState(false)
  const [portalTarget, setPortalTarget] = useState<HTMLElement | null>(null)
  const [active, setActive] = useState(0)
  const [query, setQuery] = useState("")
  const [position, setPosition] = useState({ left: 16, top: 16, width: 240, maxHeight: 320 })
  const filtered = options.filter(option => !query || option.label.toLocaleLowerCase().includes(query.toLocaleLowerCase()))
  const enabledIndexes = filtered.flatMap((option, index) => option.disabled ? [] : [index])
  const selected = options.find(option => option.value === value)
  const selectedIndex = Math.max(0, options.findIndex(option => option.value === value && !option.disabled))
  const expanded = open && !disabled && options.some(option => !option.disabled)
  const focused = filtered[active] && !filtered[active].disabled ? active : enabledIndexes[0]
  const inside = (target: Node | null) => !!target && (root.current?.contains(target) || popup.current?.contains(target))

  useEffect(() => {
    if (!expanded) return
    function outside(event: PointerEvent) {
      const target = event.target as Node
      if (!root.current?.contains(target) && !popup.current?.contains(target)) setOpen(false)
    }
    document.addEventListener("pointerdown", outside)
    return () => document.removeEventListener("pointerdown", outside)
  }, [expanded])
  useLayoutEffect(() => {
    if (!expanded) return
    function reposition() {
      const rect = trigger.current?.getBoundingClientRect()
      if (!rect) return
      const margin = 16
      const width = Math.min(Math.max(fullWidth ? rect.width : 240, rect.width), window.innerWidth - margin * 2)
      const below = window.innerHeight - rect.bottom - margin - 8
      const above = rect.top - margin - 8
      const flip = below < 160 && above > below
      const maxHeight = Math.max(40, Math.min(320, flip ? above : below))
      const height = Math.min(popup.current?.scrollHeight || 320, maxHeight)
      const left = Math.max(margin, Math.min(align === "end" ? rect.right - width : rect.left, window.innerWidth - width - margin))
      const top = flip ? Math.max(margin, rect.top - height - 8) : rect.bottom + 8
      setPosition(previous => previous.left === left && previous.top === top && previous.width === width && previous.maxHeight === maxHeight ? previous : { left, top, width, maxHeight })
    }
    reposition()
    window.addEventListener("resize", reposition)
    document.addEventListener("scroll", reposition, true)
    return () => { window.removeEventListener("resize", reposition); document.removeEventListener("scroll", reposition, true) }
  }, [expanded, align, fullWidth, filtered.length])
  useEffect(() => {
    if (expanded && focused !== undefined) document.getElementById(`${id}-${focused}`)?.scrollIntoView({ block: "nearest" })
  }, [expanded, focused, id])
  useEffect(() => { if (expanded && searchable) searchInput.current?.focus() }, [expanded, searchable])

  function show(index = selectedIndex) {
    typed.current = { text: "", at: 0 }
    if (disabled || controlDisabled(trigger.current) || !options.some(option => !option.disabled)) return
    setPortalTarget(root.current?.closest<HTMLElement>('[role="dialog"]') ?? document.body)
    setQuery("")
    setActive(index)
    setOpen(true)
  }
  function close(restoreFocus = false) { setOpen(false); typed.current = { text: "", at: 0 }; if (restoreFocus) trigger.current?.focus() }
  function choose(index: number | undefined) {
    if (disabled || controlDisabled(trigger.current)) { close(); return }
    if (index === undefined || !filtered[index] || filtered[index].disabled) return
    onChange(filtered[index].value)
    close(true)
  }
  function typeAhead(key: string, now: number) {
    const previous = now - typed.current.at < 700 ? typed.current.text : ""
    const text = previous + key.toLocaleLowerCase()
    const prefix = [...text].every(character => character === text[0]) ? text[0] : text
    const from = expanded ? focused ?? 0 : selectedIndex
    const start = prefix.length === 1 ? from + 1 : from
    for (let offset = 0; offset < filtered.length; offset++) {
      const index = (start + offset) % filtered.length
      if (!filtered[index].disabled && filtered[index].label.toLocaleLowerCase().startsWith(prefix)) { setPortalTarget(root.current?.closest<HTMLElement>('[role="dialog"]') ?? document.body); setActive(index); setOpen(true); break }
    }
    typed.current = { text, at: now }
  }
  function leaveWithTab(event: KeyboardEvent<HTMLElement>) {
    const button = trigger.current
    if (!button) { close(); return }
    const dialog = button.closest<HTMLElement>('[role="dialog"]')
    const scope = dialog ?? document.body
    const candidates = [...scope.querySelectorAll<HTMLElement>('button, a[href], input, select, textarea, summary, [tabindex]')].filter(element => {
      if (element.tabIndex < 0 || element.matches(":disabled") || element.closest('[hidden], [inert], [data-select-popup]')) return false
      const details = element.closest("details:not([open])")
      if (details && element !== details.querySelector("summary")) return false
      for (let ancestor: HTMLElement | null = element; ancestor; ancestor = ancestor.parentElement) {
        const style = window.getComputedStyle(ancestor)
        if (style.display === "none" || style.visibility === "hidden") return false
      }
      return true
    })
    const index = candidates.indexOf(button)
    const nextIndex = index + (event.shiftKey ? -1 : 1)
    const next = candidates[nextIndex] ?? (dialog ? event.shiftKey ? candidates.at(-1) : candidates[0] : undefined)
    close(!next)
    if (next) { event.preventDefault(); next.focus() }
  }

  function onKey(event: KeyboardEvent<HTMLElement>) {
    if (event.key === "Escape" && expanded) { event.preventDefault(); event.stopPropagation(); close(true); return }
    if (disabled || controlDisabled(trigger.current) || !options.length) return
    const searching = event.currentTarget === searchInput.current
    if (event.altKey && event.key === "ArrowUp") { event.preventDefault(); close(true) }
    else if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key) && !(searching && ["Home", "End"].includes(event.key))) {
      event.preventDefault(); typed.current = { text: "", at: 0 }
      if (event.key === "Home") { show(options.findIndex(option => !option.disabled)); return }
      if (event.key === "End") { show(options.flatMap((option, index) => option.disabled ? [] : [index]).at(-1) ?? 0); return }
      if (!expanded) { show(); return }
      if (!enabledIndexes.length) return
      const index = enabledIndexes.indexOf(focused ?? -1)
      setActive(enabledIndexes[(index + (event.key === "ArrowDown" ? 1 : -1) + enabledIndexes.length) % enabledIndexes.length] ?? 0)
    } else if (event.key === "Enter" || (event.key === " " && !searching && (!typed.current.text || event.timeStamp - typed.current.at >= 700))) {
      event.preventDefault(); if (expanded) choose(focused); else show()
    } else if (event.key === "Escape") {
      if (expanded) { event.preventDefault(); event.stopPropagation(); close(true) }
    } else if (event.key === "Tab" && expanded) leaveWithTab(event)
    else if (!searching && event.key.length === 1 && !event.metaKey && !event.ctrlKey && !event.altKey) {
      event.preventDefault()
      if (searchable) { show(); setQuery(event.key); setActive(0) } else typeAhead(event.key, event.timeStamp)
    }
  }
  const groups = filtered.reduce<{ name?: string; start: number; entries: Option[] }[]>((result, option, index) => {
    const last = result.at(-1)
    if (last && last.name === option.group) last.entries.push(option)
    else result.push({ name: option.group, start: index, entries: [option] })
    return result
  }, [])
  return <div ref={root} className={cn("relative min-w-0 max-w-full", fullWidth ? "w-full" : "shrink-0")} onBlur={event => { if (!inside(event.relatedTarget as Node | null)) setOpen(false) }}>
    <button ref={trigger} id={`${id}-trigger`} type="button" role="combobox" aria-label={label} disabled={disabled || !options.some(option => !option.disabled)} data-select-trigger=""
      aria-haspopup="listbox" aria-expanded={expanded} aria-controls={expanded ? id : undefined}
      aria-activedescendant={expanded && focused !== undefined ? `${id}-${focused}` : undefined}
      onClick={() => expanded ? close() : show()} onKeyDown={onKey}
      className={cn(DROPDOWN_TRIGGER, "min-w-0 max-w-full", fullWidth && "w-full", expanded && "border-emerald-400/40 bg-emerald-400/10", className)}>
      <span className="min-w-0 truncate">{selected?.label ?? (value || label)}</span><ExpandMoreIcon className="h-4 w-4 shrink-0 text-slate-400" />
    </button>
    {expanded && createPortal(<div ref={popup} data-select-popup="" data-select-menu-popup="" data-select-owner={`${id}-trigger`} style={position}
      className={cn(DROPDOWN_SURFACE, "fixed z-[3000] overflow-y-auto p-1.5")}>
      {searchable && <div className="sticky top-0 z-10 bg-[#11191e] pb-1">
        <input ref={searchInput} type="search" aria-label={`Search ${label.toLowerCase()}`} placeholder={searchPlaceholder ?? "Search…"}
          value={query} onChange={event => { setQuery(event.target.value); setActive(0) }} onKeyDown={onKey}
          role="combobox" aria-expanded="true" aria-controls={id} aria-activedescendant={focused !== undefined ? `${id}-${focused}` : undefined}
          className="w-full rounded-xl border border-white/10 bg-white/5 px-3 py-2 text-sm text-slate-200 outline-none focus:border-emerald-400/60" />
      </div>}
      <div role="listbox" id={id} aria-label={label}>
        {groups.map(group => <div key={group.start} role={group.name ? "group" : undefined} aria-label={group.name}>
          {group.name && <p aria-hidden="true" className="px-3 pb-1 pt-3 text-[10px] font-semibold uppercase tracking-wider text-slate-400">{group.name}</p>}
          {group.entries.map((option, offset) => {
            const index = group.start + offset
            return <div key={option.value} id={`${id}-${index}`} role="option" data-value={option.value} aria-selected={option.value === value} aria-disabled={option.disabled || undefined}
              onPointerMove={() => { if (!option.disabled) setActive(index) }} onPointerDown={event => event.preventDefault()} onClick={() => choose(index)}
              className={cn(DROPDOWN_OPTION, "justify-between", option.disabled ? "cursor-not-allowed opacity-40" : "cursor-pointer", focused === index && "bg-white/10", option.value === value && "text-emerald-300")}>
              {option.label}{option.value === value && <CheckIcon className="h-4 w-4 shrink-0" />}
            </div>
          })}
        </div>)}
        {!filtered.length && <p className="px-3 py-4 text-sm text-slate-400" role="status">No matching options</p>}
      </div>
    </div>, portalTarget ?? document.body)}
  </div>
}

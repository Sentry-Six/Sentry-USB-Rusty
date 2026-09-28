import { DROPDOWN_SURFACE, DROPDOWN_OPTION } from "@/components/ui/dropdownStyles"
import { useState, useEffect, useRef, useCallback, useMemo } from "react"
import {
  ArrowBackIcon,
  CloseIcon,
  CreateNewFolderIcon,
  DeleteIcon,
  DownloadIcon,
  DraftIcon,
  FolderIcon,
  ProgressActivityIcon,
  SearchIcon,
  UploadIcon,
} from "@/components/icons"
import { cn } from "@/lib/utils"
import { SelectMenu } from "@/components/ui/SelectMenu"
import { responseError, uploadRelativePath } from "@/lib/file-upload"

type SortOption =
  | "name-asc"
  | "name-desc"
  | "date-newest"
  | "date-oldest"
  | "size-largest"
  | "size-smallest"
  | "type"
const SORT_LABELS: Record<SortOption, string> = {
  "name-asc": "Name (A–Z)",
  "name-desc": "Name (Z–A)",
  "date-newest": "Newest",
  "date-oldest": "Oldest",
  "size-largest": "Largest",
  "size-smallest": "Smallest",
  type: "Type",
}
interface FileEntry {
  name: string
  path: string
  is_dir: boolean
  size: number
  mod_time: string
}
interface DriveTab {
  id: string
  base: string
  config?: string
}
const ALL_DRIVES: DriveTab[] = [
  { id: "TeslaCam", base: "/mutable/TeslaCam", config: "has_cam" },
  { id: "Lock Sounds", base: "/mutable/LockChime" },
  { id: "Wraps", base: "/mutable/Wraps" },
  { id: "License Plates", base: "/mutable/LicensePlate" },
  { id: "Music", base: "/var/www/html/fs/Music", config: "has_music" },
  { id: "LightShow", base: "/var/www/html/fs/LightShow", config: "has_lightshow" },
  { id: "Boombox", base: "/var/www/html/fs/Boombox", config: "has_boombox" },
  { id: "USB Drive", base: "/mutable" },
]
interface UploadProgress {
  file: File
  fileName: string
  destination: string
  loaded: number
  total: number
  done: boolean
  error: string | null
  conflict: boolean
}
let remembered: { drive: string; path: string; search: string; sort: SortOption } | null = null
const buttonClass =
  "inline-flex min-h-9 items-center justify-center gap-2 rounded-xl border border-white/10 bg-white/5 px-3 py-2 text-sm text-slate-300 hover:bg-white/10 disabled:opacity-50"
function formatSize(bytes: number): string {
  if (!bytes) return "0 B"
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), 3)
  return `${(bytes / 1024 ** i).toFixed(i ? 1 : 0)} ${["B", "KB", "MB", "GB"][i]}`
}

export default function Files() {
  const [drives, setDrives] = useState<DriveTab[]>([])
  const [activeDrive, setActiveDrive] = useState<DriveTab | null>(null)
  const [currentPath, setCurrentPath] = useState("")
  const [effectiveBase, setEffectiveBase] = useState("")
  const [files, setFiles] = useState<FileEntry[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [operationError, setOperationError] = useState<string | null>(null)
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [busy, setBusy] = useState(false)
  const [search, setSearch] = useState(remembered?.search ?? "")
  const [sort, setSort] = useState<SortOption>(remembered?.sort ?? "name-asc")
  const [visibleCount, setVisibleCount] = useState(100)
  const [uploads, setUploads] = useState<UploadProgress[]>([])
  const [uploading, setUploading] = useState(false)
  const [dragging, setDragging] = useState(false)
  const uploadRef = useRef<HTMLInputElement>(null)
  const folderRef = useRef<HTMLInputElement>(null)
  const uploadMenuRef = useRef<HTMLDetailsElement>(null)
  const request = useRef(0)
  const controller = useRef<AbortController | null>(null)
  const locationRef = useRef("")
  const mounted = useRef(true)
  const uploadBusy = useRef(false)
  const dragDepth = useRef(0)

  useEffect(() => {
    mounted.current = true
    const abort = new AbortController()
    void (async () => {
      let available = ALL_DRIVES
      try {
        const response = await fetch("/api/config", { signal: abort.signal })
        if (!response.ok) throw new Error("Configuration unavailable")
        const config = await response.json()
        available = ALL_DRIVES.filter((d) => !d.config || config[d.config] === "yes")
      } catch {
        if (abort.signal.aborted) return
      }
      const chosen = available.find((d) => d.id === remembered?.drive) ?? available[0]
      setDrives(available)
      setActiveDrive(chosen)
      const savedPath = remembered?.drive === chosen.id ? remembered.path : ""
      setCurrentPath(
        savedPath === chosen.base || savedPath.startsWith(chosen.base + "/")
          ? savedPath
          : chosen.base,
      )
    })()
    return () => {
      mounted.current = false
      abort.abort()
      controller.current?.abort()
    }
  }, [])

  const fetchFiles = useCallback(
    async (path: string, query: string) => {
      const id = ++request.current
      controller.current?.abort()
      const abort = new AbortController()
      controller.current = abort
      setLoading(true)
      setError(null)
      try {
        const params = new URLSearchParams({ path })
        if (query) params.set("search", query)
        const response = await fetch(`/api/files/ls?${params}`, { signal: abort.signal })
        if (!response.ok) throw new Error(await responseError(response, "Could not load folder"))
        const raw = await response.json()
        if (id !== request.current || abort.signal.aborted) return
        const entries: FileEntry[] = Array.isArray(raw) ? raw : (raw.entries ?? [])
        if (activeDrive && path === activeDrive.base && !query) {
          const child = entries.find((e) => e.is_dir && e.name === activeDrive.id)
          if (child) {
            setEffectiveBase(child.path)
            setCurrentPath(child.path)
            return
          }
        }
        setFiles(entries)
      } catch (e) {
        if (id === request.current && !abort.signal.aborted)
          setError(e instanceof Error ? e.message : "Could not load folder")
      } finally {
        if (id === request.current && !abort.signal.aborted) setLoading(false)
      }
    },
    [activeDrive],
  )

  useEffect(() => {
    const changedPath = locationRef.current !== currentPath
    locationRef.current = currentPath
    controller.current?.abort()
    request.current++
    setSelected(new Set())
    setVisibleCount(100)
    if (changedPath) setFiles([])
    if (!currentPath) return
    const timer = setTimeout(() => void fetchFiles(currentPath, search), changedPath ? 0 : 300)
    return () => {
      clearTimeout(timer)
      controller.current?.abort()
    }
  }, [currentPath, search, fetchFiles])

  useEffect(() => {
    if (activeDrive && currentPath)
      remembered = { drive: activeDrive.id, path: currentPath, search, sort }
  }, [activeDrive, currentPath, search, sort])

  function navigate(path: string) {
    controller.current?.abort()
    request.current++
    setCurrentPath(path)
    setSearch("")
    setOperationError(null)
  }
  function switchDrive(id: string) {
    const drive = drives.find((d) => d.id === id)
    if (!drive) return
    setActiveDrive(drive)
    setEffectiveBase("")
    navigate(drive.base)
  }
  function toggle(path: string) {
    setSelected((prev) => {
      const next = new Set(prev)
      if (next.has(path)) next.delete(path)
      else next.add(path)
      return next
    })
  }
  const sorted = useMemo(
    () =>
      [...files].sort((a, b) => {
        if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1
        const name = a.name.localeCompare(b.name, undefined, { sensitivity: "base" })
        if (sort === "name-desc") return -name
        if (sort === "date-newest") return Date.parse(b.mod_time) - Date.parse(a.mod_time) || name
        if (sort === "date-oldest") return Date.parse(a.mod_time) - Date.parse(b.mod_time) || name
        if (sort === "size-largest") return b.size - a.size || name
        if (sort === "size-smallest") return a.size - b.size || name
        if (sort === "type")
          return (
            (a.name.split(".").pop() ?? "").localeCompare(b.name.split(".").pop() ?? "") || name
          )
        return name
      }),
    [files, sort],
  )

  async function deleteSelected() {
    if (busy || !selected.size || !confirm(`Permanently delete ${selected.size} selected item(s)?`))
      return
    const path = currentPath
    setBusy(true)
    setOperationError(null)
    const failed = new Set<string>()
    const messages: string[] = []
    for (const item of selected) {
      try {
        const response = await fetch(`/api/files?path=${encodeURIComponent(item)}`, {
          method: "DELETE",
        })
        if (!response.ok) throw new Error(await responseError(response, "Delete failed"))
      } catch (e) {
        failed.add(item)
        messages.push(
          `${item.split("/").pop()}: ${e instanceof Error ? e.message : "Delete failed"}`,
        )
      }
    }
    if (mounted.current && locationRef.current === path) {
      await fetchFiles(path, search)
      setSelected(failed)
      setOperationError(messages.length ? messages.join(" · ") : null)
    }
    setBusy(false)
  }
  async function newFolder() {
    const name = prompt("Folder name:")?.trim()
    if (!name) return
    if ([".", ".."].includes(name) || /[/\\\0]/.test(name)) {
      setOperationError("Enter a folder name without path separators.")
      return
    }
    const path = currentPath
    setBusy(true)
    setOperationError(null)
    try {
      const response = await fetch("/api/files/mkdir", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ path: `${path}/${name}` }),
      })
      if (!response.ok) throw new Error(await responseError(response, "Could not create folder"))
      if (locationRef.current === path) await fetchFiles(path, search)
    } catch (e) {
      setOperationError(e instanceof Error ? e.message : "Could not create folder")
    } finally {
      setBusy(false)
    }
  }
  function sendUpload(item: UploadProgress, index: number, overwrite = false): Promise<void> {
    return new Promise((resolve) => {
      const update = (patch: Partial<UploadProgress>) => {
        if (mounted.current)
          setUploads((prev) => prev.map((u, i) => (i === index ? { ...u, ...patch } : u)))
      }
      const form = new FormData()
      form.append("path", item.destination)
      form.append("relative_path", item.fileName)
      form.append("overwrite", String(overwrite))
      form.append("file", item.file)
      const xhr = new XMLHttpRequest()
      xhr.open("POST", "/api/files/upload")
      update({ done: false, error: null, conflict: false, loaded: 0 })
      xhr.upload.onprogress = (e) => {
        if (e.lengthComputable) update({ loaded: e.loaded, total: e.total })
      }
      xhr.onload = () => {
        let error = null
        if (xhr.status < 200 || xhr.status >= 300) {
          try {
            error = JSON.parse(xhr.responseText).error
          } catch {
            /* Non-JSON proxy errors still get a useful fallback. */
          }
          error ||= `Upload failed (${xhr.status})`
        }
        update({ done: true, error, conflict: xhr.status === 409, loaded: error ? 0 : item.total })
        resolve()
      }
      xhr.onerror = () => {
        update({ done: true, error: "Connection lost. Retry the upload." })
        resolve()
      }
      xhr.onabort = () => {
        update({ done: true, error: "Upload cancelled." })
        resolve()
      }
      xhr.send(form)
    })
  }
  async function processFiles(list: File[]) {
    if (!list.length || uploadBusy.current) return
    let batch: UploadProgress[]
    try {
      batch = list.map((file) => ({
        file,
        fileName: uploadRelativePath(file),
        destination: currentPath,
        loaded: 0,
        total: file.size,
        done: false,
        error: null,
        conflict: false,
      }))
    } catch (e) {
      setOperationError(e instanceof Error ? e.message : "Invalid upload")
      return
    }
    uploadBusy.current = true
    setUploading(true)
    setUploads(batch)
    for (let i = 0; i < batch.length; i++) await sendUpload(batch[i], i)
    uploadBusy.current = false
    if (!mounted.current) return
    setUploading(false)
    if (uploadRef.current) uploadRef.current.value = ""
    if (folderRef.current) folderRef.current.value = ""
    if (locationRef.current === batch[0].destination) await fetchFiles(batch[0].destination, search)
  }
  async function retryUpload(index: number, overwrite = false) {
    const item = uploads[index]
    if (
      uploadBusy.current ||
      !item ||
      (overwrite && !confirm(`Replace ${item.fileName} in ${item.destination}?`))
    )
      return
    uploadBusy.current = true
    setUploading(true)
    await sendUpload(item, index, overwrite)
    uploadBusy.current = false
    setUploading(false)
    if (locationRef.current === item.destination) await fetchFiles(item.destination, search)
  }
  function downloadSelected() {
    const form = document.createElement("form")
    form.method = "POST"
    form.action = "/api/files/download-zip-multi"
    form.style.display = "none"
    const input = document.createElement("input")
    input.type = "hidden"
    input.name = "paths"
    input.value = JSON.stringify([...selected])
    form.appendChild(input)
    document.body.appendChild(form)
    form.submit()
    form.remove()
  }

  if (!activeDrive)
    return (
      <div className="p-8 text-slate-400" role="status">
        Loading files…
      </div>
    )
  const base = effectiveBase || activeDrive.base
  const crumbs = currentPath.slice(base.length).split("/").filter(Boolean)
  return (
    <div className="flex min-h-[70vh] flex-col gap-4">
      <header className="flex flex-wrap items-center justify-between gap-3">
        <h1 className="text-2xl font-bold text-slate-100">Files</h1>
        <div className="flex gap-2">
          <button className={buttonClass} disabled={busy} onClick={() => void newFolder()}>
            <CreateNewFolderIcon className="h-4 w-4" />
            New folder
          </button>
          <details ref={uploadMenuRef} className="relative">
            <summary className={cn(buttonClass, "cursor-pointer list-none")}>
              <UploadIcon className="h-4 w-4" />
              Upload
            </summary>
            <div className={`absolute right-0 top-full z-30 mt-2 grid w-44 gap-1 p-2 ${DROPDOWN_SURFACE}`}>
              <button
                className={DROPDOWN_OPTION}
                disabled={uploading}
                onClick={() => {
                  uploadMenuRef.current?.removeAttribute("open")
                  uploadRef.current?.click()
                }}
              >
                Files
              </button>
              <button
                className={DROPDOWN_OPTION}
                disabled={uploading}
                onClick={() => {
                  uploadMenuRef.current?.removeAttribute("open")
                  folderRef.current?.click()
                }}
              >
                Folder
              </button>
            </div>
          </details>
          <input
            ref={uploadRef}
            type="file"
            multiple
            className="hidden"
            onChange={(e) => void processFiles(Array.from(e.target.files ?? []))}
          />
            <input
            ref={folderRef}
            type="file"
            multiple
            {...{ webkitdirectory: "" }}
            className="hidden"
            onChange={(e) => void processFiles(Array.from(e.target.files ?? []))}
          />
        </div>
      </header>
      <div className="flex flex-wrap gap-2">
        <SelectMenu label="File location" value={activeDrive.id} onChange={switchDrive}
          options={drives.map(drive => ({ value: drive.id, label: drive.id, group: drive.id === "USB Drive" ? "Advanced locations" : "Media" }))} />
        <div className="relative min-w-40 flex-1">
          <SearchIcon className="pointer-events-none absolute left-3 top-3 h-4 w-4 text-slate-400" />
          <input
            aria-label="Search files"
            placeholder="Search this location"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className="min-h-10 w-full rounded-xl border border-white/10 bg-white/5 py-2 pl-9 pr-3 text-sm text-slate-200"
          />
        </div>
        <SelectMenu label="Sort files" value={sort} align="end" onChange={value => setSort(value as SortOption)}
          options={Object.entries(SORT_LABELS).map(([value, label]) => ({ value, label }))} />
      </div>
      {operationError && (
        <div
          role="alert"
          className="rounded-xl border border-red-400/20 bg-red-400/10 p-3 text-sm text-red-300"
        >
          {operationError}
        </div>
      )}
      {!!uploads.length && (
        <section aria-label="Upload results" className="glass-card space-y-3 p-4">
          <div className="flex justify-between gap-2">
            <span className="text-sm text-slate-200">
              {uploading
                ? "Uploading…"
                : uploads.some((u) => u.error)
                  ? "Some uploads need attention"
                  : "Uploads complete"}
            </span>
            {!uploading && (
              <button aria-label="Dismiss upload results" onClick={() => setUploads([])}>
                <CloseIcon className="h-5 w-5" />
              </button>
            )}
          </div>
          <div className="max-h-64 space-y-3 overflow-y-auto">
            {uploads.map((u, i) => (
              <div key={`${u.destination}/${u.fileName}`}>
                <div className="flex flex-wrap justify-between gap-2 text-sm">
                  <span className="break-all text-slate-300">{u.fileName}</span>
                  <span className={u.error ? "text-red-300" : "text-emerald-300"}>
                    {u.error
                      ? "Failed"
                      : u.done
                        ? "Done"
                        : `${Math.min(100, Math.round((u.loaded / Math.max(1, u.total)) * 100))}%`}
                  </span>
                </div>
                {u.error ? (
                  <div className="mt-1 flex flex-wrap items-center gap-2">
                    <span className="text-xs text-red-300">{u.error}</span>
                    <button
                      className={buttonClass}
                      disabled={uploading}
                      onClick={() => void retryUpload(i, u.conflict)}
                    >
                      {u.conflict ? "Replace existing file" : "Retry"}
                    </button>
                  </div>
                ) : (
                  <progress
                    className="mt-1 h-1 w-full accent-emerald-400"
                    value={u.done ? u.total || 1 : u.loaded}
                    max={u.total || 1}
                  />
                )}
              </div>
            ))}
          </div>
        </section>
      )}
      <section
        className={cn("glass-card relative overflow-hidden", dragging && "ring-2 ring-emerald-400")}
        onDragEnter={(e) => {
          e.preventDefault()
          dragDepth.current++
          setDragging(true)
        }}
        onDragLeave={(e) => {
          e.preventDefault()
          if (--dragDepth.current <= 0) setDragging(false)
        }}
        onDragOver={(e) => e.preventDefault()}
        onDrop={(e) => {
          e.preventDefault()
          dragDepth.current = 0
          setDragging(false)
          if (
            Array.from(e.dataTransfer.items).some((item) => item.webkitGetAsEntry?.()?.isDirectory)
          ) {
            setOperationError("Choose Upload → Folder to preserve its folder structure.")
            return
          }
          void processFiles(Array.from(e.dataTransfer.files))
        }}
      >
        {dragging && (
          <div className="pointer-events-none absolute inset-0 z-20 grid place-content-center bg-slate-950/90 text-emerald-300">
            Drop files to upload
          </div>
        )}
        <nav
          aria-label="Folder path"
          className="flex flex-wrap items-center gap-2 border-b border-white/10 px-4 py-3 text-sm text-slate-300"
        >
          {currentPath !== base && (
            <button
              aria-label="Parent folder"
              className="p-1"
              onClick={() => navigate(currentPath.split("/").slice(0, -1).join("/") || base)}
            >
              <ArrowBackIcon className="h-4 w-4" />
            </button>
          )}
          <button onClick={() => navigate(base)}>{activeDrive.id}</button>
          {crumbs.map((part, i) => (
            <span key={i} className="flex items-center gap-2">
              <span aria-hidden="true">/</span>
              <button onClick={() => navigate(`${base}/${crumbs.slice(0, i + 1).join("/")}`)}>
                {part}
              </button>
            </span>
          ))}
        </nav>
        {selected.size > 0 && (
          <div className="flex flex-wrap items-center gap-2 border-b border-white/10 bg-emerald-500/5 px-4 py-2">
            <span className="mr-auto text-sm text-slate-300">{selected.size} selected</span>
            <button className={buttonClass} onClick={downloadSelected}>
              <DownloadIcon className="h-4 w-4" />
              Download
            </button>
            <button className={buttonClass} disabled={busy} onClick={() => void deleteSelected()}>
              <DeleteIcon className="h-4 w-4" />
              Delete
            </button>
            <button className={buttonClass} onClick={() => setSelected(new Set())}>
              Clear
            </button>
          </div>
        )}
        {loading && (
          <div role="status" className="flex items-center gap-2 px-4 py-2 text-sm text-slate-400">
            <ProgressActivityIcon className="h-4 w-4 animate-spin" />
            Loading…
          </div>
        )}
        {error ? (
          <div role="alert" className="p-4 text-sm text-red-300">
            {error}{" "}
            <button className={buttonClass} onClick={() => void fetchFiles(currentPath, search)}>
              Retry
            </button>
          </div>
        ) : (
          <>
            <div className="overflow-x-auto">
              <table className="w-full text-left text-sm">
                <thead className="text-xs text-slate-400">
                  <tr>
                    <th className="w-12 p-3">
                      <input
                        aria-label="Select all listed files"
                        type="checkbox"
                        checked={files.length > 0 && selected.size === files.length}
                        onChange={(e) =>
                          setSelected(
                            e.target.checked ? new Set(files.map((f) => f.path)) : new Set(),
                          )
                        }
                      />
                    </th>
                    <th className="py-3">Name</th>
                    <th className="hidden p-3 text-right sm:table-cell">Modified</th>
                    <th className="p-3 text-right">Size</th>
                  </tr>
                </thead>
                <tbody>
                  {sorted.slice(0, visibleCount).map((f) => (
                    <tr
                      key={f.path}
                      className={cn(
                        "border-t border-white/5 hover:bg-white/5",
                        selected.has(f.path) && "bg-emerald-500/10",
                      )}
                    >
                      <td className="p-3">
                        <input
                          aria-label={`Select ${f.name}`}
                          type="checkbox"
                          checked={selected.has(f.path)}
                          onChange={() => toggle(f.path)}
                        />
                      </td>
                      <td className="max-w-[50vw] py-3 pr-3">
                        <button
                          className="flex max-w-full items-center gap-3 text-left text-slate-200"
                          onClick={() => (f.is_dir ? navigate(f.path) : toggle(f.path))}
                        >
                          {f.is_dir ? (
                            <FolderIcon className="h-4 w-4 shrink-0 text-emerald-400" />
                          ) : (
                            <DraftIcon className="h-4 w-4 shrink-0 text-slate-400" />
                          )}
                          <span className="break-all">{f.name}</span>
                        </button>
                      </td>
                      <td className="hidden whitespace-nowrap p-3 text-right text-xs text-slate-400 sm:table-cell">
                        {f.mod_time ? new Date(f.mod_time).toLocaleDateString() : "—"}
                      </td>
                      <td className="whitespace-nowrap p-3 text-right text-xs text-slate-400">
                        {f.is_dir ? "—" : formatSize(f.size)}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            {!loading && !files.length && (
              <p className="p-8 text-center text-sm text-slate-400">
                {search ? "No matching files" : "This folder is empty"}
              </p>
            )}
            {sorted.length > visibleCount && (
              <div className="p-4 text-center">
                <button className={buttonClass} onClick={() => setVisibleCount((n) => n + 100)}>
                  Show more · {visibleCount} of {sorted.length}
                </button>
              </div>
            )}
          </>
        )}
      </section>
    </div>
  )
}

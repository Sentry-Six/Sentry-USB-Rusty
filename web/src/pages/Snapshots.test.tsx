import assert from "node:assert/strict"
import test from "node:test"
import { act, createElement } from "react"
import { Window } from "happy-dom"
import Snapshots from "./Snapshots.tsx"

test("a delayed pre-delete list cannot resurrect a removed snapshot", async () => {
  const win = new Window({ url: "http://localhost/snapshots" })
  const values = {
    window: win,
    document: win.document,
    navigator: win.navigator,
    IS_REACT_ACT_ENVIRONMENT: true,
  }
  const originals = Object.keys(values).map(
    (key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const,
  )
  for (const [key, value] of Object.entries(values))
    Object.defineProperty(globalThis, key, { configurable: true, value })
  const oldFetch = globalThis.fetch
  const entry = (id: number) => ({
    id: `snap-${id}`,
    created_unix: id * 100,
    older_count: id - 1,
    cumulative_reclaim_bytes: id * 1024,
  })
  let listCalls = 0
  let stale: ((response: Response) => void) | undefined
  const deleted: string[] = []
  globalThis.fetch = async (input, init) => {
    const path = String(input)
    if (init?.method === "DELETE") {
      deleted.push(path)
      return Response.json({})
    }
    if (path.includes("free-space"))
      return Response.json({
        mounted: true,
        total_bytes: 1000,
        used_bytes: 950,
        available_bytes: 50,
      })
    listCalls++
    if (listCalls === 2)
      return new Promise((resolve) => {
        stale = resolve
      })
    return Response.json({
      snapshots: listCalls === 1 ? [entry(1), entry(2)] : [entry(2), entry(3)],
    })
  }
  const { createRoot } = await import("react-dom/client")
  const container = win.document.createElement("div")
  win.document.body.append(container)
  const root = createRoot(container)
  try {
    await act(async () => root.render(createElement(Snapshots)))
    await act(async () =>
      [...container.querySelectorAll("button")].find((b) => b.textContent === "Refresh")!.click(),
    )
    assert.ok(stale)
    await act(async () =>
      container.querySelector<HTMLElement>('[aria-label="Delete only snap-1"]')!.click(),
    )
    await act(async () =>
      [...win.document.querySelectorAll("button")]
        .find((b) => b.textContent === "Delete permanently")!
        .click(),
    )
    await act(async () => stale!(Response.json({ snapshots: [entry(1), entry(2)] })))
    assert.deepEqual(deleted, ["/api/snapshots/snap-1"])
    assert.equal(listCalls, 3)
    assert.equal(container.querySelector('[aria-label="Delete only snap-1"]'), null)
    assert.ok(container.querySelector('[aria-label="Delete only snap-3"]'))
  } finally {
    await act(async () => root.unmount())
    globalThis.fetch = oldFetch
    for (const [key, descriptor] of originals) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor)
      else Reflect.deleteProperty(globalThis, key)
    }
    win.close()
  }
})

test("snapshots finish loading after StrictMode cancels the first mount request", async () => {
  const { StrictMode } = await import("react")
  const win = new Window({ url: "http://localhost/snapshots" })
  const values = { window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true }
  const originals = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const oldFetch = globalThis.fetch
  globalThis.fetch = async input => String(input).includes("free-space")
    ? Response.json({ mounted: true, total_bytes: 1e12, available_bytes: 5e10, used_bytes: 95e10 })
    : Response.json({ snapshots: [{ id: "snap-existing", created_unix: 100, older_count: 0, cumulative_reclaim_bytes: 1024 }] })
  const { createRoot } = await import("react-dom/client")
  const container = win.document.createElement("div"); win.document.body.append(container)
  const root = createRoot(container)
  try {
    await act(async () => root.render(createElement(StrictMode, null, createElement(Snapshots))))
    assert.ok(container.querySelector('[aria-label="Delete only snap-existing"]'))
    assert.ok(!container.textContent.includes("Loading snapshots"))
  } finally {
    await act(async () => root.unmount()); globalThis.fetch = oldFetch
    for (const [key, descriptor] of originals) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
})

test("delete preview follows exact chronological IDs in both sort orders without deleting", async () => {
  const win = new Window({ url: "http://localhost/snapshots" })
  const values = { window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true }
  const originals = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const oldFetch = globalThis.fetch
  const methods: string[] = []
  globalThis.fetch = async (input, init) => {
    methods.push(init?.method ?? "GET")
    return String(input).includes("free-space") ? Response.json({ mounted: false }) : Response.json({ snapshots: [
      { id: "newest", created_unix: 300, older_count: 2, cumulative_reclaim_bytes: 4096 },
      { id: "oldest", created_unix: 100, older_count: 0, cumulative_reclaim_bytes: 1024 },
      { id: "middle", created_unix: 200, older_count: 1, cumulative_reclaim_bytes: 2048 },
    ] })
  }
  const { createRoot } = await import("react-dom/client")
  const container = win.document.createElement("div"); win.document.body.append(container)
  const root = createRoot(container)
  const highlighted = () => [...container.querySelectorAll('[data-delete-preview="true"]')].map(row => row.getAttribute('data-snapshot-id'))
  try {
    await act(async () => root.render(createElement(Snapshots)))
    const through = container.querySelector<HTMLElement>('[aria-label="Delete through middle"]')!
    await act(async () => through.dispatchEvent(new win.MouseEvent("mouseover", { bubbles: true })))
    assert.deepEqual(highlighted(), ["oldest", "middle"])
    assert.match(container.querySelector('#snapshot-delete-preview')!.textContent, /2 snapshots.*2.0 KB/)
    await act(async () => through.dispatchEvent(new win.MouseEvent("mouseout", { bubbles: true })))
    assert.deepEqual(highlighted(), [])
    await act(async () => container.querySelector<HTMLElement>('[aria-label="Sort snapshots"]')!.click())
    await act(async () => win.document.querySelector<HTMLElement>('[data-value="newest"]')!.click())
    await act(async () => through.focus())
    assert.deepEqual(highlighted(), ["middle", "oldest"])
    await act(async () => through.click())
    assert.match(win.document.querySelector('[role="dialog"]')!.textContent, /2 snapshots selected/)
    assert.deepEqual(highlighted(), ["middle", "oldest"])
    assert.ok(methods.every(method => method === 'GET'))
  } finally {
    await act(async () => root.unmount()); globalThis.fetch = oldFetch
    for (const [key, descriptor] of originals) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
})

test("tied timestamps preserve the API reclaim prefix in both sorts and delete exactly those IDs", async () => {
  const win = new Window({ url: "http://localhost/snapshots" })
  const values = { window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true }
  const originals = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const oldFetch = globalThis.fetch
  const deleted: string[] = []
  // Legacy servers may return ties in directory order, not lexical ID order.
  const entries = [
    { id: "snap-200", created_unix: 100, older_count: 0, cumulative_reclaim_bytes: 1024 },
    { id: "snap-100", created_unix: 100, older_count: 1, cumulative_reclaim_bytes: 3072 },
    { id: "snap-300", created_unix: 300, older_count: 2, cumulative_reclaim_bytes: 8192 },
  ]
  globalThis.fetch = async (input, init) => {
    const path = String(input)
    if (init?.method === "DELETE") { deleted.push(path); return Response.json({}) }
    return path.includes("free-space") ? Response.json({ mounted: false })
      : Response.json({ snapshots: entries.filter(entry => !deleted.includes(`/api/snapshots/${entry.id}`)) })
  }
  const { createRoot } = await import("react-dom/client")
  const container = win.document.createElement("div"); win.document.body.append(container)
  const root = createRoot(container)
  const highlighted = () => [...container.querySelectorAll('[data-delete-preview="true"]')].map(row => row.getAttribute("data-snapshot-id"))
  try {
    await act(async () => root.render(createElement(Snapshots)))
    const through = container.querySelector<HTMLElement>('[aria-label="Delete through snap-100"]')!
    await act(async () => through.focus())
    assert.deepEqual(highlighted(), ["snap-200", "snap-100"])
    assert.match(container.querySelector("#snapshot-delete-preview")!.textContent, /2 snapshots.*3.0 KB/)
    await act(async () => container.querySelector<HTMLElement>('[aria-label="Sort snapshots"]')!.click())
    await act(async () => win.document.querySelector<HTMLElement>('[data-value="newest"]')!.click())
    await act(async () => through.focus())
    assert.deepEqual(highlighted(), ["snap-100", "snap-200"])
    await act(async () => through.click())
    assert.match(win.document.querySelector('[role="dialog"]')!.textContent, /2 snapshots selected/)
    assert.deepEqual(deleted, [])
    await act(async () => [...win.document.querySelectorAll("button")].find(button => button.textContent === "Delete permanently")!.click())
    assert.deepEqual(deleted, ["/api/snapshots/snap-200", "/api/snapshots/snap-100"])
    assert.ok(container.querySelector('[data-snapshot-id="snap-300"]'))
  } finally {
    await act(async () => root.unmount()); globalThis.fetch = oldFetch
    for (const [key, descriptor] of originals) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
})

test("a timed out deletion unlocks the modal, preserves unknown IDs and never continues or retries the batch", async () => {
  const win = new Window({ url: "http://localhost/snapshots" })
  const values = { window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true }
  const originals = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const oldFetch = globalThis.fetch, oldTimeout = globalThis.setTimeout, oldClearTimeout = globalThis.clearTimeout
  const deadlines = new Map<number, () => void>()
  let timerId = 10000
  globalThis.setTimeout = ((callback: () => void, delay?: number, ...args: unknown[]) => {
    if (delay !== 30000) return oldTimeout(callback, delay, ...args)
    const id = timerId++
    deadlines.set(id, () => { deadlines.delete(id); callback() })
    return id
  }) as typeof setTimeout
  globalThis.clearTimeout = ((id: ReturnType<typeof setTimeout>) => {
    if (typeof id === "number" && deadlines.has(id)) deadlines.delete(id)
    else oldClearTimeout(id)
  }) as typeof clearTimeout
  const deletes: string[] = []
  let stalled: ((response: Response) => void) | undefined
  let signal: AbortSignal | undefined
  let listCalls = 0
  globalThis.fetch = async (input, init) => {
    const path = String(input)
    if (init?.method === "DELETE") {
      deletes.push(path)
      if (deletes.length === 2) {
        signal = init.signal as AbortSignal
        // Deliberately ignore abort: a late server reply must not continue the batch.
        return new Promise<Response>(resolve => { stalled = resolve })
      }
      return Response.json({})
    }
    if (path.includes("free-space")) return Response.json({ mounted: false })
    listCalls++
    return Response.json({ snapshots: (listCalls === 1 ? [1, 2, 3] : [2, 3]).map(id => ({
      id: `snap-${id}`, created_unix: id * 100, older_count: id - 1, cumulative_reclaim_bytes: id * 1024,
    })) })
  }
  const { createRoot } = await import("react-dom/client")
  const container = win.document.createElement("div"); win.document.body.append(container)
  const root = createRoot(container)
  try {
    await act(async () => root.render(createElement(Snapshots)))
    await act(async () => container.querySelector<HTMLElement>('[aria-label="Delete through snap-3"]')!.click())
    await act(async () => [...win.document.querySelectorAll("button")].find(button => button.textContent === "Delete permanently")!.click())
    assert.deepEqual(deletes, ["/api/snapshots/snap-1", "/api/snapshots/snap-2"])
    assert.match(win.document.querySelector('[role="dialog"]')!.textContent, /1 \/ 3 removed/)
    assert.equal(deadlines.size, 1)
    await act(async () => [...deadlines.values()][0]())
    assert.equal(signal?.aborted, true)
    assert.equal(win.document.querySelector('[role="dialog"]'), null)
    assert.match(container.querySelector('[role="alert"]')!.textContent, /1 of 3 snapshots confirmed removed.*snap-2 timed out and may still complete/)
    assert.equal(listCalls, 2)
    assert.equal(container.querySelector('[data-snapshot-id="snap-1"]'), null)
    assert.ok(container.querySelector('[data-snapshot-id="snap-2"]'))
    assert.ok(container.querySelector('[data-snapshot-id="snap-3"]'))
    await act(async () => stalled!(Response.json({})))
    assert.deepEqual(deletes, ["/api/snapshots/snap-1", "/api/snapshots/snap-2"])
    assert.equal(deadlines.size, 0)
    await act(async () => container.querySelector<HTMLElement>('[aria-label="Delete through snap-3"]')!.click())
    assert.match(win.document.querySelector('[role="dialog"]')!.textContent, /2 snapshots selected/)
    assert.equal(deletes.length, 2, "retry requires a fresh confirmation")
  } finally {
    await act(async () => root.unmount())
    globalThis.fetch = oldFetch; globalThis.setTimeout = oldTimeout; globalThis.clearTimeout = oldClearTimeout
    for (const [key, descriptor] of originals) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
})

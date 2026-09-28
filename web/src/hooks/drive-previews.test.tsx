import assert from "node:assert/strict"
import test from "node:test"
import { act, createElement } from "react"
import { MemoryRouter } from "react-router-dom"
import { Window } from "happy-dom"
import { invalidateDriveApiCache } from "../api/drives.ts"
import { computeFilteredStats } from "../lib/drive-stats.ts"
import { useDrivesList } from "./useDrivesList.ts"

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } })
}
function drive(hour: number) {
  return { id: hour, startTime: `2026-09-28T${String(hour).padStart(2,"0")}:00:00.000`, endTime: `2026-09-28T${String(hour).padStart(2,"0")}:05:00.000`, distanceMi: 2, distanceKm: 3.2, durationMs: 300000, tags: [] }
}
function page(hours: number[], page = 1, total = hours.length) {
  return { drives: hours.map(drive), page, total, limit: 10, revision: "preview-test", tags: [], stats: { ...computeFilteredStats([]), count: total } }
}
async function environment(run: (container: HTMLElement, root: import("react-dom/client").Root) => Promise<void>) {
  const browser = new Window({ url: "http://localhost/" })
  const previous = new Map<string, PropertyDescriptor | undefined>()
  for (const [key,value] of Object.entries({ window: browser, document: browser.document, navigator: browser.navigator, IS_REACT_ACT_ENVIRONMENT: true })) {
    previous.set(key, Object.getOwnPropertyDescriptor(globalThis,key))
    Object.defineProperty(globalThis,key,{ configurable:true,writable:true,value })
  }
  const fetch = globalThis.fetch
  const { createRoot } = await import("react-dom/client")
  const container = browser.document.createElement("div")
  browser.document.body.append(container)
  const root = createRoot(container as unknown as HTMLElement)
  invalidateDriveApiCache()
  try { await run(container as unknown as HTMLElement,root) }
  finally {
    await act(async () => root.unmount())
    globalThis.fetch = fetch
    browser.close()
    for (const [key,value] of previous) { if (value) Object.defineProperty(globalThis,key,value); else Reflect.deleteProperty(globalThis,key) }
  }
}
function previewRequests() {
  const pending: { start: string; signal: AbortSignal | null | undefined; resolve: (value: Response) => void }[] = []
  const fetch = (url: string, signal?: AbortSignal | null) => new Promise<Response>((resolve) => {
    pending.push({ start: new URL(url,"http://localhost").searchParams.get("starts")!, signal, resolve })
  })
  return { pending, fetch }
}
function status(history: ReturnType<typeof useDrivesList>) {
  return history.visible.map(d => `${d.id}:${history.routePreviewStatus.get(d.startTime) ?? "loading"}:${history.routesByStartTime.get(d.startTime)?.length ?? 0}`).join(";")
}

test("previews load one row at a time in display order; empty and failed routes finish without blocking later rows", async () => environment(async (container,root) => {
  const requests = previewRequests()
  globalThis.fetch = async (url, options) => String(url).includes("/routes?") ? requests.fetch(String(url),options?.signal) : json(page([13,12,11,10]))
  function Probe() { return createElement("p",null,status(useDrivesList())) }
  await act(async () => root.render(createElement(MemoryRouter,{initialEntries:["/drives?range=all&tag=ordered"]},createElement(Probe))))
  assert.equal(container.textContent,"13:loading:0;12:loading:0;11:loading:0;10:loading:0")
  assert.deepEqual(requests.pending.map(r=>r.start),[drive(13).startTime])
  await act(async () => requests.pending[0].resolve(json([{ startTime: drive(13).startTime, points: [[53,-113],[54,-114]] }])))
  assert.equal(container.textContent,"13:ready:2;12:loading:0;11:loading:0;10:loading:0")
  assert.equal(requests.pending.length,2)
  await act(async () => requests.pending[1].resolve(json([])))
  assert.equal(container.textContent,"13:ready:2;12:ready:0;11:loading:0;10:loading:0")
  await act(async () => requests.pending[2].resolve(json({error:"unavailable"},503)))
  assert.equal(container.textContent,"13:ready:2;12:ready:0;11:unavailable:0;10:loading:0")
  assert.deepEqual(requests.pending.map(r=>r.start),[13,12,11,10].map(hour=>drive(hour).startTime))
  await act(async () => requests.pending[3].resolve(json([{startTime:drive(10).startTime,points:[[53,-113]]}])))
  assert.equal(container.textContent,"13:ready:2;12:ready:0;11:unavailable:0;10:ready:1")
}))

test("changing pages aborts old previews, ignores their late response and stops their queue", async () => environment(async (container,root) => {
  const requests = previewRequests()
  let history!: ReturnType<typeof useDrivesList>
  globalThis.fetch = async (url, options) => String(url).includes("/routes?") ? requests.fetch(String(url),options?.signal) : json(new URL(String(url),"http://localhost").searchParams.get("page") === "2" ? page([6],2,11) : page([9,8],1,11))
  function Probe() { history=useDrivesList(); return createElement("p",null,status(history)) }
  await act(async () => root.render(createElement(MemoryRouter,{initialEntries:["/drives?range=all&tag=cancel"]},createElement(Probe))))
  assert.equal(requests.pending[0].start,drive(9).startTime)
  await act(async () => history.setPage(2))
  assert.equal(requests.pending[0].signal?.aborted,true)
  assert.equal(requests.pending[1].start,drive(6).startTime)
  await act(async () => requests.pending[0].resolve(json([{startTime:drive(9).startTime,points:[[53,-113]]}])))
  assert.equal(container.textContent,"6:loading:0")
  assert.equal(history.routesByStartTime.has(drive(9).startTime),false)
  assert.equal(requests.pending.length,2)
  await act(async () => requests.pending[1].resolve(json([])))
  assert.equal(container.textContent,"6:ready:0")
}))

test("a timed-out preview does not keep the rest of the page waiting", async () => environment(async (container,root) => {
  const originalTimeout=globalThis.setTimeout
  let expire: (() => void) | undefined
  globalThis.setTimeout=((callback: () => void,delay?: number) => {
    if (delay === 30_000) { expire=callback; return 999 as unknown as ReturnType<typeof setTimeout> }
    return originalTimeout(callback,delay)
  }) as typeof setTimeout
  let requests=0
  globalThis.fetch=async (url,options) => {
    if (!String(url).includes("/routes?")) return json(page([5,4]))
    if (++requests > 1) return json([])
    return new Promise<Response>((_,reject) => options?.signal?.addEventListener("abort",()=>reject(new DOMException("Aborted","AbortError"))))
  }
  function Probe() { return createElement("p",null,status(useDrivesList())) }
  try {
    await act(async () => root.render(createElement(MemoryRouter,{initialEntries:["/drives?range=all&tag=timeout"]},createElement(Probe))))
    assert.equal(container.textContent,"5:loading:0;4:loading:0")
    await act(async () => expire?.())
    assert.equal(container.textContent,"5:unavailable:0;4:ready:0")
    assert.equal(requests,2)
  } finally { globalThis.setTimeout=originalTimeout }
}))


test("a failed refresh retains completed maps and ends loading for unfinished previews", async () => environment(async (container,root) => {
  const requests = previewRequests()
  let history!: ReturnType<typeof useDrivesList>
  let failList = false
  globalThis.fetch = async (url, options) => String(url).includes("/routes?") ? requests.fetch(String(url),options?.signal)
    : failList ? json({ error: "unavailable" }, 503) : json(page([3,2]))
  function Probe() { history=useDrivesList(); return createElement("p",null,status(history)) }
  await act(async () => root.render(createElement(MemoryRouter,{initialEntries:["/drives?range=all&tag=refresh"]},createElement(Probe))))
  await act(async () => requests.pending[0].resolve(json([{startTime:drive(3).startTime,points:[[53,-113]]}])))
  assert.equal(container.textContent,"3:ready:1;2:loading:0")
  failList = true
  await act(async () => { await history.refresh() })
  assert.equal(requests.pending[1].signal?.aborted,true)
  assert.ok(history.error)
  assert.equal(container.textContent,"3:ready:1;2:unavailable:0")
  await act(async () => requests.pending[1].resolve(json([{startTime:drive(2).startTime,points:[[53,-113]]}])))
  assert.equal(container.textContent,"3:ready:1;2:unavailable:0")
  failList = false
  await act(async () => { await history.refresh() })
  assert.equal(container.textContent,"3:loading:0;2:loading:0")
  assert.equal(requests.pending[2].start,drive(3).startTime)
}))

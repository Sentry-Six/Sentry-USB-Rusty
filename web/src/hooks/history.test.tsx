import assert from "node:assert/strict"
import test from "node:test"
import { act, createElement, StrictMode } from "react"
import { MemoryRouter } from "react-router-dom"
import { Window } from "happy-dom"
import { computeFilteredStats } from "../lib/drive-stats.ts"
import { invalidateDriveApiCache } from "../api/drives.ts"
import { useDrivesList } from "./useDrivesList.ts"
import { useChargingHistory } from "./useChargingHistory.ts"

async function environment(run: (container: HTMLElement, root: import("react-dom/client").Root) => Promise<void>) {
  const browser = new Window({ url:"http://localhost/" })
  const previous = new Map<string, PropertyDescriptor | undefined>()
  for (const [key,value] of Object.entries({ window:browser, document:browser.document, navigator:browser.navigator, IS_REACT_ACT_ENVIRONMENT:true })) {
    previous.set(key,Object.getOwnPropertyDescriptor(globalThis,key))
    Object.defineProperty(globalThis,key,{ configurable:true,writable:true,value })
  }
  const fetch = globalThis.fetch
  const { createRoot } = await import("react-dom/client")
  const container = browser.document.createElement("div")
  browser.document.body.append(container)
  const root = createRoot(container as unknown as HTMLElement)
  try { await run(container as unknown as HTMLElement,root) }
  finally {
    await act(async () => root.unmount())
    globalThis.fetch=fetch
    browser.close()
    for (const [key,value] of previous) { if (value) Object.defineProperty(globalThis,key,value); else Reflect.deleteProperty(globalThis,key) }
  }
}
function json(body: unknown) { return new Response(JSON.stringify(body),{ headers:{"Content-Type":"application/json"} }) }

test("drive rows appear while bounded optional previews are still loading", async () => environment(async (container,root) => {
  const requests: string[]=[]
  let previewsAborted=false
  globalThis.fetch=async (url,options) => {
    const path=String(url);requests.push(path)
    if (path.startsWith("/api/drives/routes?")) return new Promise<Response>((_,reject) => options?.signal?.addEventListener("abort",()=> { previewsAborted=true;reject(new DOMException("Aborted","AbortError")) }))
    const drive = { id:1,startTime:"2026-09-27T12:00:00.000",endTime:"2026-09-27T12:05:00.000",distanceMi:2,distanceKm:3.2,durationMs:300000,tags:[] }
    return json({ drives:[drive], total:1,page:1,limit:10,tags:[],stats:{...computeFilteredStats([]),count:1},revision:"test",capabilities:{additiveTags:true} })
  }
  function Probe() {
    const history=useDrivesList()
    return createElement("p",null,history.loading ? "loading" : `rows:${history.visible.length}`)
  }
  await act(async () => root.render(createElement(MemoryRouter,{initialEntries:["/drives?range=all"]},createElement(Probe))))
  assert.equal(container.textContent,"rows:1")
  assert.ok(requests[0].includes("limit=10"))
  const previews=requests.find((url)=>url.includes("/routes?"))!
  assert.equal(new URL(previews,"http://localhost").searchParams.get("starts"),"2026-09-27T12:00:00.000")
  await act(async () => root.render(null))
  assert.equal(previewsAborted,true)
}))

test("charging renders history despite optional failures and retains it on return", async () => environment(async (container,root) => {
  let requests=0
  globalThis.fetch=async (url) => {
    requests++
    if (String(url) === "/api/charging" || String(url).startsWith("/api/charging?")) return json({ sessions:[{ id:1000,tags:[] }] })
    return new Response("unavailable",{status:503})
  }
  function Probe() {
    const history=useChargingHistory()
    return createElement("p",null,history.loading ? "loading" : `rows:${history.sessions.length};warning:${!!history.warning}`)
  }
  await act(async () => root.render(createElement(Probe)))
  assert.equal(container.textContent,"rows:1;warning:true")
  const before=requests
  await act(async () => root.render(null))
  await act(async () => root.render(createElement(Probe)))
  assert.match(container.textContent ?? "",/^rows:1/)
  assert.equal(requests,before)
}))

test("charging completion refreshes final totals without waiting for navigation", async () => environment(async (container,root) => {
  const { invalidateChargingHistory } = await import("./useChargingHistory.ts")
  invalidateChargingHistory()
  const originalTimeout=globalThis.setTimeout
  let scheduled: (() => Promise<void>) | undefined
  globalThis.setTimeout=((callback: (...args: unknown[]) => void, delay?: number, ...args: unknown[]) => {
    if (delay === 30_000) { scheduled=callback as () => Promise<void>; return 999 as unknown as ReturnType<typeof setTimeout> }
    return originalTimeout(callback,delay,...args)
  }) as typeof setTimeout
  let charging=true, historyRequests=0
  globalThis.fetch=async (url) => {
    const path=String(url)
    if (path.startsWith("/api/charging?")) { historyRequests++; return json({ sessions:[{id:1000,tags:[],energyAddedKwh:charging?2:5}] }) }
    if (path === "/api/charging/tags") return json([])
    return json({charging})
  }
  function Probe() {
    const history=useChargingHistory()
    return createElement("p",null,String(history.sessions[0]?.energyAddedKwh))
  }
  try {
    await act(async () => root.render(createElement(Probe)))
    assert.equal(container.textContent,"2")
    charging=false
    await act(async () => { await scheduled?.() })
    assert.equal(container.textContent,"5")
    assert.equal(historyRequests,2)
  } finally { globalThis.setTimeout=originalTimeout }
}))


test("StrictMode displays a legacy array through local paging and filters", async () => environment(async (container,root) => {
  invalidateDriveApiCache()
  const drives=Array.from({length:14},(_,index)=>({ id:index,startTime:`2026-09-28T${String(index).padStart(2,"0")}:00:00.000`,endTime:`2026-09-28T${String(index).padStart(2,"0")}:05:00.000`,distanceMi:2,distanceKm:3.2,durationMs:300000,tags:[index<12?"Work":"Home"] }))
  globalThis.fetch=async (url)=>String(url).includes("/routes?") ? json([]) : json(drives)
  function Probe() {
    const history=useDrivesList()
    return createElement("p",null,history.error ?? (history.loading ? "loading" : `rows:${history.visible.length};total:${history.total};km:${history.filteredStats.totalDistanceKm.toFixed(1)}`))
  }
  await act(async()=>root.render(createElement(StrictMode,null,createElement(MemoryRouter,{initialEntries:["/drives?range=all&tag=Work&page=2"]},createElement(Probe)))))
  assert.equal(container.textContent,"rows:2;total:12;km:38.4")
}))

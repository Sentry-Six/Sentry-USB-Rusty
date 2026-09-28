import assert from "node:assert/strict"
import test from "node:test"
import { act, createElement, StrictMode } from "react"
import { Window } from "happy-dom"
import { MobileNotificationsSection } from "./MobileNotificationsSection.tsx"

async function environment(run: (container: HTMLElement, root: import("react-dom/client").Root, deadlines: Map<number, () => void>) => Promise<void>) {
  const browser=new Window({url:"http://localhost/"})
  const descriptors=["window","document","navigator","IS_REACT_ACT_ENVIRONMENT"].map((key)=>[key,Object.getOwnPropertyDescriptor(globalThis,key)] as const)
  for (const [key,value] of Object.entries({window:browser,document:browser.document,navigator:browser.navigator,IS_REACT_ACT_ENVIRONMENT:true})) Object.defineProperty(globalThis,key,{configurable:true,value})
  const originalFetch=globalThis.fetch, originalTimeout=globalThis.setTimeout, originalClear=globalThis.clearTimeout
  const deadlines=new Map<number,()=>void>()
  let next=10_000
  globalThis.setTimeout=((callback: (...args: unknown[])=>void, delay?: number, ...args: unknown[])=>{
    if (delay === 12_000) {
      const id=next++
      deadlines.set(id,()=>{deadlines.delete(id);callback(...args)})
      return id
    }
    return originalTimeout(callback,delay,...args)
  }) as typeof setTimeout
  globalThis.clearTimeout=((id: ReturnType<typeof setTimeout>)=>{
    if (typeof id === "number" && deadlines.has(id)) deadlines.delete(id)
    else originalClear(id)
  }) as typeof clearTimeout
  const {createRoot}=await import("react-dom/client")
  const container=browser.document.createElement("div");browser.document.body.append(container)
  const root=createRoot(container as unknown as HTMLElement)
  try { await run(container as unknown as HTMLElement,root,deadlines) }
  finally {
    await act(async()=>root.unmount())
    globalThis.fetch=originalFetch;globalThis.setTimeout=originalTimeout;globalThis.clearTimeout=originalClear
    for (const [key,descriptor] of descriptors) { if(descriptor) Object.defineProperty(globalThis,key,descriptor);else Reflect.deleteProperty(globalThis,key) }
    browser.close()
  }
}
const phone={id:"phone-1",device_name:"Test phone",platform:"ios",paired_at:"2026-09-28"}
function button(container: HTMLElement, text: string) {
  const found=[...container.querySelectorAll("button")].find((element)=>element.textContent?.trim() === text)
  assert.ok(found,`missing ${text} button`)
  return found
}

test("paired-device timeout ends loading, retries, and ignores late replies under StrictMode",async()=>environment(async(container,root,deadlines)=>{
  const requests:{signal:AbortSignal; resolve:(response:Response)=>void}[]=[]
  globalThis.fetch=async(url,init)=>{
    assert.equal(String(url),"/api/notifications/paired-devices")
    assert.equal(init?.method,undefined,"test must only make read requests")
    return new Promise<Response>((resolve)=>requests.push({signal:init!.signal as AbortSignal,resolve}))
  }
  await act(async()=>root.render(createElement(StrictMode,null,createElement(MobileNotificationsSection))))
  assert.equal(requests.length,2)
  assert.equal(requests[0].signal.aborted,true)
  assert.match(container.textContent ?? "",/Loading paired devices/)
  assert.equal(deadlines.size,1)
  await act(async()=>{[...deadlines.values()][0]()})
  assert.equal(requests[1].signal.aborted,true)
  assert.match(container.textContent ?? "",/timed out/)
  assert.equal(container.querySelector('[role="status"]'),null)
  assert.doesNotMatch(container.textContent ?? "",/No mobile devices paired/)
  await act(async()=>button(container,"Retry").click())
  assert.equal(requests.length,3)
  assert.match(container.textContent ?? "",/Loading paired devices/)
  await act(async()=>requests[2].resolve(Response.json({devices:[phone]})))
  assert.match(container.textContent ?? "",/Test phone/)
  assert.equal(deadlines.size,0)
  await act(async()=>{requests[0].resolve(Response.json({devices:[]}));requests[1].resolve(Response.json({devices:[]}))})
  assert.match(container.textContent ?? "",/Test phone/)
  assert.doesNotMatch(container.textContent ?? "",/No mobile devices paired/)
  await act(async()=>button(container,"Refresh").click())
  await act(async()=>root.render(null))
  assert.equal(requests[3].signal.aborted,true)
  assert.equal(deadlines.size,0)
}))

test("paired-device failures and malformed responses preserve last good devices",async()=>environment(async(container,root)=>{
  let response=()=>Response.json({devices:[phone]})
  globalThis.fetch=async()=>response()
  await act(async()=>root.render(createElement(MobileNotificationsSection)))
  assert.match(container.textContent ?? "",/Test phone/)
  response=()=>Response.json({success:true})
  await act(async()=>button(container,"Refresh").click())
  assert.match(container.textContent ?? "",/invalid paired-device list/)
  assert.match(container.textContent ?? "",/Test phone/)
  response=()=>new Response("<html/>",{headers:{"Content-Type":"text/html"}})
  await act(async()=>button(container,"Retry").click())
  assert.match(container.textContent ?? "",/web page instead/)
  assert.match(container.textContent ?? "",/Test phone/)
  response=()=>Response.json({error:"offline"},{status:502})
  await act(async()=>button(container,"Retry").click())
  assert.match(container.textContent ?? "",/Could not load paired devices/)
  assert.doesNotMatch(container.textContent ?? "",/No mobile devices paired/)
  response=()=>Response.json({devices:[],error:"unavailable"})
  await act(async()=>button(container,"Retry").click())
  assert.match(container.textContent ?? "",/Test phone/)
  assert.doesNotMatch(container.textContent ?? "",/No mobile devices paired/)
  response=()=>Response.json({devices:[]})
  await act(async()=>button(container,"Retry").click())
  assert.match(container.textContent ?? "",/No mobile devices paired yet/)
  assert.doesNotMatch(container.textContent ?? "",/Test phone|Could not load/)
}))

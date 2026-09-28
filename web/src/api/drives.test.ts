import assert from "node:assert/strict"
import test from "node:test"
import { bulkDeleteDrives, fetchDriveDetail, fetchDrivePage, fetchVisibleRoutePreviews, invalidateDriveApiCache, setDriveTags } from "./drives.ts"
import { computeFilteredStats } from "../lib/drive-stats.ts"
import type { DriveSummary } from "../types/drives.ts"

function drive(id: number, tags = ["Work"]): DriveSummary {
  return { id, startTime:`2026-09-28T${String(id).padStart(2,"0")}:00:00.000`,endTime:`2026-09-28T${String(id).padStart(2,"0")}:05:00.000`,distanceMi:10,distanceKm:16,durationMs:300000,
    fsdEngagedMs:60000,fsdDistanceMi:5,fsdDistanceKm:8,fsdPercent:50,fsdDisengagements:1,autosteerEngagedMs:0,autosteerDistanceKm:0,taccEngagedMs:0,taccDistanceKm:0,tags } as DriveSummary
}
function json(value: unknown, status = 200) { return new Response(JSON.stringify(value),{status,headers:{"Content-Type":"application/json"}}) }
async function withFetch(run: () => Promise<void>) {
  const original=globalThis.fetch
  invalidateDriveApiCache()
  try { await run() } finally { globalThis.fetch=original; invalidateDriveApiCache() }
}

test("legacy arrays are filtered before paging, with full matching stats and cached navigation", async () => withFetch(async()=> {
  let requests=0
  const all=Array.from({length:15},(_,id)=>drive(id,id<12?["Work"]:["Home"]))
  globalThis.fetch=async()=>{requests++;return json(all)}
  const first=await fetchDrivePage("limit=10&page=1&tag=Work&sort=desc")
  assert.equal(first.drives.length,10)
  assert.equal(first.drives[0].id,11)
  assert.equal(first.total,12)
  assert.equal(first.stats.totalDistanceKm,192)
  assert.deepEqual(first.tags,["Home","Work"])
  const second=await fetchDrivePage("limit=10&page=2&tag=Work&sort=desc")
  assert.deepEqual(second.drives.map((row)=>row.id),[1,0])
  assert.equal(second.revision,first.revision)
  assert.equal(requests,1)
  const dates=await fetchDrivePage("limit=10&from=2026-09-28T03%3A00%3A00&to=2026-09-28T05%3A00%3A00&sort=asc")
  assert.deepEqual(dates.drives.map((row)=>row.id),[3,4])
}))

test("modern pages are validated; HTML fallback is read-only and malformed data stays an error", async()=>withFetch(async()=>{
  const calls:string[]=[]
  globalThis.fetch=async(url)=>{
    calls.push(String(url))
    return String(url).includes("?") ? new Response("<!doctype html><html/>",{headers:{"Content-Type":"text/html"}}) : json([drive(1)])
  }
  assert.equal((await fetchDrivePage("limit=10")).total,1)
  assert.deepEqual(calls,["/api/drives?limit=10","/api/drives"])
  invalidateDriveApiCache()
  globalThis.fetch=async()=>json({success:true})
  await assert.rejects(fetchDrivePage("limit=10"),/invalid paginated response/)
  globalThis.fetch=async()=>json({revision:"r1",drives:[drive(1)],total:1,page:1,limit:10,tags:["Work"],stats:computeFilteredStats([drive(1)]),capabilities:{additiveTags:true}})
  assert.equal((await fetchDrivePage("limit=10")).drives[0].id,1)
}))

test("legacy details restore summary telemetry and verify identity on numeric GET fallback",async()=>withFetch(async()=>{
  const row={...drive(3),batteryPctStart:80,startLocation:"Example"}
  globalThis.fetch=async(url)=>String(url).includes("?") ? json([row]) : json({ ...drive(3),points:[[50,10,0,0]] })
  await fetchDrivePage("limit=10")
  const enriched=await fetchDriveDetail(row.startTime)
  assert.equal(enriched.batteryPctStart,80)
  assert.equal(enriched.startLocation,"Example")
  const calls:string[]=[]
  globalThis.fetch=async(url)=>{
    calls.push(String(url))
    if (String(url)==="/api/drives") return json([row])
    if (String(url)==="/api/drives/3") return json({...drive(3),points:[[50,10,0,0]]})
    return new Response("<html/>",{headers:{"Content-Type":"text/html"}})
  }
  assert.equal((await fetchDriveDetail(row.startTime)).startTime,row.startTime)
  assert.ok(calls.includes("/api/drives/3"))
  globalThis.fetch=async(url)=>String(url)==="/api/drives" ? json([row]) : String(url)==="/api/drives/3" ? json({...drive(4),points:[[50,10,0,0]]}) : json({},404)
  await assert.rejects(fetchDriveDetail(row.startTime),/drive changed while loading/)
}))

test("legacy bulk tagging preserves freshly read tags and never retries an uncertain write",async()=>withFetch(async()=>{
  const row=drive(3,["Existing"])
  globalThis.fetch=async()=>json([row])
  await fetchDrivePage("limit=10")
  const writes:{path:string;body:unknown}[]=[]
  globalThis.fetch=async(url,init)=>{
    if (!init?.method) return json([{...row,tags:["Existing","Added elsewhere"]}])
    writes.push({path:String(url),body:JSON.parse(String(init.body))})
    return json({success:true})
  }
  await setDriveTags(row.startTime,["Trip"],true)
  assert.deepEqual(writes,[{path:`/api/drives/${encodeURIComponent(row.startTime)}/tags`,body:{tags:["Existing","Added elsewhere","Trip"]}}])
  globalThis.fetch=async()=>new Response("<html/>",{headers:{"Content-Type":"text/html"}})
  await assert.rejects(bulkDeleteDrives([row.startTime]),/web page instead of API data/)
}))

test("legacy all-history previews are reduced to requested stable keys",async()=>withFetch(async()=>{
  const rows=[drive(1),drive(2)]
  globalThis.fetch=async()=>json(rows)
  await fetchDrivePage("limit=10")
  let requests=0
  globalThis.fetch=async()=>{requests++;return json(rows.map((row)=>({id:row.id,startTime:row.startTime,points:[[50,10],[50.1,10.1]]})))}
  assert.deepEqual((await fetchVisibleRoutePreviews([rows[1].startTime])).map((row)=>row.startTime),[rows[1].startTime])
  assert.equal((await fetchVisibleRoutePreviews([rows[0].startTime])).length,1)
  assert.equal(requests,1)
}))

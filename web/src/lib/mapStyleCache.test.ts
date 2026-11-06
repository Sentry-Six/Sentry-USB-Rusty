import test from "node:test"
import assert from "node:assert/strict"
import { cachedStyle, prefetchStyles } from "./mapStyleCache"

const ok = (url: string) => Promise.resolve({ ok: true, json: () => Promise.resolve({ url }) } as unknown as Response)
const failing = () => Promise.resolve({ ok: false, status: 503 } as Response)

test("style switches use prefetched style JSON once it arrives", async () => {
  const urls = ["https://tiles.example/a.json", "https://tiles.example/b.json"]
  assert.equal(cachedStyle(urls[0]), urls[0])
  const requested: string[] = []
  prefetchStyles(urls, (input) => {
    const url = String(input)
    requested.push(url)
    return url === urls[0] ? ok(url) : failing()
  })
  assert.deepEqual(requested, urls)
  // Already fetched or in flight: no second request.
  prefetchStyles([urls[0]], () => { throw new Error("refetched") })
  await new Promise(resolve => setTimeout(resolve))
  assert.deepEqual(cachedStyle(urls[0]), { url: urls[0] })
  // A failed fetch leaves the URL for MapLibre and is retried next time.
  assert.equal(cachedStyle(urls[1]), urls[1])
  prefetchStyles(urls, (input) => { requested.push(String(input)); return ok(String(input)) })
  assert.deepEqual(requested, [...urls, urls[1]])
})

import assert from 'node:assert/strict'
import test from 'node:test'
import { createLivenessProbe } from './liveness.ts'

test('liveness stays cheap on supported servers and remembers an older server', async () => {
  const original = globalThis.fetch
  const urls: string[] = []
  let oldServer = false
  globalThis.fetch = async url => {
    urls.push(String(url))
    return oldServer && url === '/api/health' ? new Response('', { status: 404 }) : Response.json({ ok: true })
  }
  try {
    const modern = createLivenessProbe()
    await modern(); await modern()
    assert.deepEqual(urls.splice(0), ['/api/health', '/api/health'])
    oldServer = true
    const legacy = createLivenessProbe()
    await legacy(); await legacy()
    assert.deepEqual(urls.splice(0), ['/api/health', '/api/status', '/api/status'])
    globalThis.fetch = async url => { urls.push(String(url)); return new Response('', { status: 503 }) }
    await createLivenessProbe()()
    assert.deepEqual(urls, ['/api/health'], 'an actual failure must not trigger a heavyweight fallback')
  } finally { globalThis.fetch = original }
})

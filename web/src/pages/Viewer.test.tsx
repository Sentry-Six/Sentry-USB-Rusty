import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, StrictMode } from 'react'
import { Window } from 'happy-dom'
import Viewer from './Viewer.tsx'

async function mount(run: (container: HTMLElement) => Promise<void>) {
  const win = new Window({ url: 'http://localhost/viewer', width: 1280, height: 900 })
  const values = { window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true,
    requestAnimationFrame: win.requestAnimationFrame.bind(win), cancelAnimationFrame: win.cancelAnimationFrame.bind(win) }
  const originals = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div'); win.document.body.append(container)
  const root = createRoot(container)
  try { await act(async () => root.render(createElement(StrictMode, {}, createElement(Viewer)))); await run(container as unknown as HTMLElement) }
  finally { await act(async () => root.unmount()); for (const [key, descriptor] of originals) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }; win.close() }
}
const clip = (date: string, count: number) => ({ date, path: `/mutable/TeslaCam/RecentClips/${date}`, files: Array.from({ length: count }, (_, i) => `${date}_${String(i).padStart(2, '0')}-front.mp4`) })

test('Viewer switches clips with matching segment count and clears selection on category change', async () => {
  const oldFetch = globalThis.fetch
  globalThis.fetch = async input => {
    const url = String(input)
    if (url.startsWith('/api/clips?')) return Response.json([{ name: new URL(url, 'http://localhost').searchParams.get('category'), clips: [clip('first', 2), clip('second', 1)] }])
    if (url.includes('telemetry')) return Response.json({ frames: [] })
    return Response.json({})
  }
  try { await mount(async container => {
    const button = (text: string) => [...container.querySelectorAll('button')].find(b => b.textContent?.includes(text))!
    await act(async () => button('first').click())
    assert.ok(container.textContent?.includes('2:00'))
    await act(async () => button('second').click())
    assert.ok(container.textContent?.includes('1:00'))
    assert.ok(!container.textContent?.includes('2:00'))
    await act(async () => button('Saved').click())
    assert.ok(!container.textContent?.includes('1:00'))
  }) } finally { globalThis.fetch = oldFetch }
})

test('Viewer ignores cancelled category reads including StrictMode replay', async () => {
  const oldFetch = globalThis.fetch
  const recent: ((response: Response) => void)[] = []
  globalThis.fetch = async input => {
    const url = String(input)
    if (url.includes('category=RecentClips')) return new Promise(resolve => recent.push(resolve))
    if (url.includes('category=SavedClips')) return Response.json([{ name: 'SavedClips', clips: [clip('saved-current', 1)] }])
    return Response.json({})
  }
  try { await mount(async container => {
    await act(async () => [...container.querySelectorAll('button')].find(b => b.textContent === 'Saved')!.click())
    assert.ok(container.textContent?.includes('saved-current'))
    await act(async () => { for (const resolve of recent) resolve(Response.json([{ name: 'SavedClips', clips: [clip('stale', 1)] }])) })
    assert.ok(container.textContent?.includes('saved-current'))
    assert.ok(!container.textContent?.includes('stale'))
  }) } finally { globalThis.fetch = oldFetch }
})

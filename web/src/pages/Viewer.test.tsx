import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, StrictMode } from 'react'
import { Window } from 'happy-dom'
import Viewer from './Viewer.tsx'

async function mount(run: (container: HTMLElement) => Promise<void>) {
  const win = new Window({ url: 'http://localhost/viewer', width: 1280, height: 900 })
  const values = { window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true,
    HTMLInputElement: win.HTMLInputElement, HTMLTextAreaElement: win.HTMLTextAreaElement,
    requestAnimationFrame: win.requestAnimationFrame.bind(win), cancelAnimationFrame: win.cancelAnimationFrame.bind(win) }
  const originals = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const { createRoot } = await import('react-dom/client')
  // happy-dom never loads media. The player waits for its cameras before it reads telemetry;
  // here they count as loaded from the start.
  Object.defineProperty(win.HTMLMediaElement.prototype, 'readyState', { configurable: true, get: () => 4 })
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

test('Viewer shows the clock time of the moment on screen, anchored to each segment file', async () => {
  const oldFetch = globalThis.fetch
  const event = '2026-11-09_14-30-29'
  // The second file starts 61 seconds after the first, as the car often writes them.
  const files = ['2026-11-09_14-19-41-front.mp4', '2026-11-09_14-20-42-front.mp4']
  globalThis.fetch = async input => {
    const url = String(input)
    if (url.startsWith('/api/clips?')) return Response.json([{ name: 'RecentClips', clips: [{ date: event, path: `/TeslaCam/SentryClips/${event}`, files }] }])
    if (url.includes('telemetry')) return Response.json({ frames: [] })
    return Response.json({})
  }
  try { await mount(async container => {
    const clock = () => container.querySelector('time')?.getAttribute('datetime')
    const press = (key: string, shiftKey = false) => act(async () => {
      window.dispatchEvent(new window.KeyboardEvent('keydown', { key, shiftKey }))
    })
    assert.equal(clock(), undefined)
    await act(async () => [...container.querySelectorAll('button')].find(b => b.textContent?.includes('Nov 9'))!.click())
    assert.equal(clock(), '2026-11-09T14:19:41')
    await press('ArrowRight')
    assert.equal(clock(), '2026-11-09T14:19:46')
    // 5 + 15 × 4 = 65 seconds in. The first file is taken as 61 seconds long
    // (the gap to the next file's name), so that is 4 seconds into the second.
    for (let i = 0; i < 4; i++) await press('ArrowRight', true)
    assert.equal(clock(), '2026-11-09T14:20:46')
    // A skip straight after changing segment moves on from the new position.
    await press('ArrowRight')
    assert.equal(clock(), '2026-11-09T14:20:51')
    // Back 15 seconds lands 55 seconds into the first file again.
    await press('ArrowLeft', true)
    assert.equal(clock(), '2026-11-09T14:20:36')
  }) } finally { globalThis.fetch = oldFetch }
})

test('Viewer names the camera that triggered an event and jumps to the event', async () => {
  const oldFetch = globalThis.fetch
  const event = '2026-11-09_14-30-29'
  const files = ['2026-11-09_14-27-43', '2026-11-09_14-28-44', '2026-11-09_14-29-44'].flatMap(t => [`${t}-front.mp4`, `${t}-left_repeater.mp4`])
  globalThis.fetch = async input => {
    const url = String(input)
    if (url.startsWith('/api/clips?')) return Response.json([{ name: 'RecentClips', clips: [{ date: event, path: `/TeslaCam/SentryClips/${event}`, files,
      event: { timestamp: '2026-11-09T14:29:23', reason: 'sentry_aware_object_detection', camera: '5', city: 'Edmonton' } }] }])
    if (url.includes('telemetry')) return Response.json({ frames: [] })
    return Response.json({})
  }
  try { await mount(async container => {
    const clock = () => container.querySelector('time')?.getAttribute('datetime')
    await act(async () => [...container.querySelectorAll('button')].find(b => b.textContent?.includes('Nov 9'))!.click())
    // Tesla writes the camera as a number; 5 is the left repeater.
    assert.ok(container.textContent?.includes('Triggered: Left Repeater'))
    assert.ok(!container.textContent?.includes('Triggered: 5'))
    assert.ok(container.querySelector('[data-camera="left_repeater"]')?.textContent?.includes('⚡'))
    assert.ok(!container.querySelector('[data-camera="front"]')?.textContent?.includes('⚡'))
    // A Sentry clip with no telemetry is a parked car, and says so, as Sentry Studio does.
    assert.ok(container.querySelector('[aria-label="Telemetry"]')?.textContent?.includes('PARKED'))
    // Lengths come from the file names (61 s, then 60 s), not a flat minute each.
    assert.ok(container.textContent?.includes('0:00 / 3:01'))
    const jump = container.querySelector('button[aria-label="Jump to the event"]') as HTMLButtonElement
    assert.ok(jump.title.includes('Object Detected'))
    await act(async () => jump.click())
    assert.equal(clock(), '2026-11-09T14:29:23')
    // The E key does the same from anywhere in the clip.
    await act(async () => { window.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'ArrowLeft', shiftKey: true })) })
    assert.equal(clock(), '2026-11-09T14:29:08')
    await act(async () => { window.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'e' })) })
    assert.equal(clock(), '2026-11-09T14:29:23')
  }) } finally { globalThis.fetch = oldFetch }
})

test('Viewer plays every camera on a wide screen, and the cameras button switches to the front one', async () => {
  const oldFetch = globalThis.fetch
  const day = '2026-11-14'
  const cams = ['front', 'back', 'left_repeater', 'right_repeater', 'left_pillar', 'right_pillar']
  const files = ['2026-11-14_08-26-41', '2026-11-14_08-27-42'].flatMap(t => cams.map(c => `${t}-${c}.mp4`))
  globalThis.fetch = async input => {
    const url = String(input)
    if (url.startsWith('/api/clips?')) return Response.json([{ name: 'RecentClips', clips: [{ date: day, path: `/TeslaCam/RecentClips/${day}`, files }] }])
    if (url.includes('telemetry')) return Response.json({ frames: [] })
    return Response.json({})
  }
  try { await mount(async container => {
    const playing = () => [...container.querySelectorAll('[data-camera]')].filter(tile => tile.querySelector('video')).map(tile => tile.getAttribute('data-camera'))
    await act(async () => [...container.querySelectorAll('button')].find(b => b.textContent?.includes('Nov 14'))!.click())
    // Pillars on top, repeaters below, the front camera in the middle.
    assert.deepEqual([...container.querySelectorAll('[data-camera]')].map(t => t.getAttribute('data-camera')), ['left_pillar', 'front', 'right_pillar', 'left_repeater', 'back', 'right_repeater'])
    assert.equal(playing().length, 6)
    // Each camera keeps two players for the whole clip: one showing, one spare for the next minute.
    assert.equal(container.querySelectorAll('video').length, 12)
    assert.ok(!container.textContent?.includes('Tap to play'))
    await act(async () => (container.querySelector('button[aria-label="Play only the front camera"]') as HTMLButtonElement).click())
    assert.deepEqual(playing(), ['front'])
    // A camera that is off comes back with a tap.
    await act(async () => (container.querySelector('[data-camera="back"]') as HTMLElement).click())
    assert.deepEqual(playing(), ['front', 'back'])
    // Tapping a playing camera enlarges it; Escape brings the grid back.
    await act(async () => (container.querySelector('[data-camera="back"]') as HTMLElement).click())
    assert.deepEqual([...container.querySelectorAll('[data-camera]')].map(t => t.getAttribute('data-camera')), ['back'])
    await act(async () => { window.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Escape' })) })
    assert.equal(container.querySelectorAll('[data-camera]').length, 6)
  }) } finally { globalThis.fetch = oldFetch }
})

test('Viewer warns while the Pi is archiving, and the notice can be hidden', async () => {
  const oldFetch = globalThis.fetch
  globalThis.fetch = async input => {
    const url = String(input)
    if (url.startsWith('/api/clips?')) return Response.json([{ name: 'RecentClips', clips: [clip('first', 1)] }])
    if (url.includes('/drives/status')) return Response.json({ running: false, archiving: true, phase: 'archiving', current: 1800, total: 3660, eta_seconds: 4275 })
    if (url.includes('telemetry')) return Response.json({ frames: [] })
    return Response.json({})
  }
  try { await mount(async container => {
    const notice = () => [...container.querySelectorAll('[role="status"]')].find(el => el.textContent?.includes('archiving'))
    // Shown before a clip is picked...
    assert.ok(notice()?.textContent?.includes('Your Pi is archiving clips right now, so video may load slowly or stutter.'))
    assert.ok(notice()?.textContent?.includes('It has about 1 hour 10 minutes left.'))
    // ...and while one is open.
    await act(async () => [...container.querySelectorAll('button')].find(b => b.textContent?.includes('first'))!.click())
    assert.ok(notice())
    await act(async () => (container.querySelector('button[aria-label="Hide this notice"]') as HTMLButtonElement).click())
    assert.equal(notice(), undefined)
  }) } finally { globalThis.fetch = oldFetch }
})

test('Viewer shows no archiving notice when the Pi is idle', async () => {
  const oldFetch = globalThis.fetch
  globalThis.fetch = async input => {
    const url = String(input)
    if (url.startsWith('/api/clips?')) return Response.json([{ name: 'RecentClips', clips: [clip('first', 1)] }])
    if (url.includes('/drives/status')) return Response.json({ running: true, archiving: false, phase: 'processing' })
    return Response.json({})
  }
  try { await mount(async container => {
    assert.ok(!container.textContent?.includes('archiving'))
  }) } finally { globalThis.fetch = oldFetch }
})

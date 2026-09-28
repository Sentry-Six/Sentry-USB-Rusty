import assert from "node:assert/strict"
import test from "node:test"
import { act, createElement } from "react"
import { Window } from "happy-dom"
import Files from "./Files.tsx"

async function mount(run: (container: HTMLElement, win: Window) => Promise<void>) {
  const win = new Window({ url: "http://localhost/files" })
  const values = {
    window: win,
    document: win.document,
    navigator: win.navigator,
    IS_REACT_ACT_ENVIRONMENT: true,
    confirm: () => true,
  }
  const previous = Object.keys(values).map(
    (key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const,
  )
  for (const [key, value] of Object.entries(values))
    Object.defineProperty(globalThis, key, { configurable: true, value })
  const { createRoot } = await import("react-dom/client")
  const container = win.document.createElement("div")
  win.document.body.append(container)
  const root = createRoot(container)
  try {
    await act(async () => root.render(createElement(Files)))
    await act(async () => new Promise((resolve) => setTimeout(resolve, 20)))
    await run(container as unknown as HTMLElement, win)
  } finally {
    await act(async () => root.unmount())
    for (const [key, descriptor] of previous) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor)
      else Reflect.deleteProperty(globalThis, key)
    }
    win.close()
  }
}
const entry = (name: string, path: string, is_dir = false) => ({
  name,
  path,
  is_dir,
  size: 100,
  mod_time: "2026-09-27T12:00:00Z",
})

test("Files starts at TeslaCam and ignores a late response after location changes", async () => {
  const previous = globalThis.fetch
  let oldResolve: ((response: Response) => void) | undefined
  globalThis.fetch = async (input) => {
    const url = String(input)
    if (url === "/api/config") return Response.json({ has_cam: "yes" })
    const path = new URL(url, "http://localhost").searchParams.get("path")
    if (path === "/mutable/TeslaCam/older")
      return new Promise((resolve) => {
        oldResolve = resolve
      })
    return Response.json({
      entries:
        path === "/mutable/TeslaCam"
          ? [entry("older", "/mutable/TeslaCam/older", true)]
          : [entry("current.wav", "/mutable/LockChime/current.wav")],
    })
  }
  try {
    await mount(async (container) => {
      const location = container.querySelector<HTMLButtonElement>('[aria-label="File location"]')!
      assert.ok(location.textContent?.includes("TeslaCam"))
      await act(async () =>
        [...container.querySelectorAll("button")].find((b) => b.textContent === "older")!.click(),
      )
      await act(async () => new Promise((resolve) => setTimeout(resolve, 20)))
      assert.ok(oldResolve)
      await act(async () => location.click())
      await act(async () => container.ownerDocument.querySelector<HTMLElement>('[role="option"][data-value="Lock Sounds"]')!.click())
      await act(async () => new Promise((resolve) => setTimeout(resolve, 20)))
      await act(async () =>
        oldResolve!(
          Response.json({ entries: [entry("stale.mp4", "/mutable/TeslaCam/older/stale.mp4")] }),
        ),
      )
      assert.ok(container.textContent!.includes("current.wav"))
      assert.ok(!container.textContent!.includes("stale.mp4"))
    })
  } finally {
    globalThis.fetch = previous
  }
})

test("failed deletion keeps the item selected and explains the failure", async () => {
  const previous = globalThis.fetch
  globalThis.fetch = async (input, init) => {
    if (String(input) === "/api/config") return Response.json({ has_cam: "yes" })
    if (init?.method === "DELETE")
      return Response.json({ error: "Read-only filesystem" }, { status: 403 })
    return Response.json({ entries: [entry("keep.wav", "/mutable/LockChime/keep.wav")] })
  }
  try {
    await mount(async (container) => {
      const selected = container.querySelector<HTMLInputElement>('[aria-label="Select keep.wav"]')!
      await act(async () => selected.click())
      await act(async () =>
        [...container.querySelectorAll("button")].find((b) => b.textContent === "Delete")!.click(),
      )
      assert.ok(
        container.querySelector('[role="alert"]')?.textContent?.includes("Read-only filesystem"),
      )
      assert.equal(
        container.querySelector<HTMLInputElement>('[aria-label="Select keep.wav"]')!.checked,
        true,
      )
    })
  } finally {
    globalThis.fetch = previous
  }
})

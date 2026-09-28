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

for (const storage of ["available", "unavailable", "folder-present"] as const) {
  test(`missing Wraps is empty only when its parent confirms absence: ${storage}`, async () => {
    const previous = globalThis.fetch
    const requests: string[] = []
    globalThis.fetch = async (input, init) => {
      assert.ok(!init?.method || init.method === "GET")
      if (String(input) === "/api/config") return Response.json({ has_cam: "yes" })
      const path = new URL(String(input), "http://localhost").searchParams.get("path")!
      requests.push(path)
      if (path === "/mutable/Wraps" || (path === "/mutable" && storage === "unavailable"))
        return Response.json({ error: "Folder unavailable. Check that its drive is mounted." }, { status: 404 })
      return Response.json({ entries: path === "/mutable" && storage === "folder-present" ? [entry("Wraps", "/mutable/Wraps", true)] : [] })
    }
    try {
      await mount(async container => {
        const location = container.querySelector<HTMLButtonElement>('[aria-label="File location"]')!
        if (!location.textContent?.includes("Wraps")) {
          await act(async () => location.click())
          await act(async () => container.ownerDocument.querySelector<HTMLElement>('[role="option"][data-value="Wraps"]')!.click())
          await act(async () => new Promise(resolve => setTimeout(resolve, 20)))
        }
        assert.ok(requests.includes("/mutable"))
        assert.equal(!!container.querySelector('[role="alert"]'), storage !== "available")
        assert.equal(container.textContent!.includes("No wraps yet."), storage === "available")
      })
    } finally {
      globalThis.fetch = previous
    }
  })
}

test("missing Wraps subfolders retain the folder error", async () => {
  const previous = globalThis.fetch
  const requests: string[] = []
  globalThis.fetch = async input => {
    if (String(input) === "/api/config") return Response.json({ has_cam: "yes" })
    const path = new URL(String(input), "http://localhost").searchParams.get("path")!
    requests.push(path)
    if (path === "/mutable/Wraps/gone")
      return Response.json({ error: "Folder unavailable. Check that its drive is mounted." }, { status: 404 })
    return Response.json({ entries: [entry("gone", "/mutable/Wraps/gone", true)] })
  }
  try {
    await mount(async container => {
      await act(async () => [...container.querySelectorAll("button")].find(button => button.textContent === "gone")!.click())
      await act(async () => new Promise(resolve => setTimeout(resolve, 20)))
      assert.ok(container.querySelector('[role="alert"]')?.textContent?.includes("Folder unavailable"))
      assert.ok(!requests.includes("/mutable"))
    })
  } finally {
    globalThis.fetch = previous
  }
})

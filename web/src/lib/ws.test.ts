import assert from "node:assert/strict"
import test from "node:test"

test("subscriptions share a connecting socket and intentional disconnect never reconnects", async () => {
  const originals = ["window", "WebSocket", "setTimeout", "clearTimeout"].map(
    (key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const,
  )
  const sockets: Socket[] = []
  let scheduled = 0
  class Socket {
    static OPEN = 1
    static CONNECTING = 0
    readyState = 0
    onopen: (() => void) | null = null
    onmessage: ((event: { data: string }) => void) | null = null
    onclose: (() => void) | null = null
    onerror: (() => void) | null = null
    constructor(_url: string) {
      sockets.push(this)
    }
    close() {
      this.readyState = 3
      this.onclose?.()
    }
    send() {}
  }
  const values = {
    window: { location: { protocol: "http:", host: "localhost" } },
    WebSocket: Socket,
    setTimeout: () => {
      scheduled++
      return 1
    },
    clearTimeout: () => {},
  }
  for (const [key, value] of Object.entries(values))
    Object.defineProperty(globalThis, key, { configurable: true, value })
  try {
    const { WebSocketClient } = await import("./ws.ts")
    const client = new WebSocketClient()
    client.subscribe("status", () => {})
    client.subscribe("progress", () => {})
    client.connect()
    assert.equal(sockets.length, 1)
    client.disconnect()
    assert.equal(scheduled, 0)
    client.connect()
    assert.equal(sockets.length, 2)
    sockets[1].close()
    assert.equal(scheduled, 1)
    client.disconnect()
  } finally {
    for (const [key, descriptor] of originals) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor)
      else Reflect.deleteProperty(globalThis, key)
    }
  }
})

type MessageHandler = (data: unknown) => void
type StatusListener = (connected: boolean) => void

// Reconnect backoff bounds. First retry stays at 3s (snappy recovery
// from a brief blip), then doubles up to a 30s ceiling so a Pi that's
// mid-reboot (OTA update) or an endpoint that keeps refusing isn't hit
// ~20×/min. Reset to the floor on a successful open.
const INITIAL_RECONNECT_MS = 3000
const MAX_RECONNECT_MS = 30000

export class WebSocketClient {
  private ws: WebSocket | null = null
  private handlers: Map<string, Set<MessageHandler>> = new Map()
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null
  private reconnectDelay = INITIAL_RECONNECT_MS
  private pingTimer: ReturnType<typeof setInterval> | null = null
  private url: string
  private _connected = false
  private statusListeners: Set<StatusListener> = new Set()

  constructor() {
    const protocol = window.location.protocol === "https:" ? "wss:" : "ws:"
    this.url = `${protocol}//${window.location.host}/api/ws`
  }

  get isConnected() {
    return this._connected
  }

  private setConnected(value: boolean) {
    if (this._connected === value) return
    this._connected = value
    this.statusListeners.forEach((cb) => cb(value))
  }

  onStatusChange(cb: StatusListener): () => void {
    this.statusListeners.add(cb)
    return () => {
      this.statusListeners.delete(cb)
    }
  }

  connect() {
    if (
      this.ws &&
      (this.ws.readyState === WebSocket.OPEN || this.ws.readyState === WebSocket.CONNECTING)
    )
      return

    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }
    const socket = new WebSocket(this.url)
    this.ws = socket

    socket.onopen = () => {
      if (this.ws !== socket) return
      this.setConnected(true)
      this.reconnectDelay = INITIAL_RECONNECT_MS
      this.startPing()
    }

    socket.onmessage = (event) => {
      if (this.ws !== socket) return
      try {
        const msg = JSON.parse(event.data) as { type: string; data: unknown }
        const handlers = this.handlers.get(msg.type)
        if (handlers) {
          handlers.forEach((handler) => handler(msg.data))
        }
      } catch {
        // ignore malformed messages
      }
    }

    socket.onclose = () => {
      if (this.ws !== socket) return
      this.ws = null
      this.stopPing()
      this.setConnected(false)
      this.scheduleReconnect()
    }

    socket.onerror = () => {
      socket.close()
    }
  }

  private startPing() {
    this.stopPing()
    this.pingTimer = setInterval(() => {
      if (this.ws?.readyState === WebSocket.OPEN) {
        this.ws.send(JSON.stringify({ type: "ping" }))
      }
    }, 25000)
  }

  private stopPing() {
    if (this.pingTimer) {
      clearInterval(this.pingTimer)
      this.pingTimer = null
    }
  }

  private scheduleReconnect() {
    if (this.reconnectTimer) return
    const delay = this.reconnectDelay
    this.reconnectDelay = Math.min(this.reconnectDelay * 2, MAX_RECONNECT_MS)
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null
      this.connect()
    }, delay)
  }

  subscribe(type: string, handler: MessageHandler): () => void {
    if (!this.handlers.has(type)) {
      this.handlers.set(type, new Set())
    }
    this.handlers.get(type)!.add(handler)

    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      this.connect()
    }

    return () => {
      this.handlers.get(type)?.delete(handler)
    }
  }

  reconnect() {
    this.disconnect()
    this.connect()
  }

  disconnect() {
    this.stopPing()
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }
    // A manual reconnect should start from the fast floor, not wherever
    // the backoff had climbed to.
    this.reconnectDelay = INITIAL_RECONNECT_MS
    const socket = this.ws
    this.ws = null
    if (socket) {
      socket.onopen = socket.onmessage = socket.onclose = socket.onerror = null
      socket.close()
    }
    this.setConnected(false)
  }
}

export const wsClient = new WebSocketClient()

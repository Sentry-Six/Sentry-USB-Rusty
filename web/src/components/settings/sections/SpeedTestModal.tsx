import { useEffect, useRef, useState } from "react"
import { ProgressActivityIcon, SpeedIcon } from "@/components/icons"
import { Modal } from "@/components/ui/Modal"

async function measureOnce(signal: AbortSignal, onProgress: (mbps: string) => void) {
  const response = await fetch("/api/system/speedtest", { signal })
  if (signal.aborted) { await response.body?.cancel(); return }
  if (!response.ok || !response.body) throw new Error("Speed test failed")
  const reader = response.body.getReader()
  const cancel = () => { void reader.cancel().catch(() => {}) }
  signal.addEventListener("abort", cancel, { once: true })
  const started = Date.now()
  let bytes = 0
  let lastUpdate = started
  try {
    while (!signal.aborted) {
      const { done, value } = await reader.read()
      if (done || signal.aborted) break
      bytes += value.length
      const now = Date.now()
      if (now - lastUpdate >= 250) {
        onProgress(((bytes * 8) / ((now - started) / 1000) / 1_000_000).toFixed(1))
        lastUpdate = now
      }
    }
    const elapsed = (Date.now() - started) / 1000
    if (!signal.aborted && elapsed > 0 && bytes > 0) onProgress(((bytes * 8) / elapsed / 1_000_000).toFixed(1))
  } finally {
    signal.removeEventListener("abort", cancel)
    reader.releaseLock()
  }
}

export function SpeedTestModal({ onClose }: { onClose: () => void }) {
  const [running, setRunning] = useState(true)
  const [runId, setRunId] = useState(0)
  const [mbps, setMbps] = useState<string | null>(null)
  const [error, setError] = useState(false)
  const controllerRef = useRef<AbortController | null>(null)

  useEffect(() => {
    const controller = new AbortController()
    controllerRef.current = controller
    async function measure() {
      while (!controller.signal.aborted) {
        await measureOnce(controller.signal, value => {
          if (!controller.signal.aborted) setMbps(value)
        })
      }
    }
    void measure()
      .catch(() => { if (!controller.signal.aborted) setError(true) })
      .finally(() => { if (!controller.signal.aborted) setRunning(false) })
    return () => controller.abort()
  }, [runId])

  function startTest() {
    setRunning(true)
    setMbps(null)
    setError(false)
    setRunId(value => value + 1)
  }
  function stopTest() {
    controllerRef.current?.abort()
    setRunning(false)
  }

  return (
    <Modal
      title={
        <span className="flex items-center gap-2">
          <SpeedIcon className="h-4 w-4 text-blue-400" />
          <span>Speed Test</span>
        </span>
      }
      onClose={() => {
        stopTest()
        onClose()
      }}
      size="sm"
      footer={
        <div className="flex justify-end">
          <button
            onClick={running ? stopTest : startTest}
            className="rounded-lg bg-blue-500/15 px-3 py-1.5 text-xs font-medium text-blue-400 hover:bg-blue-500/25"
          >
            {running ? "Stop" : "Run again"}
          </button>
        </div>
      }
    >
      <div className="flex flex-col items-center justify-center gap-3 py-6">
        {running && !mbps ? (
          <>
            <ProgressActivityIcon className="h-8 w-8 animate-spin text-blue-400" />
            <p className="text-xs text-slate-500">Measuring throughput…</p>
          </>
        ) : mbps ? (
          <>
            <p className="text-4xl font-bold text-blue-400">
              {mbps} <span className="text-base font-normal text-slate-500">Mbps</span>
            </p>
            <p className="text-xs text-slate-500">
              {running ? "Continuously measuring…" : "Test complete"}
            </p>
          </>
        ) : error ? (
          <p className="text-sm text-red-400">Speed test failed. Try again?</p>
        ) : (
          <p className="text-xs text-slate-500">Test stopped</p>
        )}
      </div>
    </Modal>
  )
}

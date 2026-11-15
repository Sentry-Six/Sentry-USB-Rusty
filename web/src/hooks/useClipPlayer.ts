import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { estimateSegmentSeconds } from "@/lib/clipTime"
import { createPlaybackClock } from "@/lib/playbackClock"
import { needsFewerCameras, recentStalls, type Stall } from "@/lib/streamHealth"

/** One minute of a clip: its start time and each camera's file. */
export interface ClipSet {
  timestamp: string
  cameras: Record<string, string>
}

/**
 * Each camera has two video elements, a pair of buffers. One shows the
 * current minute; a few seconds before it ends, the other loads the next
 * minute, and the two swap. The picture changes minute without a gap, and no
 * video element is ever torn down and rebuilt mid-clip.
 */
export type Slot = 0 | 1
const other = (slot: Slot): Slot => (slot === 0 ? 1 : 0)

export type VideoEvent = "loadstart" | "loadedmetadata" | "canplay" | "playing" | "waiting" | "seeked" | "ended" | "error"

// How long before a minute ends its successor starts loading.
const PRELOAD_SECONDS = 8
// Cameras further than this from the lead camera are pulled back in line.
const DRIFT_SECONDS = 0.2
// How often the clock is told the time; the picture itself is the browser's.
const CLOCK_INTERVAL_MS = 66
// A seek always waits for data; that wait says nothing about the connection.
const SEEK_GRACE_MS = 3000
// The browser keeps only six connections open to the Pi, and each camera
// needs one to load. So the next minute waits its turn until every camera on
// screen has its picture, unless the minute is this close to ending.
const PRELOAD_REGARDLESS_SECONDS = 3
// A camera with no picture for this long, while the lead camera plays on, is
// started again rather than left on its loading mark.
const STUCK_MS = 4000

// Whether a video has a file to play at all (a camera can be missing from a
// minute, or its file can fail to load).
const hasFile = (el: HTMLVideoElement) => el.hasAttribute("src") && !el.error
// Whether it has a picture and enough data to carry on from where it is.
const isReady = (el: HTMLVideoElement) => el.readyState >= 3 && !el.seeking

const noop = () => {}

interface Core {
  segment: number
  slots: [number | null, number | null]
  front: Slot
  playing: boolean
}

interface ClipPlayerOptions {
  clipSets: ClipSet[]
  /** The cameras laid out on screen, in order. */
  cameras: string[]
  /** Play every camera from the start, or only the lead camera. */
  startWithAll: boolean
  /** Called once as each next minute starts loading. */
  onNearEnd?: (nextSegment: number) => void
}

export function useClipPlayer({ clipSets, cameras, startWithAll, onNearEnd }: ClipPlayerOptions) {
  const [clock] = useState(createPlaybackClock)
  // The camera the others follow: the front one, or failing that the first
  // camera this clip has.
  const leadCamera = useMemo(() => {
    const present = (cam: string) => clipSets.some((set) => cam in set.cameras)
    return present("front") ? "front" : cameras.find(present) ?? "front"
  }, [clipSets, cameras])

  const [core, setCore] = useState<Core>({ segment: 0, slots: [0, null], front: 0, playing: false })
  const [speed, setSpeed] = useState(1)
  const [durations, setDurations] = useState<number[]>(() => estimateSegmentSeconds(clipSets.map((set) => set.timestamp)))
  const [active, setActive] = useState<ReadonlySet<string>>(() => new Set(startWithAll ? cameras : [leadCamera]))
  const [focused, setFocused] = useState<string | null>(null)
  const [waiting, setWaiting] = useState<ReadonlySet<string>>(() => new Set())
  // True once the viewer has dropped cameras because the connection lagged.
  const [reduced, setReduced] = useState(false)
  const [aspect, setAspect] = useState<number | null>(null)

  // The newest values, for event handlers and timers that outlive a render.
  const latest = useRef({ ...core, speed, durations, active, focused })
  useEffect(() => { latest.current = { ...core, speed, durations, active, focused } })
  const apply = useCallback((next: Partial<Core>) => {
    latest.current = { ...latest.current, ...next }
    setCore((prev) => ({ ...prev, ...next }))
  }, [])

  const videos = useRef(new Map<string, HTMLVideoElement>())
  const pendingSeek = useRef<number | null>(null)
  const stalls = useRef<Stall[]>([])
  const stallStart = useRef<number | null>(null)
  const ignoreStallsUntil = useRef(0)
  const keepAllCameras = useRef(false)
  const stuck = useRef(new Map<HTMLVideoElement, { since: number; tries: number }>())
  const probe = useRef<HTMLVideoElement | null>(null)
  const onNearEndRef = useRef(onNearEnd)
  useEffect(() => { onNearEndRef.current = onNearEnd }, [onNearEnd])

  const register = useCallback((cam: string, slot: Slot, el: HTMLVideoElement | null) => {
    const key = `${cam}:${slot}`
    if (el) videos.current.set(key, el)
    else videos.current.delete(key)
  }, [])
  const slotVideos = useCallback((slot: Slot) => {
    const found: HTMLVideoElement[] = []
    for (const cam of latest.current.active) {
      const el = videos.current.get(`${cam}:${slot}`)
      if (el) found.push(el)
    }
    return found
  }, [])
  const lead = useCallback(() => videos.current.get(`${leadCamera}:${latest.current.front}`), [leadCamera])

  const recordDuration = useCallback((index: number, seconds: number) => {
    if (!Number.isFinite(seconds) || seconds <= 0) return
    setDurations((prev) => {
      if (prev[index] === seconds) return prev
      const next = [...prev]
      next[index] = seconds
      return next
    })
  }, [])

  // The last minute's length cannot be guessed from file names, and it sets
  // the clip's total, so it alone is read ahead of time: once, and only
  // after the first picture is up, so it does not hold the cameras back.
  const closeProbe = useCallback(() => {
    const el = probe.current
    if (!el) return
    el.onloadedmetadata = null
    el.removeAttribute("src")
    el.load?.()
  }, [])
  const probeLastMinute = useCallback(() => {
    const last = clipSets.length - 1
    if (probe.current || last < 1) return
    const url = clipSets[last].cameras[leadCamera] ?? Object.values(clipSets[last].cameras)[0]
    if (!url) return
    const el = document.createElement("video")
    probe.current = el
    el.preload = "metadata"
    el.onloadedmetadata = () => { recordDuration(last, el.duration); closeProbe() }
    el.src = url
  }, [clipSets, closeProbe, leadCamera, recordDuration])
  useEffect(() => closeProbe, [closeProbe])

  /**
   * True while any camera on screen is still getting to where it was last
   * sent. Sending them somewhere else before then abandons loads the Pi has
   * already started, and those pile up in the browser's few connections.
   */
  const busy = useCallback(() => {
    if (pendingSeek.current !== null) return true
    return slotVideos(latest.current.front).some((el) => hasFile(el) && (el.seeking || el.readyState < 2))
  }, [slotVideos])

  const dropToLeadCamera = useCallback(() => {
    if (keepAllCameras.current || latest.current.active.size <= 1) return
    const keep = new Set([leadCamera])
    if (latest.current.focused) keep.add(latest.current.focused)
    setActive(keep)
    setReduced(true)
    stalls.current = []
  }, [leadCamera])

  /** Move to a moment inside the minute on screen. */
  const seekInSegment = useCallback((offset: number) => {
    ignoreStallsUntil.current = performance.now() + SEEK_GRACE_MS
    stuck.current.clear()
    // A minute that is still loading jumps to its pending position once it
    // is ready; keep that in step, or it would undo this newer seek.
    if (pendingSeek.current !== null) pendingSeek.current = offset
    for (const el of slotVideos(latest.current.front)) el.currentTime = offset
    clock.set(offset)
  }, [clock, slotVideos])

  /** Show another minute, starting `offset` seconds into it. */
  const goToSegment = useCallback((index: number, offset: number) => {
    const { slots, front, playing, speed: rate } = latest.current
    const back = other(front)
    clock.set(offset)
    stuck.current.clear()
    if (slots[back] === index) {
      // The spare buffers already hold that minute: swap them in.
      pendingSeek.current = null
      for (const el of slotVideos(back)) {
        if (offset > 0 || el.currentTime > 0) el.currentTime = offset
        el.playbackRate = rate
        if (playing) el.play().catch(noop)
      }
      for (const el of slotVideos(front)) el.pause()
      const freed: Core["slots"] = front === 0 ? [null, index] : [index, null]
      apply({ segment: index, front: back, slots: freed })
    } else {
      // Load it into the buffers on screen; they keep their place and state.
      ignoreStallsUntil.current = performance.now() + SEEK_GRACE_MS
      pendingSeek.current = offset
      const reloaded: Core["slots"] = front === 0 ? [index, null] : [null, index]
      apply({ segment: index, slots: reloaded })
    }
  }, [apply, clock, slotVideos])

  const globalTime = useCallback(() => {
    const { durations: lengths, segment } = latest.current
    let before = 0
    for (let i = 0; i < segment; i++) before += lengths[i] ?? 0
    return before + clock.get()
  }, [clock])

  /** Which minute a moment on the whole clip's timeline falls in, and how far into it. */
  const locate = useCallback((seconds: number) => {
    const lengths = latest.current.durations
    const total = lengths.reduce((sum, length) => sum + length, 0)
    let remaining = Math.max(0, Math.min(seconds, total))
    for (let i = 0; i < lengths.length - 1; i++) {
      if (remaining <= lengths[i] + 0.05) return { index: i, offset: Math.min(remaining, lengths[i]) }
      remaining -= lengths[i]
    }
    const last = Math.max(0, lengths.length - 1)
    return { index: last, offset: Math.min(remaining, lengths[last] ?? 0) }
  }, [])

  /** Move to a moment on the whole clip's timeline. */
  const seekToGlobal = useCallback((seconds: number) => {
    const { index, offset } = locate(seconds)
    if (index === latest.current.segment) seekInSegment(offset)
    else goToSegment(index, offset)
  }, [goToSegment, locate, seekInSegment])

  const skip = useCallback((seconds: number) => seekToGlobal(globalTime() + seconds), [globalTime, seekToGlobal])

  const togglePlay = useCallback(() => {
    const playing = !latest.current.playing
    for (const el of slotVideos(latest.current.front)) {
      if (playing) el.play().catch(noop)
      else el.pause()
    }
    apply({ playing })
  }, [apply, slotVideos])

  const onVideoEvent = useCallback((cam: string, slot: Slot, type: VideoEvent, el: HTMLVideoElement) => {
    const { front, playing, slots, speed: rate } = latest.current
    const onScreen = slot === front
    const isLead = cam === leadCamera

    if (type === "loadedmetadata") {
      el.playbackRate = rate
      const index = slots[slot]
      if (isLead && index !== null) {
        recordDuration(index, el.duration)
        if (el.videoWidth > 0 && el.videoHeight > 0) setAspect(el.videoWidth / el.videoHeight)
      }
      if (!onScreen) return
      if (pendingSeek.current !== null) {
        el.currentTime = pendingSeek.current
        if (isLead) pendingSeek.current = null
      } else {
        // A camera switched on mid-clip joins the others where they are.
        const leader = lead()
        if (leader && leader !== el && leader.currentTime > 0.1) el.currentTime = leader.currentTime
      }
      if (playing) el.play().catch(noop)
      return
    }

    if (!onScreen) return

    if (type === "ended") {
      if (!isLead) return
      const next = latest.current.segment + 1
      if (next < clipSets.length) goToSegment(next, 0)
      else apply({ playing: false })
      return
    }

    if (type === "loadstart" || type === "waiting") {
      setWaiting((prev) => (prev.has(cam) ? prev : new Set(prev).add(cam)))
      // The others hold still while the lead camera waits for data, so the
      // cameras stay in step instead of running ahead of it.
      if (type === "waiting" && isLead && playing && stallStart.current === null) {
        stallStart.current = performance.now()
        for (const follower of slotVideos(front)) if (follower !== el) follower.pause()
      }
      return
    }

    // canplay, playing, seeked, error: this camera is no longer waiting.
    setWaiting((prev) => {
      if (!prev.has(cam)) return prev
      const next = new Set(prev)
      next.delete(cam)
      return next
    })
    if (isLead && type === "canplay") probeLastMinute()
    if (isLead && stallStart.current !== null && (type === "playing" || type === "canplay")) {
      const now = performance.now()
      if (now > ignoreStallsUntil.current) stalls.current = [...recentStalls(stalls.current, now), { at: now, ms: now - stallStart.current }]
      stallStart.current = null
      if (playing) {
        for (const follower of slotVideos(front)) {
          if (follower === el) continue
          follower.currentTime = el.currentTime
          follower.play().catch(noop)
        }
      }
      if (needsFewerCameras(stalls.current, now)) dropToLeadCamera()
    }
  }, [apply, clipSets.length, dropToLeadCamera, goToSegment, lead, leadCamera, probeLastMinute, recordDuration, slotVideos])

  // While playing: tell the clock the time, and start the next minute loading
  // a few seconds before this one ends.
  useEffect(() => {
    if (!core.playing) return
    let frame = 0
    let lastTold = 0
    const tick = () => {
      const leader = lead()
      // While a minute is still loading, the lead video is not yet at the
      // requested position; keep the time the change itself set.
      if (leader && pendingSeek.current === null && leader.readyState >= 1) {
        const now = performance.now()
        if (now - lastTold >= CLOCK_INTERVAL_MS) {
          lastTold = now
          clock.set(leader.currentTime)
        }
        const { segment, slots, front } = latest.current
        const next = segment + 1
        const back = other(front)
        const left = leader.duration - leader.currentTime
        if (
          next < clipSets.length && slots[back] !== next && Number.isFinite(left) && left <= PRELOAD_SECONDS
          && (left <= PRELOAD_REGARDLESS_SECONDS || slotVideos(front).every((el) => !hasFile(el) || isReady(el)))
        ) {
          const prepared: Core["slots"] = front === 0 ? [slots[0], next] : [next, slots[1]]
          apply({ slots: prepared })
          onNearEndRef.current?.(next)
        }
      }
      frame = requestAnimationFrame(tick)
    }
    frame = requestAnimationFrame(tick)
    return () => cancelAnimationFrame(frame)
  }, [core.playing, apply, clipSets.length, clock, lead, slotVideos])

  // While playing: pull straying cameras back in line with the lead camera,
  // and give up on six cameras if the lead one has been stuck for too long.
  useEffect(() => {
    if (!core.playing) return
    const interval = setInterval(() => {
      const leader = lead()
      if (!leader) return
      if (stallStart.current !== null) {
        const now = performance.now()
        if (now > ignoreStallsUntil.current && needsFewerCameras(stalls.current, now, now - stallStart.current)) dropToLeadCamera()
        return
      }
      if (leader.paused || !isReady(leader)) return
      const now = performance.now()
      for (const follower of slotVideos(latest.current.front)) {
        if (follower === leader || !hasFile(follower)) continue
        if (!isReady(follower)) {
          // Its load or seek has gone nowhere: start it again. It rejoins
          // the others where they are once it has loaded. Each further try
          // waits twice as long, in case the connection is simply slow.
          const record = stuck.current.get(follower) ?? { since: now, tries: 0 }
          stuck.current.set(follower, record)
          if (now - record.since >= STUCK_MS * 2 ** record.tries) {
            stuck.current.set(follower, { since: now, tries: record.tries + 1 })
            follower.load()
          }
          continue
        }
        stuck.current.delete(follower)
        if (follower.paused) continue
        if (Math.abs(follower.currentTime - leader.currentTime) > DRIFT_SECONDS) follower.currentTime = leader.currentTime
      }
    }, 2000)
    return () => clearInterval(interval)
  }, [core.playing, dropToLeadCamera, lead, slotVideos])

  useEffect(() => {
    videos.current.forEach((el) => { el.playbackRate = speed })
  }, [speed])

  /** Start playing a camera that is switched off. */
  const activate = useCallback((cam: string) => {
    setActive((prev) => (prev.has(cam) ? prev : new Set(prev).add(cam)))
  }, [])

  /** Play every camera, or only the lead camera. A choice made here sticks. */
  const setAllCameras = useCallback((all: boolean) => {
    keepAllCameras.current = all
    stalls.current = []
    setReduced(false)
    if (all) setActive(new Set(cameras))
    else setActive(new Set(latest.current.focused ? [leadCamera, latest.current.focused] : [leadCamera]))
  }, [cameras, leadCamera])

  const total = useMemo(() => durations.reduce((sum, length) => sum + length, 0), [durations])
  const before = useMemo(
    () => durations.slice(0, core.segment).reduce((sum, length) => sum + length, 0),
    [durations, core.segment],
  )

  return {
    clock,
    leadCamera,
    segment: core.segment,
    slots: core.slots,
    front: core.front,
    playing: core.playing,
    speed,
    setSpeed,
    durations,
    /** Length of the whole clip, and of the minutes before the one on screen. */
    total,
    before,
    active,
    activate,
    setAllCameras,
    reduced,
    focused,
    setFocused,
    waiting,
    aspect,
    togglePlay,
    skip,
    seekToGlobal,
    locate,
    busy,
    register,
    onVideoEvent,
  }
}

export type ClipPlayer = ReturnType<typeof useClipPlayer>

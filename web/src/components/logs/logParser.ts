export type LogLevel = "error" | "warning" | "success" | "info" | "debug" | "default"

export interface ParsedLine {
  date: string // e.g. "Mar 20"
  time: string
  tag: string
  message: string
  level: LogLevel
  raw: string
  fullTs: number // full timestamp in ms, used to detect clock jumps
}

// Matches: "Day DD Mon HH:MM:SS TZ YYYY:" at the start of a line
// Captures: (DD) (Mon) (HH:MM:SS) (YYYY)
const TIMESTAMP_RE =
  /^[A-Z][a-z]{2}\s+(\d{1,2})\s+([A-Z][a-z]{2})\s+(\d{2}:\d{2}:\d{2})\s+\w+\s+(\d{4}):\s*/

const MONTH_INDEX: Record<string, number> = {
  Jan: 0, Feb: 1, Mar: 2, Apr: 3, May: 4, Jun: 5,
  Jul: 6, Aug: 7, Sep: 8, Oct: 9, Nov: 10, Dec: 11,
}

// Matches a [tag] prefix after the timestamp
const TAG_RE = /^\[([^\]]+)\]\s*/

function classifyLevel(message: string, tag: string): LogLevel {
  // Away-mode lines are always amber/orange regardless of keywords
  if (tag === "away-mode") return "warning"

  const lower = message.toLowerCase()

  // Errors — check first so "failed" always wins over softer matches
  if (
    lower.includes("error") ||
    lower.includes("failed") ||
    lower.includes("giving up") ||
    lower.includes("unable to") ||
    lower.includes("killed") ||
    lower.includes("fatal") ||
    lower.includes("connection dead")
  )
    return "error"

  // Warnings
  if (
    lower.includes("warning") ||
    lower.includes("warn") ||
    lower.includes("retrying") ||
    lower.includes("retry") ||
    lower.includes("timed out") ||
    lower.includes("timeout") ||
    lower.includes("stale lock") ||
    lower.includes("still running") ||
    lower.includes("still processing") ||
    lower.includes("unreachable") ||
    lower.includes("skipping") ||
    lower.includes("falling back") ||
    lower.includes("network lost") ||
    lower.includes("not mounted") ||
    lower.includes("not supported") ||
    lower.includes("not enabled") ||
    lower.includes("incomplete") ||
    lower.includes("discarding") ||
    lower.includes("cycling wifi")
  )
    return "warning"

  // Success
  if (
    lower.includes("nudge ok") ||
    lower.includes("success") ||
    lower.includes("enabled") ||
    lower.includes("disabled") ||
    lower.includes("restored") ||
    lower.includes("complete") ||
    lower.includes("finished") ||
    lower.includes("started") ||
    lower.includes("ready") ||
    lower.includes("acquired") ||
    lower.includes("mounted") ||
    lower.includes("unmounted") ||
    lower.includes("connected") ||
    lower.includes("disconnected") ||
    lower.includes("reachable") ||
    lower.includes("reconnected") ||
    lower.includes("took snapshot") ||
    lower.includes("synced") ||
    lower.includes("done") ||
    lower.includes("up to date") ||
    lower.includes("trim complete") ||
    lower.includes("rebuilt") ||
    lower.includes("removed") ||
    lower.includes("time adjusted")
  )
    return "success"

  // Info (tagged lines or informational keywords)
  if (
    tag ||
    lower.includes("starting") ||
    lower.includes("archiving") ||
    lower.includes("processing") ||
    lower.includes("syncing") ||
    lower.includes("copying") ||
    lower.includes("checking") ||
    lower.includes("running") ||
    lower.includes("disabling") ||
    lower.includes("restoring") ||
    lower.includes("queued") ||
    lower.includes("waiting") ||
    lower.includes("trimming") ||
    lower.includes("cleaning") ||
    lower.includes("making") ||
    lower.includes("comparing") ||
    lower.includes("detecting") ||
    lower.includes("rebuilding") ||
    lower.includes("taking snapshot") ||
    lower.includes("ensuring") ||
    lower.includes("tearing down") ||
    lower.includes("temperature monitor") ||
    lower.includes("command sent") ||
    lower.includes("update available")
  )
    return "info"

  return "default"
}

export function parseLine(raw: string): ParsedLine {
  let rest = raw
  let date = ""
  let time = ""
  let tag = ""
  let fullTs = 0

  // Extract timestamp — captures (DD) (Mon) (HH:MM:SS) (YYYY)
  const tsMatch = rest.match(TIMESTAMP_RE)
  if (tsMatch) {
    const day = parseInt(tsMatch[1], 10)
    const month = MONTH_INDEX[tsMatch[2]] ?? 0
    const year = parseInt(tsMatch[4], 10)
    date = `${tsMatch[2]} ${tsMatch[1]}` // e.g. "Mar 20"
    time = tsMatch[3]
    const [h, m, s] = time.split(":").map(Number)
    fullTs = new Date(year, month, day, h, m, s).getTime()
    rest = rest.slice(tsMatch[0].length)
  }

  // Extract [tag]
  const tagMatch = rest.match(TAG_RE)
  if (tagMatch) {
    tag = tagMatch[1]
    rest = rest.slice(tagMatch[0].length)
  }

  // Trust tracing's explicit level over keywords such as "skipping" in summaries.
  const explicitLevel = rest.match(/^\s*(ERROR|WARN|INFO|DEBUG|TRACE)\s+/)
  const levels: Record<string, LogLevel> = {
    ERROR: "error", WARN: "warning", INFO: "info", DEBUG: "debug", TRACE: "debug",
  }
  if (explicitLevel) rest = rest.slice(explicitLevel[0].length)
  const message = rest
  const level = explicitLevel ? levels[explicitLevel[1]] : classifyLevel(message, tag)

  return { date, time, tag, message, level, raw, fullTs }
}


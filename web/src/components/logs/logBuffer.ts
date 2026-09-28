export interface LogWindow { content: string; start: number }

/** Keep a bounded window and its exact UTF-8 offset for older-page requests. */
export function boundLogWindow(content: string, start: number, keep: "newest" | "oldest" = "newest", maxLines = 2000, maxBytes = 512 * 1024): LogWindow {
  const encoder = new TextEncoder()
  const lines = content.split("\n")
  const count = lines.length - (content.endsWith("\n") ? 1 : 0)
  if (count > maxLines) {
    if (keep === "newest") {
      const prefix = lines.slice(0, count - maxLines).join("\n") + "\n"
      content = content.slice(prefix.length)
      start += encoder.encode(prefix).length
    } else content = lines.slice(0, maxLines).join("\n") + "\n"
  }
  const bytes = encoder.encode(content)
  if (bytes.length <= maxBytes) return { content, start }
  if (keep === "newest") {
    let cut = bytes.length - maxBytes
    while (cut < bytes.length && (bytes[cut] & 0xc0) === 0x80) cut++
    return { content: new TextDecoder().decode(bytes.subarray(cut)), start: start + cut }
  }
  let cut = maxBytes
  while (cut > 0 && (bytes[cut] & 0xc0) === 0x80) cut--
  return { content: new TextDecoder().decode(bytes.subarray(0, cut)), start }
}

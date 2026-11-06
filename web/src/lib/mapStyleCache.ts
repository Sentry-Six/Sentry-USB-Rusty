import type { StyleSpecification } from "maplibre-gl"

// Style JSON fetched once per URL, so a light/dark switch needs no network
// round trip. The map server's styles differ only in paint values, so
// MapLibre applies the switch as a diff and repaints without reloading tiles.
const styles = new Map<string, { json: StyleSpecification | null }>()

export function prefetchStyles(urls: string[], fetchImpl: typeof fetch = globalThis.fetch) {
  for (const url of urls) {
    if (styles.has(url)) continue
    const entry: { json: StyleSpecification | null } = { json: null }
    styles.set(url, entry)
    fetchImpl(url)
      .then(res => { if (!res.ok) throw new Error(`HTTP ${res.status}`); return res.json() })
      .then(json => { entry.json = json })
      // Offline (a Pi without internet): MapLibre fetches the URL itself later.
      .catch(() => { styles.delete(url) })
  }
}

/** The fetched style when it has arrived, else the URL for MapLibre to fetch. */
export function cachedStyle(url: string): StyleSpecification | string {
  return styles.get(url)?.json ?? url
}

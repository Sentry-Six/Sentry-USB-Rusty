// Generates icons.tsx from symbols.mjs. `npm run icons -- --check` verifies it.
// Glyphs come from the pinned npm dependency; generation needs no network.
import { readFileSync, writeFileSync, existsSync } from "node:fs"
import { join, dirname } from "node:path"
import { fileURLToPath } from "node:url"
import { SYMBOLS, FLIP, LUCIDE_ORIGIN } from "./symbols.mjs"

const HERE = dirname(fileURLToPath(import.meta.url))
const WEB = join(HERE, "..", "..")
const TARGET = join(WEB, "src", "components", "icons.tsx")

const check = process.argv.includes("--check")

/** PascalCase component name for a symbol: battery_full_alt → BatteryFullAltIcon */
export const componentName = (symbol) =>
  symbol
    .split("_")
    .map((part) => part[0].toUpperCase() + part.slice(1))
    .join("") + "Icon"

/** Mirror horizontally within the symbol's own viewBox. */
const mirror = (viewBox) => {
  const [minX, , width] = viewBox.split(/\s+/).map(Number)
  return `scale(-1,1) translate(${-(2 * minX + width)},0)`
}

function readSymbol(name) {
  // Preserve legacy glyphs not distributed by the pinned package.
  const override = join(HERE, "overrides", `${name}.svg`)
  const source = existsSync(override) ? override
    : fileURLToPath(import.meta.resolve(`@material-symbols/svg-400/outlined/${name}.svg`))
  if (!existsSync(source)) {
    throw new Error(`${name}: missing from the pinned Material Symbols package — check symbols.mjs`)
  }
  const svg = readFileSync(source, "utf8")
  const inner = svg.match(/<svg[^>]*>([\s\S]*)<\/svg>/)?.[1].trim()
  if (!inner?.startsWith("<path")) throw new Error(`${name}: unexpected SVG content`)
  // Preserve each glyph's viewBox; Material Symbols use more than one grid.
  const viewBox = svg.match(/viewBox="([^"]+)"/)[1]
  return { inner, viewBox }
}

const glyphs = {}
for (const symbol of SYMBOLS) glyphs[symbol] = readSymbol(symbol)

const components = SYMBOLS.map((symbol) => {
  const { inner, viewBox } = glyphs[symbol]
  const origin = LUCIDE_ORIGIN[symbol]?.length
    ? ` (was Lucide ${LUCIDE_ORIGIN[symbol].join("/")})`
    : ""
  const flipped = FLIP.has(symbol)
  const body = flipped
    ? `\n      <g transform="${mirror(viewBox)}">\n        ${inner}\n      </g>\n    `
    : `\n      ${inner}\n    `
  const flipNote = flipped ? " Mirrored so the battery terminal faces right." : ""
  return `/** Material Symbols \`${symbol}\`${origin}.${flipNote} */
export const ${componentName(symbol)} = (props: SVGProps<SVGSVGElement>) => (
  <svg viewBox="${viewBox}" width="24" height="24" fill="currentColor" {...props}>${body}</svg>
)`
})

const output = `// Code-generated — do not edit by hand.
// Run \`npm run icons\` to regenerate; add or remove symbols in
// scripts/icons/symbols.mjs.
//
// Google Material Symbols (outlined, weight 400), vendored as inline SVG so the
// UI ships no icon font and makes no network request. Sourced from
// @material-symbols/svg-400; Apache License 2.0 — see NOTICE.
//
// Components are named after the Material symbol they render, so a glyph can be
// looked up directly at https://fonts.google.com/icons. They take the same props
// as any <svg>, so Tailwind sizing classes (h-4 w-4) work as before.

import type { SVGProps } from "react"

${components.join("\n\n")}
`

if (check) {
  const current = existsSync(TARGET) ? readFileSync(TARGET, "utf8") : ""
  if (current.replace(/\r\n/g, "\n") === output) {
    console.log(`icons.tsx is up to date (${SYMBOLS.length} symbols)`)
  } else {
    console.error("icons.tsx is out of date — run `npm run icons`")
    process.exit(1)
  }
} else {
  writeFileSync(TARGET, output)
  console.log(`wrote src/components/icons.tsx — ${SYMBOLS.length} symbols`)
}

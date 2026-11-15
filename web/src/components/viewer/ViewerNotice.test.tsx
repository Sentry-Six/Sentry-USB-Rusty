import assert from "node:assert/strict"
import test from "node:test"
import { createElement } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import ViewerNotice from "./ViewerNotice.tsx"

const text = (props: Parameters<typeof ViewerNotice>[0]) =>
  renderToStaticMarkup(createElement(ViewerNotice, props)).replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").trim()

test("nothing to say when the Pi is idle and every camera is playing", () => {
  assert.equal(text({ archiving: null }), "")
})

test("archiving: a warning with the time left, which can be hidden", () => {
  assert.equal(
    text({ archiving: { etaSeconds: 3120 }, onDismiss: () => {} }),
    "Your Pi is archiving clips right now, so video may load slowly or stutter. It has about 52 minutes left.",
  )
  assert.equal(text({ archiving: { etaSeconds: null } }), "Your Pi is archiving clips right now, so video may load slowly or stutter.")
  assert.ok(renderToStaticMarkup(createElement(ViewerNotice, { archiving: { etaSeconds: null }, onDismiss: () => {} })).includes("Hide this notice"))
})

test("dropped to the front camera: one line, with the way back", () => {
  assert.equal(
    text({ archiving: null, reduced: true, onPlayAll: () => {} }),
    "The connection is slow, so only the front camera is playing. Play every camera",
  )
})

test("archiving and dropped to the front camera: said once, not as two warnings", () => {
  assert.equal(
    text({ archiving: { etaSeconds: 3120 }, reduced: true, onPlayAll: () => {}, onDismiss: () => {} }),
    "Your Pi is archiving clips right now, so only the front camera is playing. It has about 52 minutes left. Play every camera",
  )
})

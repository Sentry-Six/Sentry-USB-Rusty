import assert from "node:assert/strict"
import test from "node:test"
import { createElement } from "react"
import { renderToStaticMarkup } from "react-dom/server"
import { Window } from "happy-dom"
import { AdvancedStep } from "./steps/AdvancedStep.tsx"
import { ReviewStep } from "./steps/ReviewStep.tsx"

for (const { overall, system, expected, unit } of [
  { overall: "F", system: "C", expected: "68.0", unit: "°C" },
  { overall: "C", system: "F", expected: "154.4", unit: "°F" },
  { overall: "F", system: undefined, expected: "154.4", unit: "°F" },
]) {
  test(`wizard system thresholds use ${system ?? "inherited"} units with overall ${overall}`, () => {
    const data: Record<string, string> = { TEMPERATURE_UNIT: overall, TEMPERATURE_WARNING: "68000" }
    if (system !== undefined) data.SYSTEM_TEMPERATURE_UNIT = system
    const props = { data, onChange() {}, onBatchChange() {} }
    const win = new Window()
    try {
      win.document.body.innerHTML = renderToStaticMarkup(createElement(AdvancedStep, props))
      const warning = [...win.document.querySelectorAll("label")].find(label => label.textContent === "Warning Threshold")!.parentElement!
      assert.equal(warning.querySelector("input")!.value, expected)
      assert.match(warning.textContent, new RegExp(unit))
      const masterLabel = overall === "F" ? "Imperial" : "Metric"
      const masterButton = [...win.document.querySelectorAll("button")].find(button => button.textContent === masterLabel)!
      assert.match(masterButton.className, /bg-blue-500/, "the override must not change the overall measurement system")

      for (const storedThreshold of ["68000", "68.0"]) {
        win.document.body.innerHTML = renderToStaticMarkup(createElement(ReviewStep, {
          ...props, data: { ...data, TEMPERATURE_WARNING: storedThreshold },
        }))
        assert.match(win.document.body.textContent, new RegExp(`TEMPERATURE_WARNING${expected.replace(".", "\\.")}${unit}`))
        if (system !== undefined) {
          assert.match(win.document.body.textContent, new RegExp(`SYSTEM_TEMPERATURE_UNIT${system === "F" ? "Fahrenheit" : "Celsius"}`))
        }
      }
    } finally {
      win.close()
    }
  })
}

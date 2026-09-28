import { readdirSync } from "node:fs"
import { join } from "node:path"
import { spawnSync } from "node:child_process"
function tests(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name)
    return entry.isDirectory() ? tests(path) : /\.test\.tsx?$/.test(entry.name) ? [path] : []
  })
}
const result = spawnSync(process.execPath, ["--import", "tsx", "--test", ...tests("src").sort()], {
  stdio: "inherit",
})
if (result.error) console.error(result.error.message)
process.exit(result.status ?? 1)

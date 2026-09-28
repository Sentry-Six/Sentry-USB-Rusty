export function uploadRelativePath(file: Pick<File, "name" | "webkitRelativePath">): string {
  const relative = file.webkitRelativePath || file.name
  if (
    !relative ||
    relative.includes("\\") ||
    relative.includes("\0") ||
    relative.split("/").some((part) => !part || part === "." || part === "..")
  ) {
    throw new Error("Invalid upload path")
  }
  return relative
}

export async function responseError(response: Response, fallback: string): Promise<string> {
  const data = await response.json().catch(() => null)
  return typeof data?.error === "string" ? data.error : `${fallback} (${response.status})`
}

/** Older Pi releases have only /status. Remember that discovery for this page. */
export function createLivenessProbe() {
  let endpoint = "/api/health"
  return async (signal?: AbortSignal): Promise<Response> => {
    let response = await fetch(endpoint, { signal })
    if (response.status === 404 && endpoint === "/api/health") {
      endpoint = "/api/status"
      response = await fetch(endpoint, { signal })
    }
    return response
  }
}

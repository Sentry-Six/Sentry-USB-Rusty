import { createContext, useContext, useState, useEffect, useRef, type ReactNode } from "react"

type AuthState = "loading" | "authenticated" | "unauthenticated"
interface AuthContextValue {
  state: AuthState
  authRequired: boolean
  login: (username: string, password: string) => Promise<string | null>
  logout: () => Promise<void>
}
const AuthContext = createContext<AuthContextValue>({
  state: "loading", authRequired: false,
  login: async () => null, logout: async () => {},
})
export function useAuth() { return useContext(AuthContext) }

export function AuthProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<AuthState>("loading")
  const [authRequired, setAuthRequired] = useState(false)
  const generation = useRef(0)
  const activeCheck = useRef<AbortController | null>(null)
  const mutating = useRef(false)
  const mounted = useRef(false)

  useEffect(() => {
    mounted.current = true
    let stopped = false
    const checkAuth = () => {
      if (stopped || mutating.current || activeCheck.current) return
      const request = new AbortController()
      activeCheck.current = request
      const ticket = generation.current
      const current = () => !stopped && !request.signal.aborted && generation.current === ticket
      return fetch("/api/auth/check", { signal: request.signal })
        .then((response) => {
          if (!response.ok) throw new Error("Authentication check failed")
          return response.json()
        })
        .then((data) => {
          if (!current()) return
          setAuthRequired(data.auth_required)
          setState(data.authenticated || !data.auth_required ? "authenticated" : "unauthenticated")
        })
        .catch(() => {
          // API authorization still protects data during connectivity failures.
          if (current()) setState("authenticated")
        })
        .finally(() => { if (activeCheck.current === request) activeCheck.current = null })
    }
    void checkAuth()
    const interval = setInterval(checkAuth, 10_000)
    return () => {
      stopped = true
      mounted.current = false
      activeCheck.current?.abort()
      activeCheck.current = null
      clearInterval(interval)
    }
  }, [])

  const beginMutation = () => {
    generation.current++
    mutating.current = true
    activeCheck.current?.abort()
    activeCheck.current = null
    return generation.current
  }
  async function login(username: string, password: string): Promise<string | null> {
    const ticket = beginMutation()
    try {
      const response = await fetch("/api/auth/login", {
        method: "POST", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ username, password }),
      })
      if (response.ok) {
        if (mounted.current && generation.current === ticket) setState("authenticated")
        return null
      }
      const data = await response.json()
      return data.error || "Invalid username or password"
    } catch { return "Connection failed. Please try again." }
    finally { if (generation.current === ticket) mutating.current = false }
  }
  async function logout() {
    const ticket = beginMutation()
    try {
      await fetch("/api/auth/logout", { method: "POST" }).catch(() => {})
      if (mounted.current && generation.current === ticket) setState("unauthenticated")
    } finally { if (generation.current === ticket) mutating.current = false }
  }

  return <AuthContext.Provider value={{ state, authRequired, login, logout }}>{children}</AuthContext.Provider>
}

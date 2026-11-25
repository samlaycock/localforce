// localforce's own UI API (`/__localforce/api`) and the Salesforce REST API, which records are
// created and edited through so the page sees the same validation an app would.

import { stored } from "@/lib/storage"
import type { Info } from "@/lib/types"

export const API_VERSION = "62.0"

/** A Salesforce error: REST errors carry `errorCode`, DML results `statusCode`. */
export type SfError = {
  message: string
  errorCode?: string
  statusCode?: string
  fields?: string[]
}

export class ApiError extends Error {
  status: number
  data: unknown

  constructor(message: string, status: number, data: unknown) {
    super(message)
    this.status = status
    this.data = data
  }
}

/** Call the UI API. Errors carry the server's message (or Salesforce errors as `data`). */
export async function api<T>(
  path: string,
  init?: { method?: string; body?: unknown; signal?: AbortSignal }
): Promise<T> {
  const res = await fetch(`/__localforce/api${path}`, {
    method: init?.method ?? "GET",
    headers:
      init?.body === undefined ? {} : { "content-type": "application/json" },
    body: init?.body === undefined ? undefined : JSON.stringify(init.body),
    signal: init?.signal,
  })
  const text = await res.text()
  const data: unknown = text ? JSON.parse(text) : null
  if (!res.ok) throw new ApiError(errorMessage(data, res), res.status, data)
  return data as T
}

function errorMessage(data: unknown, res: Response) {
  if (Array.isArray(data)) {
    return (data as SfError[]).map((e) => e.message).join("\n")
  }
  if (data && typeof data === "object" && "error" in data) {
    return String(data.error)
  }
  return res.statusText || `HTTP ${res.status}`
}

// The REST API needs a session. The page starts one for the user it acts as.
let session: { token: string; userId: string } | null = null
let preferred: string | null = null

/** The user records are created and edited as: the saved choice, else the org's default. */
export function actingUser(info: Info) {
  const saved = stored("user")
  return info.users.some((u) => u.id === saved) ? saved! : info.defaultUser
}

export function setActingUser(userId: string) {
  preferred = userId
  session = null
}

/** The first time the page knows the users, act as the saved one. */
export function initActingUser(info: Info) {
  if (preferred === null) setActingUser(actingUser(info))
}

async function startSession() {
  const s = await api<{ accessToken: string; userId: string }>("/session", {
    method: "POST",
    body: { user: preferred },
  })
  session = { token: s.accessToken, userId: s.userId }
  return session
}

/** Call the Salesforce REST API as the acting user. Errors come back as the body, not thrown. */
export async function rest(
  method: string,
  path: string,
  body?: unknown,
  retried = false
): Promise<{ status: number; body: unknown }> {
  const { token } = session ?? (await startSession())
  const res = await fetch(`/services/data/v${API_VERSION}${path}`, {
    method,
    headers: {
      authorization: `Bearer ${token}`,
      // Marks the request as the UI's, so the API log can hide it.
      "x-localforce-ui": "1",
      ...(body === undefined ? {} : { "content-type": "application/json" }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  if (res.status === 401 && !retried) {
    session = null
    return rest(method, path, body, true)
  }
  const text = await res.text()
  return { status: res.status, body: text ? JSON.parse(text) : null }
}

/** Salesforce errors from a failed REST call, in one shape. */
export function sfErrors(body: unknown): SfError[] {
  if (Array.isArray(body)) return body as SfError[]
  return [{ message: JSON.stringify(body) }]
}

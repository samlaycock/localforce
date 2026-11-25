import {
  useRouteLoaderData,
  type ShouldRevalidateFunctionArgs,
} from "react-router"

import { api, initActingUser } from "@/lib/api"
import type { Info, ObjectSummary } from "@/lib/types"

/** What every page needs: the org's info and users, and its objects with their counts. */
export async function rootLoader() {
  const [info, objects] = await Promise.all([
    api<Info>("/info"),
    api<ObjectSummary[]>("/objects"),
  ])
  initActingUser(info)
  return { info, objects }
}

export function useRoot() {
  const data = useRouteLoaderData<typeof rootLoader>("root")
  if (!data) throw new Error("useRoot needs the root route's data")
  return data
}

/** The object a record id belongs to, by its key prefix. */
export function objectOfId(objects: ObjectSummary[], id: string) {
  return objects.find((o) => o.keyPrefix === id.slice(0, 3))
}

/**
 * A `shouldRevalidate` that reloads after a change (an explicit `revalidate()`, which keeps the
 * URL) or when one of `params` changes, but not on other navigations such as typing a search.
 */
export function reloadOnChangeOr(...params: string[]) {
  return ({
    currentUrl,
    nextUrl,
    currentParams,
    nextParams,
  }: ShouldRevalidateFunctionArgs) =>
    currentUrl.href === nextUrl.href ||
    params.some((p) => currentParams[p] !== nextParams[p])
}

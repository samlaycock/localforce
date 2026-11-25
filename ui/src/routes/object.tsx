import {
  Outlet,
  redirect,
  useLoaderData,
  useLocation,
  useNavigate,
  useRevalidator,
  type LoaderFunctionArgs,
} from "react-router"
import { toast } from "sonner"

import { ConfirmButton } from "@/components/confirm-button"
import { SourceBadge } from "@/components/source-badge"
import { Badge } from "@/components/ui/badge"
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { api, ApiError } from "@/lib/api"
import { reloadOnChangeOr } from "@/lib/root"
import { loadSchema } from "@/lib/schema"
import { store } from "@/lib/storage"

export async function objectLoader({ params }: LoaderFunctionArgs) {
  try {
    const schema = await loadSchema(params.object ?? "")
    store("object", schema.name)
    return schema
  } catch (e) {
    // An object that's gone (deleted, or from an old link): show Account instead.
    if (e instanceof ApiError && e.status === 404) {
      store("object", null)
      throw redirect("/data/Account")
    }
    throw e
  }
}

/** Reload the schema after changes and on another object, not while paging or searching. */
export const objectShouldRevalidate = reloadOnChangeOr("object")

type Tab = "records" | "fields" | "metadata"

export function ObjectView() {
  const schema = useLoaderData<typeof objectLoader>()
  const { pathname } = useLocation()
  const navigate = useNavigate()
  const { revalidate } = useRevalidator()
  const base = `/data/${schema.name}`
  const tab: Tab = pathname.endsWith("/fields")
    ? "fields"
    : pathname.endsWith("/metadata")
      ? "metadata"
      : "records"

  async function deleteObject() {
    try {
      await api(`/objects/${schema.name}`, { method: "DELETE" })
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e))
      return
    }
    toast(`${schema.name} deleted`)
    store("object", null)
    await navigate("/data/Account")
    await revalidate()
  }

  return (
    <section className="flex h-full min-w-0 flex-col">
      <title>{`${schema.labelPlural} · localforce`}</title>
      <header className="flex flex-col gap-1 px-6 pt-4">
        <div className="flex flex-wrap items-center gap-2">
          <h1 className="text-xl font-semibold tracking-tight">
            {schema.labelPlural}
          </h1>
          <code className="text-sm text-muted-foreground">{schema.name}</code>
          <Badge
            variant="outline"
            className="font-mono"
            title="Key prefix: record ids start with this"
          >
            {schema.keyPrefix}
          </Badge>
          <SourceBadge source={schema.source} />
          {schema.editable && (
            <ConfirmButton
              className="ml-auto"
              variant="destructive"
              size="sm"
              title={`Delete ${schema.name}?`}
              description={`${schema.name} and all ${(schema.count + schema.deletedCount).toLocaleString()} of its records are deleted. This can't be undone.`}
              action="Delete object"
              onConfirm={deleteObject}
            >
              Delete object
            </ConfirmButton>
          )}
        </div>
        {schema.description && (
          <p className="text-sm text-muted-foreground">{schema.description}</p>
        )}
        <Tabs
          value={tab}
          onValueChange={(t: Tab) =>
            void navigate(t === "records" ? base : `${base}/${t}`)
          }
          className="mt-2"
        >
          <TabsList variant="line">
            <TabsTrigger value="records">
              Records ({schema.count.toLocaleString()})
            </TabsTrigger>
            <TabsTrigger value="fields">
              Fields & relationships ({schema.fields.length})
            </TabsTrigger>
            <TabsTrigger value="metadata">SFDX metadata</TabsTrigger>
          </TabsList>
        </Tabs>
      </header>
      <div className="min-h-0 flex-1 border-t">
        <Outlet />
      </div>
    </section>
  )
}

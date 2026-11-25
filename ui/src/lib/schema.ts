import { api } from "@/lib/api"
import { stored } from "@/lib/storage"
import type { Field, Schema } from "@/lib/types"

// The object route and the records route both need an object's schema, and their loaders run
// together: they share one request.
const inflight = new Map<string, Promise<Schema>>()

export function loadSchema(name: string) {
  let schema = inflight.get(name)
  if (!schema) {
    schema = api<Schema>(`/objects/${encodeURIComponent(name)}`).finally(() =>
      inflight.delete(name)
    )
    inflight.set(name, schema)
  }
  return schema
}

export const fieldOf = (schema: Schema, name: string) =>
  schema.fields.find((f) => f.name === name)

/** The table's default columns: the name, a few required or custom fields, and when it changed. */
function defaultColumns(s: Schema) {
  const skip = new Set([
    "Id",
    "IsDeleted",
    "SystemModstamp",
    "CreatedById",
    "LastModifiedById",
    "CreatedDate",
    "LastModifiedDate",
    "OwnerId",
    s.nameField ?? "",
  ])
  if (s.fields.some((f) => f.name === "Name" && f.kind === "compound")) {
    skip.add("FirstName")
    skip.add("LastName")
  }
  const rest = s.fields
    .filter((f) => !skip.has(f.name) && f.describeType !== "textarea")
    .toSorted(
      (a, b) =>
        Number(b.required) - Number(a.required) ||
        Number(b.custom) - Number(a.custom)
    )
  return [
    ...(s.nameField ? [s.nameField] : []),
    ...rest.slice(0, 5).map((f) => f.name),
    "LastModifiedDate",
  ]
}

/** The table's columns: the ones chosen for this object, or the defaults. */
export function columns(s: Schema) {
  const saved = stored(`cols.${s.name}`)
    ?.split(",")
    .filter((n) => s.fields.some((f) => f.name === n))
  return saved?.length ? saved : defaultColumns(s)
}

/** The parent's name for a lookup, as a SOQL path: `Account.Name`. */
export function parentNamePath(f: Field | undefined) {
  return f?.relationshipName && f.referenceNameField
    ? `${f.relationshipName}.${f.referenceNameField}`
    : null
}

/** SOQL select paths for columns: lookups also fetch the parent's name. */
export function selectPaths(s: Schema, cols: string[]) {
  return cols.flatMap((c) => {
    const parent = parentNamePath(fieldOf(s, c))
    return parent ? [c, parent] : [c]
  })
}

/** A lookup's parent name from a record: `rec.Account.Name`. */
export function parentName(
  rec: Record<string, unknown> | null,
  f: Field
): string | null {
  if (!rec || !f.relationshipName || !f.referenceNameField) return null
  const parent = rec[f.relationshipName] as Record<string, unknown> | null
  const name = parent?.[f.referenceNameField]
  return typeof name === "string" ? name : null
}

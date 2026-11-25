import { CopyIcon, XIcon } from "lucide-react"
import * as React from "react"
import {
  Link,
  useLoaderData,
  useLocation,
  useNavigate,
  useRevalidator,
  useSearchParams,
  type LoaderFunctionArgs,
} from "react-router"
import { toast } from "sonner"

import { ConfirmButton } from "@/components/confirm-button"
import {
  FieldControl,
  fromInput,
  toInput,
  type InputValue,
} from "@/components/field-control"
import { FieldValue } from "@/components/field-value"
import { RecordLink } from "@/components/record-link"
import { SfErrors } from "@/components/sf-errors"
import { Badge } from "@/components/ui/badge"
import { Button, buttonVariants } from "@/components/ui/button"
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "@/components/ui/empty"
import {
  Field as FormField,
  FieldDescription,
  FieldGroup,
  FieldLabel,
  FieldLegend,
  FieldSet,
} from "@/components/ui/field"
import { ScrollArea } from "@/components/ui/scroll-area"
import { api, rest, sfErrors, type SfError } from "@/lib/api"
import { soqlString, text } from "@/lib/format"
import { loadSchema, parentName, selectPaths } from "@/lib/schema"
import type { Field, QueryResult, Related, Schema, SfRecord } from "@/lib/types"
import { cn } from "@/lib/utils"

export async function recordLoader({ params, request }: LoaderFunctionArgs) {
  const schema = await loadSchema(params.object ?? "")
  const id = params.id ?? ""
  const paths = selectPaths(
    schema,
    schema.fields.map((f) => f.name)
  )
  // `all`: records in the recycle bin open too.
  const res = await api<QueryResult>("/query", {
    method: "POST",
    body: {
      soql: `SELECT ${paths.join(", ")} FROM ${schema.name} WHERE Id = ${soqlString(id)}`,
      all: true,
    },
    signal: request.signal,
  })
  const record = (res.records[0] as SfRecord | undefined) ?? null
  const related =
    record && schema.childRelationships.length
      ? await api<Related[]>(
          `/records/${schema.name}/${record.Id}/related`
        ).catch(() => [])
      : []
  return { schema, id, record, related }
}

export async function newRecordLoader({ params }: LoaderFunctionArgs) {
  const schema = await loadSchema(params.object ?? "")
  return { schema, id: null, record: null, related: [] as Related[] }
}

export function RecordDrawer() {
  const { schema, id, record, related } = useLoaderData<typeof recordLoader>()
  const { search, key } = useLocation()
  if (id && !record) {
    return (
      <Empty className="h-full">
        <EmptyHeader>
          <EmptyTitle>Not found</EmptyTitle>
          <EmptyDescription>
            No {schema.label} with id {id}.
          </EmptyDescription>
        </EmptyHeader>
        <EmptyContent>
          <Link
            to={{ pathname: `/data/${schema.name}`, search }}
            className={buttonVariants({ variant: "outline", size: "sm" })}
          >
            Close
          </Link>
        </EmptyContent>
      </Empty>
    )
  }
  // A fresh form for each record, and once it's saved (its modstamp changes); for each new one
  // too (a clone starts from its values).
  return (
    <RecordForm
      key={
        record ? `${record.Id}-${text(record.SystemModstamp)}` : `new-${key}`
      }
      schema={schema}
      record={record}
      related={related}
    />
  )
}

type Group = "main" | "derived" | "system"

function RecordForm({
  schema,
  record,
  related,
}: {
  schema: Schema
  record: SfRecord | null
  related: Related[]
}) {
  const [params] = useSearchParams()
  const { search, state } = useLocation()
  const navigate = useNavigate()
  const { revalidate } = useRevalidator()
  const isNew = record === null
  const deleted = record?.IsDeleted === true
  const base = `/data/${schema.name}`

  const editable = (f: Field) =>
    !deleted && (isNew ? f.createable : f.updateable)

  // The form starts from the record; a new one from a clone's values, the related list's
  // parent, and each field's default.
  const [initial] = React.useState(() => {
    const draft: Record<string, unknown> = {
      ...(state as { draft?: Record<string, unknown> } | null)?.draft,
    }
    const filterField = params.get("field")
    if (isNew && filterField) draft[filterField] = params.get("value")
    const values: Record<string, InputValue> = {}
    for (const f of schema.fields) {
      if (!editable(f)) continue
      values[f.name] = toInput(
        f,
        record ? record[f.name] : (draft[f.name] ?? f.default ?? null)
      )
    }
    return values
  })
  const [values, setValues] = React.useState(initial)
  const [errors, setErrors] = React.useState<SfError[]>([])
  const [busy, setBusy] = React.useState(false)
  const changed = schema.fields.filter(
    (f) => f.name in values && values[f.name] !== initial[f.name]
  )
  const invalid = new Set(errors.flatMap((e) => e.fields ?? []))

  async function save() {
    const body: Record<string, unknown> = {}
    for (const f of isNew ? schema.fields : changed) {
      if (!(f.name in values)) continue
      const v = fromInput(f, values[f.name])
      if (!isNew || (v !== null && v !== "")) body[f.name] = v
    }
    setBusy(true)
    const res = isNew
      ? await rest("POST", `/sobjects/${schema.name}`, body)
      : await rest("PATCH", `/sobjects/${schema.name}/${record.Id}`, body)
    setBusy(false)
    if (res.status >= 400) {
      setErrors(sfErrors(res.body))
      return
    }
    setErrors([])
    toast(isNew ? `${schema.label} created` : `${schema.label} saved`)
    await revalidate()
    if (isNew) {
      const created = res.body as { id: string }
      await navigate({ pathname: `${base}/${created.id}`, search })
    }
  }

  async function remove() {
    const res = await rest("DELETE", `/sobjects/${schema.name}/${record!.Id}`)
    if (res.status >= 400) {
      setErrors(sfErrors(res.body))
      return
    }
    toast(`${schema.label} deleted`)
    await navigate({ pathname: base, search })
    await revalidate()
  }

  async function restore() {
    try {
      await api(`/records/${schema.name}/${record!.Id}/undelete`, {
        method: "POST",
      })
    } catch (e) {
      setErrors([{ message: e instanceof Error ? e.message : String(e) }])
      return
    }
    toast(`${schema.label} restored`)
    await revalidate()
  }

  /** A new record starting from this one's values, except the unique ones. */
  function clone() {
    const draft: Record<string, unknown> = {}
    for (const f of schema.fields) {
      if (
        f.createable &&
        !f.unique &&
        !f.externalId &&
        record?.[f.name] != null
      ) {
        draft[f.name] = record[f.name]
      }
    }
    void navigate({ pathname: `${base}/new`, search }, { state: { draft } })
  }

  const groups: Record<Group, Field[]> = { main: [], derived: [], system: [] }
  for (const f of schema.fields) {
    if (isNew && !editable(f)) continue
    const group: Group =
      f.kind === "system"
        ? "system"
        : ["compound", "autonumber", "formula"].includes(f.kind)
          ? "derived"
          : "main"
    groups[group].push(f)
  }
  const title = isNew ? `New ${schema.label}` : recordName(schema, record)

  const row = (f: Field) => (
    <FormField key={f.name} data-invalid={invalid.has(f.name) || undefined}>
      <FieldLabel htmlFor={`field-${f.name}`} title={f.description ?? f.name}>
        {f.label}
        {editable(f) && f.required && f.describeType !== "boolean" && (
          <span className="text-destructive">*</span>
        )}
        <code className="text-xs font-normal text-muted-foreground">
          {f.name}
        </code>
      </FieldLabel>
      {editable(f) ? (
        <FieldControl
          field={f}
          id={`field-${f.name}`}
          value={values[f.name]}
          onChange={(v) =>
            setValues((current) => ({ ...current, [f.name]: v }))
          }
          invalid={invalid.has(f.name)}
          parentLabel={parentName(record, f)}
        />
      ) : (
        <ReadOnly field={f} record={record} />
      )}
      {f.description && editable(f) && (
        <FieldDescription>{f.description}</FieldDescription>
      )}
    </FormField>
  )

  return (
    <div className="flex h-full flex-col">
      <header className="flex flex-col gap-2 border-b px-5 py-3">
        <div className="flex items-start gap-2">
          <h2 className="min-w-0 flex-1 text-lg font-semibold break-words">
            {title}
          </h2>
          <Link
            to={{ pathname: base, search }}
            className={buttonVariants({ variant: "ghost", size: "icon-sm" })}
            title="Close"
          >
            <XIcon />
            <span className="sr-only">Close</span>
          </Link>
        </div>
        {record ? (
          <div className="flex items-center gap-2">
            <Button
              variant="ghost"
              size="xs"
              className="font-mono"
              title="Copy the id"
              onClick={() =>
                void navigator.clipboard
                  .writeText(record.Id)
                  .then(() => toast("Copied"))
              }
            >
              {record.Id}
              <CopyIcon data-icon="inline-end" />
            </Button>
            {deleted && <Badge variant="destructive">In recycle bin</Badge>}
          </div>
        ) : (
          <p className="text-xs text-muted-foreground">
            Fields marked * are required.
          </p>
        )}
        <div className="flex flex-wrap gap-2">
          {deleted ? (
            <Button size="sm" onClick={() => void restore()}>
              Restore from recycle bin
            </Button>
          ) : (
            <>
              <Button
                size="sm"
                disabled={busy || (!isNew && !changed.length)}
                onClick={() => void save()}
              >
                {isNew ? "Create" : "Save"}
              </Button>
              {!isNew && (
                <>
                  <Button variant="outline" size="sm" onClick={clone}>
                    Clone
                  </Button>
                  <ConfirmButton
                    variant="destructive"
                    size="sm"
                    title={`Delete this ${schema.label.toLowerCase()}?`}
                    description={deleteMessage(schema)}
                    action="Delete"
                    onConfirm={remove}
                  >
                    Delete
                  </ConfirmButton>
                </>
              )}
            </>
          )}
        </div>
      </header>
      <ScrollArea className="min-h-0 flex-1">
        <div className="flex flex-col gap-6 p-5">
          {errors.length > 0 && <SfErrors errors={errors} />}
          <FieldGroup>{groups.main.map(row)}</FieldGroup>
          {groups.derived.length > 0 && (
            <FieldSet>
              <FieldLegend>Calculated</FieldLegend>
              <FieldGroup>{groups.derived.map(row)}</FieldGroup>
            </FieldSet>
          )}
          {!isNew && related.length > 0 && (
            <FieldSet>
              <FieldLegend>Related</FieldLegend>
              <RelatedLists schema={schema} record={record} related={related} />
            </FieldSet>
          )}
          {groups.system.length > 0 && (
            <FieldSet>
              <FieldLegend>System</FieldLegend>
              <FieldGroup>{groups.system.map(row)}</FieldGroup>
            </FieldSet>
          )}
        </div>
      </ScrollArea>
    </div>
  )
}

/** A record's name, or its id when it has none. */
const recordName = (schema: Schema, record: SfRecord) =>
  (schema.nameField && text(record[schema.nameField])) || record.Id

function deleteMessage(schema: Schema) {
  const children = schema.childRelationships
    .filter((c) => c.cascadeDelete)
    .map((c) => c.name)
  return `It moves to the recycle bin.${children.length ? ` Its ${children.join(", ")} go with it.` : ""}`
}

/** A value that can't be edited here: a lookup links to its parent, a formula shows itself. */
function ReadOnly({
  field: f,
  record,
}: {
  field: Field
  record: SfRecord | null
}) {
  const value = record?.[f.name]
  return (
    <div className="min-h-8 py-1.5 text-sm" title={f.formula ?? undefined}>
      {f.describeType === "reference" && typeof value === "string" ? (
        <RecordLink id={value} label={parentName(record, f)} />
      ) : (
        <FieldValue field={f} value={value} />
      )}
    </div>
  )
}

/** Each child relationship with its count, linking to the children. */
function RelatedLists({
  schema,
  record,
  related,
}: {
  schema: Schema
  record: SfRecord
  related: Related[]
}) {
  const label = recordName(schema, record)
  return (
    <div className="flex flex-col gap-1">
      {related.map((r) => (
        <Link
          key={`${r.childObject}.${r.field}`}
          to={{
            pathname: `/data/${r.childObject}`,
            search: `?${new URLSearchParams({ field: r.field, value: record.Id, label })}`,
          }}
          className={cn(
            buttonVariants({ variant: "ghost", size: "sm" }),
            "w-full justify-between font-normal"
          )}
        >
          <span className="truncate">
            {r.relationship}{" "}
            <span className="text-muted-foreground">
              ({r.childObject}.{r.field})
            </span>
          </span>
          <Badge variant="secondary">{r.count.toLocaleString()}</Badge>
        </Link>
      ))}
    </div>
  )
}
